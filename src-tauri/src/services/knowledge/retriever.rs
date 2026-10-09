use super::index::HnswIndex;
use super::models::SearchResult;
use super::repository::{ChunkWithEmbedding, KbRepository};
use crate::server::event_bridge::EventSink;
use sqlx::{Acquire, SqlitePool};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock, Weak,
};

/// 同一知识库的完整读改写串行；弱引用避免删除过的知识库永久占用锁表。
fn index_write_lock(kb_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    type LockMap = std::collections::HashMap<String, Weak<tokio::sync::Mutex<()>>>;
    static LOCKS: OnceLock<Mutex<LockMap>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(kb_id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(kb_id.to_string(), Arc::downgrade(&lock));
    lock
}

/// Default HNSW parameters
const DEFAULT_M: usize = 16;
const DEFAULT_EF_CONSTRUCTION: usize = 200;
const DEFAULT_EF_SEARCH: usize = 50;

/// Get the index file path for a KB.
fn index_path(kb_id: &str) -> PathBuf {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("./data"))
        .join("waliapi")
        .join("hnsw_indexes");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("kb_{}.hnsw", kb_id))
}

/// 后台任务开始后不会随 Future drop 自动停止；本地 guard 和请求预算共同取消。
struct BlockingCancellation {
    local: Arc<AtomicBool>,
    budget: Option<super::budget::Budget>,
}

impl BlockingCancellation {
    fn cancelled(&self) -> bool {
        self.local.load(Ordering::Relaxed)
            || self
                .budget
                .as_ref()
                .is_some_and(|budget| budget.check().is_err())
    }
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

async fn retrieval_blocking<T: Send + 'static>(
    stage: &'static str,
    work: impl FnOnce(&BlockingCancellation) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    // ponytail: 全进程最多 4 个检索 CPU/IO 任务，避免并发请求撑大 blocking 队列。
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let started = std::time::Instant::now();
    let local = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(local.clone());
    let cancel = BlockingCancellation {
        local,
        budget: super::budget::current(),
    };
    let permit = SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| "retrieval worker unavailable")?;
    if cancel.cancelled() {
        return Err("retrieval cancelled".into());
    }
    tracing::debug!(
        stage,
        queue_ms = started.elapsed().as_millis() as u64,
        "RAG blocking queue"
    );
    let request_id = cancel
        .budget
        .as_ref()
        .map(|budget| budget.request_id())
        .unwrap_or("");
    let span = tracing::debug_span!("rag_retrieval_worker", request_id, stage);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _entered = span.enter();
        if cancel.cancelled() {
            return Err("retrieval cancelled".into());
        }
        let started = std::time::Instant::now();
        let result = work(&cancel);
        tracing::debug!(
            stage,
            elapsed_ms = started.elapsed().as_millis() as u64,
            cancelled = cancel.cancelled(),
            "RAG blocking stage"
        );
        result
    })
    .await
    .map_err(|error| format!("retrieval worker failed: {error}"))?
}

/// 每次仍加载完整文件；未引入缺乏可靠 generation 失效机制的常驻缓存。
fn load_index(kb_id: &str, cancel: &BlockingCancellation) -> Result<Option<HnswIndex>, String> {
    let path = index_path(kb_id);
    if !path.exists() {
        return Ok(None);
    }
    match HnswIndex::load_cancellable(&path, &|| cancel.cancelled()) {
        Ok(index)
            if index.initialized && !index.is_empty() && index.entry_point < index.nodes.len() =>
        {
            Ok(Some(index))
        }
        Ok(_) => Ok(None),
        Err(error) if cancel.cancelled() => Err(error),
        Err(error) => {
            tracing::warn!(kb_id, error, "Invalid HNSW index, using linear scan");
            Ok(None)
        }
    }
}

/// HNSW 和候选正文使用同一 SQLite 读快照；过期时只读取一次完整向量。
pub async fn search(
    pool: &SqlitePool,
    kb_id: &str,
    query_embedding: &[f32],
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    if query_embedding.is_empty() {
        return Ok(Vec::new());
    }
    if query_embedding.iter().any(|value| !value.is_finite()) {
        return Err("invalid query embedding".into());
    }
    let owned_kb = kb_id.to_string();
    let index =
        retrieval_blocking("index_load", move |cancel| load_index(&owned_kb, cancel)).await?;
    let pool_started = std::time::Instant::now();
    let mut connection = pool
        .acquire()
        .await
        .map_err(|error| format!("Failed to acquire search connection: {error}"))?;
    tracing::debug!(
        kb_id,
        stage = "pool_acquire",
        elapsed_ms = pool_started.elapsed().as_millis() as u64,
        "RAG SQL stage"
    );
    let mut snapshot = connection
        .begin()
        .await
        .map_err(|error| format!("Failed to begin search snapshot: {error}"))?;

    if let Some(index) = index.filter(|index| index.dim == query_embedding.len()) {
        let sql_started = std::time::Instant::now();
        let identities = KbRepository::search_chunk_identities(&mut snapshot, kb_id)
            .await
            .map_err(|error| format!("Failed to load chunk identities: {error}"))?;
        tracing::debug!(
            kb_id,
            stage = "chunk_identities",
            elapsed_ms = sql_started.elapsed().as_millis() as u64,
            rows = identities.len(),
            "RAG SQL stage"
        );
        let query = query_embedding.to_vec();
        let results = retrieval_blocking("index_search", move |cancel| {
            let mut current_ids = std::collections::HashSet::with_capacity(identities.len());
            for identity in identities {
                if cancel.cancelled() {
                    return Err("retrieval cancelled".into());
                }
                if identity.embedding_dim != index.dim as i64
                    || identity.embedding_bytes != 8 + index.dim as i64 * 4
                    || (identity.expected_dim != 0 && identity.expected_dim != index.dim as i64)
                    || identity.index_status == "stale"
                {
                    return Ok(None);
                }
                current_ids.insert(identity.id);
            }
            let identity_count = current_ids.len();
            let mut live_count = 0;
            for node in &index.nodes {
                if cancel.cancelled() {
                    return Err("retrieval cancelled".into());
                }
                if index.tombstones.contains(&node.id) {
                    continue;
                }
                live_count += 1;
                if node.vector.len() != index.dim
                    || node.vector.iter().any(|value| !value.is_finite())
                    || !current_ids.remove(&node.id)
                {
                    return Ok(None);
                }
            }
            // 数量和完整 ID 集合均相等，不能只依赖 count（同数量替换也会失效）。
            if live_count != identity_count || !current_ids.is_empty() {
                return Ok(None);
            }
            index
                .search_cancellable(&query, top_k, &|| cancel.cancelled())
                .map(|results| {
                    results
                        .iter()
                        .all(|result| result.score.is_finite())
                        .then_some(results)
                })
        })
        .await?;
        if let Some(results) = results.filter(|results| !results.is_empty()) {
            let ids = results
                .iter()
                .map(|result| result.id.clone())
                .collect::<Vec<_>>();
            let sql_started = std::time::Instant::now();
            let chunks = KbRepository::search_chunks_by_ids(&mut snapshot, kb_id, &ids)
                .await
                .map_err(|error| format!("Failed to load candidate chunks: {error}"))?;
            tracing::debug!(
                kb_id,
                stage = "candidate_chunks",
                elapsed_ms = sql_started.elapsed().as_millis() as u64,
                rows = chunks.len(),
                "RAG SQL stage"
            );
            if chunks.len() == results.len() {
                snapshot
                    .rollback()
                    .await
                    .map_err(|error| error.to_string())?;
                let mut chunks = chunks
                    .into_iter()
                    .map(|chunk| (chunk.id.clone(), chunk))
                    .collect::<std::collections::HashMap<_, _>>();
                return Ok(results
                    .into_iter()
                    .filter_map(|result| {
                        chunks.remove(&result.id).map(|chunk| SearchResult {
                            chunk_id: chunk.id,
                            doc_id: chunk.doc_id,
                            filename: chunk.filename,
                            content: chunk.content,
                            score: result.score,
                            metadata: serde_json::from_str(&chunk.metadata)
                                .unwrap_or_else(|_| serde_json::json!({})),
                        })
                    })
                    .collect());
            }
        }
        tracing::debug!(
            kb_id,
            "HNSW snapshot is outdated, using complete linear scan"
        );
    }
    let sql_started = std::time::Instant::now();
    let chunks = KbRepository::search_vector_chunks(&mut snapshot, kb_id)
        .await
        .map_err(|error| format!("Failed to load vector chunks: {error}"))?;
    tracing::debug!(
        kb_id,
        stage = "vector_chunks",
        elapsed_ms = sql_started.elapsed().as_millis() as u64,
        rows = chunks.len(),
        "RAG SQL stage"
    );
    snapshot
        .rollback()
        .await
        .map_err(|error| error.to_string())?;
    // 释放池连接之后再做完整向量计算，hybrid 的 FTS 不会等待 CPU 扫描。
    drop(connection);
    let query = query_embedding.to_vec();
    retrieval_blocking("linear_search", move |cancel| {
        linear_search_chunks(chunks, &query, top_k, cancel)
    })
    .await
}

