//! Antigravity OAuth 辅助逻辑：本地回调、授权码交换和令牌刷新。
//!
//! 文件名和 `GeminiLogin` 类型名保留 `Gemini`，因为它们属于内部
//! provider ID、数据库兼容标识和 Gemini codec；用户界面与实际 OAuth
//! client 使用的品牌是 Antigravity。

use std::{
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex};

use super::{ProviderError, ProviderPayload, RefreshedPayload};

// OAuth client material 由部署环境提供，禁止写入仓库或日志。
// client ID 与 secret 必须成对使用，不能拿旧 Gemini CLI client 刷新本流程令牌。
pub const ANTIGRAVITY_CLIENT_ID_ENV: &str = "WALIAPI_ANTIGRAVITY_CLIENT_ID";
pub const ANTIGRAVITY_CLIENT_SECRET_ENV: &str = "WALIAPI_ANTIGRAVITY_CLIENT_SECRET";
pub const GOOGLE_OAUTH_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const ANTIGRAVITY_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
// payload marker 用于阻止旧 Gemini CLI 凭据被错误交给 Antigravity client。
pub const ANTIGRAVITY_OAUTH_CLIENT: &str = "antigravity";
pub const ANTIGRAVITY_PAYLOAD_VERSION: u64 = 2;

const OAUTH_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";

#[derive(Clone)]
pub struct GeminiLogin {
    authorize_url: String,
    token_url: String,
    client_id: Option<String>,
    client_secret: Option<String>,
    timeout: Duration,
    client: reqwest::Client,
}

#[derive(Clone, Debug)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: Option<String>,
    pub project_hint: Option<String>,
}

impl Default for GeminiLogin {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiLogin {
    pub fn new() -> Self {
        Self {
            authorize_url: GOOGLE_OAUTH_AUTHORIZE_URL.to_owned(),
            token_url: GOOGLE_OAUTH_TOKEN_URL.to_owned(),
            client_id: configured_secret(ANTIGRAVITY_CLIENT_ID_ENV),
            client_secret: configured_secret(ANTIGRAVITY_CLIENT_SECRET_ENV),
            timeout: Duration::from_secs(5 * 60),
            client: http_client(),
        }
    }

    pub fn with_endpoints(authorize_url: impl Into<String>, token_url: impl Into<String>) -> Self {
        Self {
            authorize_url: authorize_url.into(),
            token_url: token_url.into(),
            client_id: Some("test-antigravity-client-id".to_owned()),
            client_secret: Some("test-antigravity-client-secret".to_owned()),
            timeout: Duration::from_secs(5 * 60),
            client: http_client(),
        }
    }

