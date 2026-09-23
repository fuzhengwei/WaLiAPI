//! Tauri-facing Auth Account commands.
//!
//! This is the final boundary before data reaches the webview.  Keep the DTOs
//! deliberately explicit: database credential JSON and all OAuth token fields
//! must remain on the native side of this module.

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{watch, Mutex};
use uuid::Uuid;

use crate::{
    auth_provider::{
        codex_login::{CodexLogin, TauriLoginRuntime, CODEX_IMPORT_NOTICE},
        AuthAccountSummary, LoginRuntime, LoginStep, ProviderError, ProviderKind, ProviderPayload,
    },
    db::{
        models::{ModelState, QuotaState},
        repository::Repository,
    },
    AppState,
};

/// A carefully projected account representation.  Do not replace this with
/// `AuthAccount` or `AuthAccountSummary`: both can carry fields unsuitable for
/// a renderer-facing contract.
#[derive(Debug, Clone, Serialize)]
pub struct AuthAccountDto {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub account_id: String,
    pub status: String,
    pub disabled: bool,
    pub priority: i64,
    pub weight: i64,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub project_id: Option<String>,
    /// Stable, non-secret reason the account was marked invalid (e.g.
    /// "payment_required" for an unusable subscription).  Written via
    /// `Repository::mark_invalid` into `attributes_json.invalidation_reason`.
    pub invalidation_reason: Option<String>,
    pub models: Vec<ModelState>,
    pub quota: Option<QuotaState>,
    pub model_mapping: serde_json::Value,
    /// 被关闭的映射对（迁移 042）：`[[from, to], ...]`，供前端渲染开关状态。
    pub model_mapping_disabled: serde_json::Value,
    pub expires_at: Option<String>,
    #[serde(rename = "hasRefreshToken")]
    pub has_refresh_token: bool,
    pub last_refreshed_at: Option<String>,
    pub last_models_sync_at: Option<String>,
    pub next_refresh_after: Option<String>,
    pub next_retry_after: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl TryFrom<AuthAccountSummary> for AuthAccountDto {
    type Error = ProviderError;

    fn try_from(value: AuthAccountSummary) -> Result<Self, Self::Error> {
        let email = value
            .attributes
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let plan_type = value
            .attributes
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let project_id = value
            .attributes
            .get("project_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let invalidation_reason = value
            .attributes
            .get("invalidation_reason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Self {
            id: value.id,
            provider: value.provider,
            label: value.label,
            account_id: value.account_id,
            status: value.status,
            disabled: value.disabled,
            priority: value.priority,
            weight: value.weight,
            email,
            plan_type,
            project_id,
            invalidation_reason,
            models: value.models.models,
            quota: value.quota,
            model_mapping: value.model_mapping,
            model_mapping_disabled: value.model_mapping_disabled,
            expires_at: value.expires_at,
            has_refresh_token: value.has_refresh_token,
            last_refreshed_at: value.last_refreshed_at,
            last_models_sync_at: value.last_models_sync_at,
            next_refresh_after: value.next_refresh_after,
            next_retry_after: value.next_retry_after,
            created_at: value.created_at,
            updated_at: value.updated_at,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthMutationResult {
    pub account: AuthAccountDto,
    /// All accounts persisted by a batch import. Login flows contain the
    /// primary account only; sub2api imports populate the complete list.
    pub imported_accounts: Vec<AuthAccountDto>,
    /// Set when persistence succeeded but the requested follow-up operation
    /// (currently initial model sync) did not.
    pub warning: Option<String>,
    /// Import-specific non-secret operational notice.
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthLogoutResult {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthExportResult {
    pub path: String,
    pub backup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthQuotaStatus {
    pub quota: Option<QuotaState>,
    pub available: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthUpdateInput {
    pub id: String,
    pub label: String,
    pub priority: i64,
    pub weight: i64,
    pub model_mapping: Option<serde_json::Value>,
    /// 被关闭的映射对（迁移 042）：`[[from, to], ...]`。None = 不修改。
    pub model_mapping_disabled: Option<serde_json::Value>,
}

/// Renderer-safe interactive-login session.  It intentionally contains no
/// callback URL, OAuth code, or credential material.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthLoginStart {
    pub session_id: String,
}

/// Device-authorization material surfaced to the renderer during a Kimi login.
/// It carries ONLY the verification URL + user code the user must see; the
/// device code, tokens, and raw OAuth response never cross this boundary.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceVerificationDto {
    pub url: String,
    pub user_code: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthLoginSessionStatus {
    pub session_id: String,
    /// The registered provider string this session is logging into.
    pub provider: String,
    /// pending | saving | syncing | succeeded | cancelled | failed
    pub state: String,
    /// The UI maps this to a concrete progress item; it is never time-based.
    pub step: Option<String>,
    /// Present only while the device flow is waiting for authorization.
    pub verification: Option<DeviceVerificationDto>,
    pub result: Option<AuthMutationResult>,
    /// A stable, non-secret failure category for retry guidance.
    pub error_code: Option<String>,
    pub error: Option<String>,
}

struct LoginSession {
    cancel: watch::Sender<bool>,
    status: AuthLoginSessionStatus,
}

/// Process-local tombstones make repeated cancel safe and prevent a late
/// callback/token exchange from reviving a cancelled session.
pub struct LoginSessions {
    sessions: Mutex<HashMap<String, LoginSession>>,
}

struct SessionLoginRuntime {
    /// Desktop-only browser opener; headless builds have no shell to open a
    /// browser with, so device-code flows rely on the URL + code already
    /// surfaced in the polled session status.
    #[cfg(feature = "desktop-ui")]
    inner: Option<TauriLoginRuntime>,
    sessions: Arc<LoginSessions>,
    session_id: String,
    cancellation: watch::Receiver<bool>,
}

#[async_trait::async_trait]
impl LoginRuntime for SessionLoginRuntime {
    async fn open_browser(&self, url: &str) -> Result<(), ProviderError> {
        #[cfg(feature = "desktop-ui")]
        if let Some(inner) = &self.inner {
            inner.open_browser(url).await?;
            // This is an actual opener success, not a timer-driven estimate.
            self.sessions
                .set_step(&self.session_id, LoginStep::Authorizing.as_str())
                .await;
            return Ok(());
        }
        // Headless (or no shell handle): opening a browser on the server is
        // impossible/undesired; the user authorizes manually on any device.
        // Kimi treats this failure as non-fatal by design.
        Err(ProviderError::BrowserOpenFailed)
    }

    async fn set_step(&self, step: LoginStep) {
        self.sessions
            .set_step(&self.session_id, step.as_str())
            .await;
    }

    async fn present_device_authorization(
        &self,
        verification_url: &str,
        user_code: &str,
        expires_at: Option<String>,
    ) -> Result<(), ProviderError> {
        self.sessions
            .set_verification(&self.session_id, verification_url, user_code, expires_at)
            .await;
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        *self.cancellation.borrow()
    }

    async fn cancelled(&self) {
        let mut receiver = self.cancellation.clone();
        while !*receiver.borrow() && receiver.changed().await.is_ok() {}
    }
}

impl LoginSessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(&self, provider: &str) -> (String, watch::Receiver<bool>) {
        let session_id = Uuid::new_v4().to_string();
        let (cancel, receiver) = watch::channel(false);
        let status = AuthLoginSessionStatus {
            session_id: session_id.clone(),
            provider: provider.to_owned(),
            state: "pending".into(),
            step: Some("preparing".into()),
            verification: None,
            result: None,
            error_code: None,
            error: None,
        };
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), LoginSession { cancel, status });
        (session_id, receiver)
    }

    pub async fn status(&self, session_id: &str) -> Result<AuthLoginSessionStatus, String> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .map(|session| session.status.clone())
            .ok_or_else(|| "Auth login session not found".to_owned())
    }

    pub async fn cancel(&self, session_id: &str) -> Result<AuthLoginSessionStatus, String> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| "Auth login session not found".to_owned())?;
        // Terminal records are tombstones: DELETE/cancel is deliberately idempotent.
        if matches!(
            session.status.state.as_str(),
            "succeeded" | "cancelled" | "failed" | "saving" | "syncing"
        ) {
            return Ok(session.status.clone());
        }
        let _ = session.cancel.send(true);
        session.status.state = "cancelled".into();
        session.status.step = None;
        // A cancelled session must not keep the user code / URL alive.
        session.status.verification = None;
        session.status.error_code = Some("cancelled".into());
        session.status.error = Some("登录已取消，可以重新开始。".into());
        Ok(session.status.clone())
    }

    /// Surface device-authorization material while the flow waits.  Only the
    /// URL + user code are stored; never the device code or tokens.
    async fn set_verification(
        &self,
        session_id: &str,
        url: &str,
        user_code: &str,
        expires_at: Option<String>,
    ) -> bool {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return false;
        };
        if session.status.state != "pending" || *session.cancel.borrow() {
            return false;
        }
        session.status.verification = Some(DeviceVerificationDto {
            url: url.to_owned(),
            user_code: user_code.to_owned(),
            expires_at,
        });
        true
    }

    async fn set_step(&self, session_id: &str, step: &str) -> bool {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return false;
        };
        if session.status.state != "pending" || *session.cancel.borrow() {
            return false;
        }
        session.status.step = Some(step.to_owned());
        true
    }

