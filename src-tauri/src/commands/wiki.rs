use crate::AppState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tauri::State;

// Re-export models for convenience
pub use crate::services::wiki::models::*;

// ── Wiki Project commands ──

#[tauri::command]
pub async fn get_wiki_projects(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<WikiProject>, String> {
    let pool = state.db.pool.clone();
    let rows =
        sqlx::query_as::<_, WikiProject>("SELECT * FROM wiki_projects ORDER BY created_at DESC")
            .fetch_all(&pool)
            .await
            .map_err(|e| format!("DB error: {}", e))?;
    Ok(rows)
}

#[tauri::command]
pub async fn create_wiki_project(
    state: State<'_, Arc<AppState>>,
    input: CreateProjectInput,
) -> Result<WikiProject, String> {
    let pool = state.db.pool.clone();
    let project_id = crate::services::wiki::project::new_uuid();
    let schema = input
        .schema_text
        .clone()
        .unwrap_or_else(|| crate::services::wiki::repository::DEFAULT_SCHEMA.to_string());

    // Create directory structure
    crate::services::wiki::project::init_project_dir(&project_id, &schema).await?;

    let dir = crate::services::wiki::project::project_wiki_dir(&project_id);
    let wiki_dir = dir.to_string_lossy().to_string();

    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.create_project(&input, &wiki_dir).await
}

#[tauri::command]
pub async fn get_wiki_project(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<WikiProject, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.get_project(&id).await
}

#[tauri::command]
pub async fn update_wiki_project(
    state: State<'_, Arc<AppState>>,
    id: String,
    input: UpdateProjectInput,
) -> Result<WikiProject, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);

    if let Some(ref schema) = input.schema_text {
        let dir = crate::services::wiki::project::project_wiki_dir(&id);
        let schema_path = dir.join("schema").join("CLAUDE.md");
        let _ = tokio::fs::write(schema_path, schema).await;
    }

    repo.update_project(&id, &input).await
}

#[tauri::command]
pub async fn delete_wiki_project(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.delete_project(&id).await?;
    crate::services::wiki::project::remove_project_dir(&id).await?;
    Ok(())
}

// ── Wiki Pages ──

#[tauri::command]
pub async fn get_wiki_pages(
    state: State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<Vec<WikiPage>, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.list_pages(&project_id).await
}

#[tauri::command]
pub async fn delete_wiki_page(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    path: String,
) -> Result<(), String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.delete_page(&project_id, &path).await?;
    // 同时删除磁盘上的页面文件（保留目录结构）
    let dir = crate::services::wiki::project::project_wiki_dir(&project_id);
    if let Ok(rel) = crate::services::wiki::project::safe_relative_path(&path) {
        let _ = tokio::fs::remove_file(dir.join(rel)).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_wiki_page(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    path: String,
) -> Result<serde_json::Value, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);

    if let Ok(Some(page)) = repo.get_page(&project_id, &path).await {
        if let Ok(content) = crate::services::wiki::project::read_page(&project_id, &path).await {
            return Ok(serde_json::json!({
                "id": page.id,
                "project_id": page.project_id,
                "path": page.path,
                "title": page.title,
                "page_type": page.page_type,
                "content": content,
                "wikilinks": page.wikilinks,
                "frontmatter": page.frontmatter,
                "status": page.status,
                "created_at": page.created_at,
                "updated_at": page.updated_at,
            }));
        }
    }

    // Try reading from disk
    match crate::services::wiki::project::read_page(&project_id, &path).await {
        Ok(content) => {
            let title = path
                .split('/')
                .last()
                .unwrap_or(&path)
                .trim_end_matches(".md")
                .to_string();
            Ok(serde_json::json!({
                "path": path,
                "title": title,
                "content": content,
                "page_type": "unknown",
            }))
        }
        Err(e) => Err(e),
    }
}

