use super::index::HnswIndex;
use super::models::SearchResult;
use super::repository::KbRepository;
use crate::server::event_bridge::EventSink;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

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

/// Try to load the HNSW index for a KB. Returns None if not built or incompatible.
fn load_index(kb_id: &str) -> Option<HnswIndex> {
    let path = index_path(kb_id);
    if path.exists() {
        match HnswIndex::load(&path) {
            Ok(index) if index.initialized && !index.is_empty() => {
                // Sanity check: verify that the index nodes have string IDs
                if !index.nodes.is_empty() {
                    // If we successfully loaded and it has nodes, it should be compatible
                    Some(index)
                } else {
                    tracing::warn!("HNSW index for KB {} is empty, skipping", kb_id);
                    None
                }
            }
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(
                    "Failed to load HNSW index for KB {} (likely incompatible old format): {}",
                    kb_id,
                    e
                );
                None
            }
        }
    } else {
        None
    }
}

/// Search knowledge base by query embedding.
/// Uses HNSW index if available, falls back to linear scan.
pub async fn search(
    pool: &SqlitePool,
    kb_id: &str,
    query_embedding: &[f32],
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    let repo = KbRepository::new(pool.clone());

    // Try HNSW index first
    if let Some(index) = load_index(kb_id) {
        if index.dim == query_embedding.len() {
            tracing::debug!("Using HNSW index for KB {} ({} nodes)", kb_id, index.len());
            let hnsw_results = index.search(query_embedding, top_k);

            if !hnsw_results.is_empty() {
                // Load chunks and build ID map
                let chunks = repo
                    .get_chunks_by_kb(kb_id)
                    .await
                    .map_err(|e| format!("Failed to load chunks: {}", e))?;
                tracing::debug!("Loaded {} chunks from DB", chunks.len());

                // Build chunk ID -> chunk data map
                let chunk_map: std::collections::HashMap<String, _> = chunks
                    .into_iter()
                    .map(|(id, content, metadata, emb, filename, doc_id)| {
                        (id, (content, metadata, emb, filename, doc_id))
                    })
                    .collect();

                // Map chunk ID -> chunk data
                let mapped: Vec<SearchResult> = hnsw_results
                    .into_iter()
                    .filter_map(|r| {
                        if let Some((content, metadata, _emb, filename, doc_id)) =
                            chunk_map.get(&r.id)
                        {
                            let meta: serde_json::Value =
                                serde_json::from_str(metadata).unwrap_or(serde_json::json!({}));
                            tracing::debug!("Mapped chunk ID {} to filename {}", r.id, filename);
                            Some(SearchResult {
                                chunk_id: r.id,
                                doc_id: doc_id.clone(),
                                filename: filename.clone(),
                                content: content.clone(),
                                score: r.score,
                                metadata: meta,
                            })
                        } else {
                            tracing::warn!("Failed to map chunk ID {}", r.id);
                            None
                        }
                    })
                    .collect();

                if !mapped.is_empty() {
                    return Ok(mapped);
                }

                tracing::warn!(
                    "HNSW index returned results but mapping failed, falling back to linear scan"
                );
            }
        } else {
            tracing::warn!(
                "HNSW index dim ({}) != query dim ({}) for KB {}, falling back to linear scan",
                index.dim,
                query_embedding.len(),
                kb_id
            );
        }
    }

    // Fallback: linear scan
    linear_search(pool, kb_id, query_embedding, top_k).await
}

/// Linear scan search (original implementation).
async fn linear_search(
    pool: &SqlitePool,
    kb_id: &str,
    query_embedding: &[f32],
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    let repo = KbRepository::new(pool.clone());

    let chunks = repo
        .get_chunks_by_kb(kb_id)
        .await
        .map_err(|e| format!("Failed to load chunks: {}", e))?;
    tracing::debug!("Linear scan: loaded {} chunks", chunks.len());

    if chunks.is_empty() {
        return Ok(vec![]);
    }

    let query_dim = query_embedding.len();

    let mut scored: Vec<(f32, usize, String)> = Vec::with_capacity(chunks.len());
    let mut dim_mismatches = 0;

    for (i, (id, _, _, emb, _, _)) in chunks.iter().enumerate() {
        let vector = decode_embedding(emb);
        if vector.len() != query_dim {
            dim_mismatches += 1;
            continue;
        }
        let score = cosine_similarity(query_embedding, &vector);
        scored.push((score, i, id.clone()));
    }

    if dim_mismatches > 0 {
        tracing::warn!(
            "Skipped {} chunks with mismatched embedding dimensions (expected {}) in KB {}",
            dim_mismatches,
            query_dim,
            kb_id
        );
    }

    if scored.is_empty() {
        return Ok(vec![]);
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);

    let results = scored
        .into_iter()
        .filter_map(|(score, i, _)| {
            let (id, content, metadata, _emb, filename, doc_id) = &chunks[i];
            let meta: serde_json::Value =
                serde_json::from_str(metadata).unwrap_or(serde_json::json!({}));
            Some(SearchResult {
                chunk_id: id.clone(),
                doc_id: doc_id.clone(),
                filename: filename.clone(),
                content: content.clone(),
                score,
                metadata: meta,
            })
        })
        .collect();

    Ok(results)
}

