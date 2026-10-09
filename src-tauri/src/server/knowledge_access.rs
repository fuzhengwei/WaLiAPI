//! 普通 API Key 的知识库查询权限；管理端凭据仍由 admin 守卫处理。
use super::router::SharedState;
use crate::db::repository::Repository;
use crate::services::knowledge::{
    budget::Budget,
    model_client::{ModelClient, QueryError},
    models::*,
    rag,
    repository::KbRepository,
};
use axum::{
    body::Body,
    extract::{FromRequestParts, MatchedPath, Path},
    http::{HeaderMap, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{collections::HashMap, time::Instant};
use tracing::Instrument;

#[derive(Clone)]
pub struct KnowledgeAccess {
    pub kb_ids: Vec<String>,
    pub headers: HeaderMap,
}

pub async fn get_grants(pool: &sqlx::SqlitePool, id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT kb_id FROM api_key_knowledge_access WHERE api_key_id = ? ORDER BY kb_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}

pub async fn set_grants(
    pool: &sqlx::SqlitePool,
    id: &str,
    kb_ids: &[String],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    // 即使撤销全部授权，也先检查 Key 存在；外键保证不存在的 KB 不会留下半次更新。
    sqlx::query_scalar::<_, String>("SELECT id FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM api_key_knowledge_access WHERE api_key_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for kb_id in kb_ids.iter().collect::<std::collections::BTreeSet<_>>() {
        sqlx::query("INSERT INTO api_key_knowledge_access (api_key_id, kb_id) VALUES (?, ?)")
            .bind(id)
            .bind(kb_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

pub async fn authenticate(
    shared: &SharedState,
    headers: &HeaderMap,
) -> Result<KnowledgeAccess, QueryError> {
    // 使用服务端生成的编号，避免客户端把凭据等敏感内容伪装成关联编号。
    let request_id = uuid::Uuid::new_v4().to_string();
    let result = authenticate_inner(shared, headers).await;
    match result {
        Ok(mut access) => {
            access.headers.insert(
                "x-request-id",
                request_id.parse().expect("UUID is a valid header"),
            );
            Ok(access)
        }
        Err(error) => {
            let code = match error.status {
                StatusCode::UNAUTHORIZED => "authentication_failed",
                StatusCode::FORBIDDEN => "knowledge_access_denied",
                _ => "permission_check_failed",
            };
            tracing::warn!(
                request_id,
                code,
                status = error.status.as_u16(),
                "知识库认证失败"
            );
            Err(error
                .at_stage("permission", code)
                .with_request_id(&request_id))
        }
    }
}

async fn authenticate_inner(
    shared: &SharedState,
    headers: &HeaderMap,
) -> Result<KnowledgeAccess, QueryError> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| QueryError::new(StatusCode::UNAUTHORIZED, "Missing API key"))?;
    let key = Repository::new(shared.state.db.pool.clone())
        .get_api_key_by_key(token)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => {
                QueryError::new(StatusCode::UNAUTHORIZED, "Invalid or disabled API key")
            }
            _ => QueryError::new(StatusCode::INTERNAL_SERVER_ERROR, "Key lookup failed"),
        })?;
    if key
        .expires_at
        .as_deref()
        .is_some_and(crate::core::route_plan::is_expired)
    {
        return Err(QueryError::new(StatusCode::UNAUTHORIZED, "Expired API key"));
    }
    let kb_ids = get_grants(&shared.state.db.pool, &key.id)
        .await
        .map_err(|_| {
            QueryError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Knowledge access lookup failed",
            )
        })?;
    if kb_ids.is_empty() {
        return Err(QueryError::new(
            StatusCode::FORBIDDEN,
            "请在密钥页面授予该 API Key 知识库查询权限",
        ));
    }
    // 不携带客户端的路由覆盖、会话或缓存头，只保留经验证的身份。
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|_| QueryError::new(StatusCode::UNAUTHORIZED, "Invalid API key"))?,
    );
    Ok(KnowledgeAccess { kb_ids, headers })
}

