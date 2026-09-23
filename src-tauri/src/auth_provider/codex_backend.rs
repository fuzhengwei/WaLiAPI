//! Thin, deliberately fail-closed adapter for Codex's backend-api.
//!
//! The public provider trait remains provider-neutral.  This module is the only
//! place that knows the fixed Codex backend paths and response-header quota wire
//! format; it never accepts a caller-supplied backend URL in production.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use reqwest::{header, StatusCode};
use serde_json::Value;

use super::{
    codex_login::{AuthFileFormat, CodexLogin},
    LoginResult, LoginRuntime, MultiImportResult, Provider, ProviderError, ProviderKind,
    ProviderLoginContext, ProviderModels, ProviderPayload, ProviderRequest, RefreshedPayload,
};
use crate::db::models::{AuthAccount, ModelState, QuotaLimit, QuotaState, QuotaWindow};

pub const CODEX_BACKEND_BASE: &str = "https://chatgpt.com/backend-api/codex";
const RESPONSES_PATH: &str = "responses";
const MODELS_PATH: &str = "models";
// `/models` is a Codex client endpoint, not a WaLiAPI endpoint.  The backend
// filters its catalog by this value, so using our own application version (for
// example `0.1.7`) can legitimately produce an empty model list.
// ChatGPT 按 client_version 门控 Codex 模型目录。0.147.0 只下发 GPT-5.6，
// 0.156.1 起包含 GPT-6 系列；该值应与当前 Codex CLI 协议版本同步。
const CODEX_CLIENT_VERSION: &str = "0.156.1";
const CODEX_ORIGINATOR: &str = "codex_cli_rs";

fn codex_user_agent() -> String {
    format!("{CODEX_ORIGINATOR}/{CODEX_CLIENT_VERSION}")
}

/// The sole Codex provider implementation.  `with_backend_base` is intentionally
/// test-only so production calls cannot be redirected by frontend/downstream data.
#[derive(Clone)]
pub struct CodexProvider {
    client: reqwest::Client,
    backend_base: String,
    login: CodexLogin,
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexProvider {
    pub fn new() -> Self {
        Self {
            client: backend_client(),
            backend_base: CODEX_BACKEND_BASE.to_owned(),
            login: CodexLogin::new(),
        }
    }

    #[cfg(test)]
    fn with_endpoints(backend_base: String, login: CodexLogin) -> Self {
        Self {
            client: backend_client(),
            backend_base: backend_base.trim_end_matches('/').to_owned(),
            login,
        }
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.backend_base.trim_end_matches('/'))
    }

    /// The dedicated quota-status endpoint lives at the `backend-api` root, not
    /// under `/codex` (which is the outbound base).  Derive it by replacing the
    /// trailing `/codex` segment.  This is the upstream "no-traffic" source of
    /// truth for quota (the `/wham/usage` payload), distinct from the response
    /// headers parsed by [`quota_from_headers`].
    fn usage_endpoint(&self) -> String {
        let base = self.backend_base.trim_end_matches('/');
        match base.strip_suffix("/codex") {
            Some(root) => format!("{root}/wham/usage"),
            None => format!("{base}/../wham/usage"),
        }
    }

    /// Probe the dedicated quota endpoint.  A failure or a payload without
    /// quota data returns `None` so callers keep whatever was previously
    /// persisted — a quota probe never erases known state.
    async fn fetch_quota_inner(
        &self,
        account: &crate::db::models::AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<Option<QuotaState>, ProviderError> {
        let mut headers = self.auth_headers(payload, account, &header::HeaderMap::new())?;
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );
        let response = self
            .client
            .get(self.usage_endpoint())
            .headers(headers)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let body: Value = response.json().await.map_err(|_| ProviderError::Protocol)?;
        Ok(quota_from_usage_payload(&body))
    }

