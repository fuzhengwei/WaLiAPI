//! GrokProvider: fixed-URL adapter for xAI Grok CLI chat-proxy.
//!
//! A Grok OAuth account uses one wire profile decided by the route planner:
//!
//! - OpenAI Responses at the fixed `https://cli-chat-proxy.grok.com/v1/responses`
//!   with Grok CLI identity headers and `Authorization: Bearer`
//!
//! The provider performs no protocol conversion (that stays in the codec
//! registry) and never accepts a renderer/downstream-supplied base URL or
//! endpoint.  The trusted `(upstream_protocol, upstream_endpoint)` arrived from
//! the RoutePlan through `ProviderRequest`; anything outside the exact
//! allowlist fails closed before any HTTP request.

use async_trait::async_trait;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde_json::Value;

use super::{
    grok_login::{
        GrokLogin, GROK_API_BASE, GROK_AUTHENTICATE_RESPONSE_HEADER,
        GROK_AUTHENTICATE_RESPONSE_VALUE, GROK_CLIENT_IDENTIFIER, GROK_CLIENT_IDENTIFIER_HEADER,
        GROK_CLIENT_VERSION, GROK_CLIENT_VERSION_HEADER, GROK_HTTP_TIMEOUT, GROK_TOKEN_AUTH_HEADER,
        GROK_TOKEN_AUTH_VALUE, GROK_USER_AGENT,
    },
    LoginResult, LoginRuntime, Provider, ProviderError, ProviderKind, ProviderLoginContext,
    ProviderModels, ProviderPayload, ProviderRequest, RefreshedPayload,
};
use crate::db::models::{AuthAccount, ModelState, QuotaState};

const RESPONSES_PATH: &str = "responses";
const MODELS_PATH: &str = "models";

fn safe_headers() -> Vec<HeaderName> {
    vec![
        HeaderName::from_static("x-request-id"),
        HeaderName::from_static("traceparent"),
        HeaderName::from_static("tracestate"),
    ]
}

pub struct GrokProvider {
    client: reqwest::Client,
    api_base: String,
    login: GrokLogin,
}

impl Default for GrokProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GrokProvider {
    pub fn new() -> Self {
        Self::with_api_base(GROK_API_BASE.to_owned())
    }

    pub fn with_api_base(api_base: impl Into<String>) -> Self {
        Self::with_endpoints(api_base, String::new(), String::new())
    }

    /// Test constructor that overrides the chat-proxy and OAuth endpoints so
    /// tests never touch the real xAI service.
    pub fn with_endpoints(
        api_base: impl Into<String>,
        device_auth_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        let device_auth_url = device_auth_url.into();
        let token_url = token_url.into();
        let login = if device_auth_url.is_empty() {
            GrokLogin::new()
        } else {
            GrokLogin::with_endpoints(device_auth_url, token_url)
        };
        Self {
            client: reqwest::Client::builder()
                .timeout(GROK_HTTP_TIMEOUT)
                .build()
                .expect("grok provider http client"),
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            login,
        }
    }

    fn access_token(payload: &ProviderPayload) -> Result<String, ProviderError> {
        if payload.as_value().get("provider").and_then(Value::as_str) != Some("grok") {
            return Err(ProviderError::InvalidPayload);
        }
        payload
            .as_value()
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(ProviderError::InvalidPayload)
    }

    fn identity_headers(access_token: &str, is_stream: bool) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        let bearer = format!("Bearer {access_token}");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&bearer).map_err(|_| ProviderError::InvalidPayload)?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if is_stream {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(GROK_USER_AGENT));
        headers.insert(
            HeaderName::from_static(GROK_TOKEN_AUTH_HEADER),
            HeaderValue::from_static(GROK_TOKEN_AUTH_VALUE),
        );
        headers.insert(
            HeaderName::from_static(GROK_CLIENT_VERSION_HEADER),
            HeaderValue::from_static(GROK_CLIENT_VERSION),
        );
        headers.insert(
            HeaderName::from_static(GROK_CLIENT_IDENTIFIER_HEADER),
            HeaderValue::from_static(GROK_CLIENT_IDENTIFIER),
        );
        headers.insert(
            HeaderName::from_static(GROK_AUTHENTICATE_RESPONSE_HEADER),
            HeaderValue::from_static(GROK_AUTHENTICATE_RESPONSE_VALUE),
        );
        Ok(headers)
    }

    fn merge_safe_headers(base: &mut HeaderMap, caller: &HeaderMap) {
        for name in safe_headers() {
            if let Some(value) = caller.get(&name) {
                base.insert(name, value.clone());
            }
        }
    }
}