fn linear_search_chunks(
    chunks: Vec<ChunkWithEmbedding>,
    query: &[f32],
    top_k: usize,
    cancel: &BlockingCancellation,
) -> Result<Vec<SearchResult>, String> {
    let mut scored = Vec::with_capacity(chunks.len());
    for (position, chunk) in chunks.iter().enumerate() {
        if cancel.cancelled() {
            return Err("retrieval cancelled".into());
        }
        let vector = decode_embedding(&chunk.embedding);
        // 旧切片可能没有写入维度（0）；以完整解码后的实际维度验证兼容。
        if (chunk.embedding_dim != 0 && chunk.embedding_dim != query.len() as i64)
            || vector.len() != query.len()
            || vector.iter().any(|value| !value.is_finite())
        {
            continue;
        }
        let score = cosine_similarity(query, &vector);
        if score.is_finite() {
            scored.push((score, position));
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    if cancel.cancelled() {
        return Err("retrieval cancelled".into());
    }
    scored.truncate(top_k);
    Ok(scored
        .into_iter()
        .map(|(score, position)| {
            let chunk = &chunks[position];
            SearchResult {
                chunk_id: chunk.id.clone(),
                doc_id: chunk.doc_id.clone(),
                filename: chunk.filename.clone(),
                content: chunk.content.clone(),
                score,
                metadata: serde_json::from_str(&chunk.metadata)
                    .unwrap_or_else(|_| serde_json::json!({})),
            }
        })
        .collect())
}

/// 管理命令与 REST 共用的文本检索入口，单库按指定模式执行。
#[allow(clippy::too_many_arguments)]
pub async fn search_query(
    pool: &SqlitePool,
    kb_id: Option<&str>,
    query: &str,
    top_k: usize,
    search_mode: &str,
    vector_weight: f32,
    keyword_weight: f32,
    fusion_mode: FusionMode,
) -> Result<Vec<SearchResult>, String> {
    let kb_id = kb_id.filter(|id| !id.is_empty());
    if !matches!(search_mode, "keyword" | "vector" | "hybrid") {
        return Err("不支持的检索模式".to_string());
    }
    let total_weight = vector_weight + keyword_weight;
    if !vector_weight.is_finite()
        || !keyword_weight.is_finite()
        || vector_weight < 0.0
        || keyword_weight < 0.0
        || !total_weight.is_finite()
        || total_weight <= 0.0
    {
        return Err("检索权重必须是有限非负数，且总和大于零".to_string());
    }
    if let Some(kb_id) = kb_id {
        if search_mode == "keyword" {
            return keyword_only_search(pool, kb_id, query, top_k).await;
        }
    }

    let model = if let Some(kb_id) = kb_id {
        KbRepository::new(pool.clone())
            .get_kb(kb_id)
            .await
            .map_err(|e| e.to_string())?
            .embedding_model
    } else {
        None
    };
    let embeddings = super::embedder::embed(
        &[query.to_string()],
        model.as_deref().unwrap_or("text-embedding-3-small"),
        &crate::db::repository::Repository::new(pool.clone()),
    )
    .await?;
    let embedding = embeddings.first().ok_or("Failed to embed query")?;
    match kb_id {
        Some(kb_id) if search_mode == "hybrid" => {
            hybrid_search(
                pool,
                kb_id,
                query,
                embedding,
                top_k,
                vector_weight,
                keyword_weight,
                fusion_mode,
            )
            .await
        }
        Some(kb_id) => search(pool, kb_id, embedding, top_k).await,
        // 未指定知识库时沿用现有的跨库向量检索行为。
        None => search_all(pool, embedding, top_k, false).await,
    }
}

/// Search across all knowledge bases.
/// If mcp_only is true, only search KBs with mcp_enabled = 1.
pub async fn search_all(
    pool: &SqlitePool,
    query_embedding: &[f32],
    top_k: usize,
    mcp_only: bool,
) -> Result<Vec<SearchResult>, String> {
    search_all_with_details(pool, query_embedding, top_k, mcp_only, false)
        .await
        .map(|(results, _)| results)
}

/// 跨库仍按向量检索；默认保留正常库的结果，布尔值表示存在局部失败。
/// 显式严格模式不接受部分成功。摘要只含安全计数，不包含知识库名单或原始错误。
pub(crate) async fn search_all_with_details(
    pool: &SqlitePool,
    query_embedding: &[f32],
    top_k: usize,
    mcp_only: bool,
    strict_retrieval: bool,
) -> Result<(Vec<SearchResult>, bool), String> {
    check_cross_kb_budget()?;
    let repo = KbRepository::new(pool.clone());

    let kbs = repo.get_all_kbs().await;
    check_cross_kb_budget()?;
    let kbs = kbs.map_err(|_| "cross_kb_list_failed".to_string())?;

    let active_kbs: Vec<_> = kbs
        .iter()
        .filter(|kb| kb.status == 1 && (!mcp_only || kb.mcp_enabled == 1))
        .collect();

    if active_kbs.is_empty() {
        return Ok((vec![], false));
    }

    let mut all_results = Vec::new();
    let mut failed_kbs = 0;
    for kb in &active_kbs {
        check_cross_kb_budget()?;
        let result = search(pool, &kb.id, query_embedding, top_k).await;
        // 取消和阶段/总体截止优先于部分成功，不能作为坏库被忽略。
        check_cross_kb_budget()?;
        match result {
            Ok(results) => all_results.extend(results),
            Err(error) => {
                // 同一个共享池无法取得读连接，不属于可跳过的单库内容故障。
                if error.starts_with("Failed to acquire search connection:")
                    || error.starts_with("Failed to begin search snapshot:")
                {
                    return Err("cross_kb_pool_failed".into());
                }
                if error == "retrieval cancelled" {
                    return Err("client_cancelled".into());
                }
                if strict_retrieval {
                    return Err("cross_kb_search_failed".into());
                }
                failed_kbs += 1;
            }
        }
    }

    if failed_kbs > 0 {
        tracing::warn!(
            code = "knowledge_base_search_partial",
            actual_mode = "vector",
            searched_kbs = active_kbs.len(),
            failed_kbs,
            "Cross-KB vector search returned partial results"
        );
    }

    all_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(top_k);
    check_cross_kb_budget()?;

    Ok((all_results, failed_kbs > 0))
}

fn check_cross_kb_budget() -> Result<(), String> {
    if let Some(budget) = super::budget::current() {
        budget.check_request().map_err(|elapsed| match elapsed {
            super::budget::BudgetElapsed::Cancelled => "client_cancelled".to_string(),
            _ => "rag_deadline_exceeded".to_string(),
        })?;
        budget.check().map_err(|elapsed| match elapsed {
            super::budget::BudgetElapsed::Cancelled => "client_cancelled".to_string(),
            _ => "stage_timeout".to_string(),
        })?;
    }
    Ok(())
}

/// Get the embedding dimension for a KB by checking the first valid chunk.
pub async fn detect_embedding_dim(pool: &SqlitePool, kb_id: &str) -> Result<Option<usize>, String> {
    let repo = KbRepository::new(pool.clone());
    let chunks = repo
        .get_chunks_by_kb(kb_id)
        .await
        .map_err(|e| format!("Failed to load chunks: {}", e))?;

    for (_, _, _, emb, _, _) in &chunks {
        let vector = decode_embedding(emb);
        if !vector.is_empty() {
            return Ok(Some(vector.len()));
        }
    }

    Ok(None)
}

// ════════════════════════════════════════════════════════
// Index management
// ════════════════════════════════════════════════════════

/// 按文档增量更新 HNSW 索引（C-06/R1）：库内该文档现存 chunk 与索引内
/// 该文档节点做差集——多出 insert、消失墓碑。索引文件缺失 / 旧格式（节点
/// 无 doc_id，bincode 跨格式不可读或读了无法定位文档）/ 索引已空 → 回退
/// 全量 build_index。写回为文件整体覆盖（与全量构建同一交换模式，索引无
/// 内存常驻状态）；调用方应置于 spawn_blocking（沿既有模式）。
pub async fn index_delta(
    pool: &SqlitePool,
    kb_id: &str,
    doc_id: &str,
    events: &EventSink,
) -> Result<(), String> {
    let _guard = index_write_lock(kb_id).lock_owned().await;
    let repo = KbRepository::new(pool.clone());
    let path = index_path(kb_id);

    let kb = repo.get_kb(kb_id).await.map_err(|e| e.to_string())?;
    let loaded = if kb.index_status == "stale" {
        Err("embedding model changed".to_string())
    } else if path.exists() {
        HnswIndex::load(&path)
    } else {
        Err("index file missing".to_string())
    };

    let mut index = match loaded {
        Ok(i) if i.initialized && !i.is_legacy_format() && !i.is_empty() => i,
        Ok(_) | Err(_) => {
            // 索引缺失/旧格式/已掏空：回退全量重建（进度事件由 build_index 发出）
            tracing::info!(
                "Falling back to full index build for KB {} (doc {} delta)",
                kb_id,
                doc_id
            );
            return build_index_locked(pool, kb_id, events).await;
        }
    };

    // 库侧现存向量（维度不符的丢弃，与全量构建同语义）
    let chunks = repo
        .get_chunk_vectors_by_doc(doc_id)
        .await
        .map_err(|e| format!("Failed to load chunk vectors: {}", e))?;
    let current: Vec<(String, Vec<f32>)> = chunks
        .iter()
        .filter_map(|(id, blob)| {
            let v = decode_embedding(blob);
            (v.len() == index.dim).then(|| (id.clone(), v))
        })
        .collect();

    // 差集：索引有、库无 → 墓碑；库有、索引无 → 插入
    let current_ids: std::collections::HashSet<String> =
        current.iter().map(|(id, _)| id.clone()).collect();
    let mut removed = 0usize;
    for id in index.doc_node_ids(doc_id) {
        if !current_ids.contains(&id) && index.remove(&id) {
            removed += 1;
        }
    }
    let mut inserted = 0usize;
    for (id, vector) in &current {
        if !index.contains_live(id) && index.insert(id, doc_id, vector) {
            inserted += 1;
        }
    }

    index
        .save(&path)
        .map_err(|e| format!("Failed to save index: {}", e))?;

    repo.upsert_index_meta(
        kb_id,
        index.dim as i64,
        index.len() as i64,
        Some(path.to_str().unwrap_or("")),
        "ready",
    )
    .await
    .map_err(|e| format!("Failed to update index meta: {}", e))?;
    repo.update_kb_index_status(kb_id, "ready")
        .await
        .map_err(|e| format!("Failed to update KB index status: {}", e))?;

    tracing::info!(
        "Incremental index update for KB {} doc {}: +{} -{}, {} live nodes",
        kb_id,
        doc_id,
        inserted,
        removed,
        index.len()
    );

    Ok(())
}

/// Build HNSW index for a KB from all its chunks.
/// Emits `kb-index-progress` Tauri events with percentage.
pub async fn build_index(pool: &SqlitePool, kb_id: &str, events: &EventSink) -> Result<(), String> {
    let _guard = index_write_lock(kb_id).lock_owned().await;
    build_index_locked(pool, kb_id, events).await
}

/// 调用方已持有该库写锁；delta 的全量回退不能重复加锁。
async fn build_index_locked(
    pool: &SqlitePool,
    kb_id: &str,
    events: &EventSink,
) -> Result<(), String> {
    let repo = KbRepository::new(pool.clone());

    let chunks = repo
        .get_chunks_by_kb(kb_id)
        .await
        .map_err(|e| format!("Failed to load chunks: {}", e))?;

    if chunks.is_empty() {
        return Err("No chunks to index".to_string());
    }

    // Build (chunk_id, doc_id, vector) triples
    let mut items: Vec<(String, String, Vec<f32>)> = Vec::with_capacity(chunks.len());
    let mut dim = 0;

    tracing::info!("Building HNSW index, processing {} chunks...", chunks.len());
    for (id, _, _, emb, _, doc_id) in chunks.iter() {
        let vector = decode_embedding(emb);
        if !vector.is_empty() {
            if dim == 0 {
                dim = vector.len();
                tracing::debug!("Detected embedding dimension: {}", dim);
            }
            if vector.len() == dim {
                items.push((id.clone(), doc_id.clone(), vector));
            }
        }
    }
    tracing::info!("Prepared {} items for HNSW index", items.len());

    if items.is_empty() {
        return Err("No valid embeddings found".to_string());
    }

    // Create and build the index on blocking thread pool

    // Emit initial progress with total count
    let total_items = items.len();
    events.emit(
        "kb-index-progress",
        serde_json::json!({
            "kb_id": kb_id,
            "status": "building",
            "progress": 0,
            "current": 0,
            "total": total_items,
            "message": format!("准备构建索引：{} 个切片，维度 {}", total_items, dim)
        }),
    );

    let app_clone = events.clone();
    let kb_id_clone = kb_id.to_string();

    // CPU-intensive build runs on blocking thread pool to avoid starving async runtime
    let index = tokio::task::spawn_blocking(move || {
        let mut index = HnswIndex::new(dim, DEFAULT_M, DEFAULT_EF_CONSTRUCTION, DEFAULT_EF_SEARCH);
        index.build_with_progress(&items, |current, total| {
            let pct = if total > 0 {
                current * 100 / total
            } else {
                100
            };
            app_clone.emit(
                "kb-index-progress",
                serde_json::json!({
                    "kb_id": &kb_id_clone,
                    "status": "building",
                    "progress": pct,
                    "current": current,
                    "total": total,
                    "message": format!("构建中 {}/{} ({}%)", current, total, pct)
                }),
            );
        });
        index
    })
    .await
    .map_err(|e| format!("Build task panicked: {}", e))?;

    let index = index;

    // Save to file
    let path = index_path(kb_id);
    index
        .save(&path)
        .map_err(|e| format!("Failed to save index: {}", e))?;

    // Update DB metadata
    repo.upsert_index_meta(
        kb_id,
        dim as i64,
        total_items as i64,
        Some(path.to_str().unwrap_or("")),
        "ready",
    )
    .await
    .map_err(|e| format!("Failed to update index meta: {}", e))?;

    repo.update_kb_index_status(kb_id, "ready")
        .await
        .map_err(|e| format!("Failed to update KB index status: {}", e))?;

    tracing::info!(
        "HNSW index built for KB {}: {} nodes, dim {}, saved to {:?}",
        kb_id,
        total_items,
        dim,
        path
    );

    Ok(())
}

/// Drop the HNSW index for a KB.
pub async fn drop_index(pool: &SqlitePool, kb_id: &str) -> Result<(), String> {
    let _guard = index_write_lock(kb_id).lock_owned().await;
    let repo = KbRepository::new(pool.clone());

    // Delete index file
    let path = index_path(kb_id);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to remove index file: {}", e))?;
    }

    // Update DB metadata
    repo.upsert_index_meta(kb_id, 0, 0, None, "none")
        .await
        .map_err(|e| format!("Failed to update index meta: {}", e))?;

    repo.update_kb_index_status(kb_id, "none")
        .await
        .map_err(|e| format!("Failed to update KB index status: {}", e))?;

    tracing::info!("HNSW index dropped for KB {}", kb_id);

    Ok(())
}

/// Get index metadata from DB.
pub async fn get_index_status(
    pool: &SqlitePool,
    kb_id: &str,
) -> Result<Option<super::models::KbIndexMeta>, String> {
    let repo = KbRepository::new(pool.clone());
    repo.get_index_meta(kb_id)
        .await
        .map_err(|e| format!("Failed to get index meta: {}", e))
}

// ════════════════════════════════════════════════════════
// FTS5 hybrid search
// ════════════════════════════════════════════════════════

/// FTS5 full-text search
async fn fts5_search(
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    // FIX-27：空/纯符号查询直接返回空结果——此前无 token 时回退把原文
    // 整段塞进 MATCH，FTS5 会把它当查询表达式解析（空串报错、裸运算符
    // 误匹配甚至注入语法错误）。
    let tokens = tokenize_query(query);
    if tokens.is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }
    backfill_fts_projection(pool, kb_id).await?;
    let fts_query = build_fts_query(&tokens);

    fts5_expression_search(pool, kb_id, &fts_query, top_k).await
}

