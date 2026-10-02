//! Provider-neutral authentication boundary.  Provider implementations own
//! OAuth/import/HTTP details; this module owns object-safe dispatch and lookup.

pub mod codex_backend;
pub mod codex_login;
pub mod gemini_backend;
pub mod gemini_login;
pub mod grok_backend;
pub mod grok_login;
pub mod kimi_backend;
pub mod kimi_login;
pub mod maintenance;
pub mod service;
pub mod spec;
pub mod types;

use async_trait::async_trait;
use std::{collections::HashMap, sync::Arc};

pub use crate::db::models::{AuthAccount, QuotaState};
pub use spec::{AuthLoginMode, AuthNonStreamFraming, ProviderSpec};
pub use types::{
    AuthAccountSummary, AuthenticatedLogin, LoginResult, MultiImportResult, ProviderError,
    ProviderKind, ProviderLoginContext, ProviderModels, ProviderPayload, ProviderRequest,
    RefreshedPayload, ReplacementContext, ResetCredit, ResetCreditCode, ResetCreditOutcome,
    ResetCreditsSnapshot,
};

/// 流式出站客户端：只限制连接建立时间，**不设总超时**。
///
/// reqwest 的 `Client::timeout` 覆盖从发送到读完响应体的整段耗时；用它跑 SSE
/// 会在固定秒数处切断仍在正常输出的长流，下游表现为
/// `stream interrupted: error decoding response body (root: operation timed out)`。
/// 因此所有 provider 的**流式**出站都必须使用这个客户端；非流式请求继续用各自
/// 带总超时的客户端。连接超时与渠道路径的 `streaming_client` 保持一致。
pub(crate) fn streaming_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("streaming provider http client")
}

/// Progress marker for an interactive provider login.  Concrete providers map
/// their real work onto these steps; the command layer never fabricates
/// progress with timers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginStep {
    Preparing,
    Authorizing,
    Waiting,
    Exchanging,
    Saving,
    Syncing,
}

impl LoginStep {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Authorizing => "authorizing",
            Self::Waiting => "waiting",
            Self::Exchanging => "exchanging",
            Self::Saving => "saving",
            Self::Syncing => "syncing",
        }
    }
}

/// Minimal host capability needed by an interactive provider login.  Specific
/// providers may add their own local callback handling without coupling that
/// logic to Tauri commands.  New methods have no silent no-op default: every
/// implementation (production runtime and tests) must reconcile them.
#[async_trait]
pub trait LoginRuntime: Send + Sync {
    async fn open_browser(&self, url: &str) -> Result<(), ProviderError>;

    /// Persist a non-secret progress step before doing the corresponding work.
    async fn set_step(&self, step: LoginStep);

    /// Surface device-authorization details (URL + user code) before opening
    /// the browser, so the user can authorize manually if opening fails.
    async fn present_device_authorization(
        &self,
        verification_url: &str,
        user_code: &str,
        expires_at: Option<String>,
    ) -> Result<(), ProviderError>;

    /// Whether the caller asked this login to stop.
    fn is_cancelled(&self) -> bool;

    /// Wait until the caller cancels the login.  Used to interrupt HTTP waits
    /// and interval sleeps promptly rather than at the next poll boundary.
    async fn cancelled(&self);
}

/// Where a login result should land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginTarget {
    /// Create a new account, upserting by `(provider, provider_account_id)`.
    New,
    /// Overwrite the specified local account (re-login) in place.
    Replace { local_account_id: String },
}

/// Object-safe provider contract.  Credentials only cross this boundary as a
/// `ProviderPayload`, whose debug output is redacted.
#[async_trait]
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    async fn login(
        &self,
        context: &ProviderLoginContext,
        runtime: &dyn LoginRuntime,
    ) -> Result<LoginResult, ProviderError>;

    async fn import(&self, bytes: &[u8]) -> Result<LoginResult, ProviderError>;

    /// Format-aware import.  Single-account formats (codex, cpa) yield exactly
    /// one result; sub2api admin-data exports may yield several, and entries
    /// that cannot be imported (other platforms, failed refreshes) are counted
    /// in `skipped` instead of failing the whole file.
    async fn import_all(
        &self,
        bytes: &[u8],
        _format: Option<&str>,
    ) -> Result<MultiImportResult, ProviderError> {
        let result = self.import(bytes).await?;
        Ok(MultiImportResult {
            results: vec![result],
            skipped: 0,
        })
    }

    async fn refresh(&self, payload: &ProviderPayload) -> Result<RefreshedPayload, ProviderError>;

    async fn outbound(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<reqwest::Response, ProviderError>;

    async fn list_models(
        &self,
        account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<ProviderModels, ProviderError>;

    /// Probe the provider's dedicated quota endpoint.  `Ok(None)` means no quota
    /// data is currently available (callers preserve previously persisted state).
    /// The default is a no-op so providers without a dedicated endpoint stay
    /// header/cooldown-only.
    async fn fetch_quota(
        &self,
        _account: &AuthAccount,
        _payload: &ProviderPayload,
    ) -> Result<Option<QuotaState>, ProviderError> {
        Ok(None)
    }

    /// 查询 provider 提供的额度重置卡。未实现的 provider 在发网前失败。
    async fn list_reset_credits(
        &self,
        _account: &AuthAccount,
        _payload: &ProviderPayload,
    ) -> Result<ResetCreditsSnapshot, ProviderError> {
        Err(ProviderError::UnsupportedFeatures {
            pointer: "/reset_credits/list".into(),
        })
    }

    /// 消费一张额度重置卡。调用方必须传入持久化的幂等键。
    async fn consume_reset_credit(
        &self,
        _account: &AuthAccount,
        _payload: &ProviderPayload,
        _redeem_request_id: &str,
        _credit_id: &str,
    ) -> Result<ResetCreditOutcome, ProviderError> {
        Err(ProviderError::UnsupportedFeatures {
            pointer: "/reset_credits/consume".into(),
        })
    }
}

/// Runtime registry, deliberately separate from persisted provider strings.
/// An account is usable only when its provider is registered.
#[derive(Clone)]
pub struct ProviderRegistry {
    providers: HashMap<ProviderKind, Arc<dyn Provider>>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        let mut registry = Self {
            providers: HashMap::new(),
        };
        registry.register(Arc::new(codex_backend::CodexProvider::new()));
        registry.register(Arc::new(kimi_backend::KimiProvider::new()));
        registry.register(Arc::new(gemini_backend::GeminiProvider::new()));
        registry.register(Arc::new(grok_backend::GrokProvider::new()));
        registry
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.kind(), provider);
    }

    pub fn get(&self, kind: &ProviderKind) -> Result<Arc<dyn Provider>, ProviderError> {
        self.providers
            .get(kind)
            .cloned()
            .ok_or_else(|| ProviderError::UnknownProvider {
                provider: kind.to_string(),
            })
    }

    pub fn provider_for_name(&self, provider: &str) -> Result<Arc<dyn Provider>, ProviderError> {
        self.get(&ProviderKind::from(provider))
    }
}
