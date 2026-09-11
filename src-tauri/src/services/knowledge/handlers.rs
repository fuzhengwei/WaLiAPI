use super::embedder;
use super::models::*;
use super::processor;
use super::rag;
use super::repository::KbRepository;
use super::retriever;
use crate::db::repository::Repository;
use crate::server::knowledge_access::{self, KnowledgeAccess};
use crate::server::router::SharedState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    Extension,
};
use serde::Deserialize;
use sha2::Digest;
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

// ─── Knowledge Base CRUD ──────────────────────────────────────────

pub async fn list_knowledge_bases(
    State(shared): State<SharedState>,
    access: Option<Extension<KnowledgeAccess>>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.get_all_kbs().await {
        Ok(mut kbs) => {
            if let Some(Extension(access)) = access {
                kbs.retain(|kb| kb.status == 1 && access.kb_ids.contains(&kb.id));
            }
            Json(serde_json::json!({ "data": kbs })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn create_knowledge_base(
    State(shared): State<SharedState>,
    Json(input): Json<CreateKbInput>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.create_kb(&input).await {
        Ok(kb) => (StatusCode::CREATED, Json(kb)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn get_knowledge_base(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.get_kb(&id).await {
        Ok(kb) => Json(kb).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Knowledge base not found").into_response(),
    }
}

pub async fn update_knowledge_base(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateKbInput>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.update_kb(&id, &input).await {
        Ok(kb) => Json(kb).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn delete_knowledge_base(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.delete_kb(&id).await {
        Ok(_) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

// ─── Document Management ──────────────────────────────────────────

pub async fn list_documents(
    State(shared): State<SharedState>,
    Path(kb_id): Path<String>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.get_documents(&kb_id).await {
        Ok(docs) => Json(serde_json::json!({ "data": docs })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn upload_document(
    State(shared): State<SharedState>,
    Path(kb_id): Path<String>,
    Json(input): Json<UploadDocInput>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());

    let content =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &input.content) {
            Ok(c) => c,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("Invalid base64: {}", e)).into_response()
            }
        };

    let hash = sha2::Sha256::digest(&content);
    let hash_hex = hex::encode(hash);

    if let Ok(Some(_)) = repo.find_document_by_hash(&kb_id, &hash_hex).await {
        return (
            StatusCode::CONFLICT,
            "Document with same content already exists",
        )
            .into_response();
    }

    let file_type = super::parser::get_file_type(&input.filename);
    let file_size = content.len() as i64;

    // 落盘路径完全由服务端生成（uuid + 白名单扩展名），原始文件名仅入库展示；
    // kb_id 先过白名单，杜绝路径穿越（FIX-02，见 upload 模块）
    let doc_id = uuid::Uuid::new_v4().to_string();
    let file_path =
        match super::upload::storage_path(&shared.state.data_dir, &kb_id, &doc_id, &input.filename)
        {
            Ok(path) => path,
            Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
        };
    if let Some(parent) = file_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("创建库目录失败: {e}"),
            )
                .into_response();
        }
    }
    if let Err(e) = std::fs::write(&file_path, &content) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("写入文件失败: {e}"),
        )
            .into_response();
    }
    let file_path_str = file_path.to_string_lossy().to_string();

    let doc = match repo
        .create_document(
            &kb_id,
            &input.filename,
            Some(&file_path_str),
            &file_type,
            file_size,
            &hash_hex,
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DB error: {}", e),
            )
                .into_response()
        }
    };

    let kb = match repo.get_kb(&kb_id).await {
        Ok(k) => k,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("KB not found: {}", e),
            )
                .into_response()
        }
    };

    let pool = shared.state.db.pool.clone();
    let events = shared.state.events.clone();
    let doc_id_clone = doc.id.clone();
    let filename_clone = input.filename.clone();
    let emb_model = kb.embedding_model.clone();
    let settings = shared.state.settings.clone();
    let data_dir = shared.state.data_dir.clone();

    tokio::spawn(async move {
        if let Err(e) = processor::process_document(
            &pool,
            &events,
            &kb_id,
            &doc_id_clone,
            &filename_clone,
            &content,
            emb_model.as_deref(),
            &settings,
            &data_dir,
        )
        .await
        {
            tracing::error!("Document processing failed: {}", e);
        }
    });

    Json(doc).into_response()
}

pub async fn get_document(
    State(shared): State<SharedState>,
    Path((kb_id, doc_id)): Path<(String, String)>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.get_document(&doc_id).await {
        Ok(doc) if doc.kb_id == kb_id => Json(doc).into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, "Document not found").into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Document not found").into_response(),
    }
}

