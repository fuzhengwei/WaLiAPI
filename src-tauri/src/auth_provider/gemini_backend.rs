//! 使用 Antigravity OAuth 与 Code Assist API 的 Gemini provider 实现。
//!
//! `gemini` 是内部 provider/protocol 标识；Antigravity 是 UI 与上游 OAuth
//! client 品牌。这样既保持数据库和 codec 兼容，也避免把旧 Gemini CLI
//! 凭据误用于新的 OAuth client。

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use super::{
    gemini_login::{
        ensure_antigravity_payload, GeminiLogin, OAuthTokens, ANTIGRAVITY_HTTP_TIMEOUT,
    },
    LoginResult, LoginRuntime, Provider, ProviderError, ProviderKind, ProviderLoginContext,
    ProviderModels, ProviderPayload, ProviderRequest, RefreshedPayload,
};
use crate::db::models::{AuthAccount, ModelState, QuotaLimit, QuotaState, QuotaWindow};
use std::time::Duration;

// 常量名称保留 Gemini 以匹配内部 provider；实际上游是 Antigravity Code Assist。
pub const GEMINI_CODE_ASSIST_BASE: &str = "https://daily-cloudcode-pa.googleapis.com";
pub const GEMINI_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const GENERATE_PATH: &str = "v1internal:generateContent";
const STREAM_PATH: &str = "v1internal:streamGenerateContent";
const LOAD_PATH: &str = "v1internal:loadCodeAssist";
const ONBOARD_PATH: &str = "v1internal:onboardUser";
const MODELS_PATH: &str = "v1internal:fetchAvailableModels";
// 版本同时进入 User-Agent、x-goog-api-client 和 client metadata，升级时必须一起验证。
const ANTIGRAVITY_VERSION: &str = "2.5.5";
const GOOG_API_CLIENT: &str = "antigravity/2.5.5";
const ONBOARD_POLL: Duration = Duration::from_secs(5);
const ONBOARD_POLL_MAX: u32 = 24;

const CLIENT_METADATA: &str =
    r#"{"ideName":"antigravity","ideType":"ANTIGRAVITY","ideVersion":"2.5.5"}"#;