async fn backfill_fts_projection(pool: &SqlitePool, kb_id: &str) -> Result<(), String> {
    let backfill_started = std::time::Instant::now();
    let updated = KbRepository::new(pool.clone())
        .backfill_search_text_for_kb(kb_id)
        .await
        .map_err(|e| format!("FTS5 projection backfill failed: {}", e))?;
    tracing::debug!(
        kb_id,
        stage = "fts_projection",
        elapsed_ms = backfill_started.elapsed().as_millis() as u64,
        updated,
        "RAG SQL stage"
    );
    Ok(())
}

async fn fts5_expression_search(
    pool: &SqlitePool,
    kb_id: &str,
    fts_query: &str,
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    let pool_started = std::time::Instant::now();
    let mut connection = pool
        .acquire()
        .await
        .map_err(|e| format!("FTS5 connection failed: {e}"))?;
    tracing::debug!(
        kb_id,
        stage = "fts_pool_acquire",
        elapsed_ms = pool_started.elapsed().as_millis() as u64,
        "RAG SQL stage"
    );
    fts5_expression_search_on(&mut connection, kb_id, fts_query, top_k).await
}

async fn fts5_expression_search_on(
    connection: &mut sqlx::SqliteConnection,
    kb_id: &str,
    fts_query: &str,
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    let sql_started = std::time::Instant::now();
    let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT c.id, c.content, c.metadata, d.filename, c.doc_id \
         FROM kb_chunks_fts fts \
         JOIN kb_chunks c ON fts.chunk_id = c.id \
         JOIN kb_documents d ON c.doc_id = d.id \
         WHERE c.kb_id = ? AND d.status = 'ready' AND kb_chunks_fts MATCH ? \
         ORDER BY rank \
         LIMIT ?",
    )
    .bind(kb_id)
    .bind(fts_query)
    .bind(top_k as i64)
    .fetch_all(connection)
    .await
    .map_err(|e| format!("FTS5 search failed: {}", e))?;
    tracing::debug!(
        kb_id,
        stage = "fts_query",
        elapsed_ms = sql_started.elapsed().as_millis() as u64,
        rows = rows.len(),
        "RAG SQL stage"
    );
    let results = rows
        .into_iter()
        .enumerate()
        .map(|(idx, (id, content, metadata, filename, doc_id))| {
            let score = 1.0 / (1.0 + idx as f32 * 0.1);
            let meta: serde_json::Value = serde_json::from_str(&metadata).unwrap_or_default();
            SearchResult {
                chunk_id: id,
                doc_id,
                filename,
                content,
                score,
                metadata: meta,
            }
        })
        .collect();

    Ok(results)
}