impl KnowledgeAccess {
    pub async fn require_kb(
        &self,
        shared: &SharedState,
        kb_id: &str,
        mcp: bool,
    ) -> Result<KbKnowledgeBase, QueryError> {
        if kb_id.is_empty() {
            return Err(QueryError::new(
                StatusCode::BAD_REQUEST,
                "kb_id is required",
            ));
        }
        if !self.kb_ids.iter().any(|id| id == kb_id) {
            return Err(QueryError::new(
                StatusCode::FORBIDDEN,
                "Knowledge base access denied",
            ));
        }
        let kb = KbRepository::new(shared.state.db.pool.clone())
            .get_kb(kb_id)
            .await
            .map_err(|_| QueryError::new(StatusCode::NOT_FOUND, "Knowledge base not found"))?;
        if kb.status != 1 || (mcp && kb.mcp_enabled != 1) {
            return Err(QueryError::new(
                StatusCode::FORBIDDEN,
                "Knowledge base is not enabled for this endpoint",
            ));
        }
        Ok(kb)
    }
}

pub async fn require_read_access(
    shared: SharedState,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let access = match authenticate(&shared, request.headers()).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str())
        .unwrap_or("");
    let allowed = matches!(
        (request.method(), route),
        (
            &Method::GET,
            "/api/kb"
                | "/api/kb/search"
                | "/api/kb/{id}"
                | "/api/kb/{id}/stats"
                | "/api/kb/{id}/documents"
                | "/api/kb/{kb_id}/documents/{doc_id}"
        ) | (&Method::POST, "/api/kb/ask")
    );
    if !allowed {
        return QueryError::new(
            StatusCode::FORBIDDEN,
            "API Key 仅可查询已授权的知识库，管理操作需要管理员凭据",
        )
        .into_response();
    }
    if route.contains('{') {
        let (mut parts, body) = request.into_parts();
        let params =
            match Path::<HashMap<String, String>>::from_request_parts(&mut parts, &shared).await {
                Ok(Path(p)) => p,
                Err(e) => return e.into_response(),
            };
        let kb_id = params
            .get("id")
            .or_else(|| params.get("kb_id"))
            .map(String::as_str)
            .unwrap_or("");
        if let Err(e) = access.require_kb(&shared, kb_id, false).await {
            return e.into_response();
        }
        request = Request::from_parts(parts, body);
    }
    request.extensions_mut().insert(access);
    next.run(request).await
}

pub fn validate_query(
    query: &str,
    top_k: usize,
    mode: &str,
    vw: f32,
    kw: f32,
) -> Result<(), QueryError> {
    if query.trim().is_empty()
        || query.len() > 64 * 1024
        || !(1..=50).contains(&top_k)
        || !matches!(mode, "keyword" | "vector" | "hybrid")
        || !vw.is_finite()
        || !kw.is_finite()
        || vw < 0.0
        || kw < 0.0
        || vw + kw <= 0.0
    {
        return Err(QueryError::new(
            StatusCode::BAD_REQUEST,
            "Invalid query, top_k (1-50), search_mode or weights",
        ));
    }
    Ok(())
}

pub async fn ask(
    shared: &SharedState,
    access: &KnowledgeAccess,
    input: AskInput,
    mcp: bool,
) -> Result<RagAnswer, QueryError> {
    let request_id = access
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    run_request(
        request_id,
        input.timeout_ms,
        input.diagnostics,
        Box::pin(ask_inner(shared, access, input, mcp)),
    )
    .await
}

/// 所有显式预算知识请求共用取消终态与绝对截止；HTTP handler 不另起后台请求。
pub(crate) async fn run_request<T>(
    request_id: &str,
    timeout_ms: Option<u64>,
    diagnostics: bool,
    work: impl std::future::Future<Output = Result<T, QueryError>>,
) -> Result<T, QueryError> {
    let request_id = request_id.to_string();
    let budget = timeout_ms.map(|ms| Budget::new(ms, request_id.clone()));
    let mut trace = RagRequestTrace {
        request_id: request_id.clone(),
        budget: budget.clone(),
        finished: false,
        started: Instant::now(),
    };
    let work = Box::pin(work);
    let result = if let Some(budget) = &budget {
        budget
            .scope(async {
                tokio::time::timeout_at(budget.deadline(), work)
                    .await
                    .unwrap_or_else(|_| {
                        budget.cancel();
                        Err(rag::diagnostic_failure(
                            QueryError::new(StatusCode::GATEWAY_TIMEOUT, "RAG 总时间预算已耗尽")
                                .at_stage("rag", "rag_deadline_exceeded"),
                            "rag",
                            trace.started,
                            &[],
                            &request_id,
                            diagnostics,
                        ))
                    })
            })
            .instrument(tracing::info_span!("rag_request", request_id = %request_id))
            .await
    } else {
        work.instrument(tracing::info_span!("rag_request", request_id = %request_id))
            .await
    };
    trace.finished = true;
    tracing::info!(
        request_id,
        elapsed_ms = trace.started.elapsed().as_millis() as u64,
        status = if result.is_ok() {
            "completed"
        } else {
            "failed"
        },
        "RAG 请求终态"
    );
    result.map_err(|error| error.with_request_id(&request_id))
}

