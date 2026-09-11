//! RAG 的模型调用入口：外部查询复用网关的权限、额度、安全检查和日志。
use super::{embedder, models::UsageInfo};
use crate::{
    core::proxy, db::repository::Repository, server::router::SharedState,
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
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct QueryError {
    pub status: StatusCode,
    pub message: String,
}

impl QueryError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
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
        (
            self.status,
            Json(serde_json::json!({"error": {"message": self.message}})),
        )
            .into_response()
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

    pub async fn embed(&self, query: &str, model: &str) -> Result<Vec<Vec<f32>>, QueryError> {
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
                let value = read_gateway_response(response).await?;
                let embedding: Vec<f32> = serde_json::from_value(
                    value["data"][0]["embedding"].clone(),
                )
                .map_err(|_| {
                    QueryError::new(
                        StatusCode::BAD_GATEWAY,
                        "Embedding response has no float vector",
                    )
                })?;
                if embedding.is_empty() || embedding.iter().any(|v| !v.is_finite()) {
                    return Err(QueryError::new(
                        StatusCode::BAD_GATEWAY,
                        "Invalid embedding vector",
                    ));
                }
                Ok(vec![embedding])
            }
        }
    }

    pub async fn chat(&self, body: Value, purpose: &str) -> Result<ChatReply, QueryError> {
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
                let body = read_gateway_response(response).await?;
                let usage = serde_json::from_value(body["usage"].clone()).ok();
                Ok(ChatReply { body, usage })
            }
        }
    }
}

async fn read_gateway_response(response: Response) -> Result<Value, QueryError> {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .map_err(|_| QueryError::new(StatusCode::BAD_GATEWAY, "Invalid gateway response"))?;
    if !status.is_success() {
        // 不转发上游正文，避免将渠道凭据或内部信息带给知识库客户端。
        return Err(QueryError::new(status, format!("模型网关拒绝或未完成请求（HTTP {status}），请检查该 Key 的模型、渠道权限、额度及网关日志")));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| QueryError::new(StatusCode::BAD_GATEWAY, "Invalid gateway JSON"))
}
