//! RAG 的模型调用入口：外部查询复用网关的权限、额度、安全检查和日志。
use super::{
    budget::{self, BudgetElapsed},
    embedder,
    models::{RagDiagnostics, UsageInfo},
};
use crate::{
    core::{proxy, route_plan::EndpointKind},
    db::repository::Repository,
    server::router::SharedState,
    settings_store::SettingsStore,
};
use axum::{
    body::to_bytes,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};

/// 仅本地网关写入的可信失败类型，不信任上游正文或扩展字段。
#[derive(Clone)]
pub(crate) struct GatewayFailureCode(pub &'static str);

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct QueryError {
    pub status: StatusCode,
    pub message: String,
    pub stage: Option<String>,
    pub code: Option<String>,
    pub request_id: Option<String>,
    pub diagnostics: Option<Box<RagDiagnostics>>,
}

impl QueryError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            stage: None,
            code: None,
            request_id: None,
            diagnostics: None,
        }
    }
    pub fn at_stage(mut self, stage: &str, code: &str) -> Self {
        self.stage = Some(stage.to_string());
        self.code = Some(code.to_string());
        self
    }

    pub fn with_request_id(mut self, request_id: &str) -> Self {
        self.request_id = Some(request_id.to_string());
        self
    }

    /// REST 与显式 MCP 诊断共用安全响应，不返回上游正文或凭据。
    pub fn response_body(&self) -> Value {
        let mut error = serde_json::json!({"message": self.message});
        for (name, value) in [
            ("stage", &self.stage),
            ("code", &self.code),
            ("request_id", &self.request_id),
        ] {
            if let Some(value) = value {
                error[name] = Value::String(value.clone());
            }
        }
        let mut body = serde_json::json!({"error": error});
        if let Some(diagnostics) = &self.diagnostics {
            body["diagnostics"] = serde_json::to_value(diagnostics).unwrap_or(Value::Null);
        }
        body
    }

    pub fn from_budget(elapsed: BudgetElapsed, stage: &str) -> Self {
        let (status, code, message) = match elapsed {
            BudgetElapsed::Deadline => (
                StatusCode::GATEWAY_TIMEOUT,
                "rag_deadline_exceeded",
                "RAG 阶段时间预算已耗尽",
            ),
            BudgetElapsed::ChannelTimeout => (
                StatusCode::BAD_GATEWAY,
                "model_timeout",
                "模型调用达到渠道超时上限",
            ),
            BudgetElapsed::Cancelled => (
                StatusCode::from_u16(499).unwrap(),
                "client_cancelled",
                "客户端已取消请求",
            ),
        };
        let mut error = Self::new(status, message).at_stage(stage, code);
        error.request_id = budget::current().map(|budget| budget.request_id().to_string());
        error
    }
}

impl From<String> for QueryError {
    fn from(message: String) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}
impl From<&str> for QueryError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}
impl IntoResponse for QueryError {
    fn into_response(self) -> Response {
        let request_id = self.request_id.clone();
        let body = self.response_body();
        let mut response = (self.status, Json(body)).into_response();
        if let Some(value) = request_id.and_then(|id| id.parse().ok()) {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    }
}

pub struct ChatReply {
    pub body: Value,
    pub usage: Option<UsageInfo>,
}

pub enum ModelClient<'a> {
    Internal {
        pool: &'a SqlitePool,
        settings: &'a SettingsStore,
        kb_id: &'a str,
    },
    ApiKey {
        shared: &'a SharedState,
        headers: &'a HeaderMap,
    },
}