    fn credentials(&self) -> Result<(&str, &str), ProviderError> {
        match (self.client_id.as_deref(), self.client_secret.as_deref()) {
            (Some(client_id), Some(client_secret)) => Ok((client_id, client_secret)),
            _ => Err(ProviderError::LoginFailed),
        }
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn tokens_to_payload(tokens: &OAuthTokens) -> ProviderPayload {
        let mut payload = json!({
            "version": ANTIGRAVITY_PAYLOAD_VERSION,
            "oauth_client": ANTIGRAVITY_OAUTH_CLIENT,
            "access_token": tokens.access_token,
            "refresh_token": tokens.refresh_token,
        });
        if let Some(expires_at) = &tokens.expires_at {
            payload["expires_at"] = json!(expires_at);
        }
        ProviderPayload::new(payload)
    }

    pub async fn login(
        &self,
        runtime: &dyn super::LoginRuntime,
    ) -> Result<OAuthTokens, ProviderError> {
        if runtime.is_cancelled() {
            return Err(ProviderError::LoginCancelled);
        }
        runtime.set_step(super::LoginStep::Preparing).await;
        let (client_id, _) = self.credentials()?;
        // listener 只绑定回环地址；授权协议使用 localhost redirect，二者都不会
        // 把回调端口暴露到局域网。state mismatch 在 callback 层拒绝且不消耗 session。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| ProviderError::LoginFailed)?;
        let port = listener
            .local_addr()
            .map_err(|_| ProviderError::LoginFailed)?
            .port();
        let redirect_uri = format!("http://localhost:{port}/oauth-callback");
        let state = uuid::Uuid::new_v4().simple().to_string();
        let callback_state = CallbackState::new(state.clone());
        let app = Router::new()
            .route("/oauth-callback", get(oauth_callback))
            .with_state(callback_state.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let browser_url = authorization_url(&self.authorize_url, client_id, &redirect_uri, &state);

        let result = async {
            runtime.set_step(super::LoginStep::Authorizing).await;
            runtime
                .open_browser(&browser_url)
                .await
                .map_err(|_| ProviderError::BrowserOpenFailed)?;
            if runtime.is_cancelled() {
                return Err(ProviderError::LoginCancelled);
            }
            runtime.set_step(super::LoginStep::Waiting).await;
            // 成功页面只表示 callback 已收到；token exchange、保存和模型同步仍在应用内继续。
            let callback = tokio::select! {
                _ = runtime.cancelled() => Err(ProviderError::LoginCancelled),
                callback = tokio::time::timeout(self.timeout, callback_state.receive()) => {
                    callback.map_err(|_| ProviderError::LoginTimeout)?
                }
            }?;
            if runtime.is_cancelled() {
                return Err(ProviderError::LoginCancelled);
            }
            runtime.set_step(super::LoginStep::Exchanging).await;
            tokio::select! {
                _ = runtime.cancelled() => Err(ProviderError::LoginCancelled),
                result = self.exchange_code(&redirect_uri, &callback) => result,
            }
        }
        .await;
        server.abort();
        let _ = server.await;
        result
    }

    async fn exchange_code(
        &self,
        redirect_uri: &str,
        code: &str,
    ) -> Result<OAuthTokens, ProviderError> {
        let (client_id, client_secret) = self.credentials()?;
        let response = self
            .client
            .post(&self.token_url)
            .header("Accept", "application/json")
            .form(&[
                ("code", code),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("redirect_uri", redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .map_err(|_| ProviderError::TokenExchangeFailed)?;
        parse_token_response(response).await
    }

    pub async fn refresh_payload(
        &self,
        payload: &ProviderPayload,
    ) -> Result<RefreshedPayload, ProviderError> {
        ensure_antigravity_payload(payload)?;
        let refresh = payload
            .as_value()
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(ProviderError::InvalidPayload)?;
        let mut attempt = 0_u32;
        loop {
            match self.refresh_once(refresh).await {
                Ok(tokens) => {
                    return Ok(RefreshedPayload {
                        payload: Self::tokens_to_payload(&tokens),
                        last_refreshed_at: Some(Utc::now().to_rfc3339()),
                        next_refresh_after: None,
                        next_retry_after: None,
                    })
                }
                Err(RefreshError::Unauthorized) => return Err(ProviderError::Unauthorized),
                Err(RefreshError::Configuration) => return Err(ProviderError::LoginFailed),
                Err(RefreshError::Protocol) => return Err(ProviderError::Protocol),
                Err(RefreshError::Retryable) => {
                    attempt += 1;
                    if attempt >= 3 {
                        return Err(ProviderError::Retryable);
                    }
                    tokio::time::sleep(Duration::from_secs(attempt as u64)).await;
                }
            }
        }
    }

    async fn refresh_once(&self, refresh: &str) -> Result<OAuthTokens, RefreshError> {
        let (client_id, client_secret) = self
            .credentials()
            .map_err(|_| RefreshError::Configuration)?;
        let response = self
            .client
            .post(&self.token_url)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
            ])
            .send()
            .await
            .map_err(|_| RefreshError::Retryable)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(RefreshError::Unauthorized);
        }
        if status.as_u16() == 400 {
            let body = response.text().await.unwrap_or_default();
            if body.contains("invalid_grant") {
                return Err(RefreshError::Unauthorized);
            }
            return Err(RefreshError::Protocol);
        }
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            return Err(RefreshError::Retryable);
        }
        if !status.is_success() {
            return Err(RefreshError::Retryable);
        }
        parse_token_json(
            response.json().await.map_err(|_| RefreshError::Protocol)?,
            Some(refresh),
        )
        .map_err(|_| RefreshError::Protocol)
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(ANTIGRAVITY_HTTP_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

enum RefreshError {
    Configuration,
    Unauthorized,
    Retryable,
    Protocol,
}

// 不使用 PKCE 是为了匹配已验证的 Antigravity client contract，并非遗漏安全参数；
// state 仍由 WaLiAPI 本地生成并严格校验。
fn authorization_url(authorize: &str, client_id: &str, redirect_uri: &str, state: &str) -> String {
    let mut url = reqwest::Url::parse(authorize).expect("configured authorize URL");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", OAUTH_SCOPES)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state);
    url.to_string()
}

fn configured_secret(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

// 旧 payload 必须 fail closed：不能用新的 Antigravity client 刷新或发送旧凭据。
pub fn ensure_antigravity_payload(payload: &ProviderPayload) -> Result<(), ProviderError> {
    let value = payload.as_value();
    let compatible = value.get("version").and_then(Value::as_u64)
        == Some(ANTIGRAVITY_PAYLOAD_VERSION)
        && value.get("oauth_client").and_then(Value::as_str) == Some(ANTIGRAVITY_OAUTH_CLIENT);
    if compatible {
        Ok(())
    } else {
        Err(ProviderError::CredentialMigrationRequired)
    }
}

async fn parse_token_response(response: reqwest::Response) -> Result<OAuthTokens, ProviderError> {
    if !response.status().is_success() {
        return Err(ProviderError::TokenExchangeFailed);
    }
    let body: Value = response.json().await.map_err(|_| ProviderError::Protocol)?;
    parse_token_json(body, None)
}

fn parse_token_json(
    body: Value,
    fallback_refresh: Option<&str>,
) -> Result<OAuthTokens, ProviderError> {
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(ProviderError::Protocol)?
        .to_owned();
    let refresh_token = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| fallback_refresh.map(str::to_owned))
        .ok_or(ProviderError::Protocol)?;
    let expires_at = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|secs| (Utc::now() + chrono::Duration::seconds(secs as i64)).to_rfc3339());
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at,
        project_hint: None,
    })
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    used: Arc<AtomicBool>,
    sender: Arc<Mutex<Option<oneshot::Sender<Result<String, ProviderError>>>>>,
    receiver: Arc<Mutex<Option<oneshot::Receiver<Result<String, ProviderError>>>>>,
}

impl CallbackState {
    fn new(expected_state: String) -> Self {
        let (sender, receiver) = oneshot::channel();
        Self {
            expected_state,
            used: Arc::new(AtomicBool::new(false)),
            sender: Arc::new(Mutex::new(Some(sender))),
            receiver: Arc::new(Mutex::new(Some(receiver))),
        }
    }