    /// This transition is the commit gate: cancellation and entering the DB
    /// write phase are serialized under one mutex.  The verification material
    /// is cleared here so a terminal session never retains the user code/URL.
    async fn begin_save(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return false;
        };
        if session.status.state != "pending" || *session.cancel.borrow() {
            return false;
        }
        session.status.state = "saving".into();
        session.status.step = Some("saving".into());
        session.status.verification = None;
        true
    }

    async fn set_syncing(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            if session.status.state == "saving" {
                session.status.state = "syncing".into();
                session.status.step = Some("syncing".into());
            }
        }
    }

    async fn finish(&self, session_id: &str, result: Result<AuthMutationResult, ProviderError>) {
        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return;
        };
        if session.status.state == "cancelled" {
            return;
        }
        session.status.verification = None;
        match result {
            Ok(result) => {
                session.status.state = "succeeded".into();
                session.status.step = None;
                session.status.result = Some(result);
            }
            Err(error) => {
                session.status.state = "failed".into();
                session.status.step = None;
                session.status.error_code = Some(login_error_code(&error).into());
                session.status.error = Some(login_error_message(&error));
            }
        }
    }
}

impl Default for LoginSessions {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

fn login_error_code(error: &ProviderError) -> &'static str {
    match error {
        ProviderError::LoginCancelled => "cancelled",
        ProviderError::LoginTimeout => "timeout",
        ProviderError::BrowserOpenFailed => "browser_open",
        ProviderError::CallbackFailed => "callback_state",
        ProviderError::DeviceAuthorizationFailed => "device_authorization",
        ProviderError::TokenExchangeFailed => "token_exchange",
        ProviderError::AuthorizationDenied => "authorization_denied",
        ProviderError::ValidationRequired { .. } => "validation_required",
        ProviderError::PermissionDenied => "permission_denied",
        ProviderError::CredentialMigrationRequired => "credential_migration",
        _ => "login_failed",
    }
}

fn login_error_message(error: &ProviderError) -> String {
    match error {
        ProviderError::ValidationRequired { url } if url.starts_with("https://") => {
            format!("Google 账号需要先完成验证才能使用 Code Assist。请打开：{url}")
        }
        _ => match login_error_code(error) {
            "cancelled" => "登录已取消，可以重新开始。".to_owned(),
            "timeout" => "等待浏览器授权超时，请重新开始登录。".to_owned(),
            "browser_open" => "无法打开浏览器授权页，请检查默认浏览器后重试。".to_owned(),
            "callback_state" => "授权回调无效或被拒绝，请重新开始登录。".to_owned(),
            "device_authorization" => "Device Code 不可用，请检查 ChatGPT 个人安全设置或 Workspace 管理员权限；也可导入 auth.json。".to_owned(),
            "token_exchange" => "授权完成，但令牌交换失败，请重新开始登录。".to_owned(),
            "authorization_denied" => "授权被拒绝，请重新开始登录。".to_owned(),
            "validation_required" => "Google 账号需要先完成验证才能使用 Code Assist。".to_owned(),
            "permission_denied" => {
                "Google 账号没有 Antigravity Code Assist 权限，请确认账号资格后重试。".to_owned()
            }
            "credential_migration" => {
                "该账号使用旧版 Gemini CLI 凭据，必须通过 Antigravity OAuth 重新登录。".to_owned()
            }
            _ => "登录未完成，请检查浏览器授权后重试。".to_owned(),
        },
    }
}