/// Build FTS5 query string from user query using improved tokenization
fn build_fts_query(tokens: &[String]) -> String {
    // FTS5: OR-connected prefix terms for broader recall.
    // tokens 非空由调用方保证（空 token 路径在 fts5_search 提前返回）。
    tokens
        .iter()
        .map(|t| format!("\"{}\"*", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// 文档和查询使用同一套 Unicode 规范化与中文双字分词。
fn tokenize_query(query: &str) -> Vec<String> {
    super::text::query_tokens(query)
}

/// Search result with individual score breakdowns for retrieval visualization.
#[derive(Debug, Clone)]
pub struct ScoredSearchResult {
    pub result: SearchResult,
    pub vector_score: Option<f32>,
    pub keyword_score: Option<f32>,
}

/// Hybrid search: vector + FTS5 weighted merge
/// Returns results with individual score breakdowns for visualization.
pub async fn hybrid_search(
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    query_embedding: &[f32],
    top_k: usize,
    vector_weight: f32,
    keyword_weight: f32,
    fusion_mode: FusionMode,
) -> Result<Vec<SearchResult>, String> {
    let scored = hybrid_search_with_details(
        pool,
        kb_id,
        query,
        query_embedding,
        top_k,
        vector_weight,
        keyword_weight,
        fusion_mode,
    )
    .await?;
    Ok(scored.into_iter().map(|s| s.result).collect())
}

/// Hybrid search returning detailed score breakdowns.
/// 混合检索融合模式（C-06/R3）：
/// - Rrf：倒数排名融合（只用排名，天然消两路量纲差异；默认）
/// - Weighted：线性加权（历史行为，量纲敏感，保留可配回退）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionMode {
    Rrf,
    Weighted,
}

impl FusionMode {
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("weighted") {
            Self::Weighted
        } else {
            Self::Rrf
        }
    }
}

/// RRF 常数 k：排名 1 的贡献 1/(k+1)——业界惯用 60，
/// 对 top_k 量级的候选列表区分度稳定。
const RRF_K: f32 = 60.0;

/// 融合两路检索结果（纯函数）：按 mode 计算综合分并截断 top_k。
/// RRF 分数 = Σ 1/(k + rank)（rank 从 1 起，只在出现该 id 的路里计）；
/// Weighted 分数 = v_score·vw + k_score·kw（历史行为原样保留）。
pub fn fuse_scored(
    vector_results: &[SearchResult],
    keyword_results: &[SearchResult],
    top_k: usize,
    vector_weight: f32,
    keyword_weight: f32,
    mode: FusionMode,
) -> Vec<ScoredSearchResult> {
    let mut vector_map: std::collections::HashMap<String, (SearchResult, f32)> =
        std::collections::HashMap::new();
    for r in vector_results {
        vector_map.insert(r.chunk_id.clone(), (r.clone(), r.score));
    }

    let mut keyword_map: std::collections::HashMap<String, (SearchResult, f32)> =
        std::collections::HashMap::new();
    for r in keyword_results {
        keyword_map.insert(r.chunk_id.clone(), (r.clone(), r.score));
    }

    let mut all_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    all_ids.extend(vector_map.keys().cloned());
    all_ids.extend(keyword_map.keys().cloned());

    let mut scored: Vec<(String, f32, Option<f32>, Option<f32>)> = Vec::new();
    for id in &all_ids {
        let v_score = vector_map.get(id).map(|(_, s)| *s);
        let k_score = keyword_map.get(id).map(|(_, s)| *s);
        let final_score = match mode {
            FusionMode::Weighted => {
                v_score.unwrap_or(0.0) * vector_weight + k_score.unwrap_or(0.0) * keyword_weight
            }
            FusionMode::Rrf => {
                // 排名在各自路内按分数降序确定（输入已排序，这里防御性重算）
                let v_rank = vector_results
                    .iter()
                    .position(|r| &r.chunk_id == id)
                    .map(|p| p + 1);
                let k_rank = keyword_results
                    .iter()
                    .position(|r| &r.chunk_id == id)
                    .map(|p| p + 1);
                let mut score = 0.0;
                if let Some(rank) = v_rank {
                    score += 1.0 / (RRF_K + rank as f32);
                }
                if let Some(rank) = k_rank {
                    score += 1.0 / (RRF_K + rank as f32);
                }
                score
            }
        };
        scored.push((id.clone(), final_score, v_score, k_score));
    }

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);

    let mut results = Vec::with_capacity(scored.len());
    for (id, final_score, v_score, k_score) in scored {
        // Prefer vector result (has embedding metadata), fallback to keyword result
        let base = vector_map
            .get(&id)
            .map(|(r, _)| r.clone())
            .or_else(|| keyword_map.get(&id).map(|(r, _)| r.clone()));
        if let Some(mut r) = base {
            r.score = final_score;
            results.push(ScoredSearchResult {
                result: r,
                vector_score: v_score,
                keyword_score: k_score,
            });
        }
    }
    results
}

