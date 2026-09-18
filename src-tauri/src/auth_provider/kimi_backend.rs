//! KimiProvider: fixed-URL adapter for Kimi Code's coding API.
//!
//! A Kimi auth account routes two per-model wire profiles decided only by the
//! provider `/models` snapshot (kept in the account's model_states):
//!
//! - `missing`/`kimi`     → OpenAI Chat Completions
//!   `POST <coding>/v1/chat/completions`, `Authorization: Bearer`
//! - `anthropic`          → Anthropic Messages beta
//!   `POST <coding>/v1/messages?beta=true`, `x-api-key` + fixed
//!   `anthropic-version`
//!
//! The provider performs no protocol conversion (that stays in the codec
//! registry) and never accepts a renderer/downstream-supplied base URL or
//! endpoint.  The trusted `(upstream_protocol, upstream_endpoint)` arrived from
//! the RoutePlan through `ProviderRequest`; anything outside the exact
//! allowlist fails closed before any HTTP request.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use uuid::Uuid;

use super::{
    kimi_login::{KimiLogin, KIMI_HTTP_TIMEOUT},
    LoginResult, LoginRuntime, Provider, ProviderError, ProviderKind, ProviderLoginContext,
    ProviderModels, ProviderPayload, ProviderRequest, RefreshedPayload,
};
use crate::db::models::{AuthAccount, ModelState, QuotaState};

pub const KIMI_CODING_BASE: &str = "https://api.kimi.com/coding";
const CHAT_COMPLETIONS_PATH: &str = "v1/chat/completions";
const MESSAGES_BETA_PATH: &str = "v1/messages";
const BETA_QUERY: &str = "beta=true";
const MODELS_PATH: &str = "v1/models";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Device identity header used by the official Kimi transport.  The provider
/// always overwrites caller-supplied values.
const MSH_DEVICE_HEADER: &str = "x-msh-device-id";
/// Pass-through allowlist for harmless caller headers.  Caller Authorization,
/// x-api-key, and every `X-Msh-*` identity header are never passthrough.
fn safe_headers() -> Vec<HeaderName> {
    vec![
        HeaderName::from_static("x-request-id"),
        HeaderName::from_static("traceparent"),
        HeaderName::from_static("tracestate"),
    ]
}

pub struct KimiProvider {
    client: reqwest::Client,
    coding_base: String,
    login: KimiLogin,
}

impl Default for KimiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl KimiProvider {
    pub fn new() -> Self {
        Self::with_coding_base(KIMI_CODING_BASE.to_owned())
    }

    /// Test constructor: local mock URLs are allowed.
    pub fn with_coding_base(coding_base: impl Into<String>) -> Self {
        Self::with_endpoints(coding_base, String::new(), String::new())
    }

    /// Test constructor that also overrides the OAuth endpoints so login tests
    /// never touch the real Kimi service.
    pub fn with_endpoints(
        coding_base: impl Into<String>,
        device_auth_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        let device_auth_url = device_auth_url.into();
        let token_url = token_url.into();
        let login = if device_auth_url.is_empty() {
            KimiLogin::new()
        } else {
            KimiLogin::with_endpoints(device_auth_url, token_url)
        };
        Self {
            client: reqwest::Client::builder()
                .timeout(KIMI_HTTP_TIMEOUT)
                .build()
                .expect("kimi provider http client"),
            coding_base: coding_base.into(),
            login,
        }
    }