struct RagRequestTrace {
    request_id: String,
    budget: Option<Budget>,
    finished: bool,
    started: Instant,
}
impl Drop for RagRequestTrace {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(budget) = &self.budget {
                budget.cancel();
            }
            tracing::info!(
                request_id = self.request_id,
                elapsed_ms = self.started.elapsed().as_millis() as u64,
                status = "cancelled",
                "RAG future 已取消"
            );
        }
    }
}

async fn ask_inner(
    shared: &SharedState,
    access: &KnowledgeAccess,
    input: AskInput,
    mcp: bool,
) -> Result<RagAnswer, QueryError> {
    let permission_started = Instant::now();
    let request_id = access
        .headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let mut stages = Vec::new();
    let kb_id = input.kb_id.as_deref().unwrap_or("");
    let kb = access
        .require_kb(shared, kb_id, mcp)
        .await
        .map_err(|error| {
            rag::diagnostic_failure(
                error.at_stage("permission", "knowledge_access_denied"),
                "permission",
                permission_started,
                &stages,
                request_id,
                input.diagnostics,
            )
        })?;
    rag::validate_reasoning_request(input.reasoning_effort.as_deref(), input.deep_research)
        .map_err(|error| {
            rag::diagnostic_failure(
                error,
                "permission",
                permission_started,
                &stages,
                request_id,
                input.diagnostics,
            )
        })?;
    if input.deep_research {
        return Err(rag::diagnostic_failure(
            QueryError::new(
                StatusCode::FORBIDDEN,
                "API Key 查询暂不支持 deep_research，请使用普通 RAG 问答",
            )
            .at_stage("permission", "unsupported_query_mode"),
            "permission",
            permission_started,
            &stages,
            request_id,
            input.diagnostics,
        ));
    }
    let mode = input.search_mode.as_deref().unwrap_or("hybrid");
    let vw = input.vector_weight.unwrap_or(0.7);
    let kw = input.keyword_weight.unwrap_or(0.3);
    validate_query(&input.question, input.top_k, mode, vw, kw).map_err(|error| {
        rag::diagnostic_failure(
            error.at_stage("permission", "invalid_query"),
            "permission",
            permission_started,
            &stages,
            request_id,
            input.diagnostics,
        )
    })?;
    rag::validate_candidate_k(input.top_k, input.candidate_k).map_err(|error| {
        rag::diagnostic_failure(
            error,
            "permission",
            permission_started,
            &stages,
            request_id,
            input.diagnostics,
        )
    })?;
    rag::record_stage(&mut stages, "permission", "passed", permission_started);
    let client = ModelClient::ApiKey {
        shared,
        headers: &access.headers,
    };
    let result = Box::pin(rag::ask_with_client(
        &client,
        &shared.state.db.pool,
        kb_id,
        &input.question,
        kb.embedding_model
            .as_deref()
            .unwrap_or("text-embedding-3-small"),
        &input.model,
        input.top_k,
        mcp,
        input.history.as_deref().unwrap_or(&[]),
        &shared.state.settings,
        vw,
        kw,
        mode,
        input.diagnostics,
        input.allow_keyword_fallback,
        input.allow_vector_fallback,
        input.strict_retrieval,
        input.candidate_k,
        input.reasoning_effort.as_deref(),
    ))
    .await;
    match result {
        Ok(mut answer) => {
            if let Some(diagnostics) = &mut answer.diagnostics {
                diagnostics.stages.splice(0..0, stages);
            }
            tracing::info!(request_id, "知识库问答完成");
            Ok(answer)
        }
        Err(mut error) => {
            if let Some(diagnostics) = &mut error.diagnostics {
                diagnostics.stages.splice(0..0, stages);
            }
            Err(error)
        }
    }
}