#[tauri::command]
pub async fn save_wiki_page(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    path: String,
    content: String,
) -> Result<(), String> {
    let pool = state.db.pool.clone();
    crate::services::wiki::project::write_page(&project_id, &path, &content).await?;

    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let title = path
        .split('/')
        .last()
        .unwrap_or(&path)
        .trim_end_matches(".md")
        .to_string();
    let page_type = if path.contains("entities/") {
        "entity"
    } else if path.contains("concepts/") {
        "concept"
    } else if path.contains("summaries/") {
        "summary"
    } else if path.ends_with("index.md") {
        "index"
    } else if path.ends_with("log.md") {
        "log"
    } else {
        "entity"
    };
    let token_count = (content.len() / 4) as i64;
    let _ = repo
        .upsert_page(
            &project_id,
            &path,
            &title,
            page_type,
            &hash,
            token_count,
            "[]",
            "{}",
            "[]",
        )
        .await;
    let _ = crate::services::wiki::project::append_log(&project_id, &format!("update | {}", path))
        .await;
    Ok(())
}

// ── Wiki Sources ──

#[tauri::command]
pub async fn get_wiki_sources(
    state: State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<Vec<WikiSource>, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.list_sources(&project_id).await
}

#[tauri::command]
pub async fn add_wiki_source(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    input: AddSourceInput,
) -> Result<WikiSource, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);

    let content_bytes = input
        .content
        .as_deref()
        .map(|c| crate::services::wiki::project::resolve_source_content(&input.filename, c));

    let content_hash = content_bytes.as_ref().map(|b| {
        let mut hasher = Sha256::new();
        hasher.update(b);
        format!("{:x}", hasher.finalize())
    });
    let file_size = content_bytes.as_ref().map(|b| b.len() as i64).unwrap_or(0);

    if let Some(ref content) = content_bytes {
        crate::services::wiki::project::write_source_file(&project_id, &input.filename, content)
            .await?;
    }

    repo.add_source(&project_id, &input, content_hash.as_deref(), file_size)
        .await
}

/// 从本地文件路径导入 Wiki 源资料（桌面端拖放上传用：tauri 拖放事件只给路径，不给文件内容）。
/// 后端直接读原始字节落盘 —— 文本与 PDF 统一按字节存，摄入时按类型读取，避免前端 base64 往返。
#[tauri::command]
pub async fn add_wiki_source_from_path(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    path: String,
) -> Result<WikiSource, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);

    let filename = std::path::Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "无效的文件路径".to_string())?
        .to_string();
    let ext = filename.rsplit('.').next().unwrap_or("txt").to_lowercase();

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("读取文件失败: {}", e))?;

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = format!("{:x}", hasher.finalize());
    let file_size = bytes.len() as i64;

    crate::services::wiki::project::write_source_file(&project_id, &filename, &bytes).await?;

    let input = AddSourceInput {
        source_type: ext,
        filename,
        file_path: None,
        source_url: None,
        content: None,
    };
    repo.add_source(&project_id, &input, Some(&hash), file_size)
        .await
}

#[tauri::command]
pub async fn delete_wiki_source(
    state: State<'_, Arc<AppState>>,
    source_id: String,
) -> Result<(), String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.delete_source(&source_id).await
}

// ── Wiki Search ──

#[tauri::command]
pub async fn search_wiki(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    query: String,
    top_k: Option<usize>,
) -> Result<Vec<WikiSearchResult>, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.search_pages(&project_id, &query, top_k.unwrap_or(10))
        .await
}

// ── Wiki Graph ──

#[tauri::command]
pub async fn get_wiki_graph(
    state: State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<GraphData, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.get_graph(&project_id).await
}

// ── Wiki Tags ──

#[tauri::command]
pub async fn get_wiki_tags(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    limit: Option<usize>,
) -> Result<Vec<WikiTag>, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.get_tags(&project_id, limit.unwrap_or(15)).await
}

// ── Wiki Stats ──