fn safe_error(error: ProviderError) -> String {
    match error {
        ProviderError::ValidationRequired { .. }
        | ProviderError::PermissionDenied
        | ProviderError::CredentialMigrationRequired
        | ProviderError::BrowserOpenFailed => login_error_message(&error),
        _ => "Auth operation failed".to_owned(),
    }
}

fn storage_error() -> String {
    "Auth account storage operation failed".to_owned()
}

fn validate_account_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("Auth account id is required".to_owned());
    }
    Ok(())
}

fn provider_kind(provider: Option<String>) -> Result<ProviderKind, String> {
    let provider = provider.unwrap_or_else(|| "codex".to_owned());
    let kind = ProviderKind::from(provider.trim());
    match kind {
        ProviderKind::Codex | ProviderKind::Kimi | ProviderKind::Gemini | ProviderKind::Grok => {
            Ok(kind)
        }
        ProviderKind::Other(_) => Err("Unsupported auth provider".to_owned()),
    }
}

fn validate_update(input: &AuthUpdateInput) -> Result<(), String> {
    validate_account_id(&input.id)?;
    if input.label.trim().is_empty() {
        return Err("Auth account label must not be empty".to_owned());
    }
    if input.priority < 0 {
        return Err("Auth account priority must be at least zero".to_owned());
    }
    if input.weight < 1 {
        return Err("Auth account weight must be at least one".to_owned());
    }
    Ok(())
}

fn dto_from_account(account: crate::db::models::AuthAccount) -> Result<AuthAccountDto, String> {
    AuthAccountSummary::from_account(&account)
        .and_then(AuthAccountDto::try_from)
        .map_err(safe_error)
}

async fn sync_after_login(
    service: &crate::auth_provider::service::AuthService,
    summary: AuthAccountSummary,
    notice: Option<String>,
) -> Result<AuthMutationResult, String> {
    let account_id = summary.id.clone();
    let account = AuthAccountDto::try_from(summary).map_err(safe_error)?;
    let warning = match service.sync_models(&account_id).await {
        Ok(_) => None,
        Err(error) => {
            // Model sync failing is the top diagnostic gap for new accounts: it
            // can be a bad token, a missing /models endpoint, or an unexpected
            // payload shape.  Surface the concrete provider error (stable class
            // only, never credential material) so support can tell these apart.
            tracing::warn!(
                account_id = %account_id,
                provider = %account.provider,
                error = ?error.failure_class(),
                "model sync failed after login; account will not route until sync succeeds"
            );
            Some(
                "Account saved, but model sync failed; it will not route until sync succeeds."
                    .to_owned(),
            )
        }
    };
    Ok(AuthMutationResult {
        account: account.clone(),
        imported_accounts: vec![account.clone()],
        warning,
        notice,
    })
}

/// Generic interactive-login session runner shared by every registered provider.
///
/// The flow: resolve the provider kind from the registered spec, start a
/// session, drive `authenticate` (which never writes), then gate persistence
/// behind `begin_save` (exactly-once against cancel), `persist_authenticated`
/// (new-account upsert or locked replacement), model sync, and finish.
///
/// The command layer only ever passes a local account id for replacement; it
/// never reads or forwards `payload_json`.
async fn run_provider_login_session(
    sessions: Arc<LoginSessions>,
    session_id: String,
    provider: ProviderKind,
    target: crate::auth_provider::LoginTarget,
    login_method: crate::auth_provider::AuthLoginMode,
    cancellation: watch::Receiver<bool>,
    app: Option<tauri::AppHandle>,
    service: Arc<crate::auth_provider::service::AuthService>,
) {
    // Headless builds never construct the desktop opener; the AppHandle is
    // only meaningful when the desktop shell exists.
    #[cfg(not(feature = "desktop-ui"))]
    let _ = &app;
    let runtime = SessionLoginRuntime {
        #[cfg(feature = "desktop-ui")]
        inner: app.map(TauriLoginRuntime::new),
        sessions: sessions.clone(),
        session_id: session_id.clone(),
        cancellation,
    };
    let result = match service
        .authenticate_with_method(provider, target, login_method, &runtime)
        .await
    {
        Ok(authenticated) => {
            // The commit gate: a cancel that races persistence (or that happened
            // before the OAuth finished) makes begin_save return false and the
            // DB write never happens.
            if !sessions.begin_save(&session_id).await {
                Err(ProviderError::LoginCancelled)
            } else {
                runtime.set_step(LoginStep::Saving).await;
                match service.persist_authenticated(authenticated).await {
                    Ok(summary) => {
                        sessions.set_syncing(&session_id).await;
                        runtime.set_step(LoginStep::Syncing).await;
                        sync_after_login(&service, summary, None)
                            .await
                            .map_err(|_| ProviderError::Storage)
                    }
                    Err(error) => Err(error),
                }
            }
        }
        Err(error) => Err(error),
    };
    sessions.finish(&session_id, result).await;
}

async fn logout_local(repository: &Repository, id: &str) -> Result<AuthLogoutResult, String> {
    validate_account_id(id)?;
    // Confirm the record exists before reporting a successful local deletion.
    repository
        .get_auth_account(id)
        .await
        .map_err(|_| storage_error())?;
    // ADR-38: deletion is local-only — remove the row (payload and model
    // snapshot live in it), no provider revoke endpoint is called.
    repository
        .delete_auth_account(id)
        .await
        .map_err(|_| storage_error())?;
    Ok(AuthLogoutResult { deleted: true })
}

fn quota_is_available(quota: Option<&QuotaState>, now: DateTime<Utc>) -> bool {
    let Some(quota) = quota else {
        return true;
    };
    if !quota.exceeded {
        return true;
    }
    quota
        .next_recover_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .is_some_and(|recover_at| recover_at.with_timezone(&Utc) <= now)
}

fn import_path(path: Option<String>) -> Result<PathBuf, String> {
    match path {
        Some(path) if !path.trim().is_empty() => Ok(PathBuf::from(path)),
        _ => CodexLogin::default_auth_json_path().map_err(safe_error),
    }
}

/// 返回原生文件选择器使用的默认 Codex 凭据路径。
/// Gemini/Antigravity 凭据故意不支持导入，因为它们属于不同的 OAuth client
/// 和本地存储格式，必须通过浏览器 OAuth 登录获取。
#[tauri::command]
pub async fn auth_default_import_path(provider: Option<String>) -> Result<String, String> {
    if provider.as_deref() == Some("gemini") {
        return Err("Antigravity credentials must be added through OAuth login".to_owned());
    }
    CodexLogin::default_auth_json_path()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(safe_error)
}