    fn auth_headers(
        &self,
        payload: &ProviderPayload,
        account: &crate::db::models::AuthAccount,
        caller_headers: &header::HeaderMap,
    ) -> Result<header::HeaderMap, ProviderError> {
        let access_token = payload
            .as_value()
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(ProviderError::InvalidPayload)?;
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, bearer(access_token)?);
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("text/event-stream"),
        );
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(&codex_user_agent())
                .map_err(|_| ProviderError::InvalidPayload)?,
        );
        headers.insert(
            header::HeaderName::from_static("originator"),
            header::HeaderValue::from_static(CODEX_ORIGINATOR),
        );
        headers.insert(
            header::HeaderName::from_static("chatgpt-account-id"),
            header::HeaderValue::from_str(&account.account_id)
                .map_err(|_| ProviderError::InvalidPayload)?,
        );

        // Only a small non-auth whitelist survives the account boundary.  In
        // particular caller Authorization and x-openai-actor-authorization are
        // never relayed to a subscription account. Accept/User-Agent may
        // override the defaults for JSON-only probes such as model listing.
        for name in [header::ACCEPT, header::USER_AGENT] {
            if let Some(value) = caller_headers.get(&name) {
                headers.insert(name, value.clone());
            }
        }
        if let Some(actor) = trusted_actor_authorization(account) {
            headers.insert(
                "x-openai-actor-authorization",
                header::HeaderValue::from_str(&actor).map_err(|_| ProviderError::InvalidPayload)?,
            );
        }
        Ok(headers)
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    async fn login(
        &self,
        context: &ProviderLoginContext,
        runtime: &dyn LoginRuntime,
    ) -> Result<LoginResult, ProviderError> {
        match context.login_method {
            super::AuthLoginMode::BrowserCallback => self.login.login(runtime).await,
            super::AuthLoginMode::DeviceCode => self.login.login_device_code(runtime).await,
        }
    }

    async fn import(&self, bytes: &[u8]) -> Result<LoginResult, ProviderError> {
        self.login.import_auth_json(bytes).await
    }

    async fn import_all(
        &self,
        bytes: &[u8],
        format: Option<&str>,
    ) -> Result<MultiImportResult, ProviderError> {
        let format = match format {
            Some(name) => AuthFileFormat::from_name(name)?,
            None => AuthFileFormat::Codex,
        };
        if format != AuthFileFormat::Sub2api {
            let result = self
                .login
                .import_auth_json_with_format(bytes, format)
                .await?;
            return Ok(MultiImportResult {
                results: vec![result],
                skipped: 0,
            });
        }
        let (accounts, total) = CodexLogin::parse_sub2api_accounts(bytes)?;
        let mut skipped = total - accounts.len();
        let mut results = Vec::new();
        for account in accounts {
            // One unusable account (e.g. expired token whose refresh fails)
            // must not sink the rest of the file; it counts as skipped.
            match self.login.import_codex_account(account).await {
                Ok(result) => results.push(result),
                Err(_) => skipped += 1,
            }
        }
        Ok(MultiImportResult { skipped, results })
    }

    async fn refresh(&self, payload: &ProviderPayload) -> Result<RefreshedPayload, ProviderError> {
        self.login.refresh_payload(payload).await
    }

    async fn outbound(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<reqwest::Response, ProviderError> {
        let body = validate_backend_request(request.body)?;
        let headers = self.auth_headers(request.payload, request.account, request.headers)?;
        self.client
            .post(self.endpoint(RESPONSES_PATH))
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)
    }

    async fn list_models(
        &self,
        account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<ProviderModels, ProviderError> {
        let headers = self.auth_headers(payload, account, &header::HeaderMap::new())?;
        let mut headers = headers;
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(&codex_user_agent())
                .map_err(|_| ProviderError::InvalidPayload)?,
        );
        let response = self
            .client
            .get(self.endpoint(MODELS_PATH))
            // This is part of Codex's native `/models` request contract.  The
            // backend uses it to select models compatible with the client.
            .query(&[("client_version", CODEX_CLIENT_VERSION)])
            .headers(headers)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(ProviderError::Retryable);
        }
        let body: Value = response.json().await.map_err(|_| ProviderError::Protocol)?;
        let entries = body
            .get("data")
            .or_else(|| body.get("models"))
            .and_then(Value::as_array)
            .or_else(|| body.as_array())
            .ok_or(ProviderError::Protocol)?;
        let models = entries
            .iter()
            .filter_map(|entry| match entry {
                Value::String(id) => Some(id.clone()),
                // The ChatGPT Codex endpoint returns `models[].slug`; retain
                // `id` for OpenAI-compatible proxies and older fixtures.
                Value::Object(_) => entry
                    .get("slug")
                    .or_else(|| entry.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                _ => None,
            })
            .filter(|id| !id.trim().is_empty())
            .map(|id| ModelState {
                id,
                status: "available".to_owned(),
                unavailable: false,
                next_retry_after: None,
                last_error: None,
                protocol: None,
            })
            .collect();
        Ok(models)
    }

    async fn fetch_quota(
        &self,
        account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<Option<QuotaState>, ProviderError> {
        self.fetch_quota_inner(account, payload).await
    }
}

fn backend_client() -> reqwest::Client {
    // Disabling all optional content encodings keeps this adapter from adding a
    // request content coding implicitly.
    reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .expect("reqwest client construction must not fail")
}

fn bearer(access_token: &str) -> Result<header::HeaderValue, ProviderError> {
    header::HeaderValue::from_str(&format!("Bearer {access_token}"))
        .map_err(|_| ProviderError::InvalidPayload)
}

fn trusted_actor_authorization(account: &crate::db::models::AuthAccount) -> Option<String> {
    serde_json::from_str::<Value>(&account.attributes_json)
        .ok()?
        .get("actor_authorization")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Reject unsupported non-null top-level values before any network activity and
/// force the backend's only supported stream/store mode.  Null unknown fields
/// carry no request semantics and are discarded rather than forwarded.
pub fn validate_backend_request(body: &Value) -> Result<Value, ProviderError> {
    let object = body.as_object().ok_or(ProviderError::InvalidPayload)?;
    const ALLOWED: &[&str] = &[
        "model",
        "input",
        "instructions",
        "tools",
        "tool_choice",
        "parallel_tool_calls",
        "reasoning",
        "prompt_cache_key",
        "stream_options",
        "service_tier",
        "text",
        "client_metadata",
        "include",
        "stream",
        "store",
    ];
    // `prompt_cache_options` is a caching hint that accompanies
    // `prompt_cache_key`; it carries no request semantics the backend needs and
    // the Chat path already discards it (see `responses_codec::encode_messages`
    // DROPPED). Stripping keeps both downstream protocols behaving the same
    // instead of rejecting Responses clients that send it (e.g. WaLiCode).
    const STRIPPED: &[&str] = &["max_output_tokens", "metadata", "prompt_cache_options"];
    for (key, value) in object {
        if !ALLOWED.contains(&key.as_str()) && !STRIPPED.contains(&key.as_str()) && !value.is_null()
        {
            return Err(ProviderError::UnsupportedFeatures {
                pointer: format!("/{key}"),
            });
        }
    }
    let mut encoded = serde_json::Map::new();
    for key in ALLOWED
        .iter()
        .copied()
        .filter(|key| !matches!(*key, "stream" | "store"))
    {
        if let Some(value) = object.get(key) {
            encoded.insert(key.to_owned(), value.clone());
        }
    }
    encoded.insert("stream".to_owned(), Value::Bool(true));
    encoded.insert("store".to_owned(), Value::Bool(false));
    Ok(Value::Object(encoded))
}

/// Parse all rate-limit response headers.  `None` means no quota state should be
/// written, which preserves `null` for providers/responses that report no limits.
pub fn quota_from_headers(
    headers: &header::HeaderMap,
    status: StatusCode,
    previous: Option<&QuotaState>,
    now: DateTime<Utc>,
) -> Option<QuotaState> {
    let mut limits = parse_limits(headers);
    if limits.is_empty() && status != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }

    let mut quota = QuotaState {
        version: 1,
        exceeded: false,
        reason: None,
        next_recover_at: None,
        backoff_level: 0,
        limits: std::mem::take(&mut limits),
    };
    let exhausted_resets = quota
        .limits
        .iter()
        .flat_map(|limit| [limit.primary.as_ref(), limit.secondary.as_ref()])
        .flatten()
        .filter(|window| window.used_percent.is_some_and(|used| used >= 100.0))
        .filter_map(|window| window.reset_at.as_deref())
        .filter_map(parse_reset_at)
        .collect::<Vec<_>>();
    let has_exhausted_window = quota.limits.iter().any(|limit| {
        [limit.primary.as_ref(), limit.secondary.as_ref()]
            .into_iter()
            .flatten()
            .any(|window| window.used_percent.is_some_and(|used| used >= 100.0))
    });

    if status == StatusCode::TOO_MANY_REQUESTS {
        quota.exceeded = true;
        quota.reason = Some("quota".to_owned());
        quota.backoff_level = previous.map_or(1, |old| old.backoff_level.saturating_add(1));
        let recover_at = retry_after(headers, now).or_else(|| {
            exhausted_resets.iter().max().copied().or_else(|| {
                let seconds =
                    60_i64.saturating_mul(2_i64.saturating_pow(quota.backoff_level.min(8) as u32));
                Some(now + Duration::seconds(seconds))
            })
        });
        quota.next_recover_at = recover_at.map(|time| time.to_rfc3339());
    } else if has_exhausted_window {
        quota.exceeded = true;
        quota.reason = Some("quota".to_owned());
        quota.next_recover_at = exhausted_resets.iter().max().map(|time| time.to_rfc3339());
    }
    Some(quota)
}