#[async_trait]
impl Provider for GrokProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Grok
    }

    async fn login(
        &self,
        context: &ProviderLoginContext,
        runtime: &dyn LoginRuntime,
    ) -> Result<LoginResult, ProviderError> {
        match context.login_method {
            super::AuthLoginMode::DeviceCode => {}
            super::AuthLoginMode::BrowserCallback => {
                return Err(ProviderError::UnsupportedFeatures {
                    pointer: "provider.login.grok.browser_callback".into(),
                });
            }
        }
        let result = self.login.login(runtime).await?;
        if let Some(replacement) = &context.replacement {
            // Replacement must keep the same provider identity.  Overwriting
            // account_id would let a different xAI subject clobber this row.
            if result.account_id != replacement.provider_account_id {
                return Err(ProviderError::InvalidPayload);
            }
        }
        Ok(result)
    }

    async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
        Err(ProviderError::UnsupportedFeatures {
            pointer: "provider.import.grok".into(),
        })
    }

    async fn refresh(&self, payload: &ProviderPayload) -> Result<RefreshedPayload, ProviderError> {
        self.login.refresh_payload(payload).await
    }

    async fn outbound(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<reqwest::Response, ProviderError> {
        if request.upstream_protocol != "responses" || request.upstream_endpoint != "responses" {
            return Err(ProviderError::Protocol);
        }
        let access_token = Self::access_token(request.payload)?;
        let mut headers = Self::identity_headers(&access_token, request.is_stream)?;
        Self::merge_safe_headers(&mut headers, request.headers);
        self.client
            .post(format!("{}/{RESPONSES_PATH}", self.api_base))
            .headers(headers)
            .json(request.body)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)
    }

    async fn list_models(
        &self,
        _account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<ProviderModels, ProviderError> {
        let access_token = Self::access_token(payload)?;
        let mut headers = Self::identity_headers(&access_token, false)?;
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let response = self
            .client
            .get(format!("{}/{MODELS_PATH}", self.api_base))
            .headers(headers)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(ProviderError::Retryable);
        }
        let body: Value = response.json().await.map_err(|_| ProviderError::Protocol)?;
        normalize_grok_models(&body).ok_or(ProviderError::Protocol)
    }

    async fn fetch_quota(
        &self,
        _account: &AuthAccount,
        _payload: &ProviderPayload,
    ) -> Result<Option<QuotaState>, ProviderError> {
        Ok(None)
    }
}