pub async fn hybrid_search_with_details(
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    query_embedding: &[f32],
    top_k: usize,
    vector_weight: f32,
    keyword_weight: f32,
    fusion_mode: FusionMode,
) -> Result<Vec<ScoredSearchResult>, String> {
    let (vector_results, keyword_results) = tokio::join!(
        search(pool, kb_id, query_embedding, top_k * 2),
        fts5_search(pool, kb_id, query, top_k * 2),
    );

    let vector_results = vector_results.unwrap_or_default();
    let keyword_results = keyword_results.unwrap_or_default();

    Ok(fuse_scored(
        &vector_results,
        &keyword_results,
        top_k,
        vector_weight,
        keyword_weight,
        fusion_mode,
    ))
}

/// Keyword-only search using FTS5 (no vector search).
pub async fn keyword_only_search(
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    fts5_search(pool, kb_id, query, top_k).await
}

pub(crate) fn exact_anchor_match(content: &str, anchor: &str) -> bool {
    if anchor.is_empty() {
        return false;
    }
    let content = content.to_lowercase();
    let anchor = super::text::normalize_radicals(anchor).to_lowercase();
    content.match_indices(&anchor).any(|(offset, _)| {
        if !anchor.is_ascii() || !anchor.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return true;
        }
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        !content[..offset].chars().next_back().is_some_and(is_word)
            && !content[offset + anchor.len()..]
                .chars()
                .next()
                .is_some_and(is_word)
    })
}

/// 每个明确标记的规范句最多取 400 字，避免反例中的大量 SQL 词压过规则正文。
fn normative_passages(content: &str) -> Vec<(usize, String)> {
    let mut passages = Vec::new();
    let mut offset = 0;
    let lines: Vec<_> = content.split_inclusive('\n').collect();
    for (i, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        if line.contains('【') {
            let mut passage = line.to_string();
            for raw_next in lines.iter().skip(i + 1) {
                let next = raw_next.trim_end_matches(['\r', '\n']);
                if next.contains('【')
                    || next.contains("正例")
                    || next.contains("反例")
                    || next.contains("检查方式")
                {
                    break;
                }
                passage.push('\n');
                passage.push_str(next);
                if passage.chars().count() >= 400 {
                    break;
                }
            }
            passages.push((offset, passage.chars().take(400).collect()));
        }
        // split_inclusive 保留真实 CRLF / LF 字符，偏移仍对应原始正文。
        offset += raw_line.chars().count();
    }
    passages
}

/// 窗口优先展示匹配具体锚点的规范句；来源与可选重排共用。
pub fn evidence_window(content: &str, anchors: &[String], limit: usize) -> (String, usize) {
    let normalized = super::text::normalize_radicals(content);
    let mut passages = normative_passages(&normalized);
    passages.sort_by_cached_key(|(_, passage)| {
        std::cmp::Reverse(
            anchors
                .iter()
                .filter(|a| exact_anchor_match(passage, a))
                .count(),
        )
    });
    if let Some((start, _)) = passages
        .first()
        .filter(|(_, passage)| anchors.iter().any(|a| exact_anchor_match(passage, a)))
    {
        let chars: Vec<_> = content.chars().collect();
        let start = (*start).min(chars.len());
        return (chars.iter().skip(start).take(limit).collect(), start);
    }
    super::text::match_window(content, anchors, limit)
}

pub fn section_at(content: &str, character_offset: usize) -> Option<String> {
    let mut section = None;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        if offset > character_offset {
            break;
        }
        let number: String = line
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let parts: Vec<_> = number.trim_end_matches('.').split('.').collect();
        if parts.len() >= 2
            && parts
                .iter()
                .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        {
            section = Some(number.trim_end_matches('.').to_string());
        }
        offset += line.chars().count();
    }
    section
}

// ════════════════════════════════════════════════════════
// Utility functions
// ════════════════════════════════════════════════════════

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

pub fn decode_embedding(blob: &[u8]) -> Vec<f32> {
    bincode::deserialize(blob).unwrap_or_default()
}

/// Encode embedding to BLOB for storage.
pub fn encode_embedding(vec: &[f32]) -> Vec<u8> {
    bincode::serialize(vec).unwrap_or_default()
}

// ════════════════════════════════════════════════════════
// Token estimation utilities
// ════════════════════════════════════════════════════════

/// Estimate token count: ~4 chars/token for ASCII, ~2 chars/token for CJK.
pub fn estimate_tokens(text: &str) -> usize {
    let ascii_chars = text.chars().filter(|c| c.is_ascii()).count();
    let non_ascii_chars = text.chars().filter(|c| !c.is_ascii()).count();
    (ascii_chars / 4) + (non_ascii_chars / 2) + 1
}

