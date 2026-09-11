//! 普通 API Key 的知识库查询权限；管理端凭据仍由 admin 守卫处理。
use super::router::SharedState;
use crate::db::repository::Repository;
use crate::services::knowledge::{
    model_client::{ModelClient, QueryError},
    models::*,
    rag,
    repository::KbRepository,
    retriever,
};
use axum::{
    body::Body,
    extract::{FromRequestParts, MatchedPath, Path},
    http::{HeaderMap, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::collections::HashMap;

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
    let kb_id = input.kb_id.as_deref().unwrap_or("");
    let kb = access.require_kb(shared, kb_id, mcp).await?;
    if input.deep_research {
        return Err(QueryError::new(
            StatusCode::FORBIDDEN,
            "API Key 查询暂不支持 deep_research，请使用普通 RAG 问答",
        ));
    }
    let mode = input.search_mode.as_deref().unwrap_or("hybrid");
    let vw = input.vector_weight.unwrap_or(0.7);
    let kw = input.keyword_weight.unwrap_or(0.3);
    validate_query(&input.question, input.top_k, mode, vw, kw)?;
    let client = ModelClient::ApiKey {
        shared,
        headers: &access.headers,
    };
    rag::ask_with_client(
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
    )
    .await
}

pub async fn search(
    shared: &SharedState,
    access: &KnowledgeAccess,
    input: AskInput,
    mcp: bool,
) -> Result<Vec<SearchResult>, QueryError> {
    let kb_id = input.kb_id.as_deref().unwrap_or("");
    let kb = access.require_kb(shared, kb_id, mcp).await?;
    let mode = input.search_mode.as_deref().unwrap_or("hybrid");
    let vw = input.vector_weight.unwrap_or(0.7);
    let kw = input.keyword_weight.unwrap_or(0.3);
    validate_query(&input.question, input.top_k, mode, vw, kw)?;
    let pool = &shared.state.db.pool;
    if mode == "keyword" {
        return retriever::keyword_only_search(pool, kb_id, &input.question, input.top_k)
            .await
            .map_err(Into::into);
    }
    let client = ModelClient::ApiKey {
        shared,
        headers: &access.headers,
    };
    let vectors = client
        .embed(
            &input.question,
            kb.embedding_model
                .as_deref()
                .unwrap_or("text-embedding-3-small"),
        )
        .await?;
    let vector = vectors.first().ok_or("Missing query embedding")?;
    if mode == "vector" {
        retriever::search(pool, kb_id, vector, input.top_k)
            .await
            .map_err(Into::into)
    } else {
        let fusion =
            retriever::FusionMode::parse(&shared.state.settings.get_str("kb.fusion_mode", "rrf"));
        retriever::hybrid_search_with_details(
            pool,
            kb_id,
            &input.question,
            vector,
            input.top_k,
            vw,
            kw,
            fusion,
        )
        .await
        .map(|results| results.into_iter().map(|r| r.result).collect())
        .map_err(Into::into)
    }
}
