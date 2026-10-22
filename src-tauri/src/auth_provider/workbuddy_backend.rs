//! WorkBuddy auth-account adapter for the SaaS chat-completions endpoint.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

use super::{
    workbuddy_login::{
        WorkBuddyLogin, WORKBUDDY_API_BASE, WORKBUDDY_HTTP_TIMEOUT, WORKBUDDY_WEBSITE_BASE,
    },
    LoginResult, LoginRuntime, Provider, ProviderError, ProviderKind, ProviderLoginContext,
    ProviderModels, ProviderPayload, ProviderRequest, RefreshedPayload,
};
use crate::db::models::{AuthAccount, ModelState, QuotaState};

const CHAT_COMPLETIONS_PATH: &str = "/v2/chat/completions";
const CONFIG_PATH: &str = "/v3/config";
const FALLBACK_MODELS: &[&str] = &["hy3", "hy3-b", "hy3-c", "step-5-preview"];
const WORKBUDDY_API_USER_AGENT: &str = "WorkBuddy/5.8.2";
/// 上游 11150 错误（reasoning effort 不支持）的兜底档位，实测对全部可路由模型安全
const WORKBUDDY_DEFAULT_REASONING_EFFORT: &str = "medium";

fn safe_headers() -> Vec<HeaderName> {
    vec![
        HeaderName::from_static("x-request-id"),
        HeaderName::from_static("traceparent"),
        HeaderName::from_static("tracestate"),
    ]
}

pub struct WorkBuddyProvider {
    client: reqwest::Client,
    stream_client: reqwest::Client,
    api_base: String,
    login: WorkBuddyLogin,
}

impl Default for WorkBuddyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkBuddyProvider {
    pub fn new() -> Self {
        Self::with_endpoints(WORKBUDDY_API_BASE, WORKBUDDY_WEBSITE_BASE)
    }

    pub fn with_endpoints(api_base: impl Into<String>, website_base: impl Into<String>) -> Self {
        let api_base = api_base.into();
        let login = if api_base == WORKBUDDY_API_BASE {
            WorkBuddyLogin::new()
        } else {
            WorkBuddyLogin::with_endpoints(api_base.clone(), website_base)
        };
        Self {
            client: reqwest::Client::builder()
                .timeout(WORKBUDDY_HTTP_TIMEOUT)
                .build()
                .expect("workbuddy provider http client"),
            stream_client: super::streaming_http_client(),
            api_base: api_base.trim_end_matches('/').to_owned(),
            login,
        }
    }

    fn endpoint(payload: &ProviderPayload) -> String {
        payload
            .as_value()
            .get("endpoint")
            .and_then(Value::as_str)
            .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
            .map(|value| value.trim_end_matches('/'))
            .unwrap_or(WORKBUDDY_API_BASE)
            .to_owned()
    }

    fn access_token(payload: &ProviderPayload) -> Result<&str, ProviderError> {
        payload
            .as_value()
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(ProviderError::InvalidPayload)
    }

    fn request_headers(
        request: &ProviderRequest<'_>,
        access_token: &str,
    ) -> Result<HeaderMap, ProviderError> {
        let payload = request.payload.as_value();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(WORKBUDDY_API_USER_AGENT),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}"))
                .map_err(|_| ProviderError::InvalidPayload)?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("x-requested-with"),
            HeaderValue::from_static("XMLHttpRequest"),
        );
        headers.insert(
            HeaderName::from_static("x-product"),
            HeaderValue::from_static("SaaS"),
        );
        headers.insert(
            HeaderName::from_static("x-ide-type"),
            HeaderValue::from_static("WorkBuddy"),
        );
        headers.insert(
            HeaderName::from_static("x-ide-name"),
            HeaderValue::from_static("WorkBuddy"),
        );
        headers.insert(
            HeaderName::from_static("x-private-data"),
            HeaderValue::from_static("true"),
        );
        headers.insert(
            HeaderName::from_static("x-codebuddy-request"),
            HeaderValue::from_static("1"),
        );
        let version = payload
            .get("client_version")
            .and_then(Value::as_str)
            .unwrap_or("5.8.2");
        if let Ok(value) = HeaderValue::from_str(version) {
            headers.insert(HeaderName::from_static("x-ide-version"), value);
        }
        for (source, target) in [
            ("user_id", "x-user-id"),
            ("enterprise_id", "x-enterprise-id"),
            ("domain", "x-domain"),
        ] {
            if let Some(value) = payload
                .get(source)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
            {
                if let Ok(header) = HeaderValue::from_str(value) {
                    headers.insert(HeaderName::from_static(target), header);
                }
            }
        }
        if let Some(enterprise_id) = payload.get("enterprise_id").and_then(Value::as_str) {
            if let Ok(value) = HeaderValue::from_str(enterprise_id) {
                headers.insert(HeaderName::from_static("x-tenant-id"), value);
            }
        }
        for name in safe_headers() {
            if let Some(value) = request.headers.get(&name) {
                headers.insert(name, value.clone());
            }
        }
        Ok(headers)
    }

    fn client_for(&self, is_stream: bool) -> &reqwest::Client {
        if is_stream {
            &self.stream_client
        } else {
            &self.client
        }
    }
}