/// One `/wham/usage` window object -> a [`QuotaWindow`] (seconds -> minutes,
/// epoch -> RFC3339), or `None` when the field is absent or carries no data.
fn usage_window(value: Option<&Value>) -> Option<QuotaWindow> {
    let value = value?;
    let used_percent = value.get("used_percent")?.as_f64()?;
    let window_minutes = value.get("limit_window_seconds")?.as_i64()? / 60;
    let reset_at = value
        .get("reset_at")
        .and_then(Value::as_i64)
        .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
        .map(|time| time.to_rfc3339());
    // Same `has_data` rule as the header parser: an all-empty window must not
    // manufacture an empty bar.
    let has_data = used_percent != 0.0 || window_minutes != 0 || reset_at.is_some();
    has_data.then(|| QuotaWindow {
        used_percent: Some(used_percent),
        window_minutes: Some(window_minutes),
        reset_at,
    })
}

/// Parse the dedicated `GET /backend-api/wham/usage` payload into a
/// [`QuotaState`].  Unlike response headers (minutes / RFC3339), this endpoint
/// reports `limit_window_seconds` in seconds and `reset_at` as a UNIX epoch
/// seconds; both are normalized here.  `None` preserves whatever quota was
/// previously persisted (a failed probe never wipes known state).
pub fn quota_from_usage_payload(payload: &Value) -> Option<QuotaState> {
    let rate_limit = payload.get("rate_limit")?;
    let primary = usage_window(rate_limit.get("primary_window"))?;

    // Both windows are kept when present: plus/pro plans now report a 5h
    // primary and a weekly secondary, and the card renders them together.
    // Each window independently passes through the `has_data` filter above.
    let limit = QuotaLimit {
        limit_id: "codex".to_owned(),
        limit_name: None,
        primary: Some(primary),
        secondary: usage_window(rate_limit.get("secondary_window")),
        credits: None,
    };
    if limit.primary.is_none() && limit.secondary.is_none() {
        return None;
    }

    let exceeded = rate_limit.get("limit_reached").and_then(Value::as_bool) == Some(true)
        || [Some(&limit.primary), Some(&limit.secondary)]
            .into_iter()
            .flatten()
            .flatten()
            .any(|window| window.used_percent.is_some_and(|used| used >= 100.0));
    let next_recover_at = if exceeded {
        // Earliest exhausted-window reset; when none parses (e.g.
        // limit_reached with a null reset), fall back to the soonest known
        // window reset so routing still gets a retry hint.
        quota_exhausted_reset(&limit)
            .map(|time| time.to_rfc3339())
            .or_else(|| {
                [limit.primary.as_ref(), limit.secondary.as_ref()]
                    .into_iter()
                    .flatten()
                    .filter_map(|window| window.reset_at.as_deref())
                    .filter_map(|value| DateTime::parse_from_rfc3339(value).ok())
                    .min()
                    .map(|time| time.to_rfc3339())
            })
    } else {
        None
    };
    Some(QuotaState {
        version: 1,
        exceeded,
        reason: exceeded.then(|| "quota".to_owned()),
        next_recover_at,
        backoff_level: 0,
        limits: vec![limit],
    })
}

/// Earliest RFC3339 reset among a limit's windows that are actually exhausted.
fn quota_exhausted_reset(limit: &QuotaLimit) -> Option<DateTime<Utc>> {
    [limit.primary.as_ref(), limit.secondary.as_ref()]
        .into_iter()
        .flatten()
        .filter(|window| window.used_percent.is_some_and(|used| used >= 100.0))
        .filter_map(|window| window.reset_at.as_deref())
        .filter_map(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|time| time.with_timezone(&Utc))
        .min()
}

fn retry_after(headers: &header::HeaderMap, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        return (seconds >= 0).then_some(now + Duration::seconds(seconds));
    }
    DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
        .filter(|time| *time >= now)
}