#[tauri::command]
pub async fn get_wiki_stats(
    state: State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<serde_json::Value, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.get_stats(&project_id).await
}

// ── Wiki Ingest ──

#[tauri::command]
pub async fn ingest_wiki_source(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    source_id: String,
) -> Result<serde_json::Value, String> {
    let pool = state.db.pool.clone();
    crate::services::wiki::ingest::ingest_source(
        &state.events,
        &state.settings,
        &pool,
        &project_id,
        &source_id,
    )
    .await
    .map(|r| {
        serde_json::json!({
            "status": "done",
            "pages_created": r.pages_created,
            "page_paths": r.page_paths,
        })
    })
    .map_err(|e| {
        let pool_clone = pool.clone();
        let sid = source_id.clone();
        let pid = project_id.clone();
        let err = e.clone();
        let events = state.events.clone();
        let cancelled = err.starts_with("CANCELLED");
        tokio::spawn(async move {
            let repo = crate::services::wiki::repository::WikiRepository::new(pool_clone);
            if cancelled {
                // 用户主动停止：来源落 cancelled（而非 failed），便于区分
                let _ = repo.update_source_status(&sid, "cancelled", 0, None).await;
            } else {
                let _ = repo
                    .update_source_status(&sid, "failed", 0, Some(&err))
                    .await;
            }
            events.emit(
                "wiki-source-progress",
                serde_json::json!({
                    "source_id": sid,
                    "project_id": pid,
                    "filename": "",
                    "stage": if cancelled { "cancelled" } else { "error" },
                    "progress": 0,
                    "detail": &err,
                }),
            );
        });
        e
    })
}

/// 取消指定来源的摄入任务（停止按钮）。
/// 将 queue 中 running/pending 的任务置 cancelled；摄入循环检查到后尽快终止，
/// 不再继续 OCR 与 LLM 生成。来源状态同时落 cancelled。
#[tauri::command]
pub async fn cancel_wiki_ingest(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    source_id: String,
) -> Result<serde_json::Value, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool);
    repo.cancel_source_ingest(&source_id).await?;
    let _ = repo
        .update_source_status(&source_id, "cancelled", 0, None)
        .await;
    let _ = state.events.emit(
        "wiki-source-progress",
        serde_json::json!({
            "source_id": source_id,
            "project_id": project_id,
            "filename": "",
            "stage": "cancelled",
            "progress": 0,
            "detail": "已停止处理",
        }),
    );
    Ok(serde_json::json!({ "status": "cancelled" }))
}

#[tauri::command]
pub async fn rescan_wiki_sources(
    state: State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<serde_json::Value, String> {
    let pool = state.db.pool.clone();
    let repo = crate::services::wiki::repository::WikiRepository::new(pool.clone());

    let sources = repo.list_sources(&project_id).await?;
    let pending: Vec<_> = sources.iter().filter(|s| s.status == "pending").collect();
    let mut results = Vec::new();

    for source in &pending {
        match crate::services::wiki::ingest::ingest_source(
            &state.events,
            &state.settings,
            &pool,
            &project_id,
            &source.id,
        )
        .await
        {
            Ok(r) => results.push(serde_json::json!({
                "source_id": source.id,
                "filename": source.filename,
                "status": "done",
                "pages": r.pages_created,
            })),
            Err(e) => results.push(serde_json::json!({
                "source_id": source.id,
                "filename": source.filename,
                "status": "failed",
                "error": e,
            })),
        }
    }

    Ok(serde_json::json!({
        "status": "done",
        "processed": pending.len(),
        "results": results,
    }))
}

// ── Wiki 项目备份：导出 / 导入 ──────────────────────────────────────────────

/// 导出指定 Wiki 项目为 zip 备份到 `target_dir`（本地目录）。
#[tauri::command]
pub async fn export_wiki_project(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    target_dir: String,
) -> Result<crate::services::wiki::backup::WikiExportResult, String> {
    crate::services::wiki::backup::export_project(&state.db.pool, &project_id, &target_dir).await
}

/// 从本地 zip 备份文件导入（桌面端：对话框选中文件后传路径）。
#[tauri::command]
pub async fn import_wiki_backup(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<crate::services::wiki::backup::WikiImportResult, String> {
    crate::services::wiki::backup::import_backup_from_path(&state.db.pool, &path).await
}

/// 从 zip 备份字节导入（Web 端：浏览器选中文件读为 base64 传入）。
#[tauri::command]
pub async fn import_wiki_backup_bytes(
    state: State<'_, Arc<AppState>>,
    base64: String,
) -> Result<crate::services::wiki::backup::WikiImportResult, String> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &base64)
        .map_err(|e| format!("备份文件解码失败: {e}"))?;
    crate::services::wiki::backup::import_backup_from_bytes(&state.db.pool, &bytes).await
}