pub async fn delete_document(
    State(shared): State<SharedState>,
    Path((kb_id, doc_id)): Path<(String, String)>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());

    if let Ok(doc) = repo.get_document(&doc_id).await {
        if let Some(path) = &doc.file_path {
            std::fs::remove_file(path).ok();
        }
        // 级联删除 OCR 页级缓存（以内容哈希为键）
        super::ocr::cache::remove_cache(&shared.state.data_dir, &doc.content_hash);
    }

    match repo.delete_document(&doc_id).await {
        Ok(_) => {
            repo.update_kb_counts(&kb_id).await.ok();
            // 增量摘除该文档的索引向量（best-effort：失败仅日志，不阻断删除）。
            // 此前删除文档完全不更新 HNSW 文件，死向量留存到下次全量重建。
            {
                let pool = shared.state.db.pool.clone();
                let events = shared.state.events.clone();
                let kb_id = kb_id.clone();
                let doc_id = doc_id.clone();
                tokio::task::spawn_blocking(move || {
                    let rt = tokio::runtime::Handle::current();
                    rt.block_on(async {
                        if let Err(e) =
                            super::retriever::index_delta(&pool, &kb_id, &doc_id, &events).await
                        {
                            tracing::warn!(
                                "Failed to update HNSW index after doc delete ({}): {}",
                                doc_id,
                                e
                            );
                        }
                    });
                });
            }
            (StatusCode::NO_CONTENT, "").into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn reindex_document(
    State(shared): State<SharedState>,
    Path((_kb_id, doc_id)): Path<(String, String)>,
) -> Response {
    let pool = shared.state.db.pool.clone();
    let events = shared.state.events.clone();
    let settings = shared.state.settings.clone();
    let data_dir = shared.state.data_dir.clone();

    tokio::spawn(async move {
        if let Err(e) =
            processor::reindex_document(&pool, &events, &doc_id, &settings, &data_dir).await
        {
            tracing::error!("Reindex failed: {}", e);
        }
    });

    Json(serde_json::json!({ "message": "Reindex started" })).into_response()
}

// ─── Search ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub kb_id: Option<String>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    pub search_mode: Option<String>,
}

fn default_top_k() -> usize {
    5
}

pub async fn search(
    State(shared): State<SharedState>,
    Query(query): Query<SearchQuery>,
    access: Option<Extension<KnowledgeAccess>>,
) -> Response {
    if let Some(Extension(access)) = access {
        let input: AskInput = serde_json::from_value(serde_json::json!({
            "question": query.q, "kb_id": query.kb_id, "top_k": query.top_k,
            "search_mode": query.search_mode.unwrap_or_else(|| "vector".into()),
        }))
        .expect("valid search input");
        return match knowledge_access::search(&shared, &access, input, false).await {
            Ok(results) => Json(serde_json::json!({"data": results})).into_response(),
            Err(error) => error.into_response(),
        };
    }
    let repo = Repository::new(shared.state.db.pool.clone());

    let emb_model = if let Some(kb_id) = &query.kb_id {
        let kb_repo = KbRepository::new(shared.state.db.pool.clone());
        kb_repo
            .get_kb(kb_id)
            .await
            .ok()
            .and_then(|kb| kb.embedding_model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string())
    } else {
        "text-embedding-3-small".to_string()
    };

    let embeddings = match embedder::embed(&[query.q.clone()], &emb_model, &repo).await {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Embedding failed: {}", e),
            )
                .into_response()
        }
    };

    if embeddings.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to embed query").into_response();
    }

    let query_emb = &embeddings[0];

    let results = if let Some(kb_id) = &query.kb_id {
        retriever::search(&shared.state.db.pool, kb_id, query_emb, query.top_k).await
    } else {
        retriever::search_all(&shared.state.db.pool, query_emb, query.top_k, false).await
    };

    match results {
        Ok(results) => Json(serde_json::json!({ "data": results })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Search failed: {}", e),
        )
            .into_response(),
    }
}