fn parse_limits(headers: &header::HeaderMap) -> Vec<QuotaLimit> {
    #[derive(Default)]
    struct Parts {
        limit_name: Option<String>,
        credits: Option<f64>,
        primary: BTreeMap<String, String>,
        secondary: BTreeMap<String, String>,
    }
    let mut by_id: BTreeMap<String, Parts> = BTreeMap::new();
    for (name, value) in headers {
        let Ok(value) = value.to_str() else { continue };
        let name = name.as_str().to_ascii_lowercase();
        let Some(rest) = name.strip_prefix("x-") else {
            continue;
        };
        if let Some(id) = rest.strip_suffix("-limit-name") {
            if !id.is_empty() {
                by_id.entry(id.to_owned()).or_default().limit_name = Some(value.to_owned());
            }
            continue;
        }
        if let Some(id) = rest.strip_suffix("-credits") {
            if !id.is_empty() {
                by_id.entry(id.to_owned()).or_default().credits = value.parse().ok();
            }
            continue;
        }
        for (kind, marker) in [("primary", "-primary-"), ("secondary", "-secondary-")] {
            if let Some((id, field)) = rest.rsplit_once(marker) {
                if id.is_empty() || !matches!(field, "used-percent" | "window-minutes" | "reset-at")
                {
                    continue;
                }
                let parts = by_id.entry(id.to_owned()).or_default();
                match kind {
                    "primary" => {
                        parts.primary.insert(field.to_owned(), value.to_owned());
                    }
                    _ => {
                        parts.secondary.insert(field.to_owned(), value.to_owned());
                    }
                }
                break;
            }
        }
    }
    by_id
        .into_iter()
        .map(|(limit_id, parts)| QuotaLimit {
            limit_id,
            limit_name: parts.limit_name,
            primary: quota_window(parts.primary),
            secondary: quota_window(parts.secondary),
            credits: parts.credits,
        })
        // A limit entry with no windows and no credits carries nothing
        // displayable (e.g. `codex-credits-has` with only an empty header) —
        // drop it so the UI never renders an empty quota card.
        .filter(|limit| {
            limit.primary.is_some() || limit.secondary.is_some() || limit.credits.is_some()
        })
        .collect()
}

fn quota_window(parts: BTreeMap<String, String>) -> Option<QuotaWindow> {
    // Mirror upstream codex `rate_limits.rs` `has_data`: a window is only kept
    // when it carries real data (used_percent != 0, window_minutes != 0, or a
    // parseable reset).  A header like `x-codex-secondary-used-percent: 0` alone
    // must not manufacture an empty "secondary" window.
    let used_percent = parts
        .get("used-percent")
        .and_then(|value| value.parse().ok());
    let window_minutes = parts
        .get("window-minutes")
        .and_then(|value| value.parse().ok());
    let reset_at = parts
        .get("reset-at")
        .and_then(|value| parse_reset_at(value));
    let has_data = used_percent.is_some_and(|used| used != 0.0)
        || window_minutes.is_some_and(|minutes| minutes != 0)
        || reset_at.is_some();
    has_data.then(|| QuotaWindow {
        used_percent,
        window_minutes,
        reset_at: reset_at.map(|time| time.to_rfc3339()),
    })
}