#[async_trait]
impl Provider for WorkBuddyProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::WorkBuddy
    }

    async fn login(
        &self,
        context: &ProviderLoginContext,
        runtime: &dyn LoginRuntime,
    ) -> Result<LoginResult, ProviderError> {
        if matches!(context.login_method, super::AuthLoginMode::BrowserCallback) {
            return Err(ProviderError::LoginFailed);
        }
        let mut result = self.login.login(runtime).await?;
        if let Some(replacement) = &context.replacement {
            if let Some(endpoint) = replacement.previous_payload.as_value().get("endpoint") {
                let mut payload = result.payload.as_value().clone();
                payload["endpoint"] = endpoint.clone();
                result.payload = ProviderPayload::new(payload);
            }
        } else if self.api_base != WORKBUDDY_API_BASE {
            let mut payload = result.payload.as_value().clone();
            payload["endpoint"] = Value::String(self.api_base.clone());
            result.payload = ProviderPayload::new(payload);
        }
        Ok(result)
    }

    async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
        Err(ProviderError::UnsupportedFeatures {
            pointer: "provider.import.workbuddy".into(),
        })
    }

    async fn refresh(&self, payload: &ProviderPayload) -> Result<RefreshedPayload, ProviderError> {
        let mut refreshed = self.login.refresh_payload(payload).await?;
        if let Some(endpoint) = payload.as_value().get("endpoint") {
            let mut next_payload = refreshed.payload.as_value().clone();
            next_payload["endpoint"] = endpoint.clone();
            refreshed.payload = ProviderPayload::new(next_payload);
        }
        Ok(refreshed)
    }

    async fn outbound(
        &self,
        request: ProviderRequest<'_>,
    ) -> Result<reqwest::Response, ProviderError> {
        if request.upstream_protocol != "openai" || request.upstream_endpoint != "chat_completions"
        {
            return Err(ProviderError::Protocol);
        }
        let access_token = Self::access_token(request.payload)?;
        let url = format!("{}{CHAT_COMPLETIONS_PATH}", Self::endpoint(request.payload));
        let headers = Self::request_headers(&request, access_token)?;
        // 非流式请求统一改写为流式上行，便于下游聚合 SSE 后还原完整响应
        let mut body = request.body.clone();
        if !request.is_stream {
            body["stream"] = Value::Bool(true);
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }
        // 上游对未显式声明 reasoning_effort 的请求返回 400（11150），
        // 按渠道账号类型统一注入默认档位（medium），模型级差异由上游自行裁决
        if body
            .get("reasoning_effort")
            .map(Value::is_null)
            .unwrap_or(true)
        {
            body["reasoning_effort"] = Value::String(WORKBUDDY_DEFAULT_REASONING_EFFORT.into());
        }
        let response = self
            .client_for(request.is_stream)
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if request.is_stream || !is_sse_response(&response) {
            return Ok(response);
        }
        let payload = response.text().await.map_err(|_| ProviderError::Protocol)?;
        let completion = aggregate_chat_stream(&payload)?;
        let body = serde_json::to_vec(&completion).map_err(|_| ProviderError::Protocol)?;
        Ok(reqwest::Response::from(
            axum::http::Response::builder()
                .status(axum::http::StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .map_err(|_| ProviderError::Protocol)?,
        ))
    }

    async fn list_models(
        &self,
        _account: &AuthAccount,
        payload: &ProviderPayload,
    ) -> Result<ProviderModels, ProviderError> {
        let access_token = Self::access_token(payload)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static(WORKBUDDY_API_USER_AGENT),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}"))
                .map_err(|_| ProviderError::InvalidPayload)?,
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(user_id) = payload.as_value().get("user_id").and_then(Value::as_str) {
            if let Ok(value) = HeaderValue::from_str(user_id) {
                headers.insert(HeaderName::from_static("x-user-id"), value);
            }
        }
        if let Some(enterprise_id) = payload
            .as_value()
            .get("enterprise_id")
            .and_then(Value::as_str)
        {
            if let Ok(value) = HeaderValue::from_str(enterprise_id) {
                headers.insert(HeaderName::from_static("x-enterprise-id"), value.clone());
                headers.insert(HeaderName::from_static("x-tenant-id"), value);
            }
        }
        let response = self
            .client
            .get(format!("{}{CONFIG_PATH}", Self::endpoint(payload)))
            .headers(headers)
            .send()
            .await
            .map_err(|_| ProviderError::Retryable)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(ProviderError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(ProviderError::Retryable);
        }
        let body: Value = response.json().await.map_err(|_| ProviderError::Protocol)?;
        let models = normalize_models(&body);
        if models.is_empty() {
            return Ok(FALLBACK_MODELS
                .iter()
                .map(|id| ModelState {
                    id: (*id).to_owned(),
                    status: "available".into(),
                    unavailable: false,
                    next_retry_after: None,
                    last_error: None,
                    protocol: Some("openai".into()),
                })
                .collect());
        }
        Ok(models)
    }

    async fn fetch_quota(
        &self,
        _account: &AuthAccount,
        _payload: &ProviderPayload,
    ) -> Result<Option<QuotaState>, ProviderError> {
        Ok(None)
    }
}

