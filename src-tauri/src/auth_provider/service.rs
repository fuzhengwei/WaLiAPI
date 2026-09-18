use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    auth_provider::{
        AuthAccountSummary, AuthenticatedLogin, LoginRuntime, LoginTarget, ProviderError,
        ProviderKind, ProviderLoginContext, ProviderPayload, ProviderRegistry, ProviderRequest,
        ReplacementContext,
    },
    db::{
        models::{AuthAccount, AuthAccountUpsert, ModelStates},
        repository::Repository,
    },
};

/// Injectable time source: refresh decisions are deterministic in tests and do
/// not require a background timer in the service itself.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Repository orchestration around generic providers.  One mutex per account
/// serializes refresh-token rotation.  The lock is held through re-read and
/// persistence so concurrent callers cannot overwrite a newer refresh token.
pub struct AuthService {
    repository: Arc<Repository>,
    registry: ProviderRegistry,
    clock: Arc<dyn Clock>,
    refresh_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl AuthService {
    pub fn new(repository: Arc<Repository>, registry: ProviderRegistry) -> Self {
        Self::with_clock(repository, registry, Arc::new(SystemClock))
    }

    pub fn with_clock(
        repository: Arc<Repository>,
        registry: ProviderRegistry,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            repository,
            registry,
            clock,
            refresh_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Compatibility entrypoint used by legacy synchronou commands and import.
    /// It resolves `authenticate` + `persist_authenticated` for a brand-new
    /// account; interactive replacement logins go through the session runner
    /// so the commit gate serializes cancel with the DB write.
    pub async fn login(
        &self,
        kind: ProviderKind,
        runtime: &dyn LoginRuntime,
    ) -> Result<AuthAccountSummary, ProviderError> {
        let authenticated = self.authenticate(kind, LoginTarget::New, runtime).await?;
        self.persist_authenticated(authenticated).await
    }

    /// Run the interactive provider login without touching the database.
    ///
    /// Replacement targets are resolved here: the service loads the existing
    /// local account, validates its provider, and hands the provider a
    /// sanitized [`ReplacementContext`].  The provider itself must refuse to
    /// start OAuth if, e.g. a Kimi payload carries no usable `device_id`.
    pub async fn authenticate(
        &self,
        kind: ProviderKind,
        target: LoginTarget,
        runtime: &dyn LoginRuntime,
    ) -> Result<AuthenticatedLogin, ProviderError> {
        let login_method = crate::auth_provider::spec::provider_spec(&kind)
            .map(|spec| spec.login_mode)
            .ok_or_else(|| ProviderError::UnknownProvider {
                provider: kind.to_string(),
            })?;
        self.authenticate_with_method(kind, target, login_method, runtime)
            .await
    }

    pub async fn authenticate_with_method(
        &self,
        kind: ProviderKind,
        target: LoginTarget,
        login_method: crate::auth_provider::AuthLoginMode,
        runtime: &dyn LoginRuntime,
    ) -> Result<AuthenticatedLogin, ProviderError> {
        let provider = self.registry.get(&kind)?;
        let replacement = match &target {
            LoginTarget::New => None,
            LoginTarget::Replace { local_account_id } => {
                let account = self.get_account(local_account_id).await?;
                if account.provider != kind.to_string() {
                    // Never let a login from one provider overwrite another.
                    return Err(ProviderError::InvalidPayload);
                }
                let previous_payload = Self::payload_for(&account)?;
                let previous_attributes =
                    serde_json::from_str(&account.attributes_json).unwrap_or(Value::Null);
                Some(ReplacementContext {
                    local_account_id: local_account_id.clone(),
                    provider_account_id: account.account_id.clone(),
                    previous_payload,
                    previous_attributes,
                })
            }
        };
        let context = ProviderLoginContext {
            login_method,
            replacement,
        };
        let result = provider.login(&context, runtime).await?;
        Ok(AuthenticatedLogin {
            replacement: context.replacement,
            kind,
            result,
        })
    }

    /// Persist an authenticated login, either as a new account or as a
    /// locked in-place replacement that atomically invalidates old models.
    pub async fn persist_authenticated(
        &self,
        authenticated: AuthenticatedLogin,
    ) -> Result<AuthAccountSummary, ProviderError> {
        let AuthenticatedLogin {
            kind,
            result,
            replacement,
        } = authenticated;
        match replacement {
            None => self.upsert_login_result(kind, result).await,
            Some(replacement) => self.persist_replacement(kind, result, replacement).await,
        }
    }

    /// Replace an existing account's credentials under the same per-account
    /// mutex used by refresh-token rotation, with an optimistic precondition on
    /// `(id, provider, account_id)`.  The write atomically clears the model
    /// snapshot and last-sync timestamp so a route created against a changed
    /// model catalog can never use stale models; routing resumes only after a
    /// successful model sync.  A deleted account, a provider/device change, or
    /// a concurrent refresh that rotated `account_id` fails closed.
    async fn persist_replacement(
        &self,
        kind: ProviderKind,
        result: crate::auth_provider::LoginResult,
        replacement: ReplacementContext,
    ) -> Result<AuthAccountSummary, ProviderError> {
        // Re-login keeps the same provider account identity (Kimi: the same
        // `device_id`).  A different identity means the replacement boundary is
        // stale or the provider drifted; fail closed rather than create a row
        // under a new identity.
        if result.account_id != replacement.provider_account_id {
            return Err(ProviderError::InvalidPayload);
        }
        let lock = {
            let mut locks = self.refresh_locks.lock().await;
            locks
                .entry(replacement.local_account_id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        let outcome = self
            .persist_replacement_locked(kind, result, replacement)
            .await;
        drop(_guard);
        self.prune_idle_refresh_locks().await;
        outcome
    }

    /// FIX-22：锁清理——仅映射持有（无并发等待者）的条目移除；
    /// 已 clone 出去的（strong_count > 1）保留，长驻进程不再泄漏。
    async fn prune_idle_refresh_locks(&self) {
        let mut locks = self.refresh_locks.lock().await;
        locks.retain(|_, v| Arc::strong_count(v) > 1);
    }

    async fn persist_replacement_locked(
        &self,
        kind: ProviderKind,
        result: crate::auth_provider::LoginResult,
        replacement: ReplacementContext,
    ) -> Result<AuthAccountSummary, ProviderError> {
        // 调用方已持有该账号的 refresh 锁。
        //
        // Re-read and revalidate under the lock: a concurrent refresh may have
        // rotated the token, or the account may have been deleted meanwhile.
        // A missing account is a stale replacement boundary, not a generic
        // storage failure, so fail closed with the same category as other
        // precondition mismatches.
        let account = self
            .get_account(&replacement.local_account_id)
            .await
            .map_err(|_| ProviderError::InvalidPayload)?;
        if account.provider != kind.to_string()
            || account.account_id != replacement.provider_account_id
        {
            return Err(ProviderError::InvalidPayload);
        }
        let updated = self
            .repository
            .replace_auth_account(
                &replacement.local_account_id,
                &replacement.provider_account_id,
                &AuthAccountUpsert {
                    provider: kind.to_string(),
                    label: result.label,
                    account_id: result.account_id,
                    attributes: result.attributes,
                    payload: result.payload.into_value(),
                    last_refreshed_at: result.last_refreshed_at,
                    next_refresh_after: result.next_refresh_after,
                    next_retry_after: result.next_retry_after,
                },
            )
            .await
            .map_err(|_| ProviderError::InvalidPayload)?;
        AuthAccountSummary::from_account(&updated)
    }

    pub async fn import(
        &self,
        kind: ProviderKind,
        bytes: &[u8],
    ) -> Result<AuthAccountSummary, ProviderError> {
        let provider = self.registry.get(&kind)?;
        let result = provider.import(bytes).await?;
        self.upsert_login_result(kind, result).await
    }

    /// Format-aware import (codex / cpa / sub2api).  Single-account formats
    /// return one summary; sub2api files persist every importable account and
    /// report how many entries were skipped.  A file where nothing could be
    /// imported fails with `ImportFailed`.
    pub async fn import_all(
        &self,
        kind: ProviderKind,
        bytes: &[u8],
        format: Option<&str>,
    ) -> Result<(Vec<AuthAccountSummary>, usize), ProviderError> {
        let provider = self.registry.get(&kind)?;
        let outcome = provider.import_all(bytes, format).await?;
        if outcome.results.is_empty() {
            return Err(ProviderError::ImportFailed);
        }
        let mut summaries = Vec::with_capacity(outcome.results.len());
        for result in outcome.results {
            summaries.push(self.upsert_login_result(kind.clone(), result).await?);
        }
        Ok((summaries, outcome.skipped))
    }

    async fn upsert_login_result(
        &self,
        kind: ProviderKind,
        result: crate::auth_provider::LoginResult,
    ) -> Result<AuthAccountSummary, ProviderError> {
        let account = self
            .repository
            .upsert_by_provider_account_id(&AuthAccountUpsert {
                provider: kind.to_string(),
                label: result.label,
                account_id: result.account_id,
                attributes: result.attributes,
                payload: result.payload.into_value(),
                last_refreshed_at: result.last_refreshed_at,
                next_refresh_after: result.next_refresh_after,
                next_retry_after: result.next_retry_after,
            })
            .await
            .map_err(|_| ProviderError::Storage)?;
        AuthAccountSummary::from_account(&account)
    }

    /// Refresh an account only if its token is missing an expiry or expires in
    /// the next five minutes.  This is the normal request-path entrypoint.
    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AuthAccountSummary, ProviderError> {
        self.refresh_account_if_due(account_id, self.clock.now() + Duration::minutes(5))
            .await
    }

    pub async fn refresh_account_if_due(
        &self,
        account_id: &str,
        refresh_before: DateTime<Utc>,
    ) -> Result<AuthAccountSummary, ProviderError> {
        self.refresh_with_lock(account_id, refresh_before, false, true)
            .await
    }

    /// Used only by explicit user refresh and the one permitted 401 retry.
    pub async fn force_refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AuthAccountSummary, ProviderError> {
        self.refresh_with_lock(account_id, self.clock.now(), true, true)
            .await
    }

    async fn refresh_with_lock(
        &self,
        account_id: &str,
        refresh_before: DateTime<Utc>,
        force: bool,
        probe_quota: bool,
    ) -> Result<AuthAccountSummary, ProviderError> {
        let lock = {
            let mut locks = self.refresh_locks.lock().await;
            locks
                .entry(account_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        let outcome = self
            .refresh_locked(account_id, refresh_before, force, probe_quota)
            .await;
        drop(_guard);
        self.prune_idle_refresh_locks().await;
        outcome
    }

    /// 在已持有账号 refresh 锁的前提下执行刷新（由 refresh_with_lock 调用）。
    async fn refresh_locked(
        &self,
        account_id: &str,
        refresh_before: DateTime<Utc>,
        force: bool,
        probe_quota: bool,
    ) -> Result<AuthAccountSummary, ProviderError> {
        // Re-read while holding the lock: a preceding caller may have rotated
        // credentials, making this caller's refresh unnecessary.
        let account = self.get_account(account_id).await?;
        let payload = Self::payload_for(&account)?;
        if !force && !Self::needs_refresh(&payload, refresh_before) {
            return AuthAccountSummary::from_account(&account);
        }

        let provider = self.registry.provider_for_name(&account.provider)?;
        let refreshed = provider.refresh(&payload).await?;
        self.repository
            .update_tokens(
                &account.id,
                refreshed.payload.as_value(),
                refreshed.last_refreshed_at.as_deref(),
                refreshed.next_refresh_after.as_deref(),
                refreshed.next_retry_after.as_deref(),
            )
            .await
            .map_err(|_| ProviderError::Storage)?;
        if probe_quota {
            // A user-invoked refresh is a natural moment to refresh quota state too.
            // The refreshed payload is re-read below so the probe always carries the
            // newest access token.
            self.sync_quota(account_id).await;
        }
        let account = self.get_account(account_id).await?;
        AuthAccountSummary::from_account(&account)
    }

    pub async fn sync_models(&self, account_id: &str) -> Result<AuthAccountSummary, ProviderError> {
        // A due token is refreshed before the model snapshot so the listing is
        // never taken with an expired access token.  The refresh is lock-guarded
        // and re-reads the persisted payload, so a concurrent rotation cannot be
        // overwritten.  A refresh failure aborts before any model request.
        let account = self.get_account(account_id).await?;
        if Self::has_refresh_token(&Self::payload_for(&account)?) {
            self.refresh_with_lock(
                account_id,
                self.clock.now() + Duration::minutes(5),
                false,
                false,
            )
            .await?;
        }
        let account = self.get_account(account_id).await?;
        let payload = Self::payload_for(&account)?;
        let provider = self.registry.provider_for_name(&account.provider)?;
        let models = provider.list_models(&account, &payload).await?;
        self.repository
            .update_models_if_success(
                account_id,
                &ModelStates { version: 1, models },
                &self.clock.now().to_rfc3339(),
            )
            .await
            .map_err(|_| ProviderError::Storage)?;
        // Model sync is the login/import/refresh-triggered path; probe quota
        // alongside so a freshly added account shows its limits without waiting
        // for traffic or the 12h maintenance cycle.
        self.sync_quota(account_id).await;
        let account = self.get_account(account_id).await?;
        AuthAccountSummary::from_account(&account)
    }

    /// Strict quota refresh used by an explicit user action.  Capability is
    /// checked before any provider lookup or request.  A due credential is
    /// refreshed without its usual quota probe, ensuring this path calls the
    /// dedicated quota endpoint exactly once.  Persistence happens only after
    /// a valid quota state has been returned, so all failures preserve cache.
    pub async fn refresh_quota(
        &self,
        account_id: &str,
    ) -> Result<AuthAccountSummary, ProviderError> {
        let account = self.get_account(account_id).await?;
        let supports_quota = crate::auth_provider::spec::provider_spec_by_name(&account.provider)
            .is_some_and(|spec| spec.supports_quota);
        if !supports_quota {
            return Err(ProviderError::UnsupportedFeatures {
                pointer: "/quota".into(),
            });
        }

        self.refresh_with_lock(
            account_id,
            self.clock.now() + Duration::minutes(5),
            false,
            false,
        )
        .await?;

        self.probe_quota(account_id).await
    }

    async fn probe_quota(&self, account_id: &str) -> Result<AuthAccountSummary, ProviderError> {
        let account = self.get_account(account_id).await?;
        let supports_quota = crate::auth_provider::spec::provider_spec_by_name(&account.provider)
            .is_some_and(|spec| spec.supports_quota);
        if !supports_quota {
            return Err(ProviderError::UnsupportedFeatures {
                pointer: "/quota".into(),
            });
        }
        let payload = Self::payload_for(&account)?;
        let provider = self.registry.provider_for_name(&account.provider)?;
        let quota = provider
            .fetch_quota(&account, &payload)
            .await?
            .ok_or(ProviderError::Protocol)?;
        self.repository
            .update_quota(account_id, Some(&quota))
            .await
            .map_err(|_| ProviderError::Storage)?;

        let account = self.get_account(account_id).await?;
        AuthAccountSummary::from_account(&account)
    }

    /// Best-effort quota synchronization for login, model sync and scheduled
    /// maintenance.  Failures remain silent and preserve the last cached quota.
    pub async fn sync_quota(&self, account_id: &str) {
        let Ok(account) = self.get_account(account_id).await else {
            return;
        };
        let supports_quota = crate::auth_provider::spec::provider_spec_by_name(&account.provider)
            .is_some_and(|spec| spec.supports_quota);
        if !supports_quota {
            return;
        }
        if let Err(error) = self.probe_quota(account_id).await {
            tracing::warn!(account_id, %error, "failed to refresh auth account quota state");
        }
    }

    /// Perform one serialized maintenance pass.  It intentionally works from
    /// persisted accounts only: active credentials are refreshed only when
    /// close to expiry, while invalid credentials are retried after their
    /// persisted backoff.  A failure on one account never prevents the next
    /// account from being considered.
    pub async fn run_maintenance_cycle(&self) {
        let accounts = match self.repository.list_auth_accounts().await {
            Ok(accounts) => accounts,
            Err(_) => {
                tracing::warn!("failed to load auth accounts for maintenance");
                return;
            }
        };
        let now = self.clock.now();

        for account in accounts {
            if account.disabled != 0 {
                continue;
            }
            let payload = match Self::payload_for(&account) {
                Ok(payload) => payload,
                Err(_) => {
                    tracing::warn!(account_id = %account.id, "skipping auth account with invalid credential payload");
                    continue;
                }
            };
            let refresh_result = match account.status.as_str() {
                "active" => {
                    // Every active account syncs models on the 12h cycle.  The
                    // refresh inside `sync_models` is lazy: a fresh token skips
                    // it, a near-expiry token is refreshed first, and a refresh
                    // failure aborts before any model request (preserving the
                    // previous snapshot).  This keeps the model list — and thus
                    // routeability of new models — fresh even for imported /
                    // long-lived tokens (ADR-8, design §7 step 3).
                    if let Err(error) = self.sync_models(&account.id).await {
                        if error.is_payment_required() {
                            // A dead subscription can never be fixed by retrying
                            // on the maintenance cadence; take the account out
                            // of routing until the user resolves it.
                            tracing::warn!(
                                account_id = %account.id,
                                "auth account subscription is not usable; marking invalid"
                            );
                            self.schedule_maintenance_retry(&account.id, Some("payment_required"))
                                .await;
                        } else {
                            tracing::warn!(account_id = %account.id, "auth account model sync failed during maintenance: {error}");
                        }
                    }
                    continue;
                }
                "invalid"
                    if Self::has_refresh_token(&payload)
                        && Self::retry_is_due(account.next_retry_after.as_deref(), now) =>
                {
                    self.force_refresh_account(&account.id).await
                }
                _ => continue,
            };

            if refresh_result.is_err() {
                self.schedule_maintenance_retry(&account.id, None).await;
                continue;
            }

            if self.sync_models(&account.id).await.is_err() {
                // `sync_models` writes only after a successful provider result,
                // so an error naturally preserves the previous model snapshot.
                tracing::warn!(account_id = %account.id, "auth account model sync failed during maintenance");
            }
        }
    }

    /// Dispatch with a fresh persisted payload.  Provider-specific HTTP policy
    /// remains in the implementation; callers never parse `payload_json`.
    ///
    /// `is_stream` / `upstream_protocol` / `upstream_endpoint` are the trusted,
    /// immutable values frozen by RoutePlan.  A 401 single replay reuses the
    /// exact same values so the retry hits the identical fixed endpoint.
    pub async fn outbound(
        &self,
        account_id: &str,
        body: &serde_json::Value,
        headers: &reqwest::header::HeaderMap,
        is_stream: bool,
        upstream_protocol: &str,
        upstream_endpoint: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        // Refresh through the summary-only API, then load raw credentials only
        // inside this module for the provider call.
        if let Err(error) = self.refresh_account(account_id).await {
            // A due refresh is part of preparing this account for outbound use.
            // Never leave a rejected credential in the active route pool, and
            // keep the maintenance retry cadence consistent with its failure path.
            self.schedule_maintenance_retry(account_id, None).await;
            return Err(error);
        }
        let response = self
            .send_with_persisted_account(
                account_id,
                body,
                headers,
                is_stream,
                upstream_protocol,
                upstream_endpoint,
            )
            .await?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            self.mark_forbidden_if_needed(account_id, response.status())
                .await;
            self.persist_quota_if_present(account_id, &response).await;
            return Ok(response);
        }

        // A 401 is the one and only internal retry.  The force refresh shares
        // the same per-account lock used by the lazy path, then re-reads the
        // persisted payload before retrying so rotated refresh tokens cannot be
        // overwritten by concurrent callers.
        if self.force_refresh_account(account_id).await.is_err() {
            // Rejected credentials get the same backoff as a failed lazy
            // refresh, so the maintenance loop does not hammer the provider
            // on the very next pass.
            self.schedule_maintenance_retry(account_id, None).await;
            return Err(ProviderError::Unauthorized);
        }
        let retry = self
            .send_with_persisted_account(
                account_id,
                body,
                headers,
                is_stream,
                upstream_protocol,
                upstream_endpoint,
            )
            .await?;
        if retry.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.schedule_maintenance_retry(account_id, None).await;
            return Err(ProviderError::Unauthorized);
        }
        self.mark_forbidden_if_needed(account_id, retry.status())
            .await;
        self.persist_quota_if_present(account_id, &retry).await;
        Ok(retry)
    }

    async fn mark_forbidden_if_needed(&self, account_id: &str, status: reqwest::StatusCode) {
        if status == reqwest::StatusCode::FORBIDDEN {
            self.schedule_maintenance_retry(account_id, Some("permission_denied"))
                .await;
        }
    }

    async fn send_with_persisted_account(
        &self,
        account_id: &str,
        body: &serde_json::Value,
        headers: &reqwest::header::HeaderMap,
        is_stream: bool,
        upstream_protocol: &str,
        upstream_endpoint: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let account = self.get_account(account_id).await?;
        let payload = Self::payload_for(&account)?;
        let provider = self.registry.provider_for_name(&account.provider)?;
        provider
            .outbound(ProviderRequest {
                account: &account,
                payload: &payload,
                body,
                headers,
                is_stream,
                upstream_protocol,
                upstream_endpoint,
            })
            .await
    }

    async fn schedule_maintenance_retry(&self, account_id: &str, reason: Option<&str>) {
        let next_retry_after = (self.clock.now() + Duration::hours(12)).to_rfc3339();
        if self
            .repository
            .mark_invalid(account_id, Some(&next_retry_after), reason)
            .await
            .is_err()
        {
            tracing::warn!(
                account_id,
                "failed to schedule auth account maintenance retry"
            );
        }
    }

    async fn persist_quota_if_present(&self, account_id: &str, response: &reqwest::Response) {
        let Ok(account) = self.get_account(account_id).await else {
            return;
        };
        if account.provider != "codex" {
            return;
        }
        let previous = match account.quota_state() {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(account_id, "invalid persisted auth quota state");
                None
            }
        };
        let Some(quota) = crate::auth_provider::codex_backend::quota_from_headers(
            response.headers(),
            response.status(),
            previous.as_ref(),
            self.clock.now(),
        ) else {
            return;
        };
        if self
            .repository
            .update_quota(account_id, Some(&quota))
            .await
            .is_err()
        {
            // Quota observability must never turn an already successful upstream
            // response into a request failure.
            tracing::warn!(account_id, "failed to persist auth account quota state");
        }
    }

    async fn get_account(&self, account_id: &str) -> Result<AuthAccount, ProviderError> {
        self.repository
            .get_auth_account(account_id)
            .await
            .map_err(|_| ProviderError::Storage)
    }

    fn payload_for(account: &AuthAccount) -> Result<ProviderPayload, ProviderError> {
        serde_json::from_str(&account.payload_json)
            .map(ProviderPayload::new)
            .map_err(|_| ProviderError::InvalidPayload)
    }

    fn needs_refresh(payload: &ProviderPayload, refresh_before: DateTime<Utc>) -> bool {
        payload
            .expires_at()
            .is_none_or(|expires_at| expires_at <= refresh_before)
    }

    fn has_refresh_token(payload: &ProviderPayload) -> bool {
        payload
            .as_value()
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty())
    }

    fn retry_is_due(next_retry_after: Option<&str>, now: DateTime<Utc>) -> bool {
        next_retry_after
            .map(|value| {
                DateTime::parse_from_rfc3339(value)
                    .map(|value| value.with_timezone(&Utc) <= now)
                    .unwrap_or(false)
            })
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use reqwest::header::HeaderMap;
    use serde_json::json;

    use super::*;
    use crate::auth_provider::{
        LoginResult, LoginStep, LoginTarget, Provider, ProviderLoginContext, ProviderModels,
        RefreshedPayload,
    };
    use crate::db::models::QuotaState;

    const ACCESS: &str = "fixture-access-token";
    const REFRESH: &str = "fixture-refresh-token";
    const ID: &str = "fixture-id-token";

    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    struct FakeProvider {
        refreshes: AtomicUsize,
        quota_hits: AtomicUsize,
        operations: AtomicUsize,
        models_fail: bool,
        refresh_fails: bool,
        quota_fail: bool,
        quota_missing: bool,
    }
    #[async_trait]
    impl Provider for FakeProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Codex
        }
        async fn login(
            &self,
            _: &ProviderLoginContext,
            _: &dyn LoginRuntime,
        ) -> Result<LoginResult, ProviderError> {
            self.operations.fetch_add(1, Ordering::SeqCst);
            Err(ProviderError::LoginFailed)
        }
        async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
            self.operations.fetch_add(1, Ordering::SeqCst);
            Err(ProviderError::ImportFailed)
        }
        async fn refresh(&self, _: &ProviderPayload) -> Result<RefreshedPayload, ProviderError> {
            self.operations.fetch_add(1, Ordering::SeqCst);
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            if self.refresh_fails {
                return Err(ProviderError::Unauthorized);
            }
            Ok(RefreshedPayload {
                payload: ProviderPayload::new(
                    json!({"access_token": "new-access", "refresh_token": REFRESH, "id_token": ID, "expires_at": "2026-08-09T02:00:00Z"}),
                ),
                last_refreshed_at: Some("2026-08-09T00:00:00Z".into()),
                next_refresh_after: None,
                next_retry_after: None,
            })
        }
        async fn outbound(
            &self,
            _: ProviderRequest<'_>,
        ) -> Result<reqwest::Response, ProviderError> {
            self.operations.fetch_add(1, Ordering::SeqCst);
            Err(ProviderError::Retryable)
        }
        async fn list_models(
            &self,
            _account: &crate::db::models::AuthAccount,
            _payload: &ProviderPayload,
        ) -> Result<ProviderModels, ProviderError> {
            self.operations.fetch_add(1, Ordering::SeqCst);
            if self.models_fail {
                return Err(ProviderError::Retryable);
            }
            Ok(vec![])
        }
        async fn fetch_quota(
            &self,
            _account: &crate::db::models::AuthAccount,
            _payload: &ProviderPayload,
        ) -> Result<Option<QuotaState>, ProviderError> {
            self.operations.fetch_add(1, Ordering::SeqCst);
            self.quota_hits.fetch_add(1, Ordering::SeqCst);
            if self.quota_fail {
                return Err(ProviderError::Retryable);
            }
            if self.quota_missing {
                return Ok(None);
            }
            Ok(Some(QuotaState {
                version: 1,
                exceeded: false,
                reason: None,
                next_recover_at: None,
                backoff_level: 0,
                limits: vec![crate::db::models::QuotaLimit {
                    limit_id: "codex".into(),
                    limit_name: None,
                    primary: Some(crate::db::models::QuotaWindow {
                        used_percent: Some(25.0),
                        window_minutes: Some(10_080),
                        reset_at: None,
                    }),
                    secondary: None,
                    credits: None,
                }],
            }))
        }
    }