pub struct GeminiProvider {
    client: reqwest::Client,
    /// 流式出站客户端（无总超时），见 [`super::streaming_http_client`]。
    stream_client: reqwest::Client,
    code_assist_base: String,
    userinfo_url: String,
    login: GeminiLogin,
    onboard_poll: Duration,
    onboard_poll_max: u32,
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self {
            client: http_client(),
            stream_client: super::streaming_http_client(),
            code_assist_base: GEMINI_CODE_ASSIST_BASE.to_owned(),
            userinfo_url: GEMINI_USERINFO_URL.to_owned(),
            login: GeminiLogin::new(),
            onboard_poll: ONBOARD_POLL,
            onboard_poll_max: ONBOARD_POLL_MAX,
        }
    }

    pub fn with_endpoints(
        code_assist_base: impl Into<String>,
        userinfo_url: impl Into<String>,
        login: GeminiLogin,
    ) -> Self {
        Self {
            client: http_client(),
            stream_client: super::streaming_http_client(),
            code_assist_base: code_assist_base.into().trim_end_matches('/').to_owned(),
            userinfo_url: userinfo_url.into(),
            login,
            onboard_poll: Duration::from_millis(5),
            onboard_poll_max: 20,
        }
    }

    fn assist_url(&self, path: &str) -> String {
        format!("{}/{}", self.code_assist_base.trim_end_matches('/'), path)
    }

    fn access_token(payload: &ProviderPayload) -> Result<String, ProviderError> {
        ensure_antigravity_payload(payload)?;
        payload
            .as_value()
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(ProviderError::InvalidPayload)
    }

    fn project_id(account: &AuthAccount) -> Result<String, ProviderError> {
        let attributes: Value =
            serde_json::from_str(&account.attributes_json).unwrap_or(Value::Null);
        attributes
            .get("project_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(ProviderError::Protocol)
    }

    async fn available_models(
        &self,
        account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<Value, ProviderError> {
        let access = Self::access_token(payload)?;
        let project = Self::project_id(account)?;
        let response = self
            .client
            .post(self.assist_url(MODELS_PATH))
            .bearer_auth(access)
            .header(CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, user_agent())
            .json(&json!({ "project": project }))
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if !response.status().is_success() {
            return Err(classify_http(response.status()));
        }
        response.json().await.map_err(|_| ProviderError::Protocol)
    }

    async fn complete_login(&self, tokens: OAuthTokens) -> Result<LoginResult, ProviderError> {
        // userinfo 是 OAuth token 有效性的第一处服务端确认；保留其 typed error，
        // 让 401、可重试的网络/服务端错误和协议错误分别进入正确的登录状态。
        let email = self.fetch_email(&tokens.access_token).await?;
        let (project_id, tier) = self
            .onboard(&tokens.access_token, tokens.project_hint.as_deref())
            .await?;
        let mut attributes = json!({
            "email": email,
            "project_id": project_id,
        });
        if let Some(tier) = tier {
            attributes["plan_type"] = json!(tier);
        }
        Ok(LoginResult {
            account_id: email.clone(),
            label: email,
            attributes,
            payload: GeminiLogin::tokens_to_payload(&tokens),
            last_refreshed_at: Some(chrono::Utc::now().to_rfc3339()),
            next_refresh_after: None,
            next_retry_after: None,
        })
    }

    async fn fetch_email(&self, access_token: &str) -> Result<String, ProviderError> {
        let response = self
            .client
            .get(&self.userinfo_url)
            .bearer_auth(access_token)
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
        body.get("email")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(ProviderError::Protocol)
    }

    async fn onboard(
        &self,
        access_token: &str,
        project_hint: Option<&str>,
    ) -> Result<(String, Option<String>), ProviderError> {
        let metadata = antigravity_metadata();
        let load_body = json!({
            "cloudaicompanionProject": project_hint,
            "metadata": metadata,
        });
        let load = self
            .client
            .post(self.assist_url(LOAD_PATH))
            .bearer_auth(access_token)
            .header(CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, user_agent())
            .json(&load_body)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if !load.status().is_success() {
            return Err(classify_http(load.status()));
        }
        let load_json: Value = load.json().await.map_err(|_| ProviderError::Protocol)?;
        if let Some(url) = validation_url(&load_json) {
            return Err(ProviderError::ValidationRequired { url });
        }
        let tier = current_tier_name(&load_json);
        if let Some(project) = project_from_load(&load_json) {
            return Ok((project, tier));
        }
        if load_json.get("currentTier").is_some() {
            // 调用方提供的 project 仅是 loadCodeAssist 的候选提示；只有服务端
            // 返回的 project 才能持久化，不能把旧账号属性冒充为服务端确认结果。
            return Err(ProviderError::Protocol);
        }

        let tier_id = available_free_tier(&load_json).ok_or(ProviderError::PermissionDenied)?;
        let onboard_body = json!({
            "tierId": tier_id,
            "metadata": metadata,
        });
        let onboard = self
            .client
            .post(self.assist_url(ONBOARD_PATH))
            .bearer_auth(access_token)
            .header(CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, user_agent())
            .json(&onboard_body)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if !onboard.status().is_success() {
            return Err(classify_http(onboard.status()));
        }
        let onboard_json: Value = onboard.json().await.map_err(|_| ProviderError::Protocol)?;
        let onboard_json = self.await_operation(access_token, onboard_json).await?;
        if let Some(project) = project_from_onboard(&onboard_json) {
            return Ok((project, tier.or(Some(tier_id))));
        }
        // project 必须来自 Code Assist 响应；候选提示不能冒充成功的 onboarding 结果。
        Err(ProviderError::Protocol)
    }

    async fn await_operation(
        &self,
        access_token: &str,
        mut body: Value,
    ) -> Result<Value, ProviderError> {
        for _ in 0..self.onboard_poll_max {
            if body.get("error").is_some_and(|value| !value.is_null()) {
                return Err(ProviderError::Protocol);
            }
            if body.get("done").and_then(Value::as_bool) == Some(true)
                || project_from_onboard(&body).is_some()
            {
                return Ok(body);
            }
            let Some(name) = body
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                return Ok(body);
            };
            tokio::time::sleep(self.onboard_poll).await;
            let response = self
                .client
                .get(self.operation_url(name))
                .bearer_auth(access_token)
                .send()
                .await
                .map_err(|_| ProviderError::Retryable)?;
            if !response.status().is_success() {
                return Err(classify_http(response.status()));
            }
            body = response.json().await.map_err(|_| ProviderError::Protocol)?;
        }
        Err(ProviderError::Protocol)
    }

    fn operation_url(&self, name: &str) -> String {
        let name = name.trim_start_matches('/');
        if name.starts_with("v1internal/") {
            format!("{}/{}", self.code_assist_base, name)
        } else if name.starts_with("operations/") {
            format!("{}/v1internal/{}", self.code_assist_base, name)
        } else {
            format!("{}/v1internal/operations/{}", self.code_assist_base, name)
        }
    }

    fn wrap_body(
        &self,
        account: &AuthAccount,
        body: &Value,
        model: &str,
    ) -> Result<Value, ProviderError> {
        let project = Self::project_id(account)?;
        let session_id = format!("waliapi-{}", account.id);
        let mut request = body.clone();
        if let Some(object) = request.as_object_mut() {
            object
                .entry("session_id")
                .or_insert_with(|| json!(session_id));
        }
        Ok(json!({
            "model": model,
            "project": project,
            "user_prompt_id": uuid::Uuid::new_v4().to_string(),
            "request": request,
        }))
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(ANTIGRAVITY_HTTP_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn project_from_onboard(body: &Value) -> Option<String> {
    body.pointer("/response/cloudaicompanionProject/id")
        .and_then(Value::as_str)
        .or_else(|| body.get("cloudaicompanionProject").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn classify_http(status: reqwest::StatusCode) -> ProviderError {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        ProviderError::Unauthorized
    } else if status == reqwest::StatusCode::FORBIDDEN {
        ProviderError::PermissionDenied
    } else if status == reqwest::StatusCode::PAYMENT_REQUIRED {
        ProviderError::PaymentRequired
    } else if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        ProviderError::Retryable
    } else {
        ProviderError::Protocol
    }
}

fn validation_url(load: &Value) -> Option<String> {
    load.get("ineligibleTiers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|tier| {
            let reason = tier.get("reasonCode").and_then(Value::as_str)?;
            if reason != "VALIDATION_REQUIRED" {
                return None;
            }
            tier.get("validationUrl")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
}

fn current_tier_name(load: &Value) -> Option<String> {
    load.pointer("/currentTier/id")
        .or_else(|| load.pointer("/paidTier/id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn available_free_tier(load: &Value) -> Option<String> {
    load.get("allowedTiers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|tier| {
            let id = tier.get("id").and_then(Value::as_str)?;
            matches!(id, "free-tier" | "FREE").then(|| id.to_owned())
        })
}

fn user_agent() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "Darwin",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    format!(
        "antigravity/{ANTIGRAVITY_VERSION} {platform}/{}",
        std::env::consts::ARCH
    )
}

fn antigravity_metadata() -> Value {
    json!({
        "ideName": "antigravity",
        "ideType": "ANTIGRAVITY",
        "ideVersion": "2.5.5",
    })
}

fn project_from_load(body: &Value) -> Option<String> {
    body.get("cloudaicompanionProject")
        .and_then(|project| {
            project
                .as_str()
                .map(str::to_owned)
                .or_else(|| project.get("id").and_then(Value::as_str).map(str::to_owned))
        })
        .filter(|project| !project.is_empty())
}

fn models_from_response(body: &Value) -> Result<ProviderModels, ProviderError> {
    let mut ids: Vec<String> = match body.get("models") {
        Some(Value::Object(models)) => models.keys().cloned().collect(),
        Some(Value::Array(models)) => models
            .iter()
            .filter_map(|model| {
                model
                    .get("id")
                    .or_else(|| model.get("modelId"))
                    .or_else(|| model.get("name"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            })
            .collect(),
        _ => Vec::new(),
    };
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err(ProviderError::Protocol);
    }
    Ok(ids
        .into_iter()
        .map(|id| ModelState {
            id,
            status: "available".to_owned(),
            unavailable: false,
            next_retry_after: None,
            last_error: None,
            protocol: Some("gemini".to_owned()),
        })
        .collect())
}

fn quota_from_models_response(body: &Value) -> Option<QuotaState> {
    let models = body.get("models")?.as_object()?;
    let mut limits = Vec::new();
    for (id, model) in models {
        let Some(info) = model.get("quotaInfo") else {
            continue;
        };
        let Some(remaining) = info.get("remainingFraction").and_then(Value::as_f64) else {
            continue;
        };
        if !remaining.is_finite() || !(0.0..=1.0).contains(&remaining) {
            continue;
        }
        limits.push(QuotaLimit {
            limit_id: id.clone(),
            limit_name: Some(
                model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_owned(),
            ),
            primary: Some(QuotaWindow {
                used_percent: Some((1.0 - remaining) * 100.0),
                window_minutes: None,
                reset_at: info
                    .get("resetTime")
                    .and_then(Value::as_str)
                    .filter(|time| !time.is_empty())
                    .map(str::to_owned),
            }),
            secondary: None,
            credits: None,
        });
    }
    if limits.is_empty() {
        return None;
    }
    Some(QuotaState {
        limits,
        ..QuotaState::default()
    })
}

fn model_from_body(body: &Value) -> String {
    body.get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("gemini-2.5-flash")
        .to_owned()
}

#[async_trait]
impl Provider for GeminiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    async fn login(
        &self,
        context: &ProviderLoginContext,
        runtime: &dyn LoginRuntime,
    ) -> Result<LoginResult, ProviderError> {
        let mut tokens = match context.login_method {
            super::AuthLoginMode::BrowserCallback => self.login.login(runtime).await?,
            super::AuthLoginMode::DeviceCode => return Err(ProviderError::LoginFailed),
        };
        if tokens.project_hint.is_none() {
            if let Some(replacement) = &context.replacement {
                tokens.project_hint = replacement
                    .previous_attributes
                    .get("project_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
        }
        self.complete_login(tokens).await
    }

    async fn import(&self, _bytes: &[u8]) -> Result<LoginResult, ProviderError> {
        Err(ProviderError::ImportFailed)
    }

    async fn refresh(&self, payload: &ProviderPayload) -> Result<RefreshedPayload, ProviderError> {
        self.login.refresh_payload(payload).await
    }

    async fn outbound(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<reqwest::Response, ProviderError> {
        if request.upstream_protocol != "gemini" || request.upstream_endpoint != "generate_content"
        {
            return Err(ProviderError::Protocol);
        }
        let access = Self::access_token(request.payload)?;
        // codec 产出的是 inner Vertex request；如果调用方已经包了一层 request，
        // 这里仍只取 inner 对象，避免重复包装。model 仅用于 Code Assist 外层信封。
        let mut inner = request
            .body
            .get("request")
            .cloned()
            .unwrap_or_else(|| request.body.clone());
        let model = inner
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| model_from_body(request.body));
        if let Some(obj) = inner.as_object_mut() {
            obj.remove("model");
            obj.remove("stream");
            obj.remove("stream_options");
        }
        let wrapped = self.wrap_body(request.account, &inner, &model)?;
        let path = if request.is_stream {
            STREAM_PATH
        } else {
            GENERATE_PATH
        };
        let mut url = self.assist_url(path);
        if request.is_stream {
            url.push_str("?alt=sse");
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access}"))
                .map_err(|_| ProviderError::InvalidPayload)?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static(if request.is_stream {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_str(&user_agent()).map_err(|_| ProviderError::Protocol)?,
        );
        headers.insert(
            HeaderName::from_static("x-goog-api-client"),
            HeaderValue::from_static(GOOG_API_CLIENT),
        );
        if let Ok(value) = HeaderValue::from_str(CLIENT_METADATA) {
            headers.insert(HeaderName::from_static("client-metadata"), value);
        }
        // 只转发明确允许的链路追踪头，避免把下游任意 header 带到上游。
        for name in ["x-request-id", "traceparent", "tracestate"] {
            if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                if let Some(value) = request.headers.get(&header_name) {
                    headers.insert(header_name, value.clone());
                }
            }
        }
        let client = if request.is_stream {
            &self.stream_client
        } else {
            &self.client
        };
        client
            .post(url)
            .headers(headers)
            .json(&wrapped)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)
    }

    async fn list_models(
        &self,
        account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<ProviderModels, ProviderError> {
        let body = self.available_models(account, payload).await?;
        // fetchAvailableModels 成功但没有可解析模型时必须失败关闭，不能伪造静态目录。
        models_from_response(&body)
    }

    async fn fetch_quota(
        &self,
        account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<Option<QuotaState>, ProviderError> {
        let body = self.available_models(account, payload).await?;
        Ok(quota_from_models_response(&body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_provider::gemini_login::GeminiLogin;
    use axum::{
        extract::{Path, State},
        routing::{get, post},
        Json, Router,
    };
    use reqwest::header::{HeaderMap, ACCEPT};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    #[derive(Clone)]
    struct Mock {
        generate_hits: Arc<Mutex<Vec<(String, Value, HeaderMap)>>>,
        load_body: Arc<Mutex<Value>>,
        load_requests: Arc<Mutex<Vec<(Value, HeaderMap)>>>,
        onboard_requests: Arc<Mutex<Vec<Value>>>,
        model_requests: Arc<Mutex<Vec<(Value, HeaderMap)>>>,
        lro: Arc<AtomicBool>,
        op_hits: Arc<Mutex<u32>>,
        userinfo_status: Arc<Mutex<reqwest::StatusCode>>,
    }

    async fn mock_assist(load: Value) -> (GeminiProvider, Mock, String) {
        let state = Mock {
            generate_hits: Arc::new(Mutex::new(Vec::new())),
            load_body: Arc::new(Mutex::new(load)),
            load_requests: Arc::new(Mutex::new(Vec::new())),
            onboard_requests: Arc::new(Mutex::new(Vec::new())),
            model_requests: Arc::new(Mutex::new(Vec::new())),
            lro: Arc::new(AtomicBool::new(false)),
            op_hits: Arc::new(Mutex::new(0)),
            userinfo_status: Arc::new(Mutex::new(reqwest::StatusCode::OK)),
        };
        let app = Router::new()
            .route(
                "/oauth2/v2/userinfo",
                get(|State(s): State<Mock>| async move {
                    let status = *s.userinfo_status.lock().unwrap();
                    (status, Json(json!({"email": "user@gmail.com"})))
                }),
            )
            .route(
                "/v1internal:loadCodeAssist",
                post(
                    |State(s): State<Mock>, headers: HeaderMap, body: axum::body::Bytes| async move {
                        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        s.load_requests.lock().unwrap().push((body, headers));
                        Json(s.load_body.lock().unwrap().clone())
                    },
                ),
            )
            .route(
                "/v1internal:onboardUser",
                post(|State(s): State<Mock>, body: axum::body::Bytes| async move {
                    let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    s.onboard_requests.lock().unwrap().push(body);
                    if s.lro.load(Ordering::SeqCst) {
                        Json(json!({ "done": false, "name": "operations/op1" }))
                    } else {
                        Json(json!({
                            "done": true,
                            "response": { "cloudaicompanionProject": { "id": "onboarded-proj" } }
                        }))
                    }
                }),
            )
            .route(
                "/v1internal/operations/{id}",
                get(|State(s): State<Mock>, Path(_id): Path<String>| async move {
                    *s.op_hits.lock().unwrap() += 1;
                    Json(json!({
                        "done": true,
                        "response": { "cloudaicompanionProject": { "id": "lro-proj" } }
                    }))
                }),
            )
            .route(
                "/v1internal:fetchAvailableModels",
                post(
                    |State(s): State<Mock>, headers: HeaderMap, body: axum::body::Bytes| async move {
                        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        s.model_requests.lock().unwrap().push((body, headers));
                        Json(json!({
                            "models": {
                                "gemini-2.5-flash": {
                                    "displayName": "Gemini Flash",
                                    "quotaInfo": {"remainingFraction": 0.25, "resetTime": "2026-09-23T14:51:42Z"}
                                },
                                "gemini-3-pro-high": {
                                    "quotaInfo": {"remainingFraction": 1.0}
                                }
                            }
                        }))
                    },
                ),
            )
            .route(
                "/v1internal:generateContent",
                post(
                    |State(s): State<Mock>, headers: HeaderMap, body: axum::body::Bytes| async move {
                        let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        s.generate_hits.lock().unwrap().push((
                            "generate".into(),
                            parsed,
                            headers,
                        ));
                        Json(json!({"response": {"candidates": [{"content": {"parts": [{"text": "ok"}]}}]}}))
                    },
                ),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let provider = GeminiProvider::with_endpoints(
            base.clone(),
            format!("{base}/oauth2/v2/userinfo"),
            GeminiLogin::with_endpoints("http://127.0.0.1/authorize", format!("{base}/token")),
        );
        (provider, state, base)
    }

    fn account(project: &str) -> AuthAccount {
        AuthAccount {
            id: "acc-1".into(),
            provider: "gemini".into(),
            label: "user@gmail.com".into(),
            account_id: "user@gmail.com".into(),
            status: "active".into(),
            disabled: 0,
            priority: 0,
            weight: 1,
            sort_order: 0,
            quota_json: None,
            model_states_json: "{\"version\":1,\"models\":[]}".into(),
            model_mapping_json: "{}".into(),
            model_mapping_disabled: "[]".into(),
            attributes_json: json!({"email":"user@gmail.com","project_id": project}).to_string(),
            payload_json: json!({"access_token":"tok","refresh_token":"ref"}).to_string(),
            last_refreshed_at: None,
            last_models_sync_at: None,
            next_refresh_after: None,
            next_retry_after: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn tokens(project_hint: Option<&str>) -> OAuthTokens {
        OAuthTokens {
            access_token: "ya29.a".to_owned(),
            refresh_token: "1//r".to_owned(),
            expires_at: None,
            project_hint: project_hint.map(str::to_owned),
        }
    }

    fn payload() -> ProviderPayload {
        ProviderPayload::new(json!({
            "version": 2,
            "oauth_client": "antigravity",
            "access_token": "tok",
            "refresh_token": "r"
        }))
    }

    #[tokio::test]
    async fn kind_is_gemini() {
        assert_eq!(GeminiProvider::new().kind(), ProviderKind::Gemini);
    }

    #[tokio::test]
    async fn userinfo_unauthorized_is_not_downgraded_to_login_failed() {
        let (provider, state, _) = mock_assist(json!({})).await;
        *state.userinfo_status.lock().unwrap() = reqwest::StatusCode::UNAUTHORIZED;

        let err = provider.complete_login(tokens(None)).await.unwrap_err();

        assert_eq!(err, ProviderError::Unauthorized);
    }

    #[tokio::test]
    async fn userinfo_server_error_remains_retryable() {
        let (provider, state, _) = mock_assist(json!({})).await;
        *state.userinfo_status.lock().unwrap() = reqwest::StatusCode::SERVICE_UNAVAILABLE;

        let err = provider.complete_login(tokens(None)).await.unwrap_err();

        assert_eq!(err, ProviderError::Retryable);
    }

    #[tokio::test]
    async fn login_completion_runs_userinfo_and_load_code_assist() {
        let (provider, state, _) = mock_assist(json!({
            "currentTier": { "id": "free-tier" },
            "paidTier": { "id": "g1-pro-tier" },
            "cloudaicompanionProject": "server-project"
        }))
        .await;
        let result = provider.complete_login(tokens(None)).await.unwrap();
        assert_eq!(result.account_id, "user@gmail.com");
        assert_eq!(result.attributes["project_id"], "server-project");
        assert_eq!(result.attributes["plan_type"], "free-tier");
        let requests = state.load_requests.lock().unwrap();
        assert_eq!(requests[0].0["metadata"], antigravity_metadata());
        assert_eq!(
            requests[0].1.get("user-agent").unwrap().to_str().unwrap(),
            user_agent()
        );
    }

    #[tokio::test]
    async fn onboard_free_tier_without_current_project() {
        let (provider, state, _) = mock_assist(json!({
            "allowedTiers": [{ "id": "free-tier", "isDefault": true }]
        }))
        .await;
        let result = provider.complete_login(tokens(None)).await.unwrap();
        assert_eq!(result.attributes["project_id"], "onboarded-proj");
        let requests = state.onboard_requests.lock().unwrap();
        assert_eq!(requests[0]["tierId"], "free-tier");
        assert_eq!(requests[0]["metadata"], antigravity_metadata());
        assert!(requests[0].get("cloudaicompanionProject").is_none());
    }

    #[tokio::test]
    async fn outbound_wraps_project_and_uses_bearer() {
        let (provider, state, _) = mock_assist(json!({})).await;
        let account = account("proj-1");
        let payload = payload();
        let body = json!({"model":"gemini-2.5-flash","contents":[{"role":"user","parts":[{"text":"hi"}]}]});
        provider
            .outbound(ProviderRequest {
                account: &account,
                payload: &payload,
                body: &body,
                headers: &HeaderMap::new(),
                is_stream: false,
                upstream_protocol: "gemini",
                upstream_endpoint: "generate_content",
            })
            .await
            .unwrap();
        let hits = state.generate_hits.lock().unwrap();
        assert_eq!(hits[0].1["project"], "proj-1");
        assert_eq!(hits[0].1["model"], "gemini-2.5-flash");
        assert_eq!(hits[0].1["request"]["contents"][0]["role"], "user");
        assert_eq!(hits[0].1["request"]["session_id"], "waliapi-acc-1");
        assert!(hits[0].1["user_prompt_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        assert!(hits[0].1["request"].get("model").is_none());
        let auth = hits[0].2.get(AUTHORIZATION).unwrap().to_str().unwrap();
        assert_eq!(auth, "Bearer tok");
        assert_eq!(
            hits[0].2.get(ACCEPT).unwrap().to_str().unwrap(),
            "application/json"
        );
        assert_eq!(
            hits[0]
                .2
                .get("x-goog-api-client")
                .unwrap()
                .to_str()
                .unwrap(),
            GOOG_API_CLIENT
        );
        assert_eq!(
            hits[0].2.get("user-agent").unwrap().to_str().unwrap(),
            user_agent()
        );
        assert_eq!(
            hits[0].2.get("client-metadata").unwrap().to_str().unwrap(),
            CLIENT_METADATA
        );
        assert!(hits[0].1["request"].get("stream").is_none());
    }

    #[tokio::test]
    async fn onboard_polls_lro_until_done() {
        let (provider, state, _) = mock_assist(json!({
            "allowedTiers": [{ "id": "free-tier", "isDefault": true }]
        }))
        .await;
        state.lro.store(true, Ordering::SeqCst);
        let result = provider.complete_login(tokens(None)).await.unwrap();
        assert_eq!(result.attributes["project_id"], "lro-proj");
        assert!(*state.op_hits.lock().unwrap() >= 1);
    }

    #[tokio::test]
    async fn current_tier_without_server_project_rejects_import_hint() {
        let (provider, _, _) = mock_assist(json!({
            "currentTier": { "id": "standard-tier" }
        }))
        .await;
        let result = provider.complete_login(tokens(Some("stale-proj"))).await;
        assert!(matches!(result, Err(ProviderError::Protocol)));
    }

    #[tokio::test]
    async fn outbound_rejects_unknown_endpoint() {
        let provider = GeminiProvider::new();
        let account = account("p");
        let payload = payload();
        let body = json!({});
        let err = provider
            .outbound(ProviderRequest {
                account: &account,
                payload: &payload,
                body: &body,
                headers: &HeaderMap::new(),
                is_stream: false,
                upstream_protocol: "openai",
                upstream_endpoint: "chat_completions",
            })
            .await
            .unwrap_err();
        assert_eq!(err, ProviderError::Protocol);
    }

    #[tokio::test]
    async fn list_models_uses_antigravity_catalog() {
        let (provider, state, _) = mock_assist(json!({})).await;
        let models = provider
            .list_models(&account("p"), &payload())
            .await
            .unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gemini-2.5-flash", "gemini-3-pro-high"]
        );
        assert!(models
            .iter()
            .all(|model| model.protocol.as_deref() == Some("gemini")));
        let requests = state.model_requests.lock().unwrap();
        assert_eq!(requests[0].0, json!({ "project": "p" }));
        assert_eq!(
            requests[0].1.get("user-agent").unwrap().to_str().unwrap(),
            user_agent()
        );
    }

    #[tokio::test]
    async fn fetch_quota_reads_antigravity_model_limits() {
        let (provider, state, _) = mock_assist(json!({})).await;
        let quota = provider
            .fetch_quota(&account("p"), &payload())
            .await
            .unwrap()
            .unwrap();
        assert!(!quota.exceeded);
        assert_eq!(quota.limits.len(), 2);
        assert_eq!(quota.limits[0].limit_id, "gemini-2.5-flash");
        assert_eq!(quota.limits[0].limit_name.as_deref(), Some("Gemini Flash"));
        assert_eq!(
            quota.limits[0].primary.as_ref().unwrap().used_percent,
            Some(75.0)
        );
        assert_eq!(
            quota.limits[0]
                .primary
                .as_ref()
                .unwrap()
                .reset_at
                .as_deref(),
            Some("2026-09-23T14:51:42Z")
        );
        assert_eq!(
            quota.limits[1].primary.as_ref().unwrap().used_percent,
            Some(0.0)
        );
        assert_eq!(
            state.model_requests.lock().unwrap()[0].0,
            json!({"project": "p"})
        );
    }

    #[test]
    fn quota_parser_ignores_missing_and_invalid_fractions() {
        let quota = quota_from_models_response(&json!({
            "models": {
                "missing": {},
                "negative": {"quotaInfo": {"remainingFraction": -0.1}},
                "too_large": {"quotaInfo": {"remainingFraction": 1.1}},
                "zero": {"quotaInfo": {"remainingFraction": 0.0}}
            }
        }))
        .unwrap();
        assert_eq!(quota.limits.len(), 1);
        assert_eq!(quota.limits[0].limit_id, "zero");
        assert_eq!(
            quota.limits[0].primary.as_ref().unwrap().used_percent,
            Some(100.0)
        );
        assert!(!quota.exceeded);
        assert!(quota_from_models_response(&json!({"models": {"empty": {}}})).is_none());
    }

    #[test]
    fn model_parser_accepts_array_shape_and_rejects_empty_catalog() {
        let models = models_from_response(&json!({
            "models": [
                { "modelId": "gemini-a" },
                { "id": "gemini-b" },
                { "name": "gemini-c" }
            ]
        }))
        .unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(
            models_from_response(&json!({ "models": {} })).unwrap_err(),
            ProviderError::Protocol
        );
    }

    #[tokio::test]
    async fn legacy_payload_is_rejected_before_outbound() {
        let provider = GeminiProvider::new();
        let err = provider
            .outbound(ProviderRequest {
                account: &account("p"),
                payload: &ProviderPayload::new(json!({
                    "version": 1,
                    "access_token": "legacy",
                    "refresh_token": "legacy-refresh"
                })),
                body: &json!({}),
                headers: &HeaderMap::new(),
                is_stream: false,
                upstream_protocol: "gemini",
                upstream_endpoint: "generate_content",
            })
            .await
            .unwrap_err();
        assert_eq!(err, ProviderError::CredentialMigrationRequired);
    }

    #[tokio::test]
    async fn gemini_cli_import_is_disabled() {
        let err = GeminiProvider::new().import(b"{}").await.unwrap_err();
        assert_eq!(err, ProviderError::ImportFailed);
    }

    #[test]
    fn classify_http_maps_google_statuses() {
        assert_eq!(
            classify_http(reqwest::StatusCode::UNAUTHORIZED),
            ProviderError::Unauthorized
        );
        assert_eq!(
            classify_http(reqwest::StatusCode::FORBIDDEN),
            ProviderError::PermissionDenied
        );
        assert_eq!(
            classify_http(reqwest::StatusCode::PAYMENT_REQUIRED),
            ProviderError::PaymentRequired
        );
        assert_eq!(
            classify_http(reqwest::StatusCode::TOO_MANY_REQUESTS),
            ProviderError::Retryable
        );
    }

    #[test]
    fn validation_url_reads_ineligible_tier() {
        assert_eq!(
            validation_url(&json!({
                "ineligibleTiers": [{
                    "reasonCode": "VALIDATION_REQUIRED",
                    "validationUrl": "https://accounts.google.com/tos"
                }]
            })),
            Some("https://accounts.google.com/tos".to_owned())
        );
        assert_eq!(
            validation_url(&json!({ "ineligibleTiers": [{ "reasonCode": "OTHER" }] })),
            None
        );
    }

    #[tokio::test]
    async fn free_tier_unavailable_does_not_attempt_onboarding() {
        let (provider, _, _) = mock_assist(json!({
            "allowedTiers": [{ "id": "standard-tier", "isDefault": true }],
            "ineligibleTiers": [{ "tierId": "free-tier", "reasonCode": "UNSUPPORTED" }]
        }))
        .await;
        let err = provider.complete_login(tokens(None)).await.unwrap_err();
        assert_eq!(err, ProviderError::PermissionDenied);
    }

    #[tokio::test]
    async fn load_code_assist_validation_required_surfaces_url() {
        let (provider, _, _) = mock_assist(json!({
            "ineligibleTiers": [{
                "reasonCode": "VALIDATION_REQUIRED",
                "validationUrl": "https://accounts.google.com/tos"
            }]
        }))
        .await;
        let err = provider.complete_login(tokens(None)).await.unwrap_err();
        assert_eq!(
            err,
            ProviderError::ValidationRequired {
                url: "https://accounts.google.com/tos".into()
            }
        );
    }
}