/// Get model context window limit.
pub fn get_model_context_limit(model: &str) -> usize {
    let m = model.to_lowercase();
    if m.contains("gpt-4o") {
        128_000
    } else if m.contains("gpt-4") {
        8_192
    } else if m.contains("gpt-3.5") {
        16_385
    } else if m.contains("claude-3")
        || m.contains("claude-sonnet")
        || m.contains("claude-opus")
        || m.contains("claude-haiku")
    {
        200_000
    } else if m.contains("gemini") {
        1_000_000
    } else if m.contains("deepseek") {
        64_000
    } else if m.contains("qwen") {
        32_000
    } else if m.contains("llama") {
        8_192
    } else if m.contains("mistral") || m.contains("mixtral") {
        32_000
    } else {
        8_192
    }
}

#[cfg(test)]
mod cross_kb_search_tests {
    use super::*;
    use crate::services::knowledge::budget::Budget;
    use std::time::Duration;

    async fn fixture_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    // 随机 ID 不会读取已有知识库的 HNSW；畸形 JSON 触发真实的单库 SQL 故障。
    async fn seed_kb(pool: &SqlitePool, metadata: Option<&str>) -> String {
        let kb_id = format!("cross-kb-fixture-{}", uuid::Uuid::new_v4());
        let doc_id = format!("{kb_id}-doc");
        let chunk_id = format!("{kb_id}-chunk");
        let now = "2026-10-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO kb_knowledge_bases (id, name, created_at, updated_at) VALUES (?, 'cross-kb-fixture', ?, ?)",
        )
        .bind(&kb_id)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO kb_documents (id, kb_id, filename, file_type, content_hash, status, created_at, updated_at) \
             VALUES (?, ?, 'fixture.txt', 'text', ?, 'ready', ?, ?)",
        )
        .bind(&doc_id)
        .bind(&kb_id)
        .bind(&doc_id)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
        if let Some(metadata) = metadata {
            sqlx::query(
                "INSERT INTO kb_chunks (id, doc_id, kb_id, chunk_index, content, token_count, embedding, embedding_dim, metadata, created_at) \
                 VALUES (?, ?, ?, 0, 'public fixture content', 4, ?, 2, ?, ?)",
            )
            .bind(&chunk_id)
            .bind(&doc_id)
            .bind(&kb_id)
            .bind(encode_embedding(&[1.0, 0.0]))
            .bind(metadata)
            .bind(now)
            .execute(pool)
            .await
            .unwrap();
        }
        chunk_id
    }

    #[tokio::test]
    async fn legacy_keeps_healthy_results_when_one_kb_fails() {
        let pool = fixture_pool().await;
        seed_kb(&pool, Some("private-marker malformed JSON")).await;
        let healthy_id = seed_kb(&pool, Some("{}")).await;

        let legacy = search_all(&pool, &[1.0, 0.0], 5, false).await.unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].chunk_id, healthy_id);
        let (results, any_failed) = search_all_with_details(&pool, &[1.0, 0.0], 5, false, false)
            .await
            .unwrap();
        assert!(any_failed);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, healthy_id);
    }

    #[tokio::test]
    async fn strict_rejects_partial_results_with_a_safe_error() {
        let pool = fixture_pool().await;
        seed_kb(&pool, Some("private-marker malformed JSON")).await;
        seed_kb(&pool, Some("{}")).await;
        let error = search_all_with_details(&pool, &[1.0, 0.0], 5, false, true)
            .await
            .unwrap_err();
        assert_eq!(error, "cross_kb_search_failed");
        assert!(!error.contains("private-marker"));
    }

    #[tokio::test]
    async fn all_local_failures_preserve_legacy_empty_but_report_partial() {
        let pool = fixture_pool().await;
        seed_kb(&pool, Some("malformed JSON one")).await;
        seed_kb(&pool, Some("malformed JSON two")).await;
        let (results, any_failed) = search_all_with_details(&pool, &[1.0, 0.0], 5, false, false)
            .await
            .unwrap();
        assert!(results.is_empty());
        assert!(any_failed);
        assert!(search_all(&pool, &[1.0, 0.0], 5, false)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn healthy_empty_kb_is_complete_and_mixed_failure_is_partial() {
        let pool = fixture_pool().await;
        seed_kb(&pool, None).await;
        let (results, any_failed) = search_all_with_details(&pool, &[1.0, 0.0], 5, false, false)
            .await
            .unwrap();
        assert!(results.is_empty());
        assert!(!any_failed);
        seed_kb(&pool, Some("malformed JSON")).await;
        let (results, any_failed) = search_all_with_details(&pool, &[1.0, 0.0], 5, false, false)
            .await
            .unwrap();
        assert!(results.is_empty());
        assert!(any_failed);
    }

    #[tokio::test]
    async fn list_failure_and_request_cancellation_are_terminal() {
        let pool = fixture_pool().await;
        pool.close().await;
        assert_eq!(
            search_all_with_details(&pool, &[1.0, 0.0], 5, false, false)
                .await
                .unwrap_err(),
            "cross_kb_list_failed"
        );
        let budget = Budget::new(1_000, "cross-kb-cancelled");
        budget.cancel();
        assert_eq!(
            budget
                .scope(search_all_with_details(&pool, &[1.0, 0.0], 5, false, false))
                .await
                .unwrap_err(),
            "client_cancelled"
        );
    }

    #[tokio::test]
    async fn stage_and_overall_deadlines_do_not_return_partial_success() {
        let pool = fixture_pool().await;
        let parent = Budget::new(1_000, "cross-kb-stage");
        let stage = parent.stage("retrieval", Duration::ZERO, Duration::ZERO);
        assert_eq!(
            stage
                .scope(search_all_with_details(&pool, &[1.0, 0.0], 5, false, false))
                .await
                .unwrap_err(),
            "stage_timeout"
        );
        let expired = Budget::new(100, "cross-kb-total");
        tokio::time::sleep(Duration::from_millis(110)).await;
        assert_eq!(
            expired
                .scope(search_all_with_details(&pool, &[1.0, 0.0], 5, false, false))
                .await
                .unwrap_err(),
            "rag_deadline_exceeded"
        );
    }
}

#[cfg(test)]
mod fts_defense_tests {
    use super::*;

    async fn fts_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// FIX-27：空/纯空白/纯符号查询不产生任何 token。
    #[test]
    fn empty_and_symbol_queries_yield_no_tokens() {
        assert!(tokenize_query("").is_empty());
        assert!(tokenize_query("   \t\n").is_empty());
        assert!(tokenize_query("!!! ??? ... '").is_empty());
        assert!(!tokenize_query("hello").is_empty());
        assert!(!tokenize_query("知识库").is_empty());
    }

    /// FIX-27：生成的 MATCH 表达式对每个 token 加引号——FTS5 运算符
    /// 单词（AND/OR/NOT/NEAR）不再被当表达式解析。
    #[test]
    fn built_query_neutralizes_fts_operators() {
        let tokens = vec!["AND".to_string(), "or".to_string(), "near".to_string()];
        assert_eq!(build_fts_query(&tokens), "\"AND\"* OR \"or\"* OR \"near\"*");
        // 内嵌引号被剥离，不破坏外层短语引用。
        let tokens = vec!["a\"b".to_string()];
        assert_eq!(build_fts_query(&tokens), "\"ab\"*");
    }

    /// FIX-27：空查询/纯符号查询不再把原文塞进 MATCH（此前空串直接
    /// FTS5 语法错误、符号原文被当表达式）——现在干净返回空结果。
    #[tokio::test]
    async fn empty_query_search_returns_empty_without_error() {
        let pool = fts_pool().await;
        for query in ["", "   ", "!!! ???", "\"\""] {
            let result = fts5_search(&pool, "kb-any", query, 5).await;
            assert!(
                result.as_ref().map(|r| r.is_empty()).unwrap_or(false),
                "查询 {query:?} 应无错返回空结果，实际: {result:?}"
            );
        }
    }
}

#[cfg(test)]
mod index_delta_tests {
    use super::*;
    use crate::services::knowledge::repository::ChunkInsert;

    async fn delta_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn sink() -> EventSink {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        EventSink::headless(tx)
    }

    async fn seed_kb(pool: &SqlitePool, kb_id: &str) {
        let now = "2026-09-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO kb_knowledge_bases (id, name, created_at, updated_at) VALUES (?, 'delta-test', ?, ?)",
        )
        .bind(kb_id)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_doc(pool: &SqlitePool, kb_id: &str, doc_id: &str) {
        let now = "2026-09-08T00:00:00Z";
        sqlx::query(
            "INSERT INTO kb_documents (id, kb_id, filename, file_type, content_hash, status, source_type, doc_meta, created_at, updated_at) \
             VALUES (?, ?, 'f.txt', 'text', ?, 'ready', 'upload', '{}', ?, ?)",
        )
        .bind(doc_id)
        .bind(kb_id)
        .bind(doc_id)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn add_chunk(pool: &SqlitePool, kb_id: &str, doc_id: &str, chunk_id: &str, seed: f32) {
        let vector = vec![seed.sin(), seed.cos(), seed * 0.01];
        KbRepository::new(pool.clone())
            .create_chunk(&ChunkInsert {
                id: chunk_id.to_string(),
                doc_id: doc_id.to_string(),
                kb_id: kb_id.to_string(),
                chunk_index: 0,
                content: format!("content {}", chunk_id),
                token_count: 1,
                embedding: encode_embedding(&vector),
                embedding_dim: vector.len() as i64,
                metadata: "{}".to_string(),
                content_hash: None,
                created_at: "2026-09-08T00:00:00Z".to_string(),
            })
            .await
            .unwrap();
    }

    fn query_vec(seed: f32) -> Vec<f32> {
        vec![seed.sin(), seed.cos(), seed * 0.01]
    }

    /// C-06/R1：单文档增/删/整删走 delta 后，索引状态与全量重建等价。
    #[tokio::test]
    async fn delta_add_update_delete_roundtrip() {
        let pool = delta_pool().await;
        let events = sink();
        let kb_id = format!("kb-delta-{}", uuid::Uuid::new_v4());
        seed_kb(&pool, &kb_id).await;
        seed_doc(&pool, &kb_id, "doc-a").await;
        seed_doc(&pool, &kb_id, "doc-b").await;

        for i in 0..8 {
            add_chunk(&pool, &kb_id, "doc-a", &format!("a-{}", i), i as f32 * 0.3).await;
        }
        for i in 0..6 {
            add_chunk(
                &pool,
                &kb_id,
                "doc-b",
                &format!("b-{}", i),
                5.0 + i as f32 * 0.3,
            )
            .await;
        }

        build_index(&pool, &kb_id, &events).await.unwrap();

        // doc-a 变更：删两块、加三块（新 chunk id，模拟重处理）
        sqlx::query("DELETE FROM kb_chunks WHERE id IN ('a-0', 'a-1')")
            .execute(&pool)
            .await
            .unwrap();
        for i in 0..3 {
            add_chunk(
                &pool,
                &kb_id,
                "doc-a",
                &format!("a-n{}", i),
                2.0 + i as f32 * 0.2,
            )
            .await;
        }
        index_delta(&pool, &kb_id, "doc-a", &events).await.unwrap();

        // doc-b 整删（FK 级联删 chunk）
        sqlx::query("DELETE FROM kb_documents WHERE id = 'doc-b'")
            .execute(&pool)
            .await
            .unwrap();
        index_delta(&pool, &kb_id, "doc-b", &events).await.unwrap();

        let index = HnswIndex::load(&index_path(&kb_id)).unwrap();
        assert!(!index.contains_live("a-0"), "removed chunk must be gone");
        assert!(!index.contains_live("a-1"));
        assert!(index.contains_live("a-n0"), "added chunk must be live");
        assert!(index.contains_live("a-n2"));
        assert!(index.contains_live("a-7"), "untouched chunk survives");
        assert!(
            index.doc_node_ids("doc-b").is_empty(),
            "deleted doc has no live nodes"
        );

        // 检索可用：新块向量查询应在新块中命中
        let results = index.search(&query_vec(2.0), 3);
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"a-n0"), "search results: {:?}", ids);

        std::fs::remove_file(index_path(&kb_id)).ok();
    }

    /// C-06/R1：索引缺失或旧格式（无 doc_id）时 delta 回退全量重建。
    #[tokio::test]
    async fn delta_falls_back_to_full_build() {
        let pool = delta_pool().await;
        let events = sink();
        let kb_id = format!("kb-delta-fb-{}", uuid::Uuid::new_v4());
        seed_kb(&pool, &kb_id).await;
        seed_doc(&pool, &kb_id, "doc-a").await;
        add_chunk(&pool, &kb_id, "doc-a", "a-0", 0.5).await;
        add_chunk(&pool, &kb_id, "doc-a", "a-1", 1.5).await;

        // 场景 1：索引文件不存在 → delta 后建立
        index_delta(&pool, &kb_id, "doc-a", &events).await.unwrap();
        let index = HnswIndex::load(&index_path(&kb_id)).unwrap();
        assert!(index.contains_live("a-0") && index.contains_live("a-1"));

        // 场景 2：旧格式索引（节点无 doc_id）→ delta 回退全量重建
        let legacy_items: Vec<(String, String, Vec<f32>)> = vec![
            ("a-0".into(), String::new(), query_vec(0.5)),
            ("a-1".into(), String::new(), query_vec(1.5)),
        ];
        let mut legacy = HnswIndex::new(3, 16, 200, 50);
        legacy.build(&legacy_items);
        legacy.save(&index_path(&kb_id)).unwrap();
        assert!(HnswIndex::load(&index_path(&kb_id))
            .unwrap()
            .is_legacy_format());

        index_delta(&pool, &kb_id, "doc-a", &events).await.unwrap();
        let rebuilt = HnswIndex::load(&index_path(&kb_id)).unwrap();
        assert!(
            !rebuilt.is_legacy_format(),
            "fallback rebuild populates doc ids"
        );
        assert!(rebuilt.contains_live("a-0"));
        assert_eq!(rebuilt.doc_node_ids("doc-a").len(), 2);

        std::fs::remove_file(index_path(&kb_id)).ok();
    }
    #[tokio::test]
    async fn concurrent_deltas_preserve_every_ready_document() {
        let pool = delta_pool().await;
        let events = sink();
        let kb_id = format!("kb-concurrent-{}", uuid::Uuid::new_v4());
        seed_kb(&pool, &kb_id).await;
        seed_doc(&pool, &kb_id, "baseline").await;
        add_chunk(&pool, &kb_id, "baseline", "baseline-chunk", 0.1).await;
        build_index(&pool, &kb_id, &events).await.unwrap();
        let docs: Vec<String> = (0..24).map(|i| format!("doc-{i}")).collect();
        for (i, doc_id) in docs.iter().enumerate() {
            seed_doc(&pool, &kb_id, doc_id).await;
            add_chunk(&pool, &kb_id, doc_id, &format!("chunk-{i}"), i as f32 + 1.0).await;
        }
        let updates = docs
            .iter()
            .map(|doc_id| index_delta(&pool, &kb_id, doc_id, &events));
        for result in futures_util::future::join_all(updates).await {
            result.unwrap();
        }
        let index = HnswIndex::load(&index_path(&kb_id)).unwrap();
        assert_eq!(index.len(), 25);
        for i in 0..24 {
            assert!(index.contains_live(&format!("chunk-{i}")));
        }
        let meta = KbRepository::new(pool.clone())
            .get_index_meta(&kb_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(meta.chunk_count, 25);
        assert_eq!(meta.status, "ready");
        drop_index(&pool, &kb_id).await.unwrap();
    }

    #[tokio::test]
    async fn drop_waits_for_the_same_write_lock_as_build_and_delta() {
        let pool = delta_pool().await;
        let events = sink();
        let kb_id = format!("kb-drop-lock-{}", uuid::Uuid::new_v4());
        seed_kb(&pool, &kb_id).await;
        seed_doc(&pool, &kb_id, "baseline").await;
        add_chunk(&pool, &kb_id, "baseline", "baseline-chunk", 0.1).await;
        // 索引不存在的 delta 回退全量构建，不能重复加锁导致死锁。
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            index_delta(&pool, &kb_id, "baseline", &events),
        )
        .await
        .unwrap()
        .unwrap();
        let guard = index_write_lock(&kb_id).lock_owned().await;
        let mut dropping = Box::pin(drop_index(&pool, &kb_id));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut dropping)
                .await
                .is_err()
        );
        assert!(index_path(&kb_id).exists());
        drop(guard);
        dropping.await.unwrap();
        assert!(!index_path(&kb_id).exists());
    }
}