/// MCP 保留历史数组结果；HTTP 新参数使用同一流程的完整元数据。
pub async fn search(
    shared: &SharedState,
    access: &KnowledgeAccess,
    input: AskInput,
    mcp: bool,
) -> Result<Vec<SearchResult>, QueryError> {
    search_with_details(shared, access, input, mcp)
        .await
        .map(|result| result.data)
}

pub async fn search_with_details(
    shared: &SharedState,
    access: &KnowledgeAccess,
    input: AskInput,
    mcp: bool,
) -> Result<SearchResponse, QueryError> {
    let request_id = access
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    run_request(
        request_id,
        input.timeout_ms,
        input.diagnostics,
        Box::pin(search_inner(shared, access, input, mcp)),
    )
    .await
}

async fn search_inner(
    shared: &SharedState,
    access: &KnowledgeAccess,
    input: AskInput,
    mcp: bool,
) -> Result<SearchResponse, QueryError> {
    let permission_started = Instant::now();
    let request_id = access
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let kb_id = input.kb_id.as_deref().unwrap_or("");
    let kb = access
        .require_kb(shared, kb_id, mcp)
        .await
        .map_err(|error| {
            rag::diagnostic_failure(
                error.at_stage("permission", "knowledge_access_denied"),
                "permission",
                permission_started,
                &[],
                request_id,
                input.diagnostics,
            )
        })?;
    let mode = input.search_mode.as_deref().unwrap_or("hybrid");
    let vw = input.vector_weight.unwrap_or(0.7);
    let kw = input.keyword_weight.unwrap_or(0.3);
    validate_query(&input.question, input.top_k, mode, vw, kw)
        .and_then(|_| rag::validate_candidate_k(input.top_k, input.candidate_k))
        .map_err(|error| {
            rag::diagnostic_failure(
                error.at_stage("permission", "invalid_query"),
                "permission",
                permission_started,
                &[],
                request_id,
                input.diagnostics,
            )
        })?;
    let mut permission_stages = Vec::new();
    rag::record_stage(
        &mut permission_stages,
        "permission",
        "passed",
        permission_started,
    );
    let client = ModelClient::ApiKey {
        shared,
        headers: &access.headers,
    };
    let result = Box::pin(rag::retrieve_with_client(
        &client,
        &shared.state.db.pool,
        kb_id,
        &input.question,
        kb.embedding_model
            .as_deref()
            .unwrap_or("text-embedding-3-small"),
        input.candidate_k.unwrap_or(input.top_k),
        mcp,
        crate::services::knowledge::retriever::FusionMode::parse(
            &shared.state.settings.get_str("kb.fusion_mode", "rrf"),
        ),
        vw,
        kw,
        mode,
        input.diagnostics,
        input.allow_keyword_fallback,
        input.allow_vector_fallback,
        input.strict_retrieval,
        false,
    ))
    .await;
    let mut retrieved = result.map_err(|mut error| {
        if let Some(diagnostics) = &mut error.diagnostics {
            diagnostics.stages.splice(0..0, permission_stages.clone());
        }
        error
    })?;
    permission_stages.append(&mut retrieved.stages);
    let mut data: Vec<_> = retrieved
        .scored_results
        .into_iter()
        .map(|result| result.result)
        .collect();
    data.truncate(input.top_k);
    if data.is_empty() {
        rag::record_stage(
            &mut permission_stages,
            "retrieval",
            "empty",
            retrieved.retrieval_started,
        );
    }
    let extended = input.timeout_ms.is_some()
        || input.allow_keyword_fallback
        || input.allow_vector_fallback
        || input.strict_retrieval
        || retrieved.degradation_reason.is_some()
        || input.diagnostics
        || input.candidate_k.is_some();
    Ok(SearchResponse {
        data,
        request_id: extended.then(|| request_id.to_string()),
        retrieval_mode: extended.then_some(retrieved.actual_mode),
        degradation_reason: retrieved.degradation_reason,
        diagnostics: input.diagnostics.then_some(RagDiagnostics {
            request_id: request_id.to_string(),
            stages: permission_stages,
        }),
    })
}