fn model_arrays(body: &Value) -> Vec<&Vec<Value>> {
    let mut arrays = Vec::new();
    for pointer in [
        "/data/models",
        "/data/data/models",
        "/models",
        "/product-config-snapshot/data/models",
        "/data/product-config-snapshot/data/models",
    ] {
        if let Some(array) = body.pointer(pointer).and_then(Value::as_array) {
            arrays.push(array);
        }
    }
    arrays
}

fn is_sse_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"))
}

fn aggregate_chat_stream(payload: &str) -> Result<Value, ProviderError> {
    let mut chunks: Vec<Value> = Vec::new();
    for line in payload.lines() {
        let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
            continue;
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        chunks.push(serde_json::from_str(data).map_err(|_| ProviderError::Protocol)?);
    }
    if chunks.is_empty() {
        return Err(ProviderError::Protocol);
    }

    let mut id = String::new();
    let mut created = 0_i64;
    let mut model = String::new();
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut finish_reason = Value::Null;
    let mut usage = Value::Null;
    let mut tool_calls: std::collections::BTreeMap<u64, Value> = Default::default();

    for chunk in chunks {
        if let Some(value) = chunk.get("id").and_then(Value::as_str) {
            id = value.to_owned();
        }
        if let Some(value) = chunk.get("created").and_then(Value::as_i64) {
            created = value;
        }
        if let Some(value) = chunk.get("model").and_then(Value::as_str) {
            model = value.to_owned();
        }
        if chunk.get("usage").is_some_and(|value| !value.is_null()) {
            usage = chunk["usage"].clone();
        }
        for choice in chunk
            .get("choices")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            if let Some(value) = choice.get("finish_reason").filter(|value| !value.is_null()) {
                finish_reason = value.clone();
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(value) = delta.get("content").and_then(Value::as_str) {
                content.push_str(value);
            }
            if let Some(value) = delta.get("reasoning_content").and_then(Value::as_str) {
                reasoning_content.push_str(value);
            }
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let entry = tool_calls.entry(index).or_insert_with(|| {
                    serde_json::json!({
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "type": "function",
                        "function": {"name": "", "arguments": ""},
                    })
                });
                if let Some(value) = call.get("id").and_then(Value::as_str) {
                    entry["id"] = Value::String(value.to_owned());
                }
                if let Some(value) = call.get("type").and_then(Value::as_str) {
                    entry["type"] = Value::String(value.to_owned());
                }
                // 跳过空 name：上游每帧重发 "name":""，空串不得覆盖首帧真名。
                if let Some(value) = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    entry["function"]["name"] = Value::String(value.to_owned());
                }
                if let Some(value) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    if let Some(arguments) = entry["function"]["arguments"].as_str() {
                        entry["function"]["arguments"] =
                            Value::String(format!("{arguments}{value}"));
                    }
                }
            }
        }
    }

    let mut message = serde_json::json!({"role": "assistant", "content": content});
    if !reasoning_content.is_empty() {
        message["reasoning_content"] = Value::String(reasoning_content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls.into_values().collect());
    }
    Ok(serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    }))
}