// ─── RAG Ask (with history + token fallback) ──────────────────────

pub async fn ask(
    State(shared): State<SharedState>,
    access: Option<Extension<KnowledgeAccess>>,
    Json(input): Json<AskInput>,
) -> Response {
    if let Some(Extension(access)) = access {
        return match knowledge_access::ask(&shared, &access, input, false).await {
            Ok(answer) => Json(answer).into_response(),
            Err(error) => error.into_response(),
        };
    }
    let kb_id = input.kb_id.clone().unwrap_or_default();

    let emb_model = if !kb_id.is_empty() {
        let kb_repo = KbRepository::new(shared.state.db.pool.clone());
        kb_repo
            .get_kb(&kb_id)
            .await
            .ok()
            .and_then(|kb| kb.embedding_model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string())
    } else {
        "text-embedding-3-small".to_string()
    };

    // Deep Research mode
    if input.deep_research && !kb_id.is_empty() {
        match rag::deep_research(
            &shared.state.db.pool,
            &kb_id,
            &input.question,
            &emb_model,
            &input.model,
            input.top_k,
            input.max_rounds,
            &shared.state.settings,
        )
        .await
        {
            Ok(answer) => Json(answer).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Deep research failed: {}", e),
            )
                .into_response(),
        }
    } else {
        // Normal RAG with history and configurable search
        let history = input.history.unwrap_or_default();
        let vector_weight = input.vector_weight.unwrap_or(0.7);
        let keyword_weight = input.keyword_weight.unwrap_or(0.3);
        let search_mode = input.search_mode.as_deref().unwrap_or("hybrid");

        match rag::ask_with_config(
            &shared.state.db.pool,
            &kb_id,
            &input.question,
            &emb_model,
            &input.model,
            input.top_k,
            false,
            &history,
            &shared.state.settings,
            vector_weight,
            keyword_weight,
            search_mode,
        )
        .await
        {
            Ok(answer) => Json(answer).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("RAG failed: {}", e),
            )
                .into_response(),
        }
    }
}

// ─── Stats ────────────────────────────────────────────────────────

pub async fn kb_stats(State(shared): State<SharedState>, Path(kb_id): Path<String>) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());

    let kb = match repo.get_kb(&kb_id).await {
        Ok(k) => k,
        Err(_) => return (StatusCode::NOT_FOUND, "KB not found").into_response(),
    };

    let docs = repo.get_documents(&kb_id).await.unwrap_or_default();
    let ready_count = docs.iter().filter(|d| d.status == "ready").count();
    let processing_count = docs.iter().filter(|d| d.status == "processing").count();
    let failed_count = docs.iter().filter(|d| d.status == "failed").count();
    let pending_count = docs.iter().filter(|d| d.status == "pending").count();

    let index_meta = repo.get_index_meta(&kb_id).await.ok().flatten();

    Json(serde_json::json!({
        "kb": kb,
        "documents": {
            "total": docs.len(),
            "ready": ready_count,
            "processing": processing_count,
            "failed": failed_count,
            "pending": pending_count,
        },
        "index": index_meta,
    }))
    .into_response()
}

// ════════════════════════════════════════════════════════
// New endpoints: Conversation History, Sources, Index, Import
// ════════════════════════════════════════════════════════

// ─── Conversation History ─────────────────────────────────────────