/// Normalize the `/models` snapshot.  Missing or empty ids are skipped; a
/// body that is not an OpenAI-style list fails closed.
fn normalize_grok_models(body: &Value) -> Option<ProviderModels> {
    let entries = body
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| body.get("models").and_then(Value::as_array))
        .or_else(|| body.as_array())?;
    let mut models = Vec::new();
    for entry in entries {
        let id = match entry {
            Value::String(id) => id.as_str(),
            Value::Object(_) => entry.get("id").and_then(Value::as_str).unwrap_or(""),
            _ => continue,
        };
        if id.trim().is_empty() {
            continue;
        }
        models.push(ModelState {
            id: id.to_owned(),
            status: "available".to_owned(),
            unavailable: false,
            next_retry_after: None,
            last_error: None,
            protocol: Some("responses".to_owned()),
        });
    }
    Some(models)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use axum::{
        extract::State,
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::{json, Value};

    use super::super::{LoginRuntime, LoginStep, Provider, ProviderLoginContext};
    use super::*;

    const ACCESS: &str = "fixture-access-token";

    fn fake_jwt(email: &str, subject: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(format!(r#"{{"email":"{email}","sub":"{subject}"}}"#).as_bytes());
        format!("{header}.{payload}.sig")
    }

    #[derive(Clone, Default)]
    struct MockState {
        responses_hits: Arc<AtomicUsize>,
        responses_headers: Arc<Mutex<Vec<HeaderMap>>>,
        bodies: Arc<Mutex<Vec<Value>>>,
        uris: Arc<Mutex<Vec<axum::http::Uri>>>,
        models_status: Arc<AtomicUsize>,
        models_response: Arc<Mutex<Value>>,
        models_headers: Arc<Mutex<Vec<HeaderMap>>>,
    }

    fn account() -> AuthAccount {
        crate::db::models::AuthAccount {
            id: "local-1".into(),
            provider: "grok".into(),
            label: "Grok".into(),
            account_id: "sub-fixture".into(),
            status: "active".into(),
            disabled: 0,
            priority: 0,
            weight: 1,
            sort_order: 0,
            quota_json: None,
            model_states_json: "{}".into(),
            model_mapping_json: "{}".into(),
            attributes_json: "{}".into(),
            payload_json: json!({
                "provider": "grok",
                "access_token": ACCESS,
                "refresh_token": "fixture-refresh-token"
            })
            .to_string(),
            last_refreshed_at: None,
            last_models_sync_at: None,
            next_refresh_after: None,
            next_retry_after: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn payload() -> ProviderPayload {
        ProviderPayload::new(json!({
            "provider": "grok",
            "access_token": ACCESS,
            "refresh_token": "fixture-refresh-token",
        }))
    }

    fn req<'a>(
        account: &'a AuthAccount,
        payload: &'a ProviderPayload,
        body: &'a Value,
        protocol: &'a str,
        endpoint: &'a str,
        is_stream: bool,
        caller: &'a HeaderMap,
    ) -> ProviderRequest<'a> {
        ProviderRequest {
            account,
            payload,
            body,
            headers: caller,
            is_stream,
            upstream_protocol: protocol,
            upstream_endpoint: endpoint,
        }
    }

    async fn mock_provider() -> (GrokProvider, MockState) {
        let state = MockState::default();
        *state.models_response.lock().unwrap() = json!({
            "data": [{"id": "grok-4"}]
        });
        let app = Router::new()
            .route(
                "/v1/responses",
                post(
                    move |State(s): State<MockState>,
                          uri: axum::extract::OriginalUri,
                          h: HeaderMap,
                          body: axum::body::Bytes| {
                        let s = s.clone();
                        async move {
                            s.responses_hits.fetch_add(1, Ordering::SeqCst);
                            s.uris.lock().unwrap().push(uri.0.clone());
                            s.responses_headers.lock().unwrap().push(h.clone());
                            s.bodies
                                .lock()
                                .unwrap()
                                .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                            (axum::http::StatusCode::OK, Json(json!({"ok": true})))
                        }
                    },
                ),
            )
            .route(
                "/v1/models",
                get(move |State(s): State<MockState>, h: HeaderMap| async move {
                    s.models_headers.lock().unwrap().push(h.clone());
                    s.uris
                        .lock()
                        .unwrap()
                        .push(axum::http::Uri::from_static("/v1/models"));
                    let status = s.models_status.load(Ordering::SeqCst);
                    if status != 0 {
                        return (
                            axum::http::StatusCode::from_u16(status as u16).unwrap(),
                            Json(json!({"error": "upstream"})),
                        )
                            .into_response();
                    }
                    let body = s.models_response.lock().unwrap().clone();
                    (axum::http::StatusCode::OK, Json(body)).into_response()
                }),
            )
            .route(
                "/oauth/device",
                post(move |_: axum::extract::State<MockState>| async {
                    Json(json!({
                        "device_code": "device-code-1",
                        "user_code": "WXYZ-1234",
                        "verification_uri_complete": "https://accounts.x.ai/sign-in",
                        "expires_in": 1800,
                        "interval": 1
                    }))
                }),
            )
            .route(
                "/oauth/token",
                post(
                    move |_: axum::extract::State<MockState>, body: axum::body::Bytes| async move {
                        let _ = body;
                        Json(json!({
                            "access_token": ACCESS,
                            "refresh_token": "fixture-refresh-token",
                            "token_type": "Bearer",
                            "expires_in": 3600,
                            "id_token": fake_jwt("user@example.test", "sub-fixture")
                        }))
                    },
                ),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = GrokProvider::with_endpoints(
            format!("http://{addr}/v1"),
            format!("http://{addr}/oauth/device"),
            format!("http://{addr}/oauth/token"),
        );
        (provider, state)
    }

    #[tokio::test]
    async fn kind_is_grok() {
        assert_eq!(GrokProvider::new().kind(), ProviderKind::Grok);
        assert_eq!(GrokProvider::new().api_base, GROK_API_BASE);
        assert!(crate::auth_provider::ProviderRegistry::new()
            .get(&ProviderKind::Grok)
            .is_ok());
    }

    #[tokio::test]
    async fn responses_profile_uses_bearer_and_blocks_caller_auth() {
        let (provider, state) = mock_provider().await;
        let account = account();
        let mut caller = HeaderMap::new();
        caller.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer caller-secret"),
        );
        caller.insert(
            reqwest::header::HeaderName::from_static("x-xai-token-auth"),
            HeaderValue::from_static("evil"),
        );
        let body = json!({"model":"grok-4","input":[]});
        provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "responses",
                "responses",
                true,
                &caller,
            ))
            .await
            .unwrap();
        let headers = state.responses_headers.lock().unwrap();
        let h = &headers[0];
        assert_eq!(
            h.get(reqwest::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {ACCESS}")
        );
        assert_eq!(
            h.get("x-xai-token-auth").unwrap().to_str().unwrap(),
            GROK_TOKEN_AUTH_VALUE
        );
        assert_eq!(
            h.get("x-grok-client-identifier").unwrap().to_str().unwrap(),
            GROK_CLIENT_IDENTIFIER
        );
        assert_eq!(
            h.get(reqwest::header::ACCEPT).unwrap().to_str().unwrap(),
            "text/event-stream"
        );
        assert_eq!(state.responses_hits.load(Ordering::SeqCst), 1);
        let uri = &state.uris.lock().unwrap()[0];
        assert_eq!(uri.path(), "/v1/responses");
    }

    #[tokio::test]
    async fn unknown_or_mismatched_profile_fails_before_http() {
        let (provider, state) = mock_provider().await;
        let account = account();
        let body = json!({});
        let result = provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "openai",
                "chat_completions",
                false,
                &HeaderMap::new(),
            ))
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::Protocol);
        let result = provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "responses",
                "chat_completions",
                false,
                &HeaderMap::new(),
            ))
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::Protocol);
        assert_eq!(state.responses_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn foreign_payload_fails_closed_before_http() {
        let (provider, state) = mock_provider().await;
        let account = account();
        let foreign = ProviderPayload::new(json!({
            "access_token": ACCESS,
            "device_id": "abc"
        }));
        let result = provider
            .outbound(req(
                &account,
                &foreign,
                &json!({}),
                "responses",
                "responses",
                false,
                &HeaderMap::new(),
            ))
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::InvalidPayload);
        assert_eq!(state.responses_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn safe_passthrough_headers_are_allowed() {
        let (provider, state) = mock_provider().await;
        let mut caller = HeaderMap::new();
        caller.insert(
            reqwest::header::HeaderName::from_static("traceparent"),
            HeaderValue::from_static("00-abc-def-01"),
        );
        provider
            .outbound(req(
                &account(),
                &payload(),
                &json!({"model":"grok-4"}),
                "responses",
                "responses",
                false,
                &caller,
            ))
            .await
            .unwrap();
        let headers = state.responses_headers.lock().unwrap();
        assert_eq!(
            headers[0].get("traceparent").unwrap().to_str().unwrap(),
            "00-abc-def-01"
        );
    }

    #[tokio::test]
    async fn list_models_fetches_fixed_url_and_normalizes() {
        let (provider, state) = mock_provider().await;
        *state.models_response.lock().unwrap() = json!({
            "data": [
                {"id": "grok-4"},
                {"id": ""},
                {"slug": "ignored-without-id"},
                "grok-3"
            ]
        });
        let models = provider.list_models(&account(), &payload()).await.unwrap();
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["grok-4", "grok-3"]);
        assert_eq!(models[0].protocol.as_deref(), Some("responses"));
        let uri = &state.uris.lock().unwrap()[0];
        assert_eq!(uri.path(), "/v1/models");
        let h = &state.models_headers.lock().unwrap()[0];
        assert_eq!(
            h.get(reqwest::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {ACCESS}")
        );
        assert_eq!(
            h.get("x-xai-token-auth").unwrap().to_str().unwrap(),
            GROK_TOKEN_AUTH_VALUE
        );
    }

    #[tokio::test]
    async fn list_models_401_maps_to_unauthorized() {
        let (provider, state) = mock_provider().await;
        state.models_status.store(401, Ordering::SeqCst);
        let result = provider.list_models(&account(), &payload()).await;
        assert!(matches!(result, Err(ProviderError::Unauthorized)));
    }

    #[tokio::test]
    async fn list_models_malformed_body_fails_closed() {
        let (provider, state) = mock_provider().await;
        *state.models_response.lock().unwrap() = json!({"unexpected": true});
        assert_eq!(
            provider
                .list_models(&account(), &payload())
                .await
                .unwrap_err(),
            ProviderError::Protocol
        );
    }

    #[tokio::test]
    async fn fetch_quota_is_none() {
        let provider = GrokProvider::new();
        assert_eq!(
            provider.fetch_quota(&account(), &payload()).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn import_is_unsupported() {
        let provider = GrokProvider::new();
        assert!(matches!(
            provider.import(b"{}").await.unwrap_err(),
            ProviderError::UnsupportedFeatures { pointer } if pointer == "provider.import.grok"
        ));
    }

    #[tokio::test]
    async fn browser_callback_login_is_rejected() {
        let (provider, _state) = mock_provider().await;
        let runtime = TestRuntime::default();
        let error = provider
            .login(
                &ProviderLoginContext {
                    login_method: crate::auth_provider::AuthLoginMode::BrowserCallback,
                    replacement: None,
                },
                &runtime,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::UnsupportedFeatures { pointer } if pointer.contains("browser_callback")
        ));
    }

    #[tokio::test]
    async fn replacement_login_keeps_matching_account_id() {
        let (provider, _state) = mock_provider().await;
        let runtime = TestRuntime::default();
        let replaced = provider
            .login(
                &ProviderLoginContext {
                    login_method: crate::auth_provider::AuthLoginMode::DeviceCode,
                    replacement: Some(super::super::ReplacementContext {
                        local_account_id: "local-1".into(),
                        provider_account_id: "sub-fixture".into(),
                        previous_payload: payload(),
                    }),
                },
                &runtime,
            )
            .await
            .unwrap();
        assert_eq!(replaced.account_id, "sub-fixture");
    }

    #[tokio::test]
    async fn replacement_login_fails_closed_on_account_id_mismatch() {
        let (provider, _state) = mock_provider().await;
        let runtime = TestRuntime::default();
        let error = provider
            .login(
                &ProviderLoginContext {
                    login_method: crate::auth_provider::AuthLoginMode::DeviceCode,
                    replacement: Some(super::super::ReplacementContext {
                        local_account_id: "local-1".into(),
                        provider_account_id: "existing-sub".into(),
                        previous_payload: payload(),
                    }),
                },
                &runtime,
            )
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::InvalidPayload);
    }

    #[test]
    fn models_malformed_or_empty_handling() {
        assert!(normalize_grok_models(&json!({"data": "nope"})).is_none());
        assert!(normalize_grok_models(&json!({})).is_none());
        let models = normalize_grok_models(&json!({"data": []})).unwrap();
        assert!(models.is_empty());
    }

    #[derive(Clone)]
    struct TestRuntime {
        cancel: Arc<tokio::sync::watch::Sender<bool>>,
        _rx: Arc<tokio::sync::watch::Receiver<bool>>,
    }
    impl Default for TestRuntime {
        fn default() -> Self {
            let (tx, rx) = tokio::sync::watch::channel(false);
            Self {
                cancel: Arc::new(tx),
                _rx: Arc::new(rx),
            }
        }
    }
    #[async_trait::async_trait]
    impl LoginRuntime for TestRuntime {
        async fn open_browser(&self, _url: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn set_step(&self, _step: LoginStep) {}
        async fn present_device_authorization(
            &self,
            _url: &str,
            _code: &str,
            _expires_at: Option<String>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_cancelled(&self) -> bool {
            *self.cancel.borrow()
        }
        async fn cancelled(&self) {
            std::future::pending::<()>().await;
        }
    }
}