fn normalize_models(body: &Value) -> ProviderModels {
    let mut models = Vec::new();
    for array in model_arrays(body) {
        for entry in array {
            let Some(id) = entry.get("id").and_then(Value::as_str).map(str::trim) else {
                continue;
            };
            if id.is_empty()
                || id.starts_with("custom-local:")
                || id.starts_with("codewise-")
                || id.starts_with("completion-")
                || id.contains("image")
            {
                continue;
            }
            if entry.get("supportsToolCall").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            // 上游配置中的模型不一定都能路由（实测部分返回 11102 service info not found），
            // 但配置里没有可靠字段区分，因此不做模型级白名单——保持全量同步，
            // 可用性由运行时熔断（model_states 的 unavailable/next_retry_after）按渠道账号实际调用结果收敛
            models.push(ModelState {
                id: id.to_owned(),
                status: "available".into(),
                unavailable: false,
                next_retry_after: None,
                last_error: None,
                protocol: Some("openai".into()),
            });
        }
    }
    models.dedup_by(|a, b| a.id == b.id);
    models
}

#[cfg(test)]
mod tests {
    use std::sync::{atomic::AtomicUsize, Arc};

    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;

    fn payload() -> ProviderPayload {
        ProviderPayload::new(json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "user_id": "user-1",
            "enterprise_id": "tenant-1",
            "domain": "example.tencent.com",
        }))
    }

    fn account() -> AuthAccount {
        AuthAccount {
            id: "local-1".into(),
            provider: "workbuddy".into(),
            label: "WorkBuddy".into(),
            account_id: "user-1".into(),
            status: "active".into(),
            disabled: 0,
            priority: 0,
            weight: 1,
            sort_order: 0,
            quota_json: None,
            model_states_json: "{}".into(),
            model_mapping_json: "{}".into(),
            model_mapping_disabled: "[]".into(),
            attributes_json: "{}".into(),
            payload_json: "{}".into(),
            last_refreshed_at: None,
            last_models_sync_at: None,
            next_refresh_after: None,
            next_retry_after: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn request<'a>(
        account: &'a AuthAccount,
        payload: &'a ProviderPayload,
        body: &'a Value,
        caller: &'a HeaderMap,
    ) -> ProviderRequest<'a> {
        ProviderRequest {
            account,
            payload,
            body,
            headers: caller,
            is_stream: false,
            upstream_protocol: "openai",
            upstream_endpoint: "chat_completions",
        }
    }

    async fn start_router(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn outbound_hits_openai_chat_completions_and_filters_caller_auth() {
        let hits = Arc::new(AtomicUsize::new(0));
        let outbound_hits = hits.clone();
        let app = Router::new().route(
            CHAT_COMPLETIONS_PATH,
            post(
                move |headers: reqwest::header::HeaderMap, Json(body): Json<Value>| {
                    let outbound_hits = outbound_hits.clone();
                    async move {
                        outbound_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer access-token");
                        assert_eq!(headers.get("x-user-id").unwrap(), "user-1");
                        assert_eq!(headers.get("x-enterprise-id").unwrap(), "tenant-1");
                        assert_eq!(headers.get("x-tenant-id").unwrap(), "tenant-1");
                        assert_eq!(headers.get("x-domain").unwrap(), "example.tencent.com");
                        assert_eq!(headers.get("x-product").unwrap(), "SaaS");
                        assert!(headers.get("caller-authorization").is_none());
                        assert!(headers.get("x-secret").is_none());
                        assert_eq!(body["model"], "hy3");
                        Json(json!({"choices": []}))
                    }
                },
            ),
        );
        let base = start_router(app).await;
        let provider = WorkBuddyProvider::with_endpoints(base.clone(), WORKBUDDY_WEBSITE_BASE);
        let mut caller = HeaderMap::new();
        caller.insert(
            reqwest::header::HeaderName::from_static("authorization"),
            reqwest::header::HeaderValue::from_static("Bearer caller-token"),
        );
        caller.insert(
            reqwest::header::HeaderName::from_static("x-request-id"),
            reqwest::header::HeaderValue::from_static("request-1"),
        );
        let local_account = account();
        let mut payload_value = payload().as_value().clone();
        payload_value["endpoint"] = Value::String(base.clone());
        let local_payload = ProviderPayload::new(payload_value);
        let body = json!({"model": "hy3", "messages": []});
        let response = provider
            .outbound(request(&local_account, &local_payload, &body, &caller))
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn list_models_normalizes_config_and_falls_back_when_empty() {
        let app = Router::new().route(
            CONFIG_PATH,
            get(|headers: reqwest::header::HeaderMap| async move {
                assert_eq!(
                    headers.get(reqwest::header::USER_AGENT).unwrap(),
                    WORKBUDDY_API_USER_AGENT
                );
                Json(json!({
                    "code": 0,
                    "data": {"models": [
                        {"id": "hy3", "supportsToolCall": true},
                        {"id": "hy3-b"},
                        {"id": "custom-local:k3"},
                        {"id": "completion-fast"},
                        {"id": "image-render"},
                        {"id": "tool-off", "supportsToolCall": false}
                    ]}
                }))
            }),
        );
        let base = start_router(app).await;
        let provider = WorkBuddyProvider::with_endpoints(base.clone(), WORKBUDDY_WEBSITE_BASE);
        let local_account = account();
        let mut payload_value = payload().as_value().clone();
        payload_value["endpoint"] = Value::String(base.clone());
        let local_payload = ProviderPayload::new(payload_value);
        let models = provider
            .list_models(&local_account, &local_payload)
            .await
            .unwrap();
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["hy3", "hy3-b"]);
    }

    #[tokio::test]
    async fn list_models_uses_fallback_when_catalog_has_no_models() {
        let app = Router::new().route(
            CONFIG_PATH,
            get(|| async { Json(json!({"code": 0, "data": {"models": []}})) }),
        );
        let base = start_router(app).await;
        let provider = WorkBuddyProvider::with_endpoints(base.clone(), WORKBUDDY_WEBSITE_BASE);
        let mut payload_value = payload().as_value().clone();
        payload_value["endpoint"] = Value::String(base.clone());
        let local_payload = ProviderPayload::new(payload_value);
        let local_account = account();
        let models = provider
            .list_models(&local_account, &local_payload)
            .await
            .unwrap();
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, FALLBACK_MODELS);
    }

    #[test]
    fn normalize_models_returns_empty_for_unrecognized_catalog() {
        let models = normalize_models(&json!({"data": {"catalog": []}}));
        assert!(models.is_empty());
    }

    #[test]
    fn aggregate_chat_stream_merges_sse_chunks_and_skips_heartbeat() {
        // 上游 SSE 帧带 `: heartbeat` 注释行与 usage 末帧，聚合后应还原完整 chat.completion
        let payload = concat!(
            ": heartbeat\n",
            "data: {\"id\":\"c1\",\"model\":\"hy3\",\"created\":1700,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\",\"reasoning_content\":\"\"},\"finish_reason\":\"\"}],\"usage\":null}\n",
            "\n",
            "data: {\"id\":\"c1\",\"model\":\"hy3\",\"created\":1700,\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Think\"}}],\"usage\":null}\n",
            "data: {\"id\":\"c1\",\"model\":\"hy3\",\"created\":1700,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}],\"usage\":null}\n",
            "data: {\"id\":\"c1\",\"model\":\"hy3\",\"created\":1700,\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"}}],\"usage\":null}\n",
            "data: {\"id\":\"c1\",\"model\":\"hy3\",\"created\":1700,\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n",
            "data: [DONE]\n",
        );
        let completion = aggregate_chat_stream(payload).unwrap();
        assert_eq!(completion["id"], "c1");
        assert_eq!(completion["model"], "hy3");
        assert_eq!(
            completion["choices"][0]["message"]["content"],
            "Hello world"
        );
        assert_eq!(
            completion["choices"][0]["message"]["reasoning_content"],
            "Think"
        );
        assert_eq!(completion["choices"][0]["finish_reason"], "stop");
        assert_eq!(completion["usage"]["total_tokens"], 5);
    }

    #[test]
    fn aggregate_chat_stream_merges_tool_calls_by_index() {
        let payload = concat!(
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get\",\"arguments\":\"{\\\"a\\\":\"}}]}}]}\n",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]}}]}\n",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            "data: [DONE]\n",
        );
        let completion = aggregate_chat_stream(payload).unwrap();
        let tool_calls = completion["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[0]["function"]["name"], "get");
        assert_eq!(tool_calls[0]["function"]["arguments"], "{\"a\":1}");
    }

    #[test]
    fn aggregate_chat_stream_rejects_payload_without_chunks() {
        assert!(aggregate_chat_stream(": heartbeat\ndata: [DONE]\n").is_err());
    }

    /// 上游每帧重发空 name 时，聚合结果必须保留首帧的真实函数名（回归测试）。
    #[test]
    fn aggregate_chat_stream_keeps_tool_name_from_first_chunk() {
        let payload = concat!(
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get\",\"arguments\":\"{\\\"a\\\":\"}}]}}]}\n",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"\",\"arguments\":\"1}\"}}]}}]}\n",
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            "data: [DONE]\n",
        );
        let completion = aggregate_chat_stream(payload).unwrap();
        let tool_calls = completion["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["function"]["name"], "get");
        assert_eq!(tool_calls[0]["function"]["arguments"], "{\"a\":1}");
    }
}