impl ModelClient<'_> {
    pub fn is_internal(&self) -> bool {
        matches!(self, Self::Internal { .. })
    }

    pub fn request_id(&self) -> Option<String> {
        match self {
            Self::ApiKey { headers, .. } => headers
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            Self::Internal { .. } => None,
        }
    }

    async fn current_api_key(
        &self,
        check_quota: bool,
    ) -> Result<Option<crate::db::models::ApiKey>, QueryError> {
        let Self::ApiKey { shared, headers } = self else {
            return Ok(None);
        };
        let token = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or("");
        let key = Repository::new(shared.state.db.pool.clone())
            .get_api_key_by_key(token)
            .await
            .map_err(|error| match error {
                sqlx::Error::RowNotFound => {
                    QueryError::new(StatusCode::UNAUTHORIZED, "API Key 无效或已停用")
                        .at_stage("permission", "authentication_failed")
                }
                _ => QueryError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法核对当前 API Key")
                    .at_stage("permission", "permission_check_failed"),
            })?;
        if key
            .expires_at
            .as_deref()
            .is_some_and(crate::core::route_plan::is_expired)
        {
            return Err(QueryError::new(StatusCode::UNAUTHORIZED, "API Key 已过期")
                .at_stage("permission", "authentication_failed"));
        }
        if check_quota && key.quota_limit > 0 && key.quota_used >= key.quota_limit {
            return Err(
                QueryError::new(StatusCode::TOO_MANY_REQUESTS, "API Key 额度已用尽").at_stage(
                    "permission",
                    if budget::current().is_some() {
                        "quota_exceeded"
                    } else {
                        "quota_or_rate_limited"
                    },
                ),
            );
        }
        Ok(Some(key))
    }

    /// 每个输出边界重新核对授权，不复用请求开始时的 grants 快照。
    pub async fn ensure_knowledge_access(&self, kb_id: &str, mcp: bool) -> Result<(), QueryError> {
        self.ensure_knowledge_access_inner(kb_id, mcp, true).await
    }

    /// 已完成模型调用后只核对身份及 KB，不能把本次合法消费刚达到额度误判为失败。
    pub async fn ensure_knowledge_access_after_answer(
        &self,
        kb_id: &str,
        mcp: bool,
    ) -> Result<(), QueryError> {
        self.ensure_knowledge_access_inner(kb_id, mcp, false).await
    }

    async fn ensure_knowledge_access_inner(
        &self,
        kb_id: &str,
        mcp: bool,
        check_quota: bool,
    ) -> Result<(), QueryError> {
        let Some(key) = self.current_api_key(check_quota).await? else {
            return Ok(());
        };
        let Self::ApiKey { shared, .. } = self else {
            unreachable!()
        };
        let granted = crate::server::knowledge_access::get_grants(&shared.state.db.pool, &key.id)
            .await
            .map_err(|_| {
                QueryError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法核对知识库授权")
                    .at_stage("permission", "permission_check_failed")
            })?;
        if !granted.iter().any(|id| id == kb_id) {
            return Err(
                QueryError::new(StatusCode::FORBIDDEN, "知识库查询权限已撤销")
                    .at_stage("permission", "knowledge_access_denied"),
            );
        }
        let kb = super::repository::KbRepository::new(shared.state.db.pool.clone())
            .get_kb(kb_id)
            .await
            .map_err(|_| {
                QueryError::new(StatusCode::NOT_FOUND, "知识库不存在")
                    .at_stage("permission", "knowledge_base_not_found")
            })?;
        if kb.status != 1 || (mcp && kb.mcp_enabled != 1) {
            return Err(QueryError::new(StatusCode::FORBIDDEN, "知识库未启用查询")
                .at_stage("permission", "knowledge_access_denied"));
        }
        Ok(())
    }

    /// 仅构造授权计划，不发送上游请求；降级前可核对模型、渠道和额度权限。
    pub async fn ensure_access(
        &self,
        kb_id: &str,
        model: &str,
        endpoint: EndpointKind,
    ) -> Result<(), QueryError> {
        self.ensure_knowledge_access(kb_id, false).await?;
        let Some(key) = self.current_api_key(true).await? else {
            return Ok(());
        };
        let Self::ApiKey { shared, .. } = self else {
            unreachable!()
        };
        let repo = Repository::new(shared.state.db.pool.clone());
        let channels = repo
            .get_enabled_channels_for_mode(endpoint.as_str(), false, &crate::utils::time::now_iso())
            .await
            .map_err(|_| {
                QueryError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法核对渠道权限")
                    .at_stage("permission", "permission_check_failed")
            })?;
        let accounts = repo
            .list_route_accounts(&crate::utils::time::now_iso())
            .await
            .map_err(|_| {
                QueryError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法核对账号权限")
                    .at_stage("permission", "permission_check_failed")
            })?;
        let flags = crate::core::feature_flags::read_feature_flags(&shared.state.settings);
        crate::core::route_plan::authorize_and_plan_with_accounts(
            &key,
            model,
            endpoint,
            &channels,
            &accounts,
            &flags,
            &serde_json::json!({"model":model}),
            &mut rand::rng(),
        )
        .map(|_| ())
        .map_err(|error| {
            let status =
                StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::BAD_GATEWAY);
            let code = match status {
                StatusCode::UNAUTHORIZED => "authentication_failed",
                StatusCode::FORBIDDEN => "access_denied",
                StatusCode::TOO_MANY_REQUESTS => {
                    if budget::current().is_some() {
                        "quota_exceeded"
                    } else {
                        "quota_or_rate_limited"
                    }
                }
                _ => "model_unavailable",
            };
            QueryError::new(status, error.message()).at_stage("permission", code)
        })
    }

    /// 降级前只重查授权，忽略暂时冷却的能力健康状态。
    pub async fn ensure_model_permission(
        &self,
        kb_id: &str,
        model: &str,
    ) -> Result<(), QueryError> {
        self.ensure_model_permission_inner(kb_id, model, true).await
    }

    /// 已成功消耗额度的向量结果仍复核身份、模型与渠道；额度仅限制下一次模型调用。
    pub async fn ensure_model_permission_after_answer(
        &self,
        kb_id: &str,
        model: &str,
    ) -> Result<(), QueryError> {
        self.ensure_model_permission_inner(kb_id, model, false)
            .await
    }

    async fn ensure_model_permission_inner(
        &self,
        kb_id: &str,
        model: &str,
        check_quota: bool,
    ) -> Result<(), QueryError> {
        self.ensure_knowledge_access_inner(kb_id, false, check_quota)
            .await?;
        let Some(mut key) = self.current_api_key(check_quota).await? else {
            return Ok(());
        };
        if !check_quota {
            // 仅此输出复核用的副本跳过额度检查，不改数据库或后续请求的鉴权。
            key.quota_limit = 0;
        }
        crate::core::route_plan::authorize_request(&key, model).map_err(|error| {
            let status =
                StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::BAD_GATEWAY);
            let code = match status {
                StatusCode::UNAUTHORIZED => "authentication_failed",
                StatusCode::TOO_MANY_REQUESTS => "quota_or_rate_limited",
                _ => "access_denied",
            };
            QueryError::new(status, error.message()).at_stage("permission", code)
        })?;
        let allowed: Vec<String> = serde_json::from_str(&key.allowed_channels).unwrap_or_default();
        let denied: Vec<String> = serde_json::from_str(&key.denied_channels).unwrap_or_default();
        if !allowed.is_empty() || !denied.is_empty() {
            let Self::ApiKey { shared, .. } = self else {
                unreachable!()
            };
            let repo = Repository::new(shared.state.db.pool.clone());
            let channels = repo.get_enabled_channels().await.map_err(|_| {
                QueryError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法核对渠道授权")
                    .at_stage("permission", "permission_check_failed")
            })?;
            let accounts = repo
                .list_route_accounts(&crate::utils::time::now_iso())
                .await
                .map_err(|_| {
                    QueryError::new(StatusCode::INTERNAL_SERVER_ERROR, "无法核对账号授权")
                        .at_stage("permission", "permission_check_failed")
                })?;
            if crate::core::route_plan::resolve_route_candidates(&channels, &accounts, model, &key)
                .is_empty()
            {
                return Err(QueryError::new(
                    StatusCode::FORBIDDEN,
                    "API Key 已没有该模型的渠道权限",
                )
                .at_stage("permission", "access_denied"));
            }
        }
        Ok(())
    }

    pub async fn embed(&self, query: &str, model: &str) -> Result<Vec<Vec<f32>>, QueryError> {
        budget::run(
            Duration::from_secs(120),
            Box::pin(self.embed_inner(query, model)),
        )
        .await
        .map_err(|elapsed| QueryError::from_budget(elapsed, "embedding"))?
    }

    async fn embed_inner(&self, query: &str, model: &str) -> Result<Vec<Vec<f32>>, QueryError> {
        match self {
            Self::Internal { pool, .. } => embedder::embed(
                &[query.to_string()],
                model,
                &Repository::new((*pool).clone()),
            )
            .await
            .map_err(Into::into),
            Self::ApiKey { shared, headers } => {
                let body = serde_json::json!({"model": model, "input": [query], "encoding_format": "float"});
                let response = crate::server::handlers::handle_knowledge_model(
                    shared,
                    headers,
                    body,
                    crate::core::route_plan::EndpointKind::Embeddings,
                )
                .await;
                let value = read_gateway_response(response, "embedding", self.request_id()).await?;
                embedder::parse_embedding_response(&value, 1).map_err(|_| {
                    let mut error =
                        QueryError::new(StatusCode::BAD_GATEWAY, "Embedding 响应缺少有效向量")
                            .at_stage("embedding", "invalid_embedding_response");
                    error.request_id = self.request_id();
                    error
                })
            }
        }
    }

    pub async fn chat(&self, body: Value, purpose: &str) -> Result<ChatReply, QueryError> {
        let stage = match purpose {
            "RAG-rewrite" => "rewrite",
            "RAG-rerank" => "rerank",
            _ => "answer",
        };
        budget::run(
            Duration::from_secs(120),
            Box::pin(self.chat_inner(body, purpose)),
        )
        .await
        .map_err(|elapsed| QueryError::from_budget(elapsed, stage))?
    }

    async fn chat_inner(&self, body: Value, purpose: &str) -> Result<ChatReply, QueryError> {
        match self {
            Self::Internal {
                pool,
                settings,
                kb_id,
            } => {
                let text = body.to_string();
                let result = proxy::handle_request(
                    &Arc::new(Repository::new((*pool).clone())),
                    settings,
                    match purpose {
                        "RAG-rewrite" => "kb-rewrite",
                        "RAG-rerank" => "kb-rerank",
                        _ => "kb-internal",
                    },
                    purpose,
                    body,
                    false,
                    Some(text),
                    Some(format!("kb-internal_{kb_id}")),
                    None,
                )
                .await
                .map_err(|(code, message)| {
                    QueryError::new(
                        StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                        message,
                    )
                })?;
                Ok(ChatReply {
                    body: result.body,
                    usage: result.usage.map(|u| UsageInfo {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        total_tokens: u.total_tokens,
                    }),
                })
            }
            Self::ApiKey { shared, headers } => {
                let response = crate::server::handlers::handle_knowledge_model(
                    shared,
                    headers,
                    body,
                    crate::core::route_plan::EndpointKind::ChatCompletions,
                )
                .await;
                let body = read_gateway_response(
                    response,
                    match purpose {
                        "RAG-rewrite" => "rewrite",
                        "RAG-rerank" => "rerank",
                        _ => "answer",
                    },
                    self.request_id(),
                )
                .await?;
                let usage = serde_json::from_value(body["usage"].clone()).ok();
                Ok(ChatReply { body, usage })
            }
        }
    }
}