#[tauri::command]
pub async fn auth_accounts_list(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<AuthAccountDto>, String> {
    let repository = Repository::new(state.db.pool.clone());
    repository
        .list_auth_accounts()
        .await
        .map_err(|_| storage_error())?
        .into_iter()
        .map(dto_from_account)
        .collect()
}

/// Pure guard: device-code providers cannot use the synchronous `auth_login`
/// (no loopback callback).  Returns the stable error so `auth_login` refuses
/// before any network call and the session API is used instead.
fn refuse_device_code_login(kind: &ProviderKind) -> Result<(), String> {
    if crate::auth_provider::spec::provider_spec(kind)
        .is_some_and(|spec| spec.login_mode == crate::auth_provider::AuthLoginMode::DeviceCode)
    {
        return Err("interactive_session_required".to_owned());
    }
    Ok(())
}

/// Validate and normalize a Codex OAuth loopback callback URL.
///
/// Only `localhost`/`127.0.0.1`/`::1` hosts on ports `1455`/`1457` with the exact
/// `/auth/callback?code|error=&state=` shape are accepted. This blocks
/// open-redirect / SSRF via a malicious callback URL before any forward.
fn normalize_codex_callback_url(callback_url: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(callback_url.trim())
        .map_err(|_| "Invalid Codex callback URL".to_owned())?;
    let valid_host = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    let valid_port = matches!(url.port(), Some(1455 | 1457));
    let has_callback = url.path() == "/auth/callback"
        && url.query_pairs().any(|(key, _)| key == "state")
        && url
            .query_pairs()
            .any(|(key, _)| key == "code" || key == "error");
    if !valid_host || !valid_port || !has_callback {
        return Err("Callback must be the localhost Codex OAuth redirect URL".to_owned());
    }
    url.set_host(Some("127.0.0.1"))
        .map_err(|_| "Invalid Codex callback host".to_owned())?;
    Ok(url)
}

#[tauri::command]
pub async fn auth_login(
    provider: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthMutationResult, String> {
    #[cfg(not(feature = "desktop-ui"))]
    {
        let _ = (&provider, &app, &state);
        return Err("OAuth 登录仅桌面版可用，请使用 auth.json 导入".to_string());
    }
    #[cfg(feature = "desktop-ui")]
    {
        let kind = provider_kind(Some(provider))?;
        refuse_device_code_login(&kind)?;
        let runtime = TauriLoginRuntime::new(app);
        let summary = state
            .auth_service
            .login(kind, &runtime)
            .await
            .map_err(safe_error)?;
        sync_after_login(&state.auth_service, summary, None).await
    }
}

/// Renderer-safe provider capability row for the login picker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderDto {
    pub id: String,
    pub display_name: String,
    pub icon_key: String,
    pub login_mode: String,
    pub login_methods: Vec<String>,
    pub supports_import: bool,
    pub supports_export: bool,
    pub supports_quota: bool,
}

/// Registered providers available for interactive login (renderer-safe spec).
#[tauri::command]
pub async fn auth_providers_list() -> Result<Vec<AuthProviderDto>, String> {
    Ok(crate::auth_provider::spec::registered_provider_specs()
        .iter()
        .map(|spec| AuthProviderDto {
            id: spec.kind.to_owned(),
            display_name: spec.display_name.to_owned(),
            icon_key: spec.icon_key.to_owned(),
            login_mode: spec.login_mode.as_str().to_owned(),
            login_methods: spec
                .login_methods
                .iter()
                .map(|method| method.as_str().to_owned())
                .collect(),
            supports_import: spec.supports_import,
            supports_export: spec.supports_export,
            supports_quota: spec.supports_quota,
        })
        .collect())
}

/// Pure gate: browser-callback OAuth needs the desktop shell (loopback
/// listener + system browser); device-code providers also work headless
/// because the verification URL + user code travel through the polled
/// session status instead.
fn login_requires_desktop_shell(login_method: crate::auth_provider::AuthLoginMode) -> bool {
    login_method == crate::auth_provider::AuthLoginMode::BrowserCallback
}

fn resolve_login_method(
    kind: &ProviderKind,
    requested: Option<&str>,
) -> Result<crate::auth_provider::AuthLoginMode, String> {
    let spec = crate::auth_provider::spec::provider_spec(kind)
        .ok_or_else(|| "Unsupported auth provider".to_owned())?;
    let method = match requested {
        Some(value) => crate::auth_provider::AuthLoginMode::parse(value)
            .ok_or_else(|| "不支持的登录方式".to_owned())?,
        None => spec.login_mode,
    };
    if !spec.login_methods.contains(&method) {
        return Err("当前服务商不支持所选登录方式".to_owned());
    }
    Ok(method)
}

/// Shared core behind the `auth_login_start` command and the headless admin
/// dispatch.  `app` is the desktop shell handle; headless callers pass `None`,
/// which restricts the flow to device-code providers.
pub(crate) async fn auth_login_start_with(
    provider: String,
    replace_account_id: Option<String>,
    login_method: Option<String>,
    app: Option<tauri::AppHandle>,
    state: &Arc<AppState>,
) -> Result<AuthLoginStart, String> {
    let kind = provider_kind(Some(provider))?;
    let login_method = resolve_login_method(&kind, login_method.as_deref())?;
    if app.is_none() && login_requires_desktop_shell(login_method) {
        return Err(
            "浏览器 OAuth（localhost 回调）仅桌面端支持；请使用 Device Code 或导入 auth.json"
                .to_string(),
        );
    }
    // Validate the local account id syntactically before spawning so a bad id
    // fails the command (not a background task).  The payload is never touched.
    let target = match replace_account_id {
        Some(id) => {
            validate_account_id(&id)?;
            crate::auth_provider::LoginTarget::Replace {
                local_account_id: id,
            }
        }
        None => crate::auth_provider::LoginTarget::New,
    };
    let provider_name = kind.to_string();
    let (session_id, cancellation) = state.login_sessions.start(&provider_name).await;
    let sessions = state.login_sessions.clone();
    let service = state.auth_service.clone();
    let task_id = session_id.clone();
    tauri::async_runtime::spawn(async move {
        run_provider_login_session(
            sessions,
            task_id,
            kind,
            target,
            login_method,
            cancellation,
            app,
            service,
        )
        .await;
    });
    Ok(AuthLoginStart { session_id })
}

#[tauri::command]
pub async fn auth_login_start(
    provider: String,
    replace_account_id: Option<String>,
    login_method: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthLoginStart, String> {
    auth_login_start_with(
        provider,
        replace_account_id,
        login_method,
        Some(app),
        state.inner(),
    )
    .await
}

#[tauri::command]
pub async fn auth_login_status(
    session_id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthLoginSessionStatus, String> {
    state.login_sessions.status(&session_id).await
}

#[tauri::command]
pub async fn auth_login_cancel(
    session_id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthLoginSessionStatus, String> {
    state.login_sessions.cancel(&session_id).await
}

#[tauri::command]
pub async fn auth_login_import(
    provider: Option<String>,
    path: Option<String>,
    format: Option<String>,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthMutationResult, String> {
    let kind = provider_kind(provider)?;
    let path = import_path(path)?;
    let bytes = fs::read(path).map_err(|_| "Unable to read auth file".to_owned())?;
    import_and_sync(&state.auth_service, kind, bytes, format.as_deref()).await
}

/// Web 管理面板：直接以文件内容导入（浏览器 `<input type=file>` 上传，无服务器路径）。
#[tauri::command]
pub async fn auth_login_import_content(
    provider: Option<String>,
    content: String,
    format: Option<String>,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthMutationResult, String> {
    let kind = provider_kind(provider)?;
    let bytes = content.into_bytes();
    if bytes.is_empty() {
        return Err("Unable to read auth file".to_owned());
    }
    import_and_sync(&state.auth_service, kind, bytes, format.as_deref()).await
}

/// Shared import tail: persist every account in the file (single- or
/// multi-account formats), then model-sync each one.  The notice aggregates
/// skip counts for multi-account files.
async fn import_and_sync(
    service: &crate::auth_provider::service::AuthService,
    kind: ProviderKind,
    bytes: Vec<u8>,
    format: Option<&str>,
) -> Result<AuthMutationResult, String> {
    let (summaries, skipped) = service
        .import_all(kind.clone(), &bytes, format)
        .await
        .map_err(safe_error)?;
    let mut warning = None;
    for summary in &summaries {
        if let Err(error) = service.sync_models(&summary.id).await {
            tracing::warn!(
                account_id = %summary.id,
                provider = %kind,
                error = ?error.failure_class(),
                "model sync failed after import; account will not route until sync succeeds"
            );
            warning = Some(
                "Accounts saved, but model sync failed; they will not route until sync succeeds."
                    .to_owned(),
            );
        }
    }
    // Keep the existing primary-account field for compatibility, but also
    // return every persisted account so batch imports can identify all rows.
    let imported_accounts = summaries
        .iter()
        .cloned()
        .map(AuthAccountDto::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(safe_error)?;
    let account = imported_accounts
        .first()
        .cloned()
        .ok_or_else(|| "导入后未找到账号".to_owned())?;
    let mut notice = CODEX_IMPORT_NOTICE.to_owned();
    if summaries.len() > 1 || skipped > 0 {
        notice = format!(
            "成功导入 {} 个账号{}。",
            summaries.len(),
            if skipped > 0 {
                format!("，跳过 {skipped} 个非 Codex 或无效条目")
            } else {
                String::new()
            }
        );
    }
    Ok(AuthMutationResult {
        account: account.clone(),
        imported_accounts,
        warning,
        notice: Some(notice),
    })
}

#[tauri::command]
pub async fn auth_logout(
    id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthLogoutResult, String> {
    let repository = Repository::new(state.db.pool.clone());
    // ADR-38: v1 deletion is local-only, no provider revoke endpoint.
    logout_local(&repository, &id).await
}

#[tauri::command]
pub async fn auth_refresh_token(
    id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthAccountDto, String> {
    validate_account_id(&id)?;
    match state.auth_service.force_refresh_account(&id).await {
        Ok(summary) => AuthAccountDto::try_from(summary).map_err(safe_error),
        Err(error) => {
            let repository = Repository::new(state.db.pool.clone());
            let _ = repository.mark_invalid(&id, None, None).await;
            Err(safe_error(error))
        }
    }
}

#[tauri::command]
pub async fn auth_refresh_quota(
    id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthAccountDto, String> {
    validate_account_id(&id)?;
    state
        .auth_service
        .refresh_quota(&id)
        .await
        .map_err(safe_error)
        .and_then(|summary| AuthAccountDto::try_from(summary).map_err(safe_error))
}

#[tauri::command]
pub async fn auth_sync_models(
    id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthAccountDto, String> {
    validate_account_id(&id)?;
    state
        .auth_service
        .sync_models(&id)
        .await
        .map_err(safe_error)
        .and_then(|summary| AuthAccountDto::try_from(summary).map_err(safe_error))
}

#[tauri::command]
pub async fn auth_export_json(
    id: String,
    path: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthExportResult, String> {
    validate_account_id(&id)?;
    let path = PathBuf::from(path);
    let repository = Repository::new(state.db.pool.clone());
    let account = repository
        .get_auth_account(&id)
        .await
        .map_err(|_| storage_error())?;
    if account.provider != "codex" {
        return Err("Unsupported auth provider".to_owned());
    }
    // The raw credential JSON is decoded only in this native command and is
    // handed immediately to the provider-specific atomic exporter.
    let payload = serde_json::from_str(&account.payload_json)
        .map(ProviderPayload::new)
        .map_err(|_| safe_error(ProviderError::InvalidPayload))?;
    let result = CodexLogin::write_auth_json(&path, &payload).map_err(safe_error)?;
    Ok(AuthExportResult {
        path: result.path.to_string_lossy().into_owned(),
        backup_path: result
            .backup_path
            .map(|path| path.to_string_lossy().into_owned()),
    })
}

/// Web 管理面板：导出 auth.json 内容（由浏览器触发下载，不写服务器磁盘）。
#[tauri::command]
pub async fn auth_export_json_content(
    id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<String, String> {
    validate_account_id(&id)?;
    let repository = Repository::new(state.db.pool.clone());
    let account = repository
        .get_auth_account(&id)
        .await
        .map_err(|_| storage_error())?;
    if account.provider != "codex" {
        return Err("Unsupported auth provider".to_owned());
    }
    let payload = serde_json::from_str(&account.payload_json)
        .map(ProviderPayload::new)
        .map_err(|_| safe_error(ProviderError::InvalidPayload))?;
    CodexLogin::export_auth_json_content(&payload).map_err(safe_error)
}

#[tauri::command]
pub async fn auth_toggle(
    id: String,
    disabled: bool,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthAccountDto, String> {
    validate_account_id(&id)?;
    let repository = Repository::new(state.db.pool.clone());
    repository
        .update_auth_account_disabled(&id, disabled)
        .await
        .map_err(|_| storage_error())?;
    dto_from_account(
        repository
            .get_auth_account(&id)
            .await
            .map_err(|_| storage_error())?,
    )
}

/// 映射对快捷开启/关闭（迁移 042）：整体替换 `model_mapping_disabled`，
/// 不触碰其余字段。前端（卡片/列表视图）乐观更新后调用。
#[tauri::command]
pub async fn auth_update_mapping_disabled(
    id: String,
    disabled: Vec<Vec<String>>,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthAccountDto, String> {
    validate_account_id(&id)?;
    let json = serde_json::to_string(&disabled).map_err(|_| storage_error())?;
    let repository = Repository::new(state.db.pool.clone());
    repository
        .update_auth_account_mapping_disabled(&id, &json)
        .await
        .map_err(|_| storage_error())?;
    dto_from_account(
        repository
            .get_auth_account(&id)
            .await
            .map_err(|_| storage_error())?,
    )
}

#[tauri::command]
pub async fn auth_quota_status(
    id: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthQuotaStatus, String> {
    validate_account_id(&id)?;
    let repository = Repository::new(state.db.pool.clone());
    let account = repository
        .get_auth_account(&id)
        .await
        .map_err(|_| storage_error())?;
    let quota = account
        .quota_state()
        .map_err(|_| safe_error(ProviderError::InvalidPayload))?;
    Ok(AuthQuotaStatus {
        available: quota_is_available(quota.as_ref(), Utc::now()),
        quota,
    })
}

#[tauri::command]
pub async fn auth_update(
    input: AuthUpdateInput,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<AuthAccountDto, String> {
    // Validate here, before creating any repository call, so invalid user
    // values cannot cause even a no-op database write.
    validate_update(&input)?;
    let model_mapping_json = input
        .model_mapping
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_else(|| "{}".to_string());
    let repository = Repository::new(state.db.pool.clone());
    // None = 不修改（沿用库中现值）；Some = 整体替换。
    let current_disabled = repository
        .get_auth_account(&input.id)
        .await
        .ok()
        .map(|account| account.model_mapping_disabled)
        .unwrap_or_else(|| "[]".to_string());
    let model_mapping_disabled_json = input
        .model_mapping_disabled
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string()))
        .unwrap_or(current_disabled);
    repository
        .update_auth_account(
            &input.id,
            input.label.trim(),
            input.priority,
            input.weight,
            &model_mapping_json,
            &model_mapping_disabled_json,
        )
        .await
        .map_err(|_| storage_error())?;
    dto_from_account(
        repository
            .get_auth_account(&input.id)
            .await
            .map_err(|_| storage_error())?,
    )
}

/// 手动拖拽排序：按传入顺序重写 sort_order（首元素最大）。
#[tauri::command]
pub async fn auth_reorder_accounts(
    ordered_ids: Vec<String>,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    auth_reorder_accounts_impl(&ordered_ids, &*state).await
}

pub async fn auth_reorder_accounts_impl(
    ordered_ids: &[String],
    state: &Arc<AppState>,
) -> Result<(), String> {
    let repository = Repository::new(state.db.pool.clone());
    repository
        .reorder_auth_accounts(ordered_ids)
        .await
        .map_err(|_| storage_error())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;
    use crate::db::{models::AuthAccountUpsert, repository::Repository};

    const ACCESS: &str = "fixture-access-token";
    const REFRESH: &str = "fixture-refresh-token";
    const ID_TOKEN: &str = "fixture-id-token";

    fn account_fixture() -> crate::db::models::AuthAccount {
        crate::db::models::AuthAccount {
            id: "account-1".into(), provider: "codex".into(), label: "Codex".into(),
            account_id: "provider-account-1".into(), status: "active".into(), disabled: 0,
            priority: 0, weight: 1, sort_order: 0, quota_json: None,
            model_states_json: json!({"version":1,"models":[]}).to_string(),
            attributes_json: json!({"email":"person@example.test","plan_type":"plus","ignored":"secret"}).to_string(),
            model_mapping_json: "{}".to_string(),
            model_mapping_disabled: "[]".to_string(),
            payload_json: json!({"access_token":ACCESS,"refresh_token":REFRESH,"id_token":ID_TOKEN,"expires_at":"2030-01-01T00:00:00Z"}).to_string(),
            last_refreshed_at: None, last_models_sync_at: None, next_refresh_after: None,
            next_retry_after: None, created_at: "2026-01-01T00:00:00Z".into(), updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn auth_update_validation_rejects_invalid_values_before_storage() {
        for input in [
            AuthUpdateInput {
                id: "account-1".into(),
                label: "   ".into(),
                priority: 0,
                weight: 1,
                model_mapping: None,
                model_mapping_disabled: None,
            },
            AuthUpdateInput {
                id: "account-1".into(),
                label: "Codex".into(),
                priority: -1,
                weight: 1,
                model_mapping: None,
                model_mapping_disabled: None,
            },
            AuthUpdateInput {
                id: "account-1".into(),
                label: "Codex".into(),
                priority: 0,
                weight: 0,
                model_mapping: None,
                model_mapping_disabled: None,
            },
        ] {
            assert!(validate_update(&input).is_err());
        }
    }

    #[test]
    fn account_and_mutation_dtos_never_serialize_credentials_or_payload_names() {
        let dto = dto_from_account(account_fixture()).unwrap();
        let list = serde_json::to_string(&vec![dto.clone()]).unwrap();
        let mutation = serde_json::to_string(&AuthMutationResult {
            account: dto,
            imported_accounts: vec![],
            warning: None,
            notice: None,
        })
        .unwrap();
        let logout = serde_json::to_string(&AuthLogoutResult { deleted: true }).unwrap();
        let export = serde_json::to_string(&AuthExportResult {
            path: "/tmp/auth.json".into(),
            backup_path: Some("/tmp/auth.json.bak".into()),
        })
        .unwrap();
        for encoded in [list, mutation, logout, export] {
            for forbidden in [
                ACCESS,
                REFRESH,
                ID_TOKEN,
                "access_token",
                "refresh_token",
                "id_token",
                "payload_json",
            ] {
                assert!(
                    !encoded.contains(forbidden),
                    "serialized response leaked {forbidden}"
                );
            }
        }
    }

    #[test]
    fn account_dto_surfaces_invalidation_reason_without_credential_material() {
        let mut account = account_fixture();
        account.status = "invalid".into();
        // Reason lives in attributes_json, mirroring Repository::mark_invalid.
        account.attributes_json = json!({
            "email": "person@example.test",
            "invalidation_reason": "payment_required"
        })
        .to_string();
        let dto = dto_from_account(account).unwrap();
        assert_eq!(dto.invalidation_reason.as_deref(), Some("payment_required"));
        // The serialized DTO carries the reason but never payload fields.
        let encoded = serde_json::to_string(&dto).unwrap();
        assert!(encoded.contains("payment_required"));
        assert!(!encoded.contains("access_token") && !encoded.contains("refresh_token"));
    }

    async fn test_repository() -> Repository {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        Repository::new(pool)
    }

    #[tokio::test]
    async fn logout_deletes_local_account_only() {
        let repository = test_repository().await;
        let account = repository
            .upsert_by_provider_account_id(&AuthAccountUpsert {
                provider: "codex".into(),
                label: "Codex".into(),
                account_id: "provider-1".into(),
                attributes: json!({}),
                payload: json!({"version":1}),
                last_refreshed_at: None,
                next_refresh_after: None,
                next_retry_after: None,
            })
            .await
            .unwrap();
        let result = logout_local(&repository, &account.id).await.unwrap();
        assert!(result.deleted);
        // Local-only deletion: no provider call, no warning surfaced (ADR-38).
        assert!(repository.get_auth_account(&account.id).await.is_err());
    }

    #[tokio::test]
    async fn cancelled_session_is_idempotent_and_never_enters_persistence() {
        let sessions = LoginSessions::new();
        let (id, _receiver) = sessions.start("codex").await;
        let first = sessions.cancel(&id).await.unwrap();
        let second = sessions.cancel(&id).await.unwrap();
        assert_eq!(first.state, "cancelled");
        assert_eq!(second.state, "cancelled");
        // This is the persistence commit gate used after callback/token work.
        assert!(!sessions.begin_save(&id).await);
        let encoded = serde_json::to_string(&second).unwrap();
        for forbidden in [
            ACCESS,
            REFRESH,
            ID_TOKEN,
            "access_token",
            "refresh_token",
            "id_token",
        ] {
            assert!(!encoded.contains(forbidden), "status leaked {forbidden}");
        }
    }

    #[tokio::test]
    async fn terminal_error_status_is_queryable_and_categorized() {
        let sessions = LoginSessions::new();
        let (id, _receiver) = sessions.start("codex").await;
        sessions.finish(&id, Err(ProviderError::LoginTimeout)).await;
        let status = sessions.status(&id).await.unwrap();
        assert_eq!(status.state, "failed");
        assert_eq!(status.error_code.as_deref(), Some("timeout"));
        assert!(status
            .error
            .as_deref()
            .is_some_and(|message| !message.is_empty()));
    }

    // --- C7: provider-neutral sessions & commands ---

    #[tokio::test]
    async fn providers_list_contains_codex_and_kimi_with_exact_capabilities() {
        let providers = auth_providers_list().await.unwrap();
        let codex = providers.iter().find(|p| p.id == "codex").unwrap();
        assert_eq!(codex.display_name, "Codex");
        assert_eq!(codex.icon_key, "codex");
        assert_eq!(codex.login_mode, "browser_callback");
        assert_eq!(
            codex.login_methods,
            vec!["browser_callback".to_owned(), "device_code".to_owned()]
        );
        assert!(codex.supports_import);
        assert!(codex.supports_export);
        assert!(codex.supports_quota);
        let kimi = providers.iter().find(|p| p.id == "kimi").unwrap();
        assert_eq!(kimi.display_name, "Kimi Code");
        assert_eq!(kimi.icon_key, "moonshot");
        assert_eq!(kimi.login_mode, "device_code");
        assert_eq!(kimi.login_methods, vec!["device_code".to_owned()]);
        assert!(!kimi.supports_import);
        assert!(!kimi.supports_export);
        assert!(!kimi.supports_quota);
        let gemini = providers.iter().find(|p| p.id == "gemini").unwrap();
        assert_eq!(gemini.display_name, "Antigravity");
        assert_eq!(gemini.icon_key, "google");
        assert_eq!(gemini.login_mode, "browser_callback");
        assert_eq!(gemini.login_methods, vec!["browser_callback".to_owned()]);
        assert!(!gemini.supports_import);
        assert!(!gemini.supports_export);
        assert!(gemini.supports_quota);
        let grok = providers.iter().find(|p| p.id == "grok").unwrap();
        assert_eq!(grok.display_name, "Grok");
        assert_eq!(grok.icon_key, "grok");
        assert_eq!(grok.login_mode, "device_code");
        assert_eq!(grok.login_methods, vec!["device_code".to_owned()]);
        assert!(!grok.supports_import);
        assert!(!grok.supports_export);
        assert!(!grok.supports_quota);
        assert_eq!(providers.len(), 4);
    }

    #[tokio::test]
    async fn default_import_path_rejects_gemini_and_keeps_codex() {
        assert!(auth_default_import_path(Some("gemini".into()))
            .await
            .is_err());
        let codex = auth_default_import_path(None).await.unwrap();
        assert!(codex.contains("auth.json"));
    }

    #[test]
    fn provider_kind_resolves_registered_providers_and_rejects_unknown() {
        assert_eq!(
            provider_kind(Some("kimi".into())).unwrap(),
            ProviderKind::Kimi
        );
        assert_eq!(
            provider_kind(Some("codex".into())).unwrap(),
            ProviderKind::Codex
        );
        assert_eq!(
            provider_kind(Some("gemini".into())).unwrap(),
            ProviderKind::Gemini
        );
        assert_eq!(
            provider_kind(Some("grok".into())).unwrap(),
            ProviderKind::Grok
        );
        assert!(provider_kind(Some("nope".into())).is_err());
    }

    #[test]
    fn login_shell_gate_only_blocks_browser_callback_providers() {
        // Device-code providers (Kimi) must be allowed headless (Docker/web
        // admin panel); browser-callback providers (Codex) still require the
        // desktop shell for the loopback listener + system browser.
        assert!(!login_requires_desktop_shell(
            crate::auth_provider::AuthLoginMode::DeviceCode
        ));
        assert!(login_requires_desktop_shell(
            crate::auth_provider::AuthLoginMode::BrowserCallback
        ));
    }

    #[test]
    fn login_method_defaults_are_backward_compatible_and_capabilities_are_enforced() {
        assert_eq!(
            resolve_login_method(&ProviderKind::Codex, None).unwrap(),
            crate::auth_provider::AuthLoginMode::BrowserCallback
        );
        assert_eq!(
            resolve_login_method(&ProviderKind::Kimi, None).unwrap(),
            crate::auth_provider::AuthLoginMode::DeviceCode
        );
        assert_eq!(
            resolve_login_method(&ProviderKind::Grok, None).unwrap(),
            crate::auth_provider::AuthLoginMode::DeviceCode
        );
        assert_eq!(
            resolve_login_method(&ProviderKind::Codex, Some("device_code")).unwrap(),
            crate::auth_provider::AuthLoginMode::DeviceCode
        );
        assert!(resolve_login_method(&ProviderKind::Gemini, Some("device_code")).is_err());
        assert!(resolve_login_method(&ProviderKind::Kimi, Some("browser_callback")).is_err());
        assert!(resolve_login_method(&ProviderKind::Grok, Some("browser_callback")).is_err());
        assert!(resolve_login_method(&ProviderKind::Codex, Some("unknown")).is_err());
    }

    #[test]
    fn login_error_message_surfaces_google_validation_url() {
        let msg = login_error_message(&ProviderError::ValidationRequired {
            url: "https://accounts.google.com/tos".into(),
        });
        assert!(msg.contains("https://accounts.google.com/tos"));
        assert_eq!(
            login_error_code(&ProviderError::PermissionDenied),
            "permission_denied"
        );
    }

    #[tokio::test]
    async fn session_stores_the_actual_provider_not_codex() {
        let sessions = LoginSessions::new();
        let (id, _receiver) = sessions.start("kimi").await;
        let status = sessions.status(&id).await.unwrap();
        assert_eq!(status.provider, "kimi");
        assert_eq!(status.state, "pending");
    }

    #[tokio::test]
    async fn verification_dto_carries_url_and_user_code_but_no_device_code() {
        let sessions = LoginSessions::new();
        let (id, _receiver) = sessions.start("kimi").await;
        sessions
            .set_verification(&id, "https://auth.example.test/verify", "ABCD-EFGH", None)
            .await;
        let status = sessions.status(&id).await.unwrap();
        let verification = status.verification.expect("verification present");
        assert_eq!(verification.url, "https://auth.example.test/verify");
        assert_eq!(verification.user_code, "ABCD-EFGH");
        let encoded = serde_json::to_string(&verification).unwrap();
        assert!(!encoded.contains("device_code"), "encoded: {encoded}");
        assert!(
            encoded.contains("ABCD-EFGH"),
            "user code must be visible, got: {encoded}"
        );
    }

    #[tokio::test]
    async fn verification_is_cleared_on_begin_save_cancel_and_finish() {
        // begin_save clears it.
        let sessions = LoginSessions::new();
        let (id, _receiver) = sessions.start("kimi").await;
        sessions
            .set_verification(&id, "https://u", "ABCD", None)
            .await;
        assert!(sessions.begin_save(&id).await);
        assert!(sessions.status(&id).await.unwrap().verification.is_none());

        // cancel clears it.
        let sessions = LoginSessions::new();
        let (id2, _receiver) = sessions.start("kimi").await;
        sessions
            .set_verification(&id2, "https://u", "ABCD", None)
            .await;
        sessions.cancel(&id2).await.unwrap();
        assert!(sessions.status(&id2).await.unwrap().verification.is_none());

        // finish (success) clears it.
        let sessions = LoginSessions::new();
        let (id3, _receiver) = sessions.start("kimi").await;
        sessions
            .set_verification(&id3, "https://u", "ABCD", None)
            .await;
        sessions
            .finish(&id3, Err(ProviderError::AuthorizationDenied))
            .await;
        assert!(sessions.status(&id3).await.unwrap().verification.is_none());
    }

    #[test]
    fn legacy_auth_login_kimi_refuses_before_network_interactive_session_required() {
        // `provider_kind("kimi")` resolves and the DeviceCode guard in
        // `auth_login` returns the stable error before any provider call.
        let kind = provider_kind(Some("kimi".into())).unwrap();
        assert_eq!(kind, ProviderKind::Kimi);
        assert_eq!(
            refuse_device_code_login(&kind),
            Err("interactive_session_required".to_owned())
        );
        assert_eq!(
            refuse_device_code_login(&ProviderKind::Grok),
            Err("interactive_session_required".to_owned())
        );
        // The same guard lets the loopback provider through.
        assert_eq!(refuse_device_code_login(&ProviderKind::Codex), Ok(()));
    }

    #[tokio::test]
    async fn terminal_session_tombstone_is_idempotent() {
        let sessions = LoginSessions::new();
        let (id, _receiver) = sessions.start("kimi").await;
        sessions
            .finish(&id, Err(ProviderError::AuthorizationDenied))
            .await;
        let first = sessions.status(&id).await.unwrap();
        // Repeated cancel after a terminal state returns the same tombstone.
        let cancelled = sessions.cancel(&id).await.unwrap();
        assert_eq!(cancelled.state, first.state);
    }

    #[test]
    fn remote_codex_callback_forwarder_only_accepts_registered_loopback_targets() {
        let url = normalize_codex_callback_url(
            "http://localhost:1455/auth/callback?code=secret-code&state=csrf-state",
        )
        .unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(1455));
        assert!(
            normalize_codex_callback_url("https://evil.example/auth/callback?code=x&state=y")
                .is_err()
        );
        assert!(
            normalize_codex_callback_url("http://localhost:9999/auth/callback?code=x&state=y")
                .is_err()
        );
        assert!(
            normalize_codex_callback_url("http://localhost:1455/other?code=x&state=y").is_err()
        );
    }
}
