use crate::db::repository::Repository;
use crate::services::knowledge::{models::*, repository::KbRepository};
use crate::AppState;
use serde::Deserialize;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn get_knowledge_bases(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<KbKnowledgeBase>, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.get_all_kbs().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_knowledge_base(
    state: State<'_, Arc<AppState>>,
    input: CreateKbInput,
) -> Result<KbKnowledgeBase, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.create_kb(&input).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_knowledge_base(
    state: State<'_, Arc<AppState>>,
    id: String,
    input: UpdateKbInput,
) -> Result<KbKnowledgeBase, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.update_kb(&id, &input).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_knowledge_base(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.delete_kb(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_kb_documents(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
) -> Result<Vec<KbDocument>, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.get_documents(&kb_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_kb_document(
    state: State<'_, Arc<AppState>>,
    doc_id: String,
    kb_id: String,
) -> Result<(), String> {
    let repo = KbRepository::new(state.db.pool.clone());
    if let Ok(doc) = repo.get_document(&doc_id).await {
        crate::services::knowledge::upload::remove_managed_file(
            &state.data_dir,
            &doc.kb_id,
            &doc.source_type,
            doc.file_path.as_deref(),
        )?;
        // 级联删除 OCR 页级缓存（以内容哈希为键）
        crate::services::knowledge::ocr::cache::remove_cache(&state.data_dir, &doc.content_hash);
    }
    repo.delete_document(&doc_id)
        .await
        .map_err(|e| e.to_string())?;
    repo.update_kb_counts(&kb_id)
        .await
        .map_err(|e| e.to_string())?;
    // 增量摘除该文档的索引向量（best-effort：失败仅日志，不阻断删除）
    {
        let pool = state.db.pool.clone();
        let events = state.events.clone();
        let kb_id = kb_id.clone();
        let doc_id = doc_id.clone();
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async {
                if let Err(e) = crate::services::knowledge::retriever::index_delta(
                    &pool, &kb_id, &doc_id, &events,
                )
                .await
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
    Ok(())
}

#[tauri::command]
pub async fn reindex_kb_document(
    state: State<'_, Arc<AppState>>,
    doc_id: String,
) -> Result<(), String> {
    let pool = state.db.pool.clone();
    crate::services::knowledge::processor::reindex_document(
        &pool,
        &state.events,
        &doc_id,
        &state.settings,
        &state.data_dir,
    )
    .await
    .map_err(|e| e)
}

#[tauri::command]
pub async fn get_kb_tags(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
    #[allow(unused_variables)] limit: Option<usize>,
) -> Result<Vec<KbTag>, String> {
    let pool = &state.db.pool;
    let limit = limit.unwrap_or(15);

    // Sample chunk contents for keyword extraction
    let chunks: Vec<(String,)> =
        sqlx::query_as("SELECT content FROM kb_chunks WHERE kb_id = ? ORDER BY RANDOM() LIMIT 200")
            .bind(&kb_id)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?;

    if chunks.is_empty() {
        return Ok(vec![]);
    }

    // Simple word frequency analysis
    let mut word_freq: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // Common stopwords (Chinese + English + code)
    const STOPWORDS: &[&str] = &[
        // English
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "must", "shall",
        "can", "need", "of", "to", "in", "for", "on", "at", "by", "with", "from", "as", "into",
        "through", "during", "before", "after", "above", "below", "up", "down", "out", "off",
        "over", "under", "again", "further", "then", "once", "here", "there", "when", "where",
        "why", "how", "all", "each", "every", "both", "few", "more", "most", "other", "some",
        "such", "no", "nor", "not", "only", "own", "same", "so", "than", "too", "very", "just",
        "also", "if", "or", "and", "but", // Code / tech common
        "function", "return", "const", "let", "var", "class", "import", "export", "default", "pub",
        "fn", "use", "mod", "struct", "impl", "self", "crate", "async", "await", "type", "enum",
        "true", "false", "null", "none", "some", "ok", "err", "string", "vec", "option", "result",
        // Chinese
        "的", "了", "在", "是", "我", "有", "和", "就", "不", "人", "都", "一", "一个", "上", "也",
        "很", "到", "说", "要", "去", "你", "会", "着", "没有", "看", "好", "自己", "这", "那",
        "与", "或", "但", "而", "且", "则", "于", "以", "及", "为", "可", "能", "对", "中", "等",
        "使", "其", "之", "所",
    ];

    let stopword_set: std::collections::HashSet<&str> = STOPWORDS.iter().copied().collect();

    for (content,) in &chunks {
        // Extract words: English words (2+ chars), Chinese bigrams
        let chars: Vec<char> = content.chars().collect();

        // English words
        let mut current_word = String::new();
        for &ch in &chars {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                current_word.push(ch);
            } else {
                if current_word.len() >= 4 {
                    let word_lower = current_word.to_lowercase();
                    if !stopword_set.contains(word_lower.as_str()) {
                        *word_freq.entry(word_lower).or_insert(0) += 1;
                    }
                }
                current_word.clear();
            }
        }
        if current_word.len() >= 4 {
            let word_lower = current_word.to_lowercase();
            if !stopword_set.contains(word_lower.as_str()) {
                *word_freq.entry(word_lower).or_insert(0) += 1;
            }
        }

        // Chinese bigrams (2-char sequences of CJK characters)
        let mut prev_cjk: Option<char> = None;
        for &ch in &chars {
            let is_cjk =
                (ch >= '\u{4e00}' && ch <= '\u{9fff}') || (ch >= '\u{3400}' && ch <= '\u{4dbf}');
            if is_cjk {
                if let Some(prev) = prev_cjk {
                    let bigram = format!("{}{}", prev, ch);
                    // Filter out bigrams where both chars are common stopwords
                    let prev_s = prev.to_string();
                    let ch_s = ch.to_string();
                    if !stopword_set.contains(prev_s.as_str())
                        && !stopword_set.contains(ch_s.as_str())
                    {
                        *word_freq.entry(bigram).or_insert(0) += 1;
                    }
                }
                prev_cjk = Some(ch);
            } else {
                prev_cjk = None;
            }
        }
    }

    // Sort by frequency and take top N
    let mut freq_vec: Vec<(String, usize)> = word_freq.into_iter().collect();
    freq_vec.sort_by(|a, b| b.1.cmp(&a.1));

    let tags: Vec<KbTag> = freq_vec
        .into_iter()
        .take(limit)
        .map(|(word, count)| KbTag { word, count })
        .collect();

    Ok(tags)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct KbTag {
    pub word: String,
    pub count: usize,
}

#[derive(Debug, Deserialize)]
pub struct KbSearchInput {
    pub query: String,
    pub kb_id: Option<String>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub vector_weight: Option<f32>,
    #[serde(default)]
    pub keyword_weight: Option<f32>,
    #[serde(default)]
    pub search_mode: Option<String>,
}

fn default_top_k() -> usize {
    5
}

#[tauri::command]
pub async fn search_knowledge_base(
    state: State<'_, Arc<AppState>>,
    input: KbSearchInput,
) -> Result<Vec<SearchResult>, String> {
    let pool = &state.db.pool;
    let repo = Repository::new(pool.clone());

    let emb_model = if let Some(kb_id) = &input.kb_id {
        let kb_repo = KbRepository::new(pool.clone());
        kb_repo
            .get_kb(kb_id)
            .await
            .ok()
            .and_then(|kb| kb.embedding_model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string())
    } else {
        "text-embedding-3-small".to_string()
    };

    let embeddings =
        crate::services::knowledge::embedder::embed(&[input.query.clone()], &emb_model, &repo)
            .await
            .map_err(|e| e)?;

    if embeddings.is_empty() {
        return Err("Failed to embed query".to_string());
    }

    let results = if let Some(kb_id) = &input.kb_id {
        crate::services::knowledge::retriever::search(pool, kb_id, &embeddings[0], input.top_k)
            .await
    } else {
        crate::services::knowledge::retriever::search_all(pool, &embeddings[0], input.top_k, false)
            .await
    };

    results.map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct KbAskInput {
    pub question: String,
    pub kb_id: Option<String>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default = "default_chat_model")]
    pub model: String,
    #[serde(default)]
    pub history: Option<Vec<ConversationMessage>>,
    #[serde(default)]
    pub deep_research: bool,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
    #[serde(default)]
    pub vector_weight: Option<f32>,
    #[serde(default)]
    pub keyword_weight: Option<f32>,
    #[serde(default)]
    pub search_mode: Option<String>,
}

fn default_chat_model() -> String {
    "gpt-4o".to_string()
}
fn default_max_rounds() -> usize {
    5
}

#[tauri::command]
pub async fn ask_knowledge_base(
    state: State<'_, Arc<AppState>>,
    input: KbAskInput,
) -> Result<RagAnswer, String> {
    let pool = &state.db.pool;
    let kb_id = input.kb_id.clone().unwrap_or_default();

    let emb_model = if !kb_id.is_empty() {
        let kb_repo = KbRepository::new(pool.clone());
        kb_repo
            .get_kb(&kb_id)
            .await
            .ok()
            .and_then(|kb| kb.embedding_model)
            .unwrap_or_else(|| "text-embedding-3-small".to_string())
    } else {
        "text-embedding-3-small".to_string()
    };

    if input.deep_research && !kb_id.is_empty() {
        crate::services::knowledge::rag::deep_research(
            pool,
            &kb_id,
            &input.question,
            &emb_model,
            &input.model,
            input.top_k,
            input.max_rounds,
            &state.settings,
        )
        .await
        .map_err(|e| e)
    } else {
        let history = input.history.unwrap_or_default();
        let vector_weight = input.vector_weight.unwrap_or(0.7);
        let keyword_weight = input.keyword_weight.unwrap_or(0.3);
        let search_mode = input.search_mode.as_deref().unwrap_or("hybrid");
        crate::services::knowledge::rag::ask_with_config(
            pool,
            &kb_id,
            &input.question,
            &emb_model,
            &input.model,
            input.top_k,
            false,
            &history,
            &state.settings,
            vector_weight,
            keyword_weight,
            search_mode,
        )
        .await
        .map_err(|e| e)
    }
}

#[tauri::command]
pub async fn get_kb_stats(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
) -> Result<serde_json::Value, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    let kb = repo.get_kb(&kb_id).await.map_err(|e| e.to_string())?;
    let docs = repo.get_documents(&kb_id).await.unwrap_or_default();
    let ready = docs.iter().filter(|d| d.status == "ready").count();
    let processing = docs.iter().filter(|d| d.status == "processing").count();
    let failed = docs.iter().filter(|d| d.status == "failed").count();

    let index_meta = repo.get_index_meta(&kb_id).await.ok().flatten();

    Ok(serde_json::json!({
        "kb": kb,
        "documents": {
            "total": docs.len(),
            "ready": ready,
            "processing": processing,
            "failed": failed,
        },
        "index": index_meta,
    }))
}

#[derive(Debug, Deserialize)]
pub struct UploadDocInput {
    pub kb_id: String,
    pub filename: String,
    pub content: String, // base64 encoded
}

#[tauri::command]
pub async fn upload_kb_document(
    state: State<'_, Arc<AppState>>,
    input: UploadDocInput,
) -> Result<KbDocument, String> {
    use sha2::Digest;

    let pool = &state.db.pool;
    let repo = KbRepository::new(pool.clone());

    let content =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &input.content)
            .map_err(|e| format!("Invalid base64: {}", e))?;

    let hash = sha2::Sha256::digest(&content);
    let hash_hex = hex::encode(hash);

    if let Ok(Some(_)) = repo.find_document_by_hash(&input.kb_id, &hash_hex).await {
        return Err("Document with same content already exists".to_string());
    }

    let file_type = crate::services::knowledge::parser::get_file_type(&input.filename);
    let file_size = content.len() as i64;

    // 落盘路径完全由服务端生成（uuid + 白名单扩展名），原始文件名仅入库展示；
    // kb_id 先过白名单，杜绝路径穿越（FIX-02，见 services::knowledge::upload）
    let doc_id = uuid::Uuid::new_v4().to_string();
    let file_path = crate::services::knowledge::upload::storage_path(
        &state.data_dir,
        &input.kb_id,
        &doc_id,
        &input.filename,
    )?;
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建库目录失败: {e}"))?;
    }
    std::fs::write(&file_path, &content).map_err(|e| format!("写入文件失败: {e}"))?;
    let file_path_str = file_path.to_string_lossy().to_string();

    let doc = repo
        .create_document(
            &input.kb_id,
            &input.filename,
            Some(&file_path_str),
            &file_type,
            file_size,
            &hash_hex,
        )
        .await
        .map_err(|e| e.to_string())?;

    let kb = repo.get_kb(&input.kb_id).await.map_err(|e| e.to_string())?;
    let emb_model = kb.embedding_model.clone();

    let pool_clone = pool.clone();
    let events_clone = state.events.clone();
    let doc_id_clone = doc.id.clone();
    let filename_clone = input.filename.clone();
    let settings_clone = state.settings.clone();
    let data_dir_clone = state.data_dir.clone();

    tokio::spawn(async move {
        if let Err(e) = crate::services::knowledge::processor::process_document(
            &pool_clone,
            &events_clone,
            &input.kb_id,
            &doc_id_clone,
            &filename_clone,
            &content,
            emb_model.as_deref(),
            &settings_clone,
            &data_dir_clone,
        )
        .await
        {
            tracing::error!("Document processing failed: {}", e);
        }
    });

    Ok(doc)
}

// ════════════════════════════════════════════════════════
// New commands: Conversations, Sources, Index, Import
// ════════════════════════════════════════════════════════

#[tauri::command]
pub async fn get_kb_conversations(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
) -> Result<Vec<KbConversation>, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.get_conversations(&kb_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clear_kb_conversations(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
) -> Result<(), String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.clear_conversations(&kb_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_kb_sources(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
) -> Result<Vec<KbSource>, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.get_sources(&kb_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_kb_source(
    state: State<'_, Arc<AppState>>,
    source_id: String,
    kb_id: String,
) -> Result<(), String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.delete_source(&source_id)
        .await
        .map_err(|e| e.to_string())?;
    repo.update_kb_counts(&kb_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn import_kb_source(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
    input: ImportSourceInput,
) -> Result<KbSource, String> {
    let pool = state.db.pool.clone();
    let events = state.events.clone();
    let repo = KbRepository::new(pool.clone());

    let source = repo
        .create_source(
            &kb_id,
            &input.source_type,
            input.repo_url.as_deref().or(input.url.as_deref()),
            input.dir_path.as_deref(),
            input.branch.as_deref(),
        )
        .await
        .map_err(|e| e.to_string())?;

    let source_id = source.id.clone();
    let source_type = input.source_type.clone();
    let settings = state.settings.clone();
    let data_dir = state.data_dir.clone();

    tokio::spawn(async move {
        let result = if source_type == "git" {
            crate::services::knowledge::importer::import_git_repo(
                &pool, &events, &kb_id, &source_id, &input, &settings, &data_dir,
            )
            .await
        } else if source_type == "url" {
            crate::services::knowledge::importer::import_url(
                &pool, &events, &kb_id, &source_id, &input, &settings, &data_dir,
            )
            .await
        } else if source_type == "local_dir" {
            crate::services::knowledge::importer::import_local_dir(
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

    Ok(source)
}

#[tauri::command]
pub async fn get_kb_index_status(
    state: State<'_, Arc<AppState>>,
    kb_id: String,
) -> Result<Option<KbIndexMeta>, String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.get_index_meta(&kb_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn build_kb_index(state: State<'_, Arc<AppState>>, kb_id: String) -> Result<(), String> {
    let pool = state.db.pool.clone();
    let events = state.events.clone();

    // Update status to building immediately
    let repo = KbRepository::new(pool.clone());
    repo.update_kb_index_status(&kb_id, "building").await.ok();

    // Spawn the actual HNSW index build
    tokio::spawn(async move {
        let kb_id_clone = kb_id.clone();

        // Emit starting event
        events.emit(
            "kb-index-progress",
            serde_json::json!({
                "kb_id": &kb_id_clone,
                "status": "building",
                "progress": 0,
                "current": 0,
                "total": 0,
                "message": "正在加载切片数据…"
            }),
        );

        match crate::services::knowledge::retriever::build_index(&pool, &kb_id_clone, &events).await
        {
            Ok(()) => {
                tracing::info!("HNSW index built successfully for KB {}", kb_id_clone);
                events.emit(
                    "kb-index-progress",
                    serde_json::json!({
                        "kb_id": &kb_id_clone,
                        "status": "ready",
                        "progress": 100,
                        "message": "索引构建完成"
                    }),
                );
            }
            Err(e) => {
                tracing::error!("Failed to build HNSW index for KB {}: {}", kb_id_clone, e);
                let repo = KbRepository::new(pool.clone());
                repo.update_kb_index_status(&kb_id_clone, "error")
                    .await
                    .ok();
                events.emit(
                    "kb-index-progress",
                    serde_json::json!({
                        "kb_id": &kb_id_clone,
                        "status": "error",
                        "message": format!("索引构建失败: {}", e)
                    }),
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn drop_kb_index(state: State<'_, Arc<AppState>>, kb_id: String) -> Result<(), String> {
    let repo = KbRepository::new(state.db.pool.clone());
    repo.upsert_index_meta(&kb_id, 0, 0, None, "none")
        .await
        .map_err(|e| e.to_string())?;
    repo.update_kb_index_status(&kb_id, "none")
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ════════════════════════════════════════════════════════
// OCR 页级缓存管理（设置页「OCR」分组使用）
// ════════════════════════════════════════════════════════

/// OCR 缓存占用信息（{data_dir}/ocr_cache/ 下每个子目录对应一篇文档）
#[derive(Debug, Clone, serde::Serialize)]
pub struct OcrCacheInfo {
    pub total_bytes: u64,
    pub doc_count: u64,
}

#[tauri::command]
pub async fn get_ocr_cache_info(state: State<'_, Arc<AppState>>) -> Result<OcrCacheInfo, String> {
    let dir = state.data_dir.join("ocr_cache");
    let mut info = OcrCacheInfo {
        total_bytes: 0,
        doc_count: 0,
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(info);
    };
    for entry in entries.flatten() {
        let doc_dir = entry.path();
        if !doc_dir.is_dir() {
            continue;
        }
        info.doc_count += 1;
        // 递归累加单文档缓存目录大小
        let mut stack = vec![doc_dir];
        while let Some(dir) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if let Ok(m) = e.metadata() {
                        info.total_bytes += m.len();
                    }
                }
            }
        }
    }
    Ok(info)
}

#[tauri::command]
pub async fn clear_ocr_cache(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let dir = state.data_dir.join("ocr_cache");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(())
}