    async fn repository() -> Arc<Repository> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Arc::new(Repository::new(pool))
    }

    async fn account(repository: &Repository) -> AuthAccount {
        repository.upsert_by_provider_account_id(&AuthAccountUpsert {
            provider: "codex".into(), label: "Fixture".into(), account_id: "account-1".into(), attributes: json!({}),
            payload: json!({"access_token": ACCESS, "refresh_token": REFRESH, "id_token": ID, "expires_at": "2026-08-09T00:01:00Z"}),
            last_refreshed_at: None, next_refresh_after: None, next_retry_after: None,
        }).await.unwrap()
    }

    /// Records steps, device-authorization surfaces and an explicit cancel flag
    /// so login flows can be asserted without touching a real browser.
    #[derive(Default)]
    struct RecordingRuntime {
        steps: Mutex<Vec<String>>,
        device_authorizations: Mutex<Vec<String>>,
        cancelled: std::sync::atomic::AtomicBool,
    }
    #[async_trait]
    impl LoginRuntime for RecordingRuntime {
        async fn open_browser(&self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn set_step(&self, step: LoginStep) {
            self.steps.lock().unwrap().push(step.as_str().to_owned());
        }
        async fn present_device_authorization(
            &self,
            url: &str,
            _user_code: &str,
            _expires_at: Option<String>,
        ) -> Result<(), ProviderError> {
            self.device_authorizations
                .lock()
                .unwrap()
                .push(url.to_owned());
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
        async fn cancelled(&self) {
            while !self.is_cancelled() {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Provider whose login succeeds with a caller-chosen identity and payload.
    struct LoginProvider {
        kind: ProviderKind,
        login_account_id: String,
        login_payload: serde_json::Value,
        login_operations: AtomicUsize,
    }
    #[async_trait]
    impl Provider for LoginProvider {
        fn kind(&self) -> ProviderKind {
            self.kind.clone()
        }
        async fn login(
            &self,
            _context: &ProviderLoginContext,
            _runtime: &dyn LoginRuntime,
        ) -> Result<LoginResult, ProviderError> {
            self.login_operations.fetch_add(1, Ordering::SeqCst);
            Ok(LoginResult {
                account_id: self.login_account_id.clone(),
                label: "Codex".into(),
                attributes: json!({}),
                payload: ProviderPayload::new(self.login_payload.clone()),
                last_refreshed_at: Some("2026-08-09T03:00:00Z".into()),
                next_refresh_after: None,
                next_retry_after: None,
            })
        }
        async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
            Err(ProviderError::ImportFailed)
        }
        async fn refresh(&self, _: &ProviderPayload) -> Result<RefreshedPayload, ProviderError> {
            Err(ProviderError::Retryable)
        }
        async fn outbound(
            &self,
            _: ProviderRequest<'_>,
        ) -> Result<reqwest::Response, ProviderError> {
            Err(ProviderError::Retryable)
        }
        async fn list_models(
            &self,
            _account: &crate::db::models::AuthAccount,
            _payload: &ProviderPayload,
        ) -> Result<ProviderModels, ProviderError> {
            Ok(vec![])
        }
    }

    fn fresh_login_payload() -> serde_json::Value {
        json!({
            "access_token": "fresh-login-access",
            "refresh_token": "fresh-login-refresh",
            "expires_at": "2026-08-09T04:00:00Z"
        })
    }

    #[tokio::test]
    async fn concurrent_due_refreshes_are_single_flight_and_re_read_the_new_payload() {
        let repository = repository().await;
        let account = account(&repository).await;
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = Arc::new(AuthService::with_clock(
            repository,
            registry,
            Arc::new(FixedClock("2026-08-09T00:00:00Z".parse().unwrap())),
        ));

        let mut calls = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let service = service.clone();
            let id = account.id.clone();
            calls.spawn(async move { service.refresh_account(&id).await.unwrap() });
        }
        let mut payloads = Vec::new();
        while let Some(result) = calls.join_next().await {
            payloads.push(result.unwrap().expires_at);
        }
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
        assert!(payloads
            .iter()
            .all(|expires_at| expires_at.as_deref() == Some("2026-08-09T02:00:00Z")));
    }

    #[tokio::test]
    async fn due_model_sync_probes_quota_once_after_refresh() {
        let repository = repository().await;
        let account = account(&repository).await;
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = AuthService::with_clock(
            repository,
            registry,
            Arc::new(FixedClock("2026-08-09T00:00:00Z".parse().unwrap())),
        );

        service.sync_models(&account.id).await.unwrap();

        assert_eq!(fake.quota_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_provider_fails_before_any_provider_operation() {
        let repository = repository().await;
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = AuthService::new(repository, registry);
        let error = service
            .import(ProviderKind::from("unknown"), b"fixture")
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ProviderError::UnknownProvider {
                provider: "unknown".into()
            }
        );
        assert_eq!(fake.operations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn provider_errors_are_safe_to_display_and_debug() {
        let error = ProviderError::UnsupportedFeatures {
            pointer: "/metadata".into(),
        };
        let rendered = format!("{error} {error:?}");
        for secret in [ACCESS, REFRESH, ID] {
            assert!(!rendered.contains(secret));
        }
    }

    #[tokio::test]
    async fn account_summary_never_serializes_provider_credentials() {
        let repository = repository().await;
        let account = account(&repository).await;
        let summary = AuthAccountSummary::from_account(&account).unwrap();
        let rendered = serde_json::to_string(&summary).unwrap();
        for secret in [ACCESS, REFRESH, ID] {
            assert!(!rendered.contains(secret));
        }
        assert!(!rendered.contains("payload_json"));
    }

    #[tokio::test]
    async fn sync_models_refreshes_a_due_account_before_syncing() {
        let repository = repository().await;
        // expires_at 2026-08-09T00:01:00Z vs fixed now 2026-08-09T00:00:00Z:
        // refresh_before = now + 5min = 00:05Z, so 00:01Z is due for refresh.
        let account = account(&repository).await;
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = AuthService::with_clock(
            repository.clone(),
            registry,
            Arc::new(FixedClock("2026-08-09T00:00:00Z".parse().unwrap())),
        );

        let summary = service.sync_models(&account.id).await.unwrap();
        // The due account is refreshed once before the model snapshot is taken.
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
        // One refresh + one list_models + one quota probe: the listing runs
        // exactly once on the refreshed payload, then the quota probe follows.
        assert_eq!(fake.operations.load(Ordering::SeqCst), 3);
        assert_eq!(summary.expires_at.as_deref(), Some("2026-08-09T02:00:00Z"));
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        assert_eq!(
            stored.payload_json,
            serde_json::to_string(&json!({"access_token":"new-access","refresh_token":REFRESH,"id_token":ID,"expires_at":"2026-08-09T02:00:00Z"})).unwrap()
        );
    }

    #[tokio::test]
    async fn failed_model_sync_keeps_the_existing_snapshot_and_timestamp() {
        let repository = repository().await;
        let account = account(&repository).await;
        let old_models = ModelStates {
            version: 1,
            models: vec![crate::db::models::ModelState {
                id: "gpt-old".into(),
                status: "available".into(),
                unavailable: false,
                next_retry_after: None,
                last_error: None,
                protocol: None,
            }],
        };
        let old_sync = "2026-08-08T00:00:00Z";
        repository
            .update_models_if_success(&account.id, &old_models, old_sync)
            .await
            .unwrap();
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: true,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake);
        let service = AuthService::new(repository.clone(), registry);
        assert_eq!(
            service.sync_models(&account.id).await.unwrap_err(),
            ProviderError::Retryable
        );
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        assert_eq!(stored.model_states().unwrap(), old_models);
        assert_eq!(stored.last_models_sync_at.as_deref(), Some(old_sync));
    }

    #[tokio::test]
    async fn sync_quota_persists_probe_result() {
        let repository = repository().await;
        let account = account(&repository).await;
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake);
        let service = AuthService::new(repository.clone(), registry);

        service.sync_quota(&account.id).await;
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        let quota = stored.quota_state().unwrap().unwrap();
        assert!(!quota.exceeded);
        assert_eq!(quota.limits.len(), 1);
        assert_eq!(
            quota.limits[0].primary.as_ref().unwrap().window_minutes,
            Some(10_080)
        );
    }

    #[tokio::test]
    async fn sync_quota_silently_preserves_existing_on_probe_failure() {
        let repository = repository().await;
        let account = account(&repository).await;
        // Persist an existing quota first.
        let existing = QuotaState {
            version: 1,
            exceeded: true,
            reason: Some("quota".into()),
            next_recover_at: Some("2026-08-10T00:00:00Z".into()),
            backoff_level: 2,
            limits: vec![],
        };
        repository
            .update_quota(&account.id, Some(&existing))
            .await
            .unwrap();
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: true,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake);
        let service = AuthService::new(repository.clone(), registry);

        // The failed probe is silent: the account still reports the old quota
        // rather than being wiped or erroring.
        service.sync_quota(&account.id).await;
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        let quota = stored.quota_state().unwrap().unwrap();
        assert!(quota.exceeded);
        assert_eq!(quota.backoff_level, 2);
        assert_eq!(
            quota.next_recover_at.as_deref(),
            Some("2026-08-10T00:00:00Z")
        );
    }

    #[tokio::test]
    async fn refresh_quota_refreshes_due_token_then_probes_once_and_returns_persisted_state() {
        // Mutation caught: probing with a stale token, or probing both during
        // token refresh and again during the explicit quota refresh.
        let repository = repository().await;
        let account = account(&repository).await;
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = AuthService::with_clock(
            repository.clone(),
            registry,
            Arc::new(FixedClock("2026-08-09T00:00:00Z".parse().unwrap())),
        );

        let summary = service.refresh_quota(&account.id).await.unwrap();

        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(fake.quota_hits.load(Ordering::SeqCst), 1);
        assert_eq!(fake.operations.load(Ordering::SeqCst), 2);
        assert_eq!(summary.expires_at.as_deref(), Some("2026-08-09T02:00:00Z"));
        let returned = summary.quota.unwrap();
        assert_eq!(
            returned.limits[0].primary.as_ref().unwrap().used_percent,
            Some(25.0)
        );
        assert_eq!(
            repository
                .get_auth_account(&account.id)
                .await
                .unwrap()
                .quota_state()
                .unwrap()
                .unwrap(),
            returned
        );
    }

    #[tokio::test]
    async fn refresh_quota_with_fresh_token_skips_token_refresh_and_probes_once() {
        // Mutation caught: forcing token rotation for every explicit quota
        // refresh instead of honoring the five-minute lazy-refresh threshold.
        let repository = repository().await;
        let account = repository
            .upsert_by_provider_account_id(&AuthAccountUpsert {
                provider: "codex".into(),
                label: "Fresh fixture".into(),
                account_id: "account-fresh".into(),
                attributes: json!({}),
                payload: json!({
                    "access_token": ACCESS,
                    "refresh_token": REFRESH,
                    "id_token": ID,
                    "expires_at": "2026-08-09T00:06:00Z"
                }),
                last_refreshed_at: None,
                next_refresh_after: None,
                next_retry_after: None,
            })
            .await
            .unwrap();
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = AuthService::with_clock(
            repository.clone(),
            registry,
            Arc::new(FixedClock("2026-08-09T00:00:00Z".parse().unwrap())),
        );

        let summary = service.refresh_quota(&account.id).await.unwrap();

        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 0);
        assert_eq!(fake.quota_hits.load(Ordering::SeqCst), 1);
        assert_eq!(fake.operations.load(Ordering::SeqCst), 1);
        assert_eq!(summary.quota.unwrap().limits.len(), 1);
        assert!(repository
            .get_auth_account(&account.id)
            .await
            .unwrap()
            .quota_state()
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn refresh_quota_returns_error_and_preserves_existing_quota_on_probe_failure() {
        // Mutation caught: clearing or replacing cached quota before a failed
        // upstream request has produced a valid new value.
        let repository = repository().await;
        let account = account(&repository).await;
        let existing = QuotaState {
            version: 1,
            exceeded: true,
            reason: Some("cached".into()),
            next_recover_at: Some("2026-08-10T00:00:00Z".into()),
            backoff_level: 3,
            limits: vec![],
        };
        repository
            .update_quota(&account.id, Some(&existing))
            .await
            .unwrap();
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: true,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake);
        let service = AuthService::with_clock(
            repository.clone(),
            registry,
            Arc::new(FixedClock("2026-08-08T00:00:00Z".parse().unwrap())),
        );

        assert_eq!(
            service.refresh_quota(&account.id).await.unwrap_err(),
            ProviderError::Retryable
        );
        assert_eq!(
            repository
                .get_auth_account(&account.id)
                .await
                .unwrap()
                .quota_state()
                .unwrap()
                .unwrap(),
            existing
        );
    }

    #[tokio::test]
    async fn refresh_quota_rejects_missing_rate_limit_and_preserves_existing_quota() {
        // Mutation caught: treating a successful HTTP response without a
        // usable rate_limit as a successful refresh that wipes old state.
        let repository = repository().await;
        let account = account(&repository).await;
        let existing = QuotaState {
            version: 1,
            exceeded: false,
            reason: Some("cached".into()),
            next_recover_at: None,
            backoff_level: 1,
            limits: vec![],
        };
        repository
            .update_quota(&account.id, Some(&existing))
            .await
            .unwrap();
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: true,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake);
        let service = AuthService::with_clock(
            repository.clone(),
            registry,
            Arc::new(FixedClock("2026-08-08T00:00:00Z".parse().unwrap())),
        );

        assert_eq!(
            service.refresh_quota(&account.id).await.unwrap_err(),
            ProviderError::Protocol
        );
        assert_eq!(
            repository
                .get_auth_account(&account.id)
                .await
                .unwrap()
                .quota_state()
                .unwrap()
                .unwrap(),
            existing
        );
    }

    #[tokio::test]
    async fn refresh_quota_rejects_provider_without_quota_before_network() {
        // Mutation caught: looking up or calling an upstream provider before
        // enforcing the renderer-safe quota capability gate.
        let repository = repository().await;
        let account = repository
            .upsert_by_provider_account_id(&AuthAccountUpsert {
                provider: "kimi".into(),
                label: "Kimi".into(),
                account_id: "kimi-1".into(),
                attributes: json!({}),
                payload: json!({}),
                last_refreshed_at: None,
                next_refresh_after: None,
                next_retry_after: None,
            })
            .await
            .unwrap();
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: false,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = AuthService::new(repository, registry);

        assert_eq!(
            service.refresh_quota(&account.id).await.unwrap_err(),
            ProviderError::UnsupportedFeatures {
                pointer: "/quota".into()
            }
        );
        assert_eq!(fake.operations.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn failed_lazy_refresh_invalidates_account_before_any_outbound_request() {
        let repository = repository().await;
        let account = account(&repository).await;
        repository
            .update_models_if_success(
                &account.id,
                &ModelStates {
                    version: 1,
                    models: vec![crate::db::models::ModelState {
                        id: "gpt-test".into(),
                        status: "available".into(),
                        unavailable: false,
                        next_retry_after: None,
                        last_error: None,
                        protocol: None,
                    }],
                },
                "2026-08-09T00:00:00Z",
            )
            .await
            .unwrap();
        let fake = Arc::new(FakeProvider {
            refreshes: AtomicUsize::new(0),
            quota_hits: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            models_fail: false,
            refresh_fails: true,
            quota_fail: false,
            quota_missing: false,
        });
        let mut registry = ProviderRegistry::new();
        registry.register(fake.clone());
        let service = AuthService::with_clock(
            repository.clone(),
            registry,
            Arc::new(FixedClock("2026-08-09T00:00:00Z".parse().unwrap())),
        );

        assert_eq!(
            service
                .outbound(
                    &account.id,
                    &json!({"model": "gpt-test"}),
                    &HeaderMap::new(),
                    true,
                    "responses",
                    "responses"
                )
                .await
                .unwrap_err(),
            ProviderError::Unauthorized
        );
        assert_eq!(fake.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(fake.operations.load(Ordering::SeqCst), 1);

        let stored = repository.get_auth_account(&account.id).await.unwrap();
        assert_eq!(stored.status, "invalid");
        assert_eq!(
            stored.next_retry_after.as_deref(),
            Some("2026-08-09T12:00:00+00:00")
        );
        assert!(repository
            .list_route_accounts("2026-08-09T00:00:00Z")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn authenticate_runs_provider_without_touching_the_database() {
        let repository = repository().await;
        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Codex,
            login_account_id: "account-1".into(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone());
        let service = AuthService::new(repository.clone(), registry);
        let runtime = RecordingRuntime::default();

        let authenticated = service
            .authenticate(ProviderKind::Codex, LoginTarget::New, &runtime)
            .await
            .unwrap();
        assert_eq!(authenticated.result.account_id, "account-1");
        assert!(authenticated.replacement.is_none());
        // Nothing was written by authenticate itself.
        assert!(repository.list_auth_accounts().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn authenticate_resolves_replacement_context_from_existing_account() {
        let repository = repository().await;
        let account = account(&repository).await;
        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Codex,
            login_account_id: account.account_id.clone(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone());
        let service = AuthService::new(repository.clone(), registry);

        let authenticated = service
            .authenticate(
                ProviderKind::Codex,
                LoginTarget::Replace {
                    local_account_id: account.id.clone(),
                },
                &RecordingRuntime::default(),
            )
            .await
            .unwrap();
        let replacement = authenticated.replacement.expect("replacement context");
        assert_eq!(replacement.local_account_id, account.id);
        assert_eq!(replacement.provider_account_id, account.account_id);
        // The provider must see the prior credential material, not raw JSON.
        assert_eq!(
            replacement.previous_payload.as_value()["access_token"],
            ACCESS
        );
    }

    #[tokio::test]
    async fn authenticate_rejects_replacement_when_provider_mismatches() {
        let repository = repository().await;
        let account = account(&repository).await;
        // The login provider is registered under Kimi, but the persisted
        // account is a `codex` row: the replacement boundary must refuse before
        // any OAuth request.
        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Kimi,
            login_account_id: "".into(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone());
        let service = AuthService::new(repository.clone(), registry);

        let error = service
            .authenticate(
                ProviderKind::Kimi,
                LoginTarget::Replace {
                    local_account_id: account.id.clone(),
                },
                &RecordingRuntime::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::InvalidPayload);
        assert_eq!(
            provider.login_operations.load(Ordering::SeqCst),
            0,
            "provider must never be asked to login for a mismatched target"
        );
    }

    #[tokio::test]
    async fn persist_authenticated_new_account_writes_by_provider_kind() {
        let repository = repository().await;
        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Codex,
            login_account_id: "remote-1".into(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone());
        let service = AuthService::new(repository.clone(), registry);

        let authenticated = service
            .authenticate(
                ProviderKind::Codex,
                LoginTarget::New,
                &RecordingRuntime::default(),
            )
            .await
            .unwrap();
        let summary = service.persist_authenticated(authenticated).await.unwrap();

        assert_eq!(summary.provider, "codex");
        assert_eq!(summary.account_id, "remote-1");
        let stored = repository.get_auth_account(&summary.id).await.unwrap();
        assert_eq!(stored.provider, "codex");
    }

    #[tokio::test]
    async fn persist_replacement_updates_same_account_and_clears_model_snapshot() {
        let repository = repository().await;
        let account = account(&repository).await;
        // Give the account a stale model snapshot + sync time first.
        let old_models = ModelStates {
            version: 1,
            models: vec![crate::db::models::ModelState {
                id: "gpt-stale".into(),
                status: "available".into(),
                unavailable: false,
                next_retry_after: None,
                last_error: None,
                protocol: None,
            }],
        };
        repository
            .update_models_if_success(&account.id, &old_models, "2026-08-08T00:00:00Z")
            .await
            .unwrap();

        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Codex,
            login_account_id: account.account_id.clone(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone());
        let service = AuthService::new(repository.clone(), registry);

        let authenticated = service
            .authenticate(
                ProviderKind::Codex,
                LoginTarget::Replace {
                    local_account_id: account.id.clone(),
                },
                &RecordingRuntime::default(),
            )
            .await
            .unwrap();
        let summary = service.persist_authenticated(authenticated).await.unwrap();
        assert_eq!(summary.id, account.id, "replacement must reuse local id");

        let stored = repository.get_auth_account(&account.id).await.unwrap();
        // Fresh credentials replaced the old ones.
        assert!(stored.payload_json.contains("fresh-login-access"));
        // Model snapshot was atomically cleared and sync timestamp reset.
        assert_eq!(stored.model_states().unwrap().models.len(), 0);
        assert_eq!(stored.last_models_sync_at, None);
        // The cleared account is not routeable until a sync rewrites models.
        assert!(repository
            .list_route_accounts("2026-08-09T00:00:00Z")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn persist_replacement_fails_closed_when_account_was_deleted() {
        let repository = repository().await;
        let account = account(&repository).await;
        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Codex,
            login_account_id: account.account_id.clone(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        let service = AuthService::new(repository.clone(), registry);

        let authenticated = service
            .authenticate(
                ProviderKind::Codex,
                LoginTarget::Replace {
                    local_account_id: account.id.clone(),
                },
                &RecordingRuntime::default(),
            )
            .await
            .unwrap();
        repository.delete_auth_account(&account.id).await.unwrap();
        assert_eq!(
            service
                .persist_authenticated(authenticated)
                .await
                .unwrap_err(),
            ProviderError::InvalidPayload
        );
    }

    #[tokio::test]
    async fn persist_replacement_fails_closed_when_identity_changed() {
        let repository = repository().await;
        let account = account(&repository).await;
        // Login returns a different account id than the persisted one.
        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Codex,
            login_account_id: "other-identity".into(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        let service = AuthService::new(repository.clone(), registry);

        let authenticated = service
            .authenticate(
                ProviderKind::Codex,
                LoginTarget::Replace {
                    local_account_id: account.id.clone(),
                },
                &RecordingRuntime::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .persist_authenticated(authenticated)
                .await
                .unwrap_err(),
            ProviderError::InvalidPayload
        );
        // Original credentials are untouched.
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        assert!(stored.payload_json.contains("fixture-access-token"));
    }

    #[tokio::test]
    async fn replacement_then_forced_refresh_uses_new_refresh_token_not_stale() {
        // Provider that can BOTH log in with fresh credentials AND refresh,
        // echoing back which refresh token it was handed so a replacement can
        // be distinguished from a stale database credential.
        #[derive(Clone)]
        struct LoginAndRefresh {
            login_account_id: String,
            login_payload: serde_json::Value,
            refresh_saw: std::sync::Arc<Mutex<Option<String>>>,
            refreshed_access: String,
        }
        #[async_trait]
        impl Provider for LoginAndRefresh {
            fn kind(&self) -> ProviderKind {
                ProviderKind::Codex
            }
            async fn login(
                &self,
                _: &ProviderLoginContext,
                _: &dyn LoginRuntime,
            ) -> Result<LoginResult, ProviderError> {
                Ok(LoginResult {
                    account_id: self.login_account_id.clone(),
                    label: "Codex".into(),
                    attributes: json!({}),
                    payload: ProviderPayload::new(self.login_payload.clone()),
                    last_refreshed_at: Some("2026-08-09T03:00:00Z".into()),
                    next_refresh_after: None,
                    next_retry_after: None,
                })
            }
            async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
                Err(ProviderError::ImportFailed)
            }
            async fn refresh(
                &self,
                payload: &ProviderPayload,
            ) -> Result<RefreshedPayload, ProviderError> {
                let token = payload
                    .as_value()
                    .get("refresh_token")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                *self.refresh_saw.lock().unwrap() = Some(token.clone());
                Ok(RefreshedPayload {
                    payload: ProviderPayload::new(json!({
                        "access_token": self.refreshed_access,
                        "refresh_token": token,
                        "expires_at": "2099-01-01T00:00:00Z"
                    })),
                    last_refreshed_at: Some("2026-08-09T03:01:00Z".into()),
                    next_refresh_after: None,
                    next_retry_after: None,
                })
            }
            async fn outbound(
                &self,
                _: ProviderRequest<'_>,
            ) -> Result<reqwest::Response, ProviderError> {
                Err(ProviderError::Retryable)
            }
            async fn list_models(
                &self,
                _: &AuthAccount,
                _: &ProviderPayload,
            ) -> Result<ProviderModels, ProviderError> {
                Ok(vec![])
            }
        }
        let saw = std::sync::Arc::new(Mutex::new(None));
        let repository = repository().await;
        let account = account(&repository).await;
        let provider = Arc::new(LoginAndRefresh {
            login_account_id: account.account_id.clone(),
            login_payload: fresh_login_payload(), // refresh_token "fresh-login-refresh"
            refresh_saw: saw.clone(),
            refreshed_access: "post-refresh-access".into(),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider.clone());
        let service = Arc::new(AuthService::new(repository.clone(), registry));

        // Replacement persists fresh credentials.
        let authenticated = service
            .authenticate(
                ProviderKind::Codex,
                LoginTarget::Replace {
                    local_account_id: account.id.clone(),
                },
                &RecordingRuntime::default(),
            )
            .await
            .unwrap();
        service.persist_authenticated(authenticated).await.unwrap();

        // A forced refresh afterwards must hand the provider the NEW refresh
        // token (fresh-login-refresh), not something stale, and the resulting
        // access token must be the refreshed one.
        service.force_refresh_account(&account.id).await.unwrap();
        assert_eq!(
            saw.lock().unwrap().as_deref(),
            Some("fresh-login-refresh"),
            "refresh must use the replacement's refresh token"
        );
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        assert!(
            stored.payload_json.contains("post-refresh-access"),
            "forced refresh result was not persisted"
        );
    }

    #[tokio::test]
    async fn runtime_records_steps_and_device_authorization() {
        let repository = repository().await;
        let provider = Arc::new(LoginProvider {
            kind: ProviderKind::Codex,
            login_account_id: "remote-1".into(),
            login_payload: fresh_login_payload(),
            login_operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        let service = AuthService::new(repository, registry);

        let runtime = RecordingRuntime::default();
        let _ = service
            .authenticate(ProviderKind::Codex, LoginTarget::New, &runtime)
            .await;
        // The provider drives steps; assert the runtime surface is usable.
        runtime.set_step(LoginStep::Preparing).await;
        runtime
            .present_device_authorization("https://auth.example.test", "ABC-DEF", None)
            .await
            .unwrap();
        assert_eq!(*runtime.steps.lock().unwrap(), vec!["preparing"]);
        assert_eq!(
            *runtime.device_authorizations.lock().unwrap(),
            vec!["https://auth.example.test"]
        );
        assert!(!runtime.is_cancelled());
    }

    #[tokio::test]
    async fn persist_replacement_then_sync_restores_routeability_with_new_snapshot() {
        let repository = repository().await;
        let account = account(&repository).await;
        // Provider that can both log in and list a protocol-tagged model.
        struct SyncProvider {
            login_account_id: String,
            login_payload: serde_json::Value,
            operations: AtomicUsize,
        }
        #[async_trait]
        impl Provider for SyncProvider {
            fn kind(&self) -> ProviderKind {
                ProviderKind::Codex
            }
            async fn login(
                &self,
                _context: &ProviderLoginContext,
                _runtime: &dyn LoginRuntime,
            ) -> Result<LoginResult, ProviderError> {
                self.operations.fetch_add(1, Ordering::SeqCst);
                Ok(LoginResult {
                    account_id: self.login_account_id.clone(),
                    label: "Codex".into(),
                    attributes: json!({}),
                    payload: ProviderPayload::new(self.login_payload.clone()),
                    last_refreshed_at: Some("2026-08-09T03:00:00Z".into()),
                    next_refresh_after: None,
                    next_retry_after: None,
                })
            }
            async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
                Err(ProviderError::ImportFailed)
            }
            async fn refresh(
                &self,
                _: &ProviderPayload,
            ) -> Result<RefreshedPayload, ProviderError> {
                Err(ProviderError::Retryable)
            }
            async fn outbound(
                &self,
                _: ProviderRequest<'_>,
            ) -> Result<reqwest::Response, ProviderError> {
                Err(ProviderError::Retryable)
            }
            async fn list_models(
                &self,
                _account: &crate::db::models::AuthAccount,
                _payload: &ProviderPayload,
            ) -> Result<ProviderModels, ProviderError> {
                Ok(vec![crate::db::models::ModelState {
                    id: "kimi-k2.5".into(),
                    status: "available".into(),
                    unavailable: false,
                    next_retry_after: None,
                    last_error: None,
                    protocol: Some("kimi".into()),
                }])
            }
        }
        let provider = Arc::new(SyncProvider {
            login_account_id: account.account_id.clone(),
            login_payload: fresh_login_payload(),
            operations: AtomicUsize::new(0),
        });
        let mut registry = ProviderRegistry::new();
        registry.register(provider);
        // Pin the clock before the fresh payload's expiry so the sync path does
        // not decide the token is due and attempt a provider refresh.
        let service = AuthService::with_clock(
            repository.clone(),
            registry,
            Arc::new(FixedClock("2026-08-08T00:00:00Z".parse().unwrap())),
        );

        // Replacement atomically clears the old snapshot.
        let authenticated = service
            .authenticate(
                ProviderKind::Codex,
                LoginTarget::Replace {
                    local_account_id: account.id.clone(),
                },
                &RecordingRuntime::default(),
            )
            .await
            .unwrap();
        service.persist_authenticated(authenticated).await.unwrap();
        assert!(repository
            .list_route_accounts("2026-08-09T03:00:00Z")
            .await
            .unwrap()
            .is_empty());

        // After a successful sync, the new protocol snapshot re-enables routing.
        service.sync_models(&account.id).await.unwrap();
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        assert_eq!(
            stored.model_states().unwrap().models[0].protocol.as_deref(),
            Some("kimi")
        );
        let routeable = repository
            .list_route_accounts("2026-08-09T03:30:00Z")
            .await
            .unwrap();
        assert_eq!(routeable.len(), 1);
        assert_eq!(routeable[0].id, account.id);
    }
}