// ─── C-06/R3：融合模式纯函数测试 ────────────────────────────────────────────
#[cfg(test)]
mod rrf_tests {
    use super::*;

    fn result(chunk_id: &str, score: f32) -> SearchResult {
        SearchResult {
            chunk_id: chunk_id.to_string(),
            doc_id: "d".to_string(),
            filename: "f.md".to_string(),
            content: String::new(),
            score,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn fusion_mode_parses_with_rrf_default() {
        assert_eq!(FusionMode::parse("rrf"), FusionMode::Rrf);
        assert_eq!(FusionMode::parse("RRF"), FusionMode::Rrf);
        assert_eq!(FusionMode::parse("weighted"), FusionMode::Weighted);
        assert_eq!(
            FusionMode::parse("unknown"),
            FusionMode::Rrf,
            "未知值回退默认 RRF"
        );
    }

    /// rag_query 的实际取值表达式（settings → FusionMode）：weighted 可切回、
    /// 未配置走默认。锁定设置存储到融合模式的接线契约。
    #[test]
    fn fusion_mode_settings_expression_matches_rag_query() {
        let dir =
            std::env::temp_dir().join(format!("waliapi-fusion-mode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::settings_store::SettingsStore::file(dir.join("settings.json"));

        // 未配置 → 默认 rrf
        let mode = FusionMode::parse(&store.get_str("kb.fusion_mode", "rrf"));
        assert_eq!(mode, FusionMode::Rrf);

        // 配置 weighted → 可切回
        store
            .set_many(&[("kb.fusion_mode".to_string(), serde_json::json!("weighted"))])
            .unwrap();
        let mode = FusionMode::parse(&store.get_str("kb.fusion_mode", "rrf"));
        assert_eq!(
            mode,
            FusionMode::Weighted,
            "设置项 weighted 必须切回线性加权"
        );
    }

    /// 量纲悬殊两路：向量分数接近 1、关键词分数微小（FTS5 bm25 常态）。
    /// 线性加权下向量路一家独大；RRF 只看排名，两路一致认可的候选（两路都排前）
    /// 应排到第一。
    #[test]
    fn rrf_beats_weighted_when_scales_are_skewed() {
        // 两路一致认可 c_both；向量路单独强推 c_vec_only（分数最高）
        let vector = vec![result("c_vec_only", 0.99), result("c_both", 0.97)];
        let keyword = vec![result("c_both", 0.0007), result("c_kw_only", 0.0005)];

        let weighted = fuse_scored(&vector, &keyword, 3, 0.7, 0.3, FusionMode::Weighted);
        assert_eq!(
            weighted[0].result.chunk_id, "c_vec_only",
            "加权前置条件：向量量纲碾压"
        );

        let rrf = fuse_scored(&vector, &keyword, 3, 0.7, 0.3, FusionMode::Rrf);
        assert_eq!(
            rrf[0].result.chunk_id, "c_both",
            "RRF 下两路都靠前的候选应排第一（排名共识优先于单路高分）"
        );
    }

    #[test]
    fn rrf_scores_are_rank_based_and_deterministic() {
        let vector = vec![result("a", 0.9), result("b", 0.5)];
        let keyword = vec![result("a", 0.1), result("b", 0.05)];
        let fused = fuse_scored(&vector, &keyword, 2, 0.7, 0.3, FusionMode::Rrf);
        // a: 两路 rank1 → 2/(k+1)；b: 两路 rank2 → 2/(k+2)；a > b
        assert_eq!(fused[0].result.chunk_id, "a");
        let expected_a = 1.0 / (60.0 + 1.0) + 1.0 / (60.0 + 1.0);
        assert!((fused[0].result.score - expected_a).abs() < 1e-6);
        // 分数明细保留两路原始值（可视化/调试用）
        assert_eq!(fused[0].vector_score, Some(0.9));
        assert_eq!(fused[0].keyword_score, Some(0.1));
    }

    #[test]
    fn weighted_mode_preserves_historical_behavior() {
        let vector = vec![result("a", 1.0)];
        let keyword = vec![result("b", 1.0)];
        let fused = fuse_scored(&vector, &keyword, 2, 0.7, 0.3, FusionMode::Weighted);
        assert_eq!(fused[0].result.chunk_id, "a");
        assert!((fused[0].result.score - 0.7).abs() < 1e-6);
        assert!((fused[1].result.score - 0.3).abs() < 1e-6);
    }

    #[test]
    fn rrf_truncates_to_top_k_and_handles_empty_lists() {
        let vector: Vec<SearchResult> = vec![result("a", 0.9), result("b", 0.8), result("c", 0.7)];
        assert_eq!(
            fuse_scored(&vector, &[], 2, 0.7, 0.3, FusionMode::Rrf).len(),
            2
        );
        assert!(fuse_scored(&[], &[], 5, 0.7, 0.3, FusionMode::Rrf).is_empty());
    }
}

#[cfg(test)]
mod blocking_cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn dropping_request_stops_its_running_blocking_worker() {
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (stopped_sender, stopped_receiver) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(retrieval_blocking("cancel_test", move |cancel| {
            let _ = started_sender.send(());
            while !cancel.cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let _ = stopped_sender.send(());
            Err::<(), _>("retrieval cancelled".into())
        }));
        tokio::time::timeout(std::time::Duration::from_secs(2), started_receiver)
            .await
            .unwrap()
            .unwrap();
        request.abort();
        let _ = request.await;
        tokio::time::timeout(std::time::Duration::from_secs(2), stopped_receiver)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn expired_stage_never_starts_background_computation() {
        let entered = Arc::new(AtomicBool::new(false));
        let captured = entered.clone();
        let budget = super::super::budget::Budget::new(100, "expired-fixture").stage(
            "retrieval",
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
        );
        let result = budget
            .scope(retrieval_blocking("expired_test", move |_| {
                captured.store(true, Ordering::Relaxed);
                Ok(())
            }))
            .await;
        assert!(result.is_err());
        assert!(!entered.load(Ordering::Relaxed));
    }
}