async fn read_gateway_response(
    response: Response,
    stage: &str,
    request_id: Option<String>,
) -> Result<Value, QueryError> {
    let status = response.status();
    let local_code = response
        .extensions()
        .get::<GatewayFailureCode>()
        .map(|code| code.0);
    let make_error = |status, code: &str, message: &str| {
        let mut error = QueryError::new(status, message).at_stage(stage, code);
        // 编号来自传入网关的本地请求头，与 request_logs.trace_id 相同；不信任上游正文或响应头。
        error.request_id = request_id.clone();
        error
    };
    if !status.is_success() {
        // 仅按已知 HTTP 状态构造稳定错误，不读取或透传上游正文。
        let (code, message) = match status {
            _ if local_code == Some("rag_deadline_exceeded") => {
                ("rag_deadline_exceeded", "RAG 阶段时间预算已耗尽")
            }
            _ if local_code == Some("upstream_authentication_failed") => (
                "upstream_authentication_failed",
                "上游凭据无效或权限被拒绝，请检查渠道配置",
            ),
            _ if local_code == Some("upstream_quota_exceeded") => {
                ("upstream_quota_exceeded", "上游额度已用尽，请检查渠道额度")
            }
            _ if local_code == Some("upstream_rate_limited") => {
                ("upstream_rate_limited", "上游请求受到限流，请稍后重试")
            }
            _ if local_code == Some("upstream_protocol_error") => {
                ("upstream_protocol_error", "上游响应无法按协议解码")
            }
            _ if local_code == Some("upstream_unavailable") => (
                "upstream_unavailable",
                "上游服务未完成请求，请根据请求编号检查日志",
            ),
            _ if local_code == Some("upstream_transport_failed") => (
                "upstream_transport_failed",
                "无法完成上游连接或传输，请检查渠道网络",
            ),
            _ if local_code == Some("model_timeout") => (
                "model_timeout",
                "模型调用超时，请检查上游服务和渠道超时配置",
            ),
            StatusCode::NOT_IMPLEMENTED if local_code == Some("endpoint_not_configured") => (
                "endpoint_not_configured",
                if stage == "embedding" {
                    "没有可用的 Embeddings 能力渠道，请在对应模型的渠道配置中启用 Embeddings 并完成渠道测试"
                } else {
                    "没有支持该模型调用端点的渠道，请检查渠道能力配置"
                },
            ),
            StatusCode::UNAUTHORIZED => (
                "authentication_failed",
                "API Key 或上游凭据无效，请检查密钥及网关日志",
            ),
            StatusCode::FORBIDDEN => ("access_denied", "API Key 没有所需模型或渠道权限"),
            StatusCode::TOO_MANY_REQUESTS => (
                "quota_or_rate_limited",
                "API Key 额度不足或请求受到限流，请检查额度和网关日志",
            ),
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => (
                "model_timeout",
                "模型调用超时，请检查上游服务和渠道超时配置",
            ),
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS => {
                ("security_blocked", "请求被安全审计策略阻断")
            }
            StatusCode::NOT_FOUND => ("model_unavailable", "请求模型不可用，请检查模型及渠道配置"),
            _ => (
                "model_request_failed",
                "模型网关未完成请求，请根据请求编号检查网关日志",
            ),
        };
        return Err(make_error(status, code, message));
    }
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|_| {
            make_error(
                StatusCode::BAD_GATEWAY,
                "invalid_model_response",
                "无法读取模型网关响应",
            )
        })?;
    serde_json::from_slice(&bytes).map_err(|_| {
        make_error(
            StatusCode::BAD_GATEWAY,
            "invalid_model_response",
            "模型网关响应不是有效 JSON",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upstream_501_cannot_spoof_local_configuration_failure() {
        let response = (StatusCode::NOT_IMPLEMENTED, Json(serde_json::json!({
            "error": {"code": "endpoint_not_configured", "type": "route_plan_error", "message": "sk-secret"}
        }))).into_response();
        let error = read_gateway_response(response, "embedding", None)
            .await
            .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("model_request_failed"));
        assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        assert!(!error.message.contains("secret"));
    }

    #[tokio::test]
    async fn gateway_errors_keep_status_stage_and_local_trace_without_leaking_body() {
        for (status, expected_code) in [
            (StatusCode::NOT_IMPLEMENTED, "endpoint_not_configured"),
            (StatusCode::UNAUTHORIZED, "authentication_failed"),
            (StatusCode::FORBIDDEN, "access_denied"),
            (StatusCode::TOO_MANY_REQUESTS, "quota_or_rate_limited"),
            (StatusCode::GATEWAY_TIMEOUT, "model_timeout"),
        ] {
            let mut response =
                (status, "sk-secret upstream credential request_id=untrusted").into_response();
            if status == StatusCode::NOT_IMPLEMENTED {
                response
                    .extensions_mut()
                    .insert(GatewayFailureCode("endpoint_not_configured"));
            }
            let error = read_gateway_response(response, "embedding", Some("local-trace".into()))
                .await
                .unwrap_err();
            assert_eq!(error.status, status);
            assert_eq!(error.stage.as_deref(), Some("embedding"));
            assert_eq!(error.code.as_deref(), Some(expected_code));
            assert_eq!(error.request_id.as_deref(), Some("local-trace"));
            assert!(!error.message.contains("secret"));
            let response = error.into_response();
            assert_eq!(response.status(), status);
            let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["error"]["code"], expected_code);
            assert_eq!(value["error"]["request_id"], "local-trace");
        }
    }
}