pub async fn list_conversations(
    State(shared): State<SharedState>,
    Path(kb_id): Path<String>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.get_conversations(&kb_id).await {
        Ok(convs) => Json(serde_json::json!({ "data": convs })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn clear_conversations(
    State(shared): State<SharedState>,
    Path(kb_id): Path<String>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.clear_conversations(&kb_id).await {
        Ok(_) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

// ─── Sources ──────────────────────────────────────────────────────

pub async fn list_sources(
    State(shared): State<SharedState>,
    Path(kb_id): Path<String>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.get_sources(&kb_id).await {
        Ok(sources) => Json(serde_json::json!({ "data": sources })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn delete_source(
    State(shared): State<SharedState>,
    Path((kb_id, source_id)): Path<(String, String)>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.delete_source(&source_id).await {
        Ok(_) => {
            repo.update_kb_counts(&kb_id).await.ok();
            (StatusCode::NO_CONTENT, "").into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn import_source(
    State(shared): State<SharedState>,
    Path(kb_id): Path<String>,
    Json(input): Json<ImportSourceInput>,
) -> Response {
    let pool = shared.state.db.pool.clone();
    let events = shared.state.events.clone();

    let repo = KbRepository::new(pool.clone());

    // Create source record
    let source = match repo
        .create_source(
            &kb_id,
            &input.source_type,
            input.repo_url.as_deref().or(input.url.as_deref()),
            input.dir_path.as_deref(),
            input.branch.as_deref(),
        )
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DB error: {}", e),
            )
                .into_response()
        }
    };

    let source_id = source.id.clone();
    let source_type = input.source_type.clone();
    let settings = shared.state.settings.clone();
    let data_dir = shared.state.data_dir.clone();

    tokio::spawn(async move {
        let result = if source_type == "git" {
            super::importer::import_git_repo(
                &pool, &events, &kb_id, &source_id, &input, &settings, &data_dir,
            )
            .await
        } else if source_type == "url" {
            super::importer::import_url(
                &pool, &events, &kb_id, &source_id, &input, &settings, &data_dir,
            )
            .await
        } else if source_type == "local_dir" {
            super::importer::import_local_dir(
                &pool, &events, &kb_id, &source_id, &input, &settings, &data_dir,
            )
            .await
        } else {
            Err(format!("Unknown source type: {}", source_type))
        };

        let repo = KbRepository::new(pool.clone());
        match result {
            Ok(count) => {
                repo.update_source_status(&source_id, "done", count as i64, None)
                    .await
                    .ok();
            }
            Err(e) => {
                repo.update_source_status(&source_id, "error", 0, Some(&e))
                    .await
                    .ok();
                tracing::error!("Import failed: {}", e);
            }
        }
    });

    Json(source).into_response()
}

// ─── Index Management ─────────────────────────────────────────────

pub async fn get_index_status(
    State(shared): State<SharedState>,
    Path(kb_id): Path<String>,
) -> Response {
    let repo = KbRepository::new(shared.state.db.pool.clone());
    match repo.get_index_meta(&kb_id).await {
        Ok(meta) => Json(serde_json::json!({ "data": meta })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("DB error: {}", e),
        )
            .into_response(),
    }
}

pub async fn build_index(State(shared): State<SharedState>, Path(kb_id): Path<String>) -> Response {
    let pool = shared.state.db.pool.clone();
    let kb_id_clone = kb_id.clone();
    let events = shared.state.events.clone();

    // Update index status to building immediately
    let repo = KbRepository::new(pool.clone());
    repo.update_kb_index_status(&kb_id_clone, "building")
        .await
        .ok();

    // Spawn on blocking thread pool — HNSW build is CPU-intensive
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        let pool = pool;
        let kb_id = kb_id_clone;

        rt.block_on(async {
            // Emit progress: starting
            events.emit(
                "kb-index-progress",
                serde_json::json!({
                    "kb_id": &kb_id,
                    "status": "building",
                    "message": "正在构建 HNSW 向量索引…"
                }),
            );

            match retriever::build_index(&pool, &kb_id, &events).await {
                Ok(()) => {
                    tracing::info!("HNSW index built successfully for KB {}", kb_id);
                    events.emit(
                        "kb-index-progress",
                        serde_json::json!({
                            "kb_id": &kb_id,
                            "status": "ready",
                            "message": "索引构建完成"
                        }),
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to build HNSW index for KB {}: {}", kb_id, e);
                    let repo = KbRepository::new(pool.clone());
                    repo.update_kb_index_status(&kb_id, "error").await.ok();
                    events.emit(
                        "kb-index-progress",
                        serde_json::json!({
                            "kb_id": &kb_id,
                            "status": "error",
                            "message": format!("索引构建失败: {}", e)
                        }),
                    );
                }
            }
        });
    });

    Json(serde_json::json!({ "message": "Index build started" })).into_response()
}

pub async fn drop_index(State(shared): State<SharedState>, Path(kb_id): Path<String>) -> Response {
    let pool = shared.state.db.pool.clone();

    match retriever::drop_index(&pool, &kb_id).await {
        Ok(()) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(e) => {
            tracing::error!("Failed to drop index for KB {}: {}", kb_id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to drop index: {}", e),
            )
                .into_response()
        }
    }
}