/// Search across all knowledge bases.
/// If mcp_only is true, only search KBs with mcp_enabled = 1.
pub async fn search_all(
    pool: &SqlitePool,
    query_embedding: &[f32],
    top_k: usize,
    mcp_only: bool,
) -> Result<Vec<SearchResult>, String> {
    let repo = KbRepository::new(pool.clone());

    let kbs = repo
        .get_all_kbs()
        .await
        .map_err(|e| format!("Failed to get KBs: {}", e))?;

    let active_kbs: Vec<_> = kbs
        .iter()
        .filter(|kb| kb.status == 1 && (!mcp_only || kb.mcp_enabled == 1))
        .collect();

    if active_kbs.is_empty() {
        return Ok(vec![]);
    }

    let mut all_results = Vec::new();
    for kb in &active_kbs {
        if let Ok(results) = search(pool, &kb.id, query_embedding, top_k).await {
            all_results.extend(results);
        }
    }

    all_results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(top_k);

    Ok(all_results)
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

    let loaded = if path.exists() {
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
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let fts_query = build_fts_query(&tokens);

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
    .bind(&fts_query)
    .bind(top_k as i64)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("FTS5 search failed: {}", e))?;
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

/// Tokenize a query string for FTS5 search.
/// - English/numbers: split by whitespace and punctuation, keep tokens with 2+ chars
/// - Chinese (CJK): extract continuous CJK character runs and generate 2-grams (bigrams)
/// - Mixed: process each segment independently, then merge
fn tokenize_query(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        // Check if CJK character
        let is_cjk = (ch >= '\u{4e00}' && ch <= '\u{9fff}')
            || (ch >= '\u{3400}' && ch <= '\u{4dbf}')
            || (ch >= '\u{f900}' && ch <= '\u{faff}');

        if is_cjk {
            // Collect continuous CJK characters
            let mut cjk_run = Vec::new();
            while i < chars.len() {
                let c = chars[i];
                let cjk = (c >= '\u{4e00}' && c <= '\u{9fff}')
                    || (c >= '\u{3400}' && c <= '\u{4dbf}')
                    || (c >= '\u{f900}' && c <= '\u{faff}');
                if !cjk {
                    break;
                }
                cjk_run.push(c);
                i += 1;
            }

            // Generate bigrams from CJK run
            if cjk_run.len() == 1 {
                // Single CJK char: use as-is
                tokens.push(cjk_run[0].to_string());
            } else {
                for w in cjk_run.windows(2) {
                    tokens.push(format!("{}{}", w[0], w[1]));
                }
            }
        } else {
            // Collect non-CJK characters as a word
            let mut word = String::new();
            while i < chars.len() {
                let c = chars[i];
                let cjk = (c >= '\u{4e00}' && c <= '\u{9fff}')
                    || (c >= '\u{3400}' && c <= '\u{4dbf}')
                    || (c >= '\u{f900}' && c <= '\u{faff}');
                if cjk {
                    break;
                }
                // Split on whitespace and common punctuation
                if c.is_whitespace()
                    || matches!(
                        c,
                        '.' | ','
                            | '!'
                            | '?'
                            | ';'
                            | ':'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '"'
                            | '\''
                            | '`'
                            | '/'
                            | '\\'
                            | '|'
                            | '<'
                            | '>'
                    )
                {
                    break;
                }
                word.push(c);
                i += 1;
            }

            // Only keep tokens with 2+ characters
            if word.chars().count() >= 2 {
                tokens.push(word);
            }

            // Skip whitespace/punctuation separator
            if i < chars.len() && !chars[i].is_alphanumeric() {
                i += 1;
            }
        }
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    tokens
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect()
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