    fn device_id(payload: &ProviderPayload) -> Result<String, ProviderError> {
        payload
            .as_value()
            .get("device_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(ProviderError::InvalidPayload)
    }

    fn access_token(payload: &ProviderPayload) -> Result<String, ProviderError> {
        payload
            .as_value()
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(ProviderError::InvalidPayload)
    }

    /// Fixed device-identity header for every Kimi coding request.
    fn device_header(device_id: &str) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(MSH_DEVICE_HEADER),
            HeaderValue::from_str(device_id).map_err(|_| ProviderError::InvalidPayload)?,
        );
        Ok(headers)
    }

    /// Build the exact fixed request for one allowlisted wire profile.
    fn build_request(
        &self,
        request: &ProviderRequest<'_>,
        device_id: &str,
        access_token: &str,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        let (url, auth_headers) = match (request.upstream_protocol, request.upstream_endpoint) {
            ("openai", "chat_completions") => {
                let url = format!("{}/{CHAT_COMPLETIONS_PATH}", self.coding_base);
                let bearer = format!("Bearer {access_token}");
                let mut headers = HeaderMap::new();
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&bearer).map_err(|_| ProviderError::InvalidPayload)?,
                );
                (url, headers)
            }
            ("anthropic", "messages_beta") => {
                // Fixed beta query is part of the transport contract; a plain
                // `messages` endpoint is never used for a Kimi account.
                let url = format!("{}/{MESSAGES_BETA_PATH}?{BETA_QUERY}", self.coding_base);
                let mut headers = HeaderMap::new();
                headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_str(access_token)
                        .map_err(|_| ProviderError::InvalidPayload)?,
                );
                headers.insert(
                    HeaderName::from_static("anthropic-version"),
                    HeaderValue::from_static(ANTHROPIC_VERSION),
                );
                (url, headers)
            }
            _ => return Err(ProviderError::Protocol),
        };

        // Device identity always wins over any caller value.
        let mut headers = Self::device_header(device_id)?;
        headers.extend(auth_headers);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if request.is_stream {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        Ok(self.client.post(url).headers(headers).json(request.body))
    }

    /// Merge only the caller passthrough allowlist onto existing headers,
    /// rejecting caller Authorization / x-api-key / device headers entirely.
    fn merge_safe_headers(base: &mut HeaderMap, caller: &HeaderMap) {
        for name in safe_headers() {
            if let Some(value) = caller.get(&name) {
                base.insert(name, value.clone());
            }
        }
    }
}

#[async_trait]
impl Provider for KimiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Kimi
    }

    async fn login(
        &self,
        context: &ProviderLoginContext,
        runtime: &dyn LoginRuntime,
    ) -> Result<LoginResult, ProviderError> {
        // Replacement re-login must reuse the existing device_id: the account
        // identity is local-only, so a new id would orphan the account.
        let device_id = match &context.replacement {
            Some(replacement) => Self::device_id(&replacement.previous_payload)?,
            None => Uuid::new_v4().simple().to_string(),
        };
        self.login.login(runtime, &device_id).await
    }

    async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
        // Kimi Code does not expose an auth.json import path.
        Err(ProviderError::UnsupportedFeatures {
            pointer: "provider.import.kimi".into(),
        })
    }

    async fn refresh(&self, payload: &ProviderPayload) -> Result<RefreshedPayload, ProviderError> {
        let device_id = Self::device_id(payload)?;
        self.login.refresh_payload(payload, &device_id).await
    }

    async fn outbound(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<reqwest::Response, ProviderError> {
        let device_id = Self::device_id(request.payload)?;
        let access_token = Self::access_token(request.payload)?;
        let builder = self.build_request(&request, &device_id, &access_token)?;

        // Materialize the fixed request (URL + headers), then attach only the
        // caller's safe passthrough headers.  Caller identity headers are
        // rejected entirely, so we never rebuild from caller input.
        let built = builder.build().map_err(|_| ProviderError::Protocol)?;
        let url = built.url().clone();
        let mut combined = built.headers().clone();
        Self::merge_safe_headers(&mut combined, request.headers);

        self.client
            .post(url)
            .headers(combined)
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
        let device_id = Self::device_id(payload)?;
        let access_token = Self::access_token(payload)?;
        let mut headers = Self::device_header(&device_id)?;
        let bearer = format!("Bearer {access_token}");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&bearer).map_err(|_| ProviderError::InvalidPayload)?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let response = self
            .client
            .get(format!("{}/{MODELS_PATH}", self.coding_base))
            .headers(headers)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Unauthorized);
        }
        if response.status() == reqwest::StatusCode::PAYMENT_REQUIRED {
            // Kimi returns 402 when the account's membership benefits cannot be
            // verified.  This is not transient: retrying on a maintenance
            // cadence is pointless, so classify it terminal so the account
            // stays out of routing and the UI can explain the subscription.
            return Err(ProviderError::PaymentRequired);
        }
        if !response.status().is_success() {
            return Err(ProviderError::Retryable);
        }
        let body: Value = response.json().await.map_err(|_| ProviderError::Protocol)?;
        Ok(normalize_kimi_models(&body))
    }

    async fn fetch_quota(
        &self,
        _account: &AuthAccount,
        _payload: &ProviderPayload,
    ) -> Result<Option<QuotaState>, ProviderError> {
        // Kimi Code has no public quota endpoint; the account stays
        // header/cooldown-only and the UI hides the quota block.
        Ok(None)
    }
}