    async fn receive(&self) -> Result<String, ProviderError> {
        let receiver = self
            .receiver
            .lock()
            .await
            .take()
            .ok_or(ProviderError::LoginFailed)?;
        receiver.await.map_err(|_| ProviderError::LoginFailed)?
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn oauth_callback(
    State(callback): State<CallbackState>,
    Query(query): Query<CallbackQuery>,
) -> axum::response::Response {
    if query.state.as_deref() != Some(callback.expected_state.as_str()) {
        return (StatusCode::BAD_REQUEST, "invalid OAuth state").into_response();
    }
    if callback.used.swap(true, Ordering::SeqCst) {
        return (StatusCode::CONFLICT, "OAuth callback already used").into_response();
    }
    let (result, status, message) = match (query.error, query.code) {
        (Some(_), _) => (
            Err(ProviderError::AuthorizationDenied),
            StatusCode::BAD_REQUEST,
            "Antigravity authorization was denied. Return to WaLiAPI for details.",
        ),
        (None, Some(code)) if !code.is_empty() => (
            Ok(code),
            StatusCode::OK,
            "Antigravity authorization received. WaLiAPI is completing sign-in; return to the app.",
        ),
        _ => (
            Err(ProviderError::CallbackFailed),
            StatusCode::BAD_REQUEST,
            "Antigravity authorization callback is incomplete. Return to WaLiAPI and retry.",
        ),
    };
    if let Some(sender) = callback.sender.lock().await.take() {
        let _ = sender.send(result);
    }
    (status, message).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_provider::{LoginRuntime, LoginStep};
    use async_trait::async_trait;
    use axum::{routing::post, Json};
    use std::sync::Mutex as StdMutex;
    use tokio::sync::watch;

    struct TestRuntime {
        cancel: watch::Receiver<bool>,
        opened: StdMutex<Vec<String>>,
        callback_bodies: StdMutex<Vec<String>>,
    }

    impl TestRuntime {
        fn new(cancel: watch::Receiver<bool>) -> Self {
            Self {
                cancel,
                opened: StdMutex::new(Vec::new()),
                callback_bodies: StdMutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LoginRuntime for TestRuntime {
        async fn open_browser(&self, url: &str) -> Result<(), ProviderError> {
            self.opened.lock().unwrap().push(url.to_owned());
            let parsed = reqwest::Url::parse(url).map_err(|_| ProviderError::BrowserOpenFailed)?;
            let redirect = parsed
                .query_pairs()
                .find(|(k, _)| k == "redirect_uri")
                .map(|(_, v)| v.into_owned())
                .ok_or(ProviderError::BrowserOpenFailed)?;
            if !(redirect.contains("127.0.0.1") || redirect.contains("localhost")) {
                return Ok(());
            }
            let state = parsed
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.into_owned())
                .ok_or(ProviderError::BrowserOpenFailed)?;
            let response = reqwest::Client::new()
                .get(format!("{redirect}?code=test-code&state={state}"))
                .send()
                .await
                .map_err(|_| ProviderError::CallbackFailed)?;
            let body = response
                .text()
                .await
                .map_err(|_| ProviderError::CallbackFailed)?;
            self.callback_bodies.lock().unwrap().push(body);
            Ok(())
        }
        async fn set_step(&self, _step: LoginStep) {}
        async fn present_device_authorization(
            &self,
            verification_url: &str,
            _user_code: &str,
            _expires_at: Option<String>,
        ) -> Result<(), ProviderError> {
            self.opened
                .lock()
                .unwrap()
                .push(verification_url.to_owned());
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            *self.cancel.borrow()
        }
        async fn cancelled(&self) {
            let mut rx = self.cancel.clone();
            while !*rx.borrow() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
    }

    async fn token_server(refresh_rotates: bool) -> String {
        let app = Router::new().route(
            "/token",
            post(move |body: axum::body::Bytes| async move {
                let raw = String::from_utf8_lossy(&body);
                if raw.contains("grant_type=refresh_token") && raw.contains("bad-refresh") {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "invalid_grant"})),
                    );
                }
                let refresh = if refresh_rotates {
                    "rotated-refresh"
                } else if raw.contains("grant_type=refresh_token") {
                    "1//keep"
                } else {
                    "1//new"
                };
                (
                    StatusCode::OK,
                    Json(json!({
                        "access_token": "ya29.new",
                        "refresh_token": refresh,
                        "expires_in": 3600,
                        "token_type": "Bearer"
                    })),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/token")
    }

    #[tokio::test]
    async fn refresh_rotates_refresh_token() {
        let token_url = token_server(true).await;
        let login = GeminiLogin::with_endpoints("http://127.0.0.1/authorize", token_url);
        let refreshed = login
            .refresh_payload(&ProviderPayload::new(json!({
                "version": 2,
                "oauth_client": "antigravity",
                "access_token": "old",
                "refresh_token": "1//keep"
            })))
            .await
            .unwrap();
        assert_eq!(refreshed.payload.as_value()["access_token"], "ya29.new");
        assert_eq!(
            refreshed.payload.as_value()["refresh_token"],
            "rotated-refresh"
        );
    }

    #[tokio::test]
    async fn refresh_invalid_grant_is_unauthorized() {
        let token_url = token_server(false).await;
        let login = GeminiLogin::with_endpoints("http://127.0.0.1/authorize", token_url);
        let err = login
            .refresh_payload(&ProviderPayload::new(json!({
                "version": 2,
                "oauth_client": "antigravity",
                "access_token": "old",
                "refresh_token": "bad-refresh"
            })))
            .await
            .unwrap_err();
        assert_eq!(err, ProviderError::Unauthorized);
    }

    #[tokio::test]
    async fn browser_login_exchanges_antigravity_code() {
        let token_url = token_server(false).await;
        let login = GeminiLogin::with_endpoints("http://127.0.0.1/authorize", &token_url)
            .with_timeout(Duration::from_secs(5));
        let (_tx, rx) = watch::channel(false);
        let runtime = TestRuntime::new(rx);
        let tokens = login.login(&runtime).await.unwrap();
        assert_eq!(tokens.access_token, "ya29.new");
        assert!(!tokens.refresh_token.is_empty());
        let opened = runtime.opened.lock().unwrap();
        assert!(!opened[0].contains("code_challenge"));
        assert!(opened[0].contains("localhost"));
        assert!(opened[0].contains("oauth-callback"));
        assert!(opened[0].contains("cclog"));
        assert!(opened[0].contains("experimentsandconfigs"));
        let callback_bodies = runtime.callback_bodies.lock().unwrap();
        assert!(callback_bodies[0].contains("completing sign-in"));
        assert!(!callback_bodies[0].contains("login complete"));
    }

    #[tokio::test]
    async fn refresh_retries_server_errors_then_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new().route(
            "/token",
            post(move || {
                let hits = hits_clone.clone();
                async move {
                    let n = hits.fetch_add(1, Ordering::SeqCst);
                    if n < 2 {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": "unavailable"})),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "access_token": "ya29.recovered",
                            "refresh_token": "1//keep",
                            "expires_in": 3600
                        })),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let login = GeminiLogin::with_endpoints(
            "http://127.0.0.1/authorize",
            format!("http://{addr}/token"),
        );
        let refreshed = login
            .refresh_payload(&ProviderPayload::new(json!({
                "version": 2,
                "oauth_client": "antigravity",
                "access_token": "old",
                "refresh_token": "1//keep"
            })))
            .await
            .unwrap();
        assert_eq!(
            refreshed.payload.as_value()["access_token"],
            "ya29.recovered"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn callback_rejects_state_mismatch_without_consuming_session() {
        let callback = CallbackState::new("expected-state".to_owned());
        let response = oauth_callback(
            State(callback.clone()),
            Query(CallbackQuery {
                code: Some("code".to_owned()),
                state: Some("wrong-state".to_owned()),
                error: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!callback.used.load(Ordering::SeqCst));
    }

    #[test]
    fn legacy_payload_requires_relogin() {
        let err = ensure_antigravity_payload(&ProviderPayload::new(json!({
            "version": 1,
            "access_token": "old",
            "refresh_token": "legacy"
        })))
        .unwrap_err();
        assert_eq!(err, ProviderError::CredentialMigrationRequired);
    }

    #[test]
    fn authorization_url_matches_antigravity_profile() {
        let url = authorization_url(
            GOOGLE_OAUTH_AUTHORIZE_URL,
            "test-antigravity-client-id",
            "http://localhost:43123/oauth-callback",
            "state-value",
        );
        let parsed = reqwest::Url::parse(&url).unwrap();
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some("test-antigravity-client-id")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://localhost:43123/oauth-callback")
        );
        assert_eq!(params.get("state").map(String::as_str), Some("state-value"));
        assert!(!params.contains_key("code_challenge"));
        assert!(params.get("scope").is_some_and(
            |scope| scope.contains("cclog") && scope.contains("experimentsandconfigs")
        ));
    }
}