fn parse_reset_at(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
        .or_else(|| {
            value
                .parse::<i64>()
                .ok()
                .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
        })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    use axum::{
        body::Bytes,
        extract::{OriginalUri, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::*;
    use crate::db::models::AuthAccount;

    #[derive(Clone, Default)]
    struct MockState {
        hits: Arc<AtomicUsize>,
        usage_hits: Arc<AtomicUsize>,
        refreshes: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
        model_queries: Arc<Mutex<Vec<Option<String>>>>,
        version_gated_models: Arc<AtomicBool>,
        statuses: Arc<Mutex<Vec<StatusCode>>>,
        models_status: Arc<Mutex<StatusCode>>,
    }

    async fn responses(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl IntoResponse {
        state.hits.fetch_add(1, Ordering::SeqCst);
        state
            .requests
            .lock()
            .await
            .push((headers, serde_json::from_slice(&body).unwrap()));
        let status = state.statuses.lock().await.pop().unwrap_or(StatusCode::OK);
        (status, Json(json!({"ok": true})))
    }

    async fn models(
        State(state): State<MockState>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        let status = *state.models_status.lock().await;
        let query = uri.query().map(str::to_owned);
        state.model_queries.lock().await.push(query.clone());
        state
            .requests
            .lock()
            .await
            .push((headers, serde_json::json!({})));
        // This is the native Codex schema: model identity is `slug`, not the
        // OpenAI-compatible `data[].id` shape.
        let slug = if state.version_gated_models.load(Ordering::SeqCst) {
            // 真实上游按 client_version 门控模型目录：0.147.0 只返回 GPT-5.6，
            // 当前 Codex 0.156.1 才会下发 GPT-6。
            if query.as_deref() == Some("client_version=0.156.1") {
                "gpt-6-sol"
            } else {
                "gpt-5.6-sol"
            }
        } else {
            "gpt-test"
        };
        (status, Json(json!({"models": [{"slug": slug}]})))
    }

    async fn refresh_token(State(state): State<MockState>) -> impl IntoResponse {
        state.refreshes.fetch_add(1, Ordering::SeqCst);
        Json(json!({
            "access_token": "refreshed-access",
            "token_type": "Bearer",
            "expires_in": 3600
        }))
    }

    async fn usage(State(state): State<MockState>) -> impl IntoResponse {
        let hits = state.usage_hits.fetch_add(1, Ordering::SeqCst);
        if hits == 0 {
            return Json(json!({
                "plan_type": "plus",
                "rate_limit": {
                    "allowed": true,
                    "limit_reached": false,
                    "primary_window": {
                        "used_percent": 58,
                        "limit_window_seconds": 604800,
                        "reset_after_seconds": 574587,
                        "reset_at": 1786867084
                    },
                    "secondary_window": null
                },
                "credits": { "has_credits": true, "unlimited": false, "balance": "1908.09" },
                "spend_control": { "reached": false, "individual_limit": null }
            }));
        }
        Json(json!({"error": "rate limited"}))
    }

    async fn provider(statuses: Vec<StatusCode>) -> (CodexProvider, MockState) {
        let state = MockState {
            statuses: Arc::new(Mutex::new(statuses)),
            ..Default::default()
        };
        let app = Router::new()
            .route("/backend-api/codex/responses", post(responses))
            .route("/backend-api/codex/models", get(models))
            .route("/backend-api/wham/usage", get(usage))
            .route("/oauth/token", post(refresh_token))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            CodexProvider::with_endpoints(
                format!("{base}/backend-api/codex"),
                CodexLogin::with_endpoints(
                    "http://127.0.0.1/unused",
                    format!("{base}/oauth/token"),
                ),
            ),
            state,
        )
    }

    fn account() -> AuthAccount {
        AuthAccount {
            id: "a1".into(),
            provider: "codex".into(),
            label: "Codex".into(),
            account_id: "remote".into(),
            status: "active".into(),
            disabled: 0,
            priority: 0,
            weight: 1,
            sort_order: 0,
            quota_json: None,
            model_states_json: "{\"version\":1,\"models\":[]}".into(),
            model_mapping_json: "{}".into(),
            model_mapping_disabled: "[]".into(),
            attributes_json: json!({"actor_authorization":"trusted-actor"}).to_string(),
            payload_json: "{}".into(),
            last_refreshed_at: None,
            last_models_sync_at: None,
            next_refresh_after: None,
            next_retry_after: None,
            created_at: "x".into(),
            updated_at: "x".into(),
        }
    }

    fn payload() -> ProviderPayload {
        ProviderPayload::new(
            json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","id_token":"fixture-id","expires_at":"2099-01-01T00:00:00Z"}),
        )
    }

    #[tokio::test]
    async fn fixed_backend_request_forces_stream_and_strips_caller_auth_headers() {
        let (provider, state) = provider(vec![]).await;
        let account = account();
        let payload = payload();
        let mut caller = HeaderMap::new();
        caller.insert(
            header::AUTHORIZATION,
            "Bearer caller-secret".parse().unwrap(),
        );
        caller.insert(
            "x-openai-actor-authorization",
            "caller-actor".parse().unwrap(),
        );
        caller.insert(header::ACCEPT, "text/event-stream".parse().unwrap());
        provider
            .outbound(ProviderRequest {
                account: &account,
                payload: &payload,
                body: &json!({"model":"gpt-test","input":"hi","stream":false}),
                headers: &caller,
                is_stream: true,
                upstream_protocol: "responses",
                upstream_endpoint: "responses",
            })
            .await
            .unwrap();
        assert_eq!(state.hits.load(Ordering::SeqCst), 1);
        let requests = state.requests.lock().await;
        let (headers, body) = &requests[0];
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(headers[header::AUTHORIZATION], "Bearer fixture-access");
        assert_eq!(headers["x-openai-actor-authorization"], "trusted-actor");
        assert!(!headers.contains_key("content-encoding"));
    }

    #[test]
    fn fixed_backend_request_never_allows_store_true() {
        let body = validate_backend_request(&json!({
            "model": "gpt-test",
            "input": "hi",
            "stream": false,
            "store": true
        }))
        .unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn backend_request_forwards_full_codex_request_body() {
        // Real codex CLI 0.147.0 `wire_api = "responses"` top-level shape
        // (spec §1.1): every field must pass through unchanged, with
        // stream/store still forced.
        let body = validate_backend_request(&json!({
            "model": "gpt-5.6-luna",
            "instructions": "You are a helpful assistant.",
            "input": [
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "hi"}]}
            ],
            "tools": [
                {"type": "function", "name": "f", "description": "f",
                 "parameters": {"type": "object", "properties": {}}}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": {"effort": "high"},
            "store": true,
            "stream": false,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "cache-key-1",
            "stream_options": {"include_usage": true},
            "service_tier": "flex",
            "text": {"verbosity": "low"},
            "client_metadata": {"x-codex-installation-id": "install-1"}
        }))
        .unwrap();

        // Newly-allowed codex fields are forwarded unchanged.
        assert_eq!(body["parallel_tool_calls"], true);
        assert_eq!(body["reasoning"], json!({"effort": "high"}));
        assert_eq!(body["prompt_cache_key"], "cache-key-1");
        assert_eq!(body["stream_options"], json!({"include_usage": true}));
        assert_eq!(body["service_tier"], "flex");
        assert_eq!(body["text"], json!({"verbosity": "low"}));

        // Existing passthrough fields are retained too.
        assert_eq!(body["instructions"], "You are a helpful assistant.");
        assert_eq!(
            body["input"],
            json!([{"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "hi"}]}])
        );
        assert_eq!(
            body["tools"],
            json!([{"type": "function", "name": "f", "description": "f",
                    "parameters": {"type": "object", "properties": {}}}])
        );
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(
            body["client_metadata"],
            json!({"x-codex-installation-id": "install-1"})
        );

        // Stream/store are still forced regardless of what the client sent.
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn backend_request_rejects_non_null_unknown_fields() {
        let error = validate_backend_request(&json!({
            "model": "gpt-test",
            "input": "hi",
            "unknown_field": 1
        }))
        .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::UnsupportedFeatures { ref pointer } if pointer == "/unknown_field"
        ));
    }

    /// Regression: a real WaLiCode `/v1/responses` body (captured from
    /// `request_logs`) was rejected with 400 at `/prompt_cache_options` before
    /// any network call. Every other field it sends is allowed or stripped.
    #[test]
    fn backend_request_strips_prompt_cache_options() {
        let body = validate_backend_request(&json!({
            "model": "gpt-5.4",
            "input": "hi",
            "instructions": "you are a coding agent",
            "max_output_tokens": 32768,
            "prompt_cache_key": "walicode-responses-v2:1dgr77u:91n97n",
            "prompt_cache_options": {"mode": "implicit"},
            "reasoning": {"effort": "medium"},
            "stream": true,
            "tools": []
        }))
        .unwrap();
        assert!(body.get("prompt_cache_options").is_none());
        // The companion key is still forwarded; only the hint is dropped.
        assert_eq!(
            body["prompt_cache_key"],
            "walicode-responses-v2:1dgr77u:91n97n"
        );
        assert_eq!(body["reasoning"], json!({"effort": "medium"}));
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn backend_request_discards_null_unknown_fields() {
        let body = validate_backend_request(&json!({
            "model": "gpt-test",
            "input": "hi",
            "unknown_null": null
        }))
        .unwrap();
        assert!(body.get("unknown_null").is_none());
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn backend_request_strips_max_output_tokens() {
        let body = validate_backend_request(&json!({
            "model": "gpt-test",
            "input": "hi",
            "max_output_tokens": 32000
        }))
        .unwrap();

        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[test]
    fn backend_request_preserves_client_metadata() {
        let metadata = json!({
            "x-codex-installation-id": "install-1",
            "x-codex-window-id": "window-1",
            "ws_request_header_x_openai_internal_codex_responses_lite": "true",
            "future-field": {"nested": [1, 2, 3]}
        });
        let body = validate_backend_request(&json!({
            "model": "gpt-5.6-luna",
            "input": "hi",
            "client_metadata": metadata,
            "include": ["reasoning.encrypted_content"]
        }))
        .unwrap();

        assert_eq!(
            body["client_metadata"]["x-codex-installation-id"],
            "install-1"
        );
        assert_eq!(body["client_metadata"]["x-codex-window-id"], "window-1");
        assert_eq!(
            body["client_metadata"]["ws_request_header_x_openai_internal_codex_responses_lite"],
            "true"
        );
        assert_eq!(
            body["client_metadata"]["future-field"]["nested"],
            json!([1, 2, 3])
        );
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
    }

    #[tokio::test]
    async fn client_metadata_is_preserved_before_account_backend() {
        let (provider, state) = provider(vec![]).await;
        let account = account();
        let payload = payload();
        provider
            .outbound(ProviderRequest {
                account: &account,
                payload: &payload,
                body: &json!({
                    "model": "gpt-5.6-luna",
                    "input": "hi",
                    "client_metadata": {
                        "x-codex-window-id": "window-1",
                        "future-field": {"enabled": true}
                    }
                }),
                headers: &HeaderMap::new(),
                is_stream: true,
                upstream_protocol: "responses",
                upstream_endpoint: "responses",
            })
            .await
            .unwrap();

        let requests = state.requests.lock().await;
        assert_eq!(
            requests[0].1["client_metadata"]["x-codex-window-id"],
            "window-1"
        );
        assert_eq!(
            requests[0].1["client_metadata"]["future-field"]["enabled"],
            true
        );
        assert!(requests[0].1.get("metadata").is_none());
    }

    #[tokio::test]
    async fn metadata_annotation_is_stripped_before_account_backend() {
        let (provider, state) = provider(vec![]).await;
        let account = account();
        let payload = payload();
        provider
            .outbound(ProviderRequest {
                account: &account,
                payload: &payload,
                body: &json!({"model":"gpt-test","metadata":{"secret":true}}),
                headers: &HeaderMap::new(),
                is_stream: true,
                upstream_protocol: "responses",
                upstream_endpoint: "responses",
            })
            .await
            .unwrap();
        assert_eq!(state.hits.load(Ordering::SeqCst), 1);
        let requests = state.requests.lock().await;
        assert!(requests[0].1.get("metadata").is_none());
    }

    #[tokio::test]
    async fn models_are_normalized_from_local_backend() {
        let (provider, state) = provider(vec![]).await;
        let account = account();
        let models = provider.list_models(&account, &payload()).await.unwrap();
        assert_eq!(models[0].id, "gpt-test");
        let requests = state.requests.lock().await;
        let (headers, _) = &requests[0];
        // Model listing goes through the same auth boundary as inference: it
        // carries the account actor header, never the caller's.
        assert_eq!(headers[header::AUTHORIZATION], "Bearer fixture-access");
        assert_eq!(headers["x-openai-actor-authorization"], "trusted-actor");
        assert_eq!(headers[header::ACCEPT], "application/json");
        assert_eq!(headers[header::USER_AGENT], codex_user_agent());
        assert_eq!(headers["originator"], CODEX_ORIGINATOR);
        assert_eq!(headers["chatgpt-account-id"], "remote");
        drop(requests);
        assert_eq!(
            state.model_queries.lock().await.as_slice(),
            [Some(format!("client_version={CODEX_CLIENT_VERSION}"))]
        );
    }

    #[tokio::test]
    async fn current_client_version_unlocks_gpt6_catalog() {
        let (provider, state) = provider(vec![]).await;
        state.version_gated_models.store(true, Ordering::SeqCst);

        let models = provider.list_models(&account(), &payload()).await.unwrap();

        assert_eq!(models[0].id, "gpt-6-sol");
    }

    async fn repository() -> Arc<crate::db::repository::Repository> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Arc::new(crate::db::repository::Repository::new(pool))
    }

    async fn persisted_account(repository: &crate::db::repository::Repository) -> AuthAccount {
        repository
            .upsert_by_provider_account_id(&crate::db::models::AuthAccountUpsert {
                provider: "codex".into(),
                label: "Codex".into(),
                account_id: "remote".into(),
                attributes: json!({}),
                payload: json!({
                    "access_token": "fixture-access",
                    "refresh_token": "fixture-refresh",
                    "id_token": "fixture-id",
                    // OAuth refresh responses can omit this value, so the
                    // provider must retain it from the persisted fixture.
                    "account_id": "remote",
                    "expires_at": "2099-01-01T00:00:00Z"
                }),
                last_refreshed_at: None,
                next_refresh_after: None,
                next_retry_after: None,
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn service_retries_401_once_then_marks_the_account_invalid() {
        let (provider, state) =
            provider(vec![StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED]).await;
        let repository = repository().await;
        let account = persisted_account(&repository).await;
        let mut registry = super::super::ProviderRegistry::new();
        registry.register(Arc::new(provider));
        let service = crate::auth_provider::service::AuthService::new(repository.clone(), registry);
        let error = service
            .outbound(
                &account.id,
                &json!({"model":"gpt-test"}),
                &HeaderMap::new(),
                true,
                "responses",
                "responses",
            )
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::Unauthorized);
        assert_eq!(state.hits.load(Ordering::SeqCst), 2);
        assert_eq!(state.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository
                .get_auth_account(&account.id)
                .await
                .unwrap()
                .status,
            "invalid"
        );
    }

    #[tokio::test]
    async fn service_retries_401_once_and_returns_the_second_success() {
        let (provider, state) = provider(vec![StatusCode::OK, StatusCode::UNAUTHORIZED]).await;
        let repository = repository().await;
        let account = persisted_account(&repository).await;
        let mut registry = super::super::ProviderRegistry::new();
        registry.register(Arc::new(provider));
        let service = crate::auth_provider::service::AuthService::new(repository.clone(), registry);
        let response = service
            .outbound(
                &account.id,
                &json!({"model":"gpt-test"}),
                &HeaderMap::new(),
                true,
                "responses",
                "responses",
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.hits.load(Ordering::SeqCst), 2);
        assert_eq!(state.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository
                .get_auth_account(&account.id)
                .await
                .unwrap()
                .status,
            "active"
        );
    }

    #[tokio::test]
    async fn failed_models_sync_preserves_the_old_snapshot_and_timestamp() {
        let (provider, state) = provider(vec![]).await;
        *state.models_status.lock().await = StatusCode::INTERNAL_SERVER_ERROR;
        let repository = repository().await;
        let account = persisted_account(&repository).await;
        let old_models = crate::db::models::ModelStates {
            version: 1,
            models: vec![ModelState {
                id: "gpt-old".into(),
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
        let mut registry = super::super::ProviderRegistry::new();
        registry.register(Arc::new(provider));
        let service = crate::auth_provider::service::AuthService::new(repository.clone(), registry);
        assert_eq!(
            service.sync_models(&account.id).await.unwrap_err(),
            ProviderError::Retryable
        );
        let stored = repository.get_auth_account(&account.id).await.unwrap();
        assert_eq!(stored.model_states().unwrap(), old_models);
        assert_eq!(
            stored.last_models_sync_at.as_deref(),
            Some("2026-08-08T00:00:00Z")
        );
    }

    #[test]
    fn quota_parser_handles_absence_dynamic_limits_and_latest_exhausted_reset() {
        let now: DateTime<Utc> = "2026-08-09T00:00:00Z".parse().unwrap();
        assert!(quota_from_headers(&HeaderMap::new(), StatusCode::OK, None, now).is_none());
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-primary-used-percent", "100".parse().unwrap());
        headers.insert("x-codex-primary-window-minutes", "300".parse().unwrap());
        headers.insert(
            "x-codex-primary-reset-at",
            "2026-08-09T05:00:00Z".parse().unwrap(),
        );
        headers.insert("x-codex-secondary-used-percent", "100".parse().unwrap());
        headers.insert(
            "x-codex-secondary-reset-at",
            "2026-08-10T00:00:00Z".parse().unwrap(),
        );
        headers.insert("x-other-primary-used-percent", "23".parse().unwrap());
        headers.insert("x-other-primary-reset-at", "not-a-date".parse().unwrap());
        let quota = quota_from_headers(&headers, StatusCode::OK, None, now).unwrap();
        assert_eq!(quota.limits.len(), 2);
        assert!(quota.exceeded);
        assert_eq!(
            quota.next_recover_at.as_deref(),
            Some("2026-08-10T00:00:00+00:00")
        );
        assert_eq!(
            quota
                .limits
                .iter()
                .find(|limit| limit.limit_id == "other")
                .unwrap()
                .primary
                .as_ref()
                .unwrap()
                .reset_at,
            None
        );
    }

    #[test]
    fn quota_429_uses_retry_after_then_backoff() {
        let now: DateTime<Utc> = "2026-08-09T00:00:00Z".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, "120".parse().unwrap());
        let quota = quota_from_headers(&headers, StatusCode::TOO_MANY_REQUESTS, None, now).unwrap();
        assert_eq!(
            quota.next_recover_at.as_deref(),
            Some("2026-08-09T00:02:00+00:00")
        );
        let invalid = HeaderMap::new();
        let quota =
            quota_from_headers(&invalid, StatusCode::TOO_MANY_REQUESTS, Some(&quota), now).unwrap();
        assert_eq!(quota.backoff_level, 2);
        assert_eq!(
            quota.next_recover_at.as_deref(),
            Some("2026-08-09T00:04:00+00:00")
        );
    }

    #[test]
    fn quota_window_discards_empty_windows_and_empty_limits() {
        let now: DateTime<Utc> = "2026-08-09T00:00:00Z".parse().unwrap();
        let mut headers = HeaderMap::new();
        // Free-account real shape: monthly primary (43200 min + reset), an
        // empty secondary (`used-percent: 0` only), and an empty credits flag.
        headers.insert("x-codex-primary-used-percent", "0".parse().unwrap());
        headers.insert("x-codex-primary-window-minutes", "43200".parse().unwrap());
        headers.insert(
            "x-codex-primary-reset-at",
            "2026-09-08T15:03:25Z".parse().unwrap(),
        );
        headers.insert("x-codex-secondary-used-percent", "0".parse().unwrap());
        // `x-codex-credits-*-credits` is a boolean flag upstream; it cannot be
        // parsed as f64, so the credits-only `codex-credits-has` limit is empty
        // and must be dropped.
        headers.insert("x-codex-credits-has-credits", "true".parse().unwrap());
        let quota = quota_from_headers(&headers, StatusCode::OK, None, now).unwrap();

        // The empty `secondary` window and the credits-only limit are dropped;
        // only the real monthly `codex` window survives.
        assert_eq!(quota.limits.len(), 1);
        let codex = quota
            .limits
            .iter()
            .find(|limit| limit.limit_id == "codex")
            .unwrap();
        assert!(
            codex.secondary.is_none(),
            "empty secondary window must be dropped"
        );
        let primary = codex.primary.as_ref().unwrap();
        assert_eq!(primary.window_minutes, Some(43_200));
        assert_eq!(primary.used_percent, Some(0.0));
        assert_eq!(
            primary.reset_at.as_deref(),
            Some("2026-09-08T15:03:25+00:00")
        );
    }

    #[test]
    fn quota_window_keeps_nonzero_usage_without_duration() {
        let now: DateTime<Utc> = "2026-08-09T00:00:00Z".parse().unwrap();
        let mut headers = HeaderMap::new();
        // A window carrying real usage but no duration/reset must be preserved.
        headers.insert("x-codex-primary-used-percent", "23".parse().unwrap());
        let quota = quota_from_headers(&headers, StatusCode::OK, None, now).unwrap();
        assert_eq!(quota.limits.len(), 1);
        let primary = quota.limits[0].primary.as_ref().unwrap();
        assert_eq!(primary.used_percent, Some(23.0));
        assert_eq!(primary.window_minutes, None);
        assert_eq!(primary.reset_at, None);
    }

    #[test]
    fn usage_payload_parses_5h_and_weekly_windows() {
        // Real `/wham/usage` shape for a plus account: a 5h primary window and
        // a 7d (604800s) weekly secondary window, UNIX-epoch resets.  The
        // parser normalizes seconds -> minutes and epoch -> RFC3339, keeping
        // both windows so the card can render the two limits together.
        let payload = json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 12,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 12345,
                    "reset_at": 1786837084
                },
                "secondary_window": {
                    "used_percent": 58,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 574587,
                    "reset_at": 1786867084
                }
            },
            "credits": { "has_credits": true, "unlimited": false, "balance": "1908.09" },
            "spend_control": { "reached": false, "individual_limit": null }
        });
        let quota = quota_from_usage_payload(&payload).unwrap();
        assert!(!quota.exceeded);
        assert!(quota.next_recover_at.is_none());
        assert_eq!(quota.limits.len(), 1);
        let limit = &quota.limits[0];
        assert_eq!(limit.limit_id, "codex");
        let primary = limit.primary.as_ref().unwrap();
        assert_eq!(primary.window_minutes, Some(300)); // 18000 / 60 = 5h
        assert_eq!(primary.used_percent, Some(12.0));
        let secondary = limit.secondary.as_ref().unwrap();
        assert_eq!(secondary.window_minutes, Some(10_080)); // 604800 / 60 = 7d
        assert_eq!(secondary.used_percent, Some(58.0));
        // 1786867084 epoch -> 2026-08-16T...Z
        assert!(secondary
            .reset_at
            .as_deref()
            .unwrap()
            .starts_with("2026-08-16T"));
    }

    #[test]
    fn usage_payload_parses_free_monthly_window() {
        let payload = json!({
            "plan_type": "free",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 0,
                    "limit_window_seconds": 2592000,
                    "reset_after_seconds": 2587279,
                    "reset_at": 1788879805
                },
                "secondary_window": null
            },
            "credits": { "has_credits": false, "unlimited": false, "balance": null },
            "spend_control": { "reached": false, "individual_limit": null }
        });
        let quota = quota_from_usage_payload(&payload).unwrap();
        let primary = quota.limits[0].primary.as_ref().unwrap();
        assert_eq!(primary.window_minutes, Some(43_200)); // 2592000 / 60
        assert_eq!(primary.used_percent, Some(0.0));
    }

    #[test]
    fn usage_payload_none_when_limit_reached_sets_exceeded() {
        let payload = json!({
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {
                    "used_percent": 100,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 0,
                    "reset_at": 1786867084
                },
                "secondary_window": null
            }
        });
        let quota = quota_from_usage_payload(&payload).unwrap();
        assert!(quota.exceeded);
        assert_eq!(quota.reason.as_deref(), Some("quota"));
        assert!(quota.next_recover_at.is_some());
    }

    #[test]
    fn usage_payload_none_on_missing_quota_data() {
        // A payload with no rate_limit (or an empty window) must yield None so
        // previously persisted quota is preserved.
        assert!(quota_from_usage_payload(&json!({ "plan_type": "plus" })).is_none());
        assert!(
            quota_from_usage_payload(&json!({ "rate_limit": { "primary_window": null } }))
                .is_none()
        );
    }

    #[tokio::test]
    async fn fetch_quota_hits_dedicated_usage_endpoint() {
        let (provider, state) = provider(vec![]).await;
        let account = account();
        let payload = payload();
        let quota = provider
            .fetch_quota(&account, &payload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.usage_hits.load(Ordering::SeqCst), 1);
        let primary = quota.limits[0].primary.as_ref().unwrap();
        // Weekly plus window normalized from seconds.
        assert_eq!(primary.window_minutes, Some(10_080));
        assert_eq!(primary.used_percent, Some(58.0));
    }
}