/// Normalize the `/models` snapshot.  Missing/`kimi` protocol → Chat profile;
/// `anthropic` → Messages beta; any other non-empty value fails closed as an
/// unavailable model (never routed as Chat).
fn normalize_kimi_models(body: &Value) -> ProviderModels {
    let Some(data) = body.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for entry in data {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.trim().is_empty() {
            continue;
        }
        let protocol = entry
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("kimi");
        let (status, unavailable, last_error) = match protocol {
            "" | "kimi" | "anthropic" => ("available".to_owned(), false, None),
            other => (
                "unavailable".to_owned(),
                true,
                Some(format!("unsupported wire protocol `{other}`")),
            ),
        };
        models.push(ModelState {
            id: id.to_owned(),
            status,
            unavailable,
            next_retry_after: None,
            last_error,
            protocol: Some(if protocol.is_empty() {
                "kimi".to_owned()
            } else {
                protocol.to_owned()
            }),
        });
    }
    models
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
    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::{json, Value};

    use super::super::{LoginRuntime, LoginStep, Provider, ProviderLoginContext};
    use super::*;

    const DEVICE_ID: &str = "d1e2b3a4c5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f";

    #[derive(Clone, Default)]
    struct MockState {
        chat_hits: Arc<AtomicUsize>,
        messages_hits: Arc<AtomicUsize>,
        chat_headers: Arc<Mutex<Vec<HeaderMap>>>,
        messages_headers: Arc<Mutex<Vec<HeaderMap>>>,
        bodies: Arc<Mutex<Vec<Value>>>,
        /// Request URIs the provider actually hit, so a test can pin the fixed
        /// URL/beta-query transport contract.
        uris: Arc<Mutex<Vec<axum::http::Uri>>>,
        /// Overrides for the `/v1/models` route: status code and JSON body.
        models_status: Arc<AtomicUsize>,
        models_response: Arc<Mutex<Value>>,
    }

    fn account() -> AuthAccount {
        crate::db::models::AuthAccount {
            id: "local-1".into(),
            provider: "kimi".into(),
            label: "Kimi Code".into(),
            account_id: DEVICE_ID.into(),
            status: "active".into(),
            disabled: 0,
            priority: 0,
            weight: 1,
            sort_order: 0,
            quota_json: None,
            model_states_json: "{}".into(),
            model_mapping_json: "{}".into(),
            attributes_json: "{}".into(),
            payload_json: json!({"access_token":"tok","device_id":DEVICE_ID}).to_string(),
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
            "access_token": "tok",
            "device_id": DEVICE_ID,
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

    async fn mock_provider() -> (KimiProvider, MockState) {
        let state = MockState::default();
        let app = Router::new()
            .route(
                "/coding/v1/chat/completions",
                post(
                    move |State(s): State<MockState>,
                          uri: axum::extract::OriginalUri,
                          h: HeaderMap,
                          body: axum::body::Bytes| {
                        let s = s.clone();
                        async move {
                            s.chat_hits.fetch_add(1, Ordering::SeqCst);
                            s.uris.lock().unwrap().push(uri.0.clone());
                            s.chat_headers.lock().unwrap().push(h.clone());
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
                "/coding/v1/messages",
                post(
                    move |State(s): State<MockState>,
                          uri: axum::extract::OriginalUri,
                          h: HeaderMap,
                          body: axum::body::Bytes| {
                        let s = s.clone();
                        async move {
                            s.messages_hits.fetch_add(1, Ordering::SeqCst);
                            s.uris.lock().unwrap().push(uri.0.clone());
                            s.messages_headers.lock().unwrap().push(h.clone());
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
                "/coding/v1/models",
                get(move |State(s): State<MockState>, h: HeaderMap| async move {
                    s.chat_headers.lock().unwrap().push(h.clone());
                    s.uris
                        .lock()
                        .unwrap()
                        .push(axum::http::Uri::from_static("/coding/v1/models"));
                    let status = s.models_status.load(std::sync::atomic::Ordering::SeqCst);
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
                        "user_code": "ABCD-EFGH",
                        "verification_uri_complete": "https://auth.example.test/verify",
                        "expires_in": 1800,
                        "interval": 1
                    }))
                }),
            )
            .route(
                "/oauth/token",
                post(
                    move |_: axum::extract::State<MockState>, body: axum::body::Bytes| async move {
                        let raw = String::from_utf8_lossy(&body);
                        let _ = raw;
                        Json(json!({
                            "access_token": "tok",
                            "refresh_token": "rot",
                            "token_type": "Bearer",
                            "expires_in": 3600
                        }))
                    },
                ),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = KimiProvider::with_endpoints(
            format!("http://{addr}/coding"),
            format!("http://{addr}/oauth/device"),
            format!("http://{addr}/oauth/token"),
        );
        (provider, state)
    }

    #[tokio::test]
    async fn kind_is_kimi() {
        assert_eq!(KimiProvider::new().kind(), ProviderKind::Kimi);
    }

    #[tokio::test]
    async fn chat_profile_uses_bearer_and_blocks_caller_auth() {
        let (provider, state) = mock_provider().await;
        let account = account();
        let mut caller = HeaderMap::new();
        caller.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer caller-secret"),
        );
        caller.insert(
            reqwest::header::HeaderName::from_static("x-api-key"),
            HeaderValue::from_static("caller-key"),
        );
        caller.insert(
            reqwest::header::HeaderName::from_static("x-msh-device-id"),
            HeaderValue::from_static("evil-device"),
        );
        let body = json!({"model":"kimi-k2.5","messages":[{"role":"user","content":"hi"}]});
        provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "openai",
                "chat_completions",
                false,
                &caller,
            ))
            .await
            .unwrap();
        let headers = state.chat_headers.lock().unwrap();
        let h = &headers[0];
        assert_eq!(
            h.get(reqwest::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer tok"
        );
        assert_eq!(h.get("x-api-key"), None);
        assert_eq!(
            h.get("x-msh-device-id").unwrap().to_str().unwrap(),
            DEVICE_ID
        );
        assert_eq!(
            h.get(reqwest::header::ACCEPT).unwrap().to_str().unwrap(),
            "application/json"
        );
        assert_eq!(state.chat_hits.load(Ordering::SeqCst), 1);
        assert_eq!(state.messages_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn chat_hit_uses_plain_chat_completions_path_with_no_beta_query() {
        let (provider, state) = mock_provider().await;
        let body = json!({"model":"kimi-k2.5","messages":[{"role":"user","content":"hi"}]});
        provider
            .outbound(req(
                &account(),
                &payload(),
                &body,
                "openai",
                "chat_completions",
                false,
                &HeaderMap::new(),
            ))
            .await
            .unwrap();
        let uri = &state.uris.lock().unwrap()[0];
        assert_eq!(uri.path(), "/coding/v1/chat/completions");
        assert_eq!(uri.query(), None);
    }

    #[tokio::test]
    async fn messages_beta_hit_forces_fixed_beta_query() {
        let (provider, state) = mock_provider().await;
        let body = json!({"model":"kimi-k2.5","messages":[]});
        provider
            .outbound(req(
                &account(),
                &payload(),
                &body,
                "anthropic",
                "messages_beta",
                true,
                &HeaderMap::new(),
            ))
            .await
            .unwrap();
        let uri = &state.uris.lock().unwrap()[0];
        assert_eq!(uri.path(), "/coding/v1/messages");
        assert_eq!(uri.query(), Some(BETA_QUERY));
    }

    #[tokio::test]
    async fn list_models_fetches_url_with_bearer_and_normalizes() {
        let (provider, state) = mock_provider().await;
        *state.models_response.lock().unwrap() = json!({
            "data": [
                {"id": "kimi-k2.5", "protocol": "kimi"},
                {"id": "kimi-k2.5-anthropic", "protocol": "anthropic"},
                {"id": "weird", "protocol": "nope"}
            ]
        });
        let models = provider.list_models(&account(), &payload()).await.unwrap();
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["kimi-k2.5", "kimi-k2.5-anthropic", "weird"]);
        // The weird protocol is fail-closed as unavailable, never routed.
        assert!(!models.iter().any(|m| m.id == "weird" && !m.unavailable));
        // URL is the fixed /v1/models path with the Bearer token and device header.
        let uri = &state.uris.lock().unwrap()[0];
        assert_eq!(uri.path(), "/coding/v1/models");
        let h = &state.chat_headers.lock().unwrap()[0];
        assert_eq!(
            h.get(reqwest::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer tok"
        );
        assert_eq!(
            h.get("x-msh-device-id").unwrap().to_str().unwrap(),
            DEVICE_ID
        );
    }

    #[tokio::test]
    async fn list_models_401_maps_to_unauthorized() {
        let (provider, state) = mock_provider().await;
        state
            .models_status
            .store(401, std::sync::atomic::Ordering::SeqCst);
        let result = provider.list_models(&account(), &payload()).await;
        assert!(matches!(
            result,
            Err(crate::auth_provider::ProviderError::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn anthropic_profile_uses_x_api_key_and_version() {
        let (provider, state) = mock_provider().await;
        let account = account();
        let mut caller = HeaderMap::new();
        caller.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer caller-secret"),
        );
        let body = json!({"model":"kimi-k2.5","messages":[]});
        provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "anthropic",
                "messages_beta",
                true,
                &caller,
            ))
            .await
            .unwrap();
        let headers = state.messages_headers.lock().unwrap();
        let h = &headers[0];
        assert_eq!(h.get("x-api-key").unwrap().to_str().unwrap(), "tok");
        assert_eq!(
            h.get("anthropic-version").unwrap().to_str().unwrap(),
            "2023-06-01"
        );
        assert_eq!(h.get(reqwest::header::AUTHORIZATION), None);
        assert_eq!(
            h.get("x-msh-device-id").unwrap().to_str().unwrap(),
            DEVICE_ID
        );
        assert_eq!(
            h.get(reqwest::header::ACCEPT).unwrap().to_str().unwrap(),
            "text/event-stream"
        );
        assert_eq!(state.messages_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_or_mismatched_profile_fails_before_http() {
        let (provider, state) = mock_provider().await;
        let account = account();
        let body = json!({});
        // Unknown protocol.
        let result = provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "weird",
                "chat_completions",
                false,
                &HeaderMap::new(),
            ))
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::Protocol);
        // Mismatched combo (openai + messages_beta).
        let result = provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "openai",
                "messages_beta",
                false,
                &HeaderMap::new(),
            ))
            .await;
        assert_eq!(result.unwrap_err(), ProviderError::Protocol);
        assert_eq!(state.chat_hits.load(Ordering::SeqCst), 0);
        assert_eq!(state.messages_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn safe_passthrough_headers_are_allowed() {
        let (provider, state) = mock_provider().await;
        let account = account();
        let mut caller = HeaderMap::new();
        caller.insert(
            reqwest::header::HeaderName::from_static("traceparent"),
            HeaderValue::from_static("00-abc-def-01"),
        );
        let body = json!({"model":"kimi"});
        provider
            .outbound(req(
                &account,
                &payload(),
                &body,
                "openai",
                "chat_completions",
                false,
                &caller,
            ))
            .await
            .unwrap();
        let headers = state.chat_headers.lock().unwrap();
        assert_eq!(
            headers[0].get("traceparent").unwrap().to_str().unwrap(),
            "00-abc-def-01"
        );
    }

    #[tokio::test]
    async fn fetch_quota_is_none() {
        let provider = KimiProvider::new();
        assert_eq!(
            provider.fetch_quota(&account(), &payload()).await.unwrap(),
            None
        );
    }

    #[test]
    fn models_are_normalized_with_protocol_and_unknown_fails_closed() {
        let body = json!({
            "data": [
                {"id": "kimi-k2.5", "protocol": "kimi"},
                {"id": "kimi-k2.5-anthropic", "protocol": "anthropic"},
                {"id": "kimi-k2.5-missing"},
                {"id": ""},
                {"id": "kimi-weird", "protocol": "mars"}
            ]
        });
        let models = normalize_kimi_models(&body);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "kimi-k2.5",
                "kimi-k2.5-anthropic",
                "kimi-k2.5-missing",
                "kimi-weird"
            ]
        );
        assert_eq!(models[0].protocol.as_deref(), Some("kimi"));
        assert!(!models[0].unavailable);
        assert_eq!(models[1].protocol.as_deref(), Some("anthropic"));
        assert_eq!(models[2].protocol.as_deref(), Some("kimi"));
        assert!(!models[2].unavailable);
        // Unknown protocol fails closed: unavailable, never routed as Chat.
        assert_eq!(models[3].protocol.as_deref(), Some("mars"));
        assert!(models[3].unavailable);
        assert_eq!(models[3].status, "unavailable");
        assert!(models[3].last_error.is_some());
    }

    #[test]
    fn models_malformed_or_empty_returns_empty() {
        assert!(normalize_kimi_models(&json!({"data": "nope"})).is_empty());
        assert!(normalize_kimi_models(&json!({})).is_empty());
    }

    #[tokio::test]
    async fn new_login_generates_fresh_device_id_and_replacement_reuses_it() {
        let (provider, _state) = mock_provider().await;
        let runtime = TestRuntime::default();

        // New login: OAuth mock succeeds, result carries a uuid-shaped id.
        let fresh = provider
            .login(
                &ProviderLoginContext {
                    login_method: crate::auth_provider::AuthLoginMode::DeviceCode,
                    replacement: None,
                },
                &runtime,
            )
            .await
            .unwrap();
        assert_eq!(
            fresh.payload.as_value()["device_id"]
                .as_str()
                .unwrap()
                .len(),
            32
        );

        // Replacement re-login reuses the persisted device_id.
        let context = ProviderLoginContext {
            login_method: crate::auth_provider::AuthLoginMode::DeviceCode,
            replacement: Some(super::super::ReplacementContext {
                local_account_id: "local-1".into(),
                provider_account_id: DEVICE_ID.into(),
                previous_payload: payload(),
                previous_attributes: json!({}),
            }),
        };
        let replaced = provider.login(&context, &runtime).await.unwrap();
        assert_eq!(
            replaced.payload.as_value()["device_id"].as_str().unwrap(),
            DEVICE_ID
        );
        assert_eq!(replaced.account_id, DEVICE_ID);
    }

    #[tokio::test]
    async fn replacement_login_fails_closed_when_previous_payload_lacks_device_id() {
        let (provider, _state) = mock_provider().await;
        let runtime = TestRuntime::default();
        let context = ProviderLoginContext {
            login_method: crate::auth_provider::AuthLoginMode::DeviceCode,
            replacement: Some(super::super::ReplacementContext {
                local_account_id: "local-1".into(),
                provider_account_id: DEVICE_ID.into(),
                previous_payload: ProviderPayload::new(json!({"access_token": "x"})),
                previous_attributes: json!({}),
            }),
        };
        // The device_id is required before any OAuth request is issued.
        assert_eq!(
            provider.login(&context, &runtime).await.unwrap_err(),
            ProviderError::InvalidPayload
        );
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