/// 打开目录选择对话框，返回选中的导出目录路径（仅桌面端；Web 端直接填服务器路径）。
#[tauri::command]
pub async fn pick_wiki_export_dir(app: tauri::AppHandle) -> Result<Option<String>, String> {
    #[cfg(not(feature = "desktop-ui"))]
    {
        let _ = &app;
        Err("目录选择仅桌面版可用，请在输入框中直接填写服务器路径".to_string())
    }
    #[cfg(feature = "desktop-ui")]
    {
        use tauri_plugin_dialog::DialogExt;

        let (tx, rx) = tokio::sync::oneshot::channel();
        app.dialog()
            .file()
            .pick_folder(move |folder_path| {
                let result = folder_path.and_then(|f| {
                    f.into_path()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string())
                });
                let _ = tx.send(result);
            });
        let result = rx.await.map_err(|_| "对话框取消".to_string())?;
        Ok(result)
    }
}

/// 打开文件选择对话框，返回选中的 zip 备份路径（仅桌面端；Web 端用浏览器文件选择器）。
#[tauri::command]
pub async fn pick_wiki_import_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    #[cfg(not(feature = "desktop-ui"))]
    {
        let _ = &app;
        Err("文件选择仅桌面版可用，请在浏览器中选择备份文件".to_string())
    }
    #[cfg(feature = "desktop-ui")]
    {
        use tauri_plugin_dialog::DialogExt;

        let (tx, rx) = tokio::sync::oneshot::channel();
        app.dialog()
            .file()
            .add_filter("Wiki 备份", &["zip"])
            .pick_file(move |file_path| {
                let result = file_path.and_then(|f| {
                    f.into_path()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string())
                });
                let _ = tx.send(result);
            });
        let result = rx.await.map_err(|_| "对话框取消".to_string())?;
        Ok(result)
    }
}

/// 重建 Wiki 目录：mode = "program"（程序化分组，零 LLM、毫秒级）| "llm"（语义归组，耗一次 LLM）。
/// 两种模式都会先做 log 一致性检测 + 一次性回溯（老项目 log 缺失时修复）。
#[tauri::command]
pub async fn rebuild_wiki_index(
    state: State<'_, Arc<AppState>>,
    project_id: String,
    mode: String,
    model: Option<String>,
) -> Result<serde_json::Value, String> {
    use crate::services::wiki::rebuild;
    let pool = state.db.pool.clone();
    let log_rebuilt = rebuild::ensure_log_rebuilt(&pool, &project_id).await?;
    let page_count = match mode.as_str() {
        "llm" => rebuild::rebuild_index_with_llm(
            &state.settings,
            &pool,
            &project_id,
            model.as_deref(),
        )
        .await?,
        _ => rebuild::rebuild_index_programmatic(&pool, &project_id).await?,
    };
    Ok(serde_json::json!({
        "mode": if mode == "llm" { "llm" } else { "program" },
        "page_count": page_count,
        "log_rebuilt": log_rebuilt,
    }))
}
