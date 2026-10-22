use super::models::*;
use super::project;
use super::repository::WikiRepository;
use crate::core::proxy;
use crate::db::repository::Repository;
use crate::server::event_bridge::EventSink;
use crate::settings_store::SettingsStore;
use crate::utils::text::truncate_utf8;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MAX_SOURCE_CONTEXT_BYTES: usize = 24_000;
const SOURCE_CONTEXT_TRUNCATION_MARKER: &str = "\n\n[... content truncated ...]";

/// Ingest a source file: read → parse → generate wiki pages via LLM → write to disk+DB.
///
/// FIX-22：失败收敛入口——此前任一步骤 `?` 直接返回，任务行永久停留在
/// running、来源停留在 pending；现在失败统一把两者落 failed 再返回错误。
pub async fn ingest_source(
    events: &EventSink,
    settings: &SettingsStore,
    pool: &sqlx::SqlitePool,
    project_id: &str,
    source_id: &str,
) -> Result<IngestResult, String> {
    let repo = WikiRepository::new(pool.clone());
    let task_id = repo
        .create_task(project_id, Some(source_id), "ingest")
        .await?;
    match ingest_source_inner(events, settings, pool, project_id, source_id, &task_id).await {
        Ok(result) => Ok(result),
        Err(e) => {
            let _ = repo
                .update_task_status(&task_id, "failed", 0, 0, 3, None, Some(&e))
                .await;
            let _ = repo
                .update_source_status(source_id, "failed", 0, Some(&e))
                .await;
            emit_wiki_progress(events, source_id, project_id, "", "error", 0, &e);
            Err(e)
        }
    }
}

async fn ingest_source_inner(
    events: &EventSink,
    settings: &SettingsStore,
    pool: &sqlx::SqlitePool,
    project_id: &str,
    source_id: &str,
    task_id: &str,
) -> Result<IngestResult, String> {
    let repo = WikiRepository::new(pool.clone());
    let db_repo = Arc::new(Repository::new(pool.clone()));

    // 1. Load source record
    let sources = repo.list_sources(project_id).await?;
    let source = sources
        .iter()
        .find(|s| s.id == source_id)
        .ok_or_else(|| format!("Source {} not found", source_id))?;

    // 2. Update task status
    repo.update_task_status(task_id, "running", 0, 0, 3, None, None)
        .await?;
    emit_wiki_progress(
        events,
        source_id,
        project_id,
        &source.filename,
        "processing",
        0,
        "准备摄入",
    );

    // 3. Read source file content.
    //    PDF 走「文字层 + 扫描/图文页 VLM OCR + 整页原稿图引用」的页级混合提取，
    //    其余类型按文本读取（历史行为不变）。
    let source_filename_owned = source.filename.clone();
    let is_pdf = source_filename_owned
        .rsplit('.')
        .next()
        .map(|ext| ext.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false);
    let content = if is_pdf {
        let raw = if let Some(ref file_path) = source.file_path {
            tokio::fs::read(file_path)
                .await
                .map_err(|e| format!("Failed to read source file: {}", e))?
        } else {
            project::read_source_file(project_id, &source_filename_owned)
                .await
                .map_err(|e| format!("Failed to read source: {}", e))?
        };
        extract_pdf_for_wiki(
            events,
            settings,
            pool,
            project_id,
            source_id,
            &source_filename_owned,
            &raw,
        )
        .await?
    } else if let Some(ref file_path) = source.file_path {
        tokio::fs::read_to_string(file_path)
            .await
            .map_err(|e| format!("Failed to read source file: {}", e))?
    } else {
        let raw = project::read_source_file(project_id, &source_filename_owned)
            .await
            .map_err(|e| format!("Failed to read source: {}", e))?;
        String::from_utf8_lossy(&raw).to_string()
    };

    let source_filename = &source_filename_owned;
    let file_ext = if is_pdf {
        // 提取结果是 Markdown，按标题分段交给生成器
        "md".to_string()
    } else {
        source_filename
            .rsplit('.')
            .next()
            .unwrap_or("txt")
            .to_lowercase()
    };

    // 4. Parse content into sections/chunks
    repo.update_task_status(task_id, "running", 10, 0, 3, None, None)
        .await?;
    emit_wiki_progress(
        events,
        source_id,
        project_id,
        &source.filename,
        "parsing",
        10,
        "解析文档",
    );
    let sections = parse_content(&content, &file_ext);

    // 5. Get project config for LLM
    let proj = repo.get_project(project_id).await?;
    let ingest_model = proj.ingest_model.as_deref().unwrap_or("gpt-4o");
    let ingest_channel_id = match proj.ingest_channel_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => {
            // Fallback: find first active channel from DB
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM channels WHERE status = 1 ORDER BY priority DESC LIMIT 1",
            )
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("DB error: {}", e))?;
            let id = row.map(|(id,)| id).ok_or_else(|| "No active channel configured. Please create a channel first or set ingest_channel_id in Wiki project settings.".to_string())?;
            id
        }
    };

    // 6. Generate wiki pages via LLM
    repo.update_task_status(task_id, "running", 30, 1, 3, None, None)
        .await?;
    emit_wiki_progress(
        events,
        source_id,
        project_id,
        &source.filename,
        "generating",
        30,
        "LLM 生成页面",
    );
    let pages = generate_wiki_pages(
        events,
        settings,
        &db_repo,
        ingest_model,
        &ingest_channel_id,
        project_id,
        source_id,
        pool,
        source_filename,
        &sections,
        proj.schema_text
            .as_deref()
            .unwrap_or(super::repository::DEFAULT_SCHEMA),
        proj.reasoning_effort.as_deref(),
        proj.temperature,
        // 摄入发生日期（本地时区），注入 prompt：log.md 的日期/`date` frontmatter 以此为准，
        // 避免模型编造日期（历史缺陷：LLM 自写 log 用 2024-01-01 等虚构时间）。
        &chrono::Local::now().format("%Y-%m-%d").to_string(),
    )
    .await?;

    // 7. Write pages to disk + DB
    repo.update_task_status(task_id, "running", 60, 2, 3, None, None)
        .await?;
    emit_wiki_progress(
        events,
        source_id,
        project_id,
        &source.filename,
        "writing",
        60,
        "写入页面",
    );
    let mut written_pages = Vec::new();
    // 源文档中的原稿图片引用（![第 N 页原稿](...)）。LLM 生成页面时可能丢弃，
    // 程序兜底：若页面里没有任何图片引用，则把原稿截图追加到页面末尾，
    // 保证 Wiki 页面 md 与 ask 结果中都包含图片路径。
    let image_refs = extract_image_refs(&content);
    for page in &pages {
        let page_path = &page.path;

        // 系统页（wiki/index.md、wiki/log.md）由程序统一维护：跳过 LLM 生成的版本，
        // 既不写盘也不入库，避免每次摄入后仅剩最后一份、覆盖累积内容。
        if crate::services::wiki::rebuild::is_system_page(page_path) {
            continue;
        }

        let mut final_content = page.content.clone();
        if !final_content.contains("![") && !image_refs.is_empty() {
            final_content.push_str("\n\n## 原稿截图\n\n");
            final_content.push_str(&image_refs);
        }

        // Write to disk
        project::write_page(project_id, page_path, &final_content).await?;

        // Compute hash
        let mut hasher = Sha256::new();
        hasher.update(final_content.as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        let token_count = (final_content.len() / 4) as i64;

        // Extract wikilinks
        let wikilinks = extract_wikilinks(&final_content);
        let wikilinks_json = serde_json::to_string(&wikilinks).unwrap_or_else(|_| "[]".to_string());

        // Extract tags from frontmatter
        let tags = extract_tags_from_frontmatter(&final_content);
        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());

        // Upsert into DB
        repo.upsert_page(
            project_id,
            page_path,
            &page.title,
            &page.page_type,
            &hash,
            token_count,
            &wikilinks_json,
            "{}",
            &tags_json,
        )
        .await?;

        written_pages.push(WrittenPage {
            path: page_path.clone(),
            title: page.title.clone(),
            page_type: page.page_type.clone(),
            wikilinks,
        });
    }

    // 8. Update graph edges from wikilinks
    repo.update_task_status(task_id, "running", 80, 2, 3, None, None)
        .await?;
    emit_wiki_progress(
        events,
        source_id,
        project_id,
        &source.filename,
        "linking",
        80,
        "更新知识图谱",
    );
    update_graph_edges(pool, project_id, &written_pages).await?;

    // 9. Update source status
    repo.update_source_status(source_id, "ingested", written_pages.len() as i64, None)
        .await?;

    // Update project counts
    let page_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM wiki_pages WHERE project_id = ? AND status = 'active'",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM wiki_sources WHERE project_id = ? AND status = 'ingested'",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE wiki_projects SET page_count=?, source_count=?, last_ingest_at=?, updated_at=? WHERE id=?"
    )
    .bind(page_count).bind(source_count).bind(&now).bind(&now).bind(project_id)
    .execute(pool).await.map_err(|e| format!("DB error: {}", e))?;

    // Append log（带 project 锁，防并发丢行）
    let _ = crate::services::wiki::rebuild::append_log_locked(
        project_id,
        &format!(
            "ingest | {} → {} pages",
            source_filename,
            written_pages.len()
        ),
    )
    .await;

    // log 一致性检测 + 一次性回溯（老项目 log 缺失时从 wiki_sources 重建历史）
    let _ = crate::services::wiki::rebuild::ensure_log_rebuilt(pool, project_id).await;

    // 程序化重建 index.md：读 DB 全量 active 页面按 type 分组（含本次新增），
    // 保证目录始终完整、累积所有源。零 LLM、毫秒级；并发摄入时由 project 锁串行化。
    let _ = crate::services::wiki::rebuild::rebuild_index_programmatic(pool, project_id).await;

    // Update task
    let result_json = serde_json::json!({
        "pages_created": written_pages.len(),
        "source": source_filename,
    })
    .to_string();
    repo.update_task_status(task_id, "done", 100, 3, 3, Some(&result_json), None)
        .await?;
    emit_wiki_progress(
        events,
        source_id,
        project_id,
        &source.filename,
        "done",
        100,
        &format!("完成，生成 {} 个页面", written_pages.len()),
    );

    Ok(IngestResult {
        pages_created: written_pages.len(),
        page_paths: written_pages.iter().map(|p| p.path.clone()).collect(),
    })
}

/// 把字节存到 `raw/assets/<token>/<filename>`，返回页面内引用的 root-relative URL。
/// token 即访问凭证（每页独立、随机 256bit），由 assets 静态路由按路径存在性校验，不记访问日志。
async fn save_asset_bytes(
    project_id: &str,
    token: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, String> {
    let dir = project::project_wiki_dir(project_id)
        .join("raw")
        .join("assets")
        .join(token);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("创建页面图像目录失败: {}", e))?;
    tokio::fs::write(dir.join(filename), bytes)
        .await
        .map_err(|e| format!("保存页面图像失败: {}", e))?;
    Ok(format!(
        "/api/wiki/projects/{}/assets/{}/{}",
        project_id, token, filename
    ))
}

/// 把 PDF 一页渲染成 JPEG 存到 `raw/assets/<token>/page-N.jpg`（整页，OCR 扫描件用）。
async fn save_page_asset(
    project_id: &str,
    page_no: usize,
    token: &str,
    jpeg: &[u8],
) -> Result<String, String> {
    save_asset_bytes(project_id, token, &format!("page-{}.jpg", page_no), jpeg).await
}

/// PDF 摄入提取（页级混合，复用知识库 OCR 的 pdfium 渲染与 VLM 识别组件）：
/// 文字层充足的页直接用文字层（零 OCR、零成本）；文字不足的扫描/图文页渲染整页 JPEG →
/// VLM OCR（配「PDF 视觉模型」），并把整页原稿存 assets 供回答中原样引用；
/// 未配置视觉模型时返回 OCR_MODEL_NOT_CONFIGURED 提示（口径与 RAG 一致）。
async fn extract_pdf_for_wiki(
    events: &EventSink,
    settings: &SettingsStore,
    pool: &sqlx::SqlitePool,
    project_id: &str,
    source_id: &str,
    filename: &str,
    pdf_bytes: &[u8],
) -> Result<String, String> {
    use crate::services::knowledge::ocr;

    let repo = WikiRepository::new(pool.clone());

    // 1. pdfium：逐页文字层（与 RAG 同一渲染入口）
    let renderer = ocr::render::lock_renderer()
        .await
        .map_err(|e| format!("{}", e))?;
    let pages_text = renderer
        .extract_pages_text(pdf_bytes)
        .map_err(|e| format!("{}", e))?;
    drop(renderer);
    let page_count = pages_text.len();

    // 2. 页级分流：仅文字层基本为空（纯扫描件）的页才走 OCR。
    //    阈值保持与 RAG 一致的 50 字符/页：操作手册等图文文档的文字层通常完整
    //    （每页步骤正文几十~几百字），截图只是按钮示意、其中文字不应作为正文解析，
    //    故不因"字数少"强拉 OCR——截图 OCR 会污染内容且非必需。
    let ocr_pages = ocr::pages_needing_ocr(&pages_text);

    // 3. All text-layer-sufficient pages: zero OCR.
    //    Each page text-layer goes into md body; embedded images (screenshots) are
    //    rendered as standalone JPEGs into assets and referenced - i.e. images WITHOUT
    //    the text layer (no mixed full-page render).
    if ocr_pages.is_empty() {
        let dpi = settings.get_u64("ocr.dpi", 200) as u32;
        let mut out = String::new();
        let renderer = ocr::render::lock_renderer()
            .await
            .map_err(|e| format!("{}", e))?;
        // 一次加载 PDF 提取全部页图片：逐页调 page_images_jpegs 会每页重新解析 PDF，
        // 大文件/多图文档耗时成倍增长（用户实测摄入变慢的根因）。
        let all_images = renderer
            .all_pages_images_jpegs(pdf_bytes, dpi)
            .unwrap_or_default();
        for (i, t) in pages_text.iter().enumerate() {
            let page_no = i + 1;
            out.push_str(&format!(
                "<!-- pdf page: {} -->\n\n{}\n\n",
                page_no,
                mark_step_headings(t)
            ));
            // Each embedded image gets its own token (access credential): ![page N screenshot M](url)
            let images = all_images.get(i).cloned().unwrap_or_default();
            for (idx, jpeg) in images.iter().enumerate() {
                let token = uuid::Uuid::new_v4().simple().to_string();
                let url = save_asset_bytes(
                    project_id,
                    &token,
                    &format!("page-{}-{}.jpg", page_no, idx + 1),
                    jpeg,
                )
                .await?;
                out.push_str(&format!(
                    "![page {0} screenshot {1}]({2})\n\n",
                    page_no,
                    idx + 1,
                    url
                ));
            }
        }
        return Ok(out.trim_end().to_string());
    }

    // 4. 需要 OCR：校验「PDF 视觉模型」配置
    let proj = repo.get_project(project_id).await?;
    let model = proj
        .pdf_ocr_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "OCR_MODEL_NOT_CONFIGURED: 该 PDF 含扫描页/图文页，请在 Wiki 项目设置的「PDF 视觉模型」中选择识别模型后重新摄入"
                .to_string()
        })?;
    if proj
        .pdf_ocr_channel_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(
            "OCR_NO_VISION_CHANNEL: 请在 Wiki 项目设置的「PDF 视觉模型」中选择对应渠道".to_string(),
        );
    }

    // 5. 逐页：渲染整页 JPEG（存 assets）+ VLM 识别（VlmOcrClient 内部按渠道 failover）
    let db_repo = Repository::new(pool.clone());
    let client = ocr::vlm::VlmOcrClient::new(&db_repo, model);
    let dpi = settings.get_u64("ocr.dpi", 200) as u32;
    let mut recognized: HashMap<usize, String> = HashMap::new();
    let mut failed_pages: Vec<usize> = Vec::new();
    let total_ocr = ocr_pages.len();
    // 显示用 PDF 总页数（用户要求：y 显示总页数，百分比按页数占比）。
    // 之前显示的是"需 OCR 页数"（如 6 页中 3 页需识别显示 /3），误导为 PDF 只有 3 页。
    let total_pages = pages_text.len().max(1);
    for (_, page_no) in ocr_pages.iter().copied().enumerate() {
        // 取消检查：用户点了停止则停止后续页识别，不再发起新的 VLM 请求
        if ingest_cancelled(pool, source_id).await {
            return Err("CANCELLED：用户已停止处理".into());
        }
        emit_wiki_progress(
            events,
            source_id,
            project_id,
            filename,
            "parsing",
            (8.0 + (page_no as f64 / total_pages as f64) * 6.0) as u8,
            &format!("PDF 识别 第 {}/{} 页", page_no, total_pages),
        );
        let jpeg = {
            let renderer = ocr::render::lock_renderer()
                .await
                .map_err(|e| format!("{}", e))?;
            renderer
                .render_page_jpeg(pdf_bytes, page_no, dpi)
                .map_err(|e| format!("{}", e))?
        };
        // 整页原稿落地 assets/<token>/page-N.jpg；每页独立 token 即访问凭证
        let token = uuid::Uuid::new_v4().simple().to_string();
        let asset_url = save_page_asset(project_id, page_no, &token, &jpeg).await?;
        // OCR 识别：图片已在上面渲染提取并落地 assets（jpeg/token/asset_url 复用，绝不重复生成）。
        // 识别请求加 3 次重试（渠道瞬时失败/超时场景），重试仅重发 VLM 识别请求。
        let mut ocr_success: Option<_> = None;
        let mut ocr_last_err = String::new();
        for attempt in 0..3 {
            match client.ocr_page(&jpeg, page_no).await {
                Ok(result) => {
                    ocr_success = Some(result);
                    break;
                }
                Err(e) => {
                    ocr_last_err = format!("{}", e);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_secs(3 + attempt as u64))
                            .await;
                    }
                }
            }
        }
        match ocr_success {
            Some(result) => {
                recognized.insert(
                    page_no,
                    format!(
                        "<!-- pdf page: {} -->\n\n{}\n\n![第 {} 页原稿]({})\n\n",
                        page_no, result.markdown, page_no, asset_url
                    ),
                );
            }
            None => {
                failed_pages.push(page_no);
                recognized.insert(
                    page_no,
                    format!(
                        "<!-- pdf page: {} -->\n\n[第 {} 页文字识别失败（重试 3 次仍失败）：{}]\n\n![第 {} 页原稿]({})\n\n",
                        page_no, page_no, ocr_last_err, page_no, asset_url
                    ),
                );
            }
        }
    }

    // 6. 过半失败整体失败（与 RAG 熔断一致）；否则失败页保留占位 + 原稿图
    if failed_pages.len() > page_count / 2 {
        return Err(format!(
            "OCR_TOO_MANY_FAILURES: {} 页中 {} 页识别失败（{}）",
            page_count,
            failed_pages.len(),
            failed_pages
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // 7. 合成全文：文字层页用文字层，OCR 页用识别结果 + 原稿图引用。
    //    文字层页内"第X步：…"行升级为 `## 标题` —— 让 parse_markdown 把操作手册切成
    //    步骤 section，LLM 看到的是结构化分步文档（而非 780 字符的扁文本），
    //    避免它把全部步骤合并成一个 summary 页（实测换模型依旧 1 页的根因）。
    //    文字层页也渲染整页原稿存 assets 并引用（与零 OCR 分支一致，保证每页都有图）。
    let mut out = String::new();
    let dpi = settings.get_u64("ocr.dpi", 200) as u32;
    // 一次加载提取全部页图片（避免逐页重新解析 PDF，实测摄入变慢的根因）
    let all_images = {
        let renderer = ocr::render::lock_renderer()
            .await
            .map_err(|e| format!("{}", e))?;
        renderer
            .all_pages_images_jpegs(pdf_bytes, dpi)
            .unwrap_or_default()
    };
    for (i, t) in pages_text.iter().enumerate() {
        // 取消检查：OCR 页识别完成后、进入 LLM 生成前仍可停止（避免"点停止后 OCR 与 LLM 之间继续执行"）
        if ingest_cancelled(pool, source_id).await {
            return Err("CANCELLED：用户已停止处理".into());
        }
        let p = i + 1;
        if let Some(page_md) = recognized.get(&p) {
            out.push_str(page_md);
        } else {
            out.push_str(&format!(
                "<!-- pdf page: {} -->\n\n{}\n\n",
                p,
                mark_step_headings(t)
            ));
            let images = all_images.get(i).cloned().unwrap_or_default();
            for (idx, jpeg) in images.iter().enumerate() {
                let token = uuid::Uuid::new_v4().simple().to_string();
                let url = save_asset_bytes(
                    project_id,
                    &token,
                    &format!("page-{}-{}.jpg", p, idx + 1),
                    jpeg,
                )
                .await?;
                out.push_str(&format!(
                    "![page {0} screenshot {1}]({2})\n\n",
                    p,
                    idx + 1,
                    url
                ));
            }
        }
    }
    Ok(out.trim_end().to_string())
}

/// 从文本中提取全部 Markdown 图片引用（`![alt](url)` 行），用于注入页面保证图片可见。
/// 仅识别标准 `![...](...)` 形态，逐段扫描拼接。
fn extract_image_refs(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        let after = &rest[start..];
        // 找 `](` 与闭括号
        if let Some(close_idx) = after.find("](") {
            let url_tail = &after[close_idx + 2..];
            if let Some(end) = url_tail.find(')') {
                let reference = &after[..close_idx + 2 + end + 1];
                // 仅收录以 /api/ 开头的资产引用，避免把其它 ![..](..) 混入
                if reference.contains("/api/") {
                    out.push_str(reference);
                    out.push('\n');
                }
                rest = &after[close_idx + 2 + end + 1..];
                continue;
            }
        }
        break;
    }
    out
}

/// 把"第X步：…"行升级为 Markdown 二级标题（标题=步骤名，内容=冒号后的正文）。
/// 仅识别中文序数（第一…十/阿拉伯数字），避免误伤普通含"步骤"的句子。
fn mark_step_headings(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(colon) = trimmed.find('：').or_else(|| trimmed.find(':')) {
            let head = &trimmed[..colon];
            if head.starts_with('第') && head.ends_with('步') && head.len() > 2 {
                // 冒号可能是 3 字节中文（：）或 1 字节英文（:），按字符宽度取正文起点
                let colon_len = trimmed[colon..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
                let rest = trimmed[colon + colon_len..].trim();
                out.push_str(&format!("## {}\n\n{}\n\n", head, rest));
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[derive(Debug, Clone)]
pub struct IngestResult {
    pub pages_created: usize,
    pub page_paths: Vec<String>,
}

struct GeneratedPage {
    path: String,
    title: String,
    page_type: String,
    content: String,
}

/// Emit wiki source ingest progress event to frontend.
fn emit_wiki_progress(
    events: &EventSink,
    source_id: &str,
    project_id: &str,
    filename: &str,
    stage: &str,
    progress: u8,
    detail: &str,
) {
    events.emit(
        "wiki-source-progress",
        serde_json::json!({
            "source_id": source_id,
            "project_id": project_id,
            "filename": filename,
            "stage": stage,
            "progress": progress,
            "detail": detail,
        }),
    );
}

/// 摄入是否已被用户取消（停止按钮 → wiki_ingest_queue status='cancelled'）。
/// 摄入是长任务（OCR + 多批 LLM），每个 await 边界检查一次，取消后尽快终止，
/// 不再继续后续页识别与 LLM 生成（已完成的资源不删除）。
async fn ingest_cancelled(pool: &sqlx::SqlitePool, source_id: &str) -> bool {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT status FROM wiki_ingest_queue WHERE source_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    row.map(|r| r.0 == "cancelled").unwrap_or(false)
}

struct WrittenPage {
    path: String,
    title: String,
    page_type: String,
    wikilinks: Vec<String>,
}

/// Parse content into sections based on file type.
fn parse_content(content: &str, file_ext: &str) -> Vec<ContentSection> {
    match file_ext.as_ref() {
        "md" | "markdown" => parse_markdown(content),
        "txt" => parse_plain_text(content),
        "json" => parse_json(content),
        _ => parse_plain_text(content),
    }
}

#[derive(Debug, Clone)]
struct ContentSection {
    heading: String,
    content: String,
}

fn parse_markdown(content: &str) -> Vec<ContentSection> {
    let mut sections = Vec::new();
    let mut current_heading = String::from("Overview");
    let mut current_content = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") {
            if !current_content.is_empty() {
                sections.push(ContentSection {
                    heading: current_heading.clone(),
                    content: current_content.clone(),
                });
                current_content.clear();
            }
            current_heading = trimmed[2..].to_string();
        } else if trimmed.starts_with("## ") {
            if !current_content.is_empty() {
                sections.push(ContentSection {
                    heading: current_heading.clone(),
                    content: current_content.clone(),
                });
                current_content.clear();
            }
            current_heading = trimmed[3..].to_string();
        } else {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    if !current_content.is_empty() {
        sections.push(ContentSection {
            heading: current_heading,
            content: current_content,
        });
    }

    // If only one small section, split by paragraphs
    if sections.len() <= 1 && content.len() > 2000 {
        return parse_plain_text(content);
    }

    if sections.is_empty() {
        sections.push(ContentSection {
            heading: "Document".to_string(),
            content: content.to_string(),
        });
    }

    sections
}

fn parse_plain_text(content: &str) -> Vec<ContentSection> {
    let mut sections = Vec::new();
    let mut chunk = String::new();
    let mut chunk_idx = 1;
    let max_chunk = 3000;

    for line in content.lines() {
        if chunk.len() + line.len() > max_chunk {
            if !chunk.is_empty() {
                sections.push(ContentSection {
                    heading: format!("Section {}", chunk_idx),
                    content: chunk.clone(),
                });
                chunk_idx += 1;
                chunk.clear();
            }
        }
        chunk.push_str(line);
        chunk.push('\n');
    }

    if !chunk.is_empty() {
        sections.push(ContentSection {
            heading: format!("Section {}", chunk_idx),
            content: chunk,
        });
    }

    if sections.is_empty() {
        sections.push(ContentSection {
            heading: "Document".to_string(),
            content: content.to_string(),
        });
    }

    sections
}

fn parse_json(content: &str) -> Vec<ContentSection> {
    // Try to parse and flatten JSON into readable sections
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(json) => {
            let pretty =
                serde_json::to_string_pretty(&json).unwrap_or_else(|_| content.to_string());
            parse_plain_text(&pretty)
        }
        Err(_) => parse_plain_text(content),
    }
}

/// Generate wiki pages from content sections via LLM.
/// 长文档防截断:按字节分批调用 LLM(每批 ≤ MAX_SOURCE_CONTEXT_BYTES),
/// 各自生成页面后合并、按 path 去重——避免一次性截断导致后半部分内容丢失
/// (历史缺陷:手册类 PDF 提取文本 >24KB 时只摄入开头首页)。
async fn generate_wiki_pages(
    events: &EventSink,
    settings: &SettingsStore,
    db_repo: &Arc<Repository>,
    model: &str,
    channel_id: &str,
    project_id: &str,
    source_id: &str,
    pool: &sqlx::SqlitePool,
    source_filename: &str,
    sections: &[ContentSection],
    schema: &str,
    reasoning_effort: Option<&str>,
    temperature: Option<f64>,
    today: &str,
) -> Result<Vec<GeneratedPage>, String> {
    let batches = split_content_batches(sections, MAX_SOURCE_CONTEXT_BYTES);
    let total = batches.len();
    let mut all: Vec<GeneratedPage> = Vec::new();

    for (i, batch) in batches.iter().enumerate() {
        let label = if total > 1 {
            format!("（源文档第 {}/{} 部分）", i + 1, total)
        } else {
            String::new()
        };
        // 取消检查：用户点了停止则立即终止，不再发起下一次 LLM 调用
        if ingest_cancelled(pool, source_id).await {
            return Err("CANCELLED：用户已停止处理".into());
        }
        emit_wiki_progress(
            events,
            source_id,
            project_id,
            source_filename,
            "generating",
            16 + (i as f64 / total.max(1) as f64 * 14.0) as u8,
            &format!(
                "LLM 生成页面{}{}",
                if total > 1 { "（分批）" } else { "" },
                label
            ),
        );
        match generate_batch(
            events,
            settings,
            db_repo,
            model,
            channel_id,
            project_id,
            source_id,
            pool,
            source_filename,
            schema,
            batch,
            &label,
            reasoning_effort,
            temperature,
            today,
        )
        .await
        {
            Ok(pages) => all.extend(pages),
            Err(e) if total > 1 => {
                tracing::warn!("Wiki 摄入第 {}/{} 批失败，跳过继续: {}", i + 1, total, e);
            }
            Err(e) => return Err(e),
        }
    }

    // 按 path 去重（保留首个）
    let mut seen = HashSet::new();
    all.retain(|p| {
        if seen.contains(&p.path) {
            false
        } else {
            seen.insert(p.path.clone());
            true
        }
    });

    if all.is_empty() {
        return Err("LLM 未生成任何 wiki 页面".to_string());
    }
    Ok(all)
}

/// 按累计字节把 sections 切成批，保证每批内容不超过 max_bytes（单节超限时单独成批）。
fn split_content_batches(
    sections: &[ContentSection],
    max_bytes: usize,
) -> Vec<Vec<ContentSection>> {
    let mut batches: Vec<Vec<ContentSection>> = Vec::new();
    let mut cur: Vec<ContentSection> = Vec::new();
    let mut cur_bytes: usize = 0;
    for s in sections {
        let bytes = format!("## {}\n{}", s.heading, s.content).len();
        if !cur.is_empty() && cur_bytes + bytes > max_bytes {
            batches.push(std::mem::take(&mut cur));
            cur_bytes = 0;
        }
        cur.push(s.clone());
        cur_bytes += bytes;
    }
    if !cur.is_empty() {
        batches.push(cur);
    }
    batches
}

/// 单批内容送 LLM 生成页面（原 generate_wiki_pages 主体；batch_label 用于多批提示）。
async fn generate_batch(
    events: &EventSink,
    settings: &SettingsStore,
    db_repo: &Arc<Repository>,
    model: &str,
    channel_id: &str,
    project_id: &str,
    source_id: &str,
    pool: &sqlx::SqlitePool,
    source_filename: &str,
    schema: &str,
    batch: &[ContentSection],
    batch_label: &str,
    reasoning_effort: Option<&str>,
    temperature: Option<f64>,
    today: &str,
) -> Result<Vec<GeneratedPage>, String> {
    let truncated = build_source_context(batch);

    // 推理档位与温度：项目级配置优先。
    // 未配置或显式 default → 不写档位字段与 thinking，完全跟随模型默认
    // （用户拍板：default 语义=最保守，避免严格上游收到未知 thinking 报错）。
    let effort: Option<String> = reasoning_effort
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "default")
        .map(|s| s.to_string());
    let temp = temperature.unwrap_or(0.3).clamp(0.0, 2.0);

    let system_prompt = format!(
        r#"You are a Wiki maintainer. Read the source document and generate structured wiki pages in Markdown.

## Language (语言强制约束)
ALWAYS write in Chinese (中文): page titles, file/directory names, frontmatter values, headings, and body content MUST be Chinese. The ONLY exception is when the source text itself is in a non-Chinese language — then preserve that original language. NEVER translate content into a different language than the source (do NOT render Chinese source content into English).
File paths MUST use Chinese file names, e.g. `concepts/产品使用入门.md`, `summaries/产品操作手册总览.md` — NEVER English or pinyin names like `step-1-login.md`.

## Wiki Schema
{}

## Instructions
1. Analyze the source document and identify key entities, concepts, and topics.
2. For each key item, generate a wiki page in Markdown format.
3. Use `[[wikilinks]]` to connect related pages.
4. Each page should have YAML frontmatter with: title, type (entity/concept/summary), tags, source.
5. Separate pages with a delimiter: ---PAGE---
6. The first line of each page should be the file path (e.g., `entities/my-item.md`).
7. (HARD RULE) ALWAYS generate MULTIPLE pages — at least 3 content pages besides index/log. NEVER collapse the whole source into one single summary page with verbatim text; each step/topic MUST become its own page.

## Output Format
```
entities/page-name.md
---
title: Page Name
type: entity
tags: [tag1, tag2]
source: {}
---
# Page Name

Content here with [[wikilinks]] to other pages.

## Details
...
---PAGE---
concepts/another-concept.md
---
title: Another Concept
type: concept
tags: [tag1]
source: {}
---
# Another Concept
...
```

## Step-by-step manuals（操作手册硬约束）
If the source is an operational manual (numbered steps, procedures, workflows):
1. Create ONE `summary` page that indexes all steps with their purpose.
2. Create SEPARATE wiki pages for each major step or topic (one page per step, or one page per subsystem/action), each containing the ORIGINAL wording of that step (URLs, button labels, commands).
3. NEVER merge all steps into a single page, NEVER drop step text.
4. Total pages for such a document MUST be at least 3.
Follow these rules regardless of how short the source text is.

## Images (图片引用必保留)
If the source content contains markdown image references (e.g. `![第 1 页原稿](/api/wiki/...)`), you MUST include them verbatim in the generated pages so the original screenshots remain viewable. NEVER drop or rewrite image URLs.

Generate 3-8 pages depending on document complexity. Focus on the most important entities and concepts."#,
        schema, source_filename, source_filename
    );

    let user_prompt = format!(
        "Today's date: {today} (use this date for `wiki/log.md` entries and `date` frontmatter; NEVER invent other dates)\n\nSource document: {}{}\n\nContent:\n{}",
        source_filename, batch_label, truncated
    );

    let mut chat_request = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt}
        ],
        "stream": false,
        "temperature": temp
    });
    // 档位仅在显式配置时写入（未配置/default → 不写，跟随模型默认）：
    // reasoning_effort（OpenAI 语义）+ thinking（Anthropic/Kimi 语义）联动。
    if let Some(effort) = &effort {
        chat_request["reasoning_effort"] = serde_json::json!(effort);
        match effort.as_str() {
            "none" | "off" => {
                chat_request["thinking"] = serde_json::json!({"type": "disabled"});
            }
            _ => {
                chat_request["thinking"] = serde_json::json!({"type": "enabled"});
            }
        }
    }

    let chat_request_str: String = serde_json::to_string(&chat_request).unwrap_or_default();

    let mut llm_answer = None;
    // LLM 生成重试：渠道不稳定（5xx，如 502）时每 5 秒重试，至多 4 次重试（共 5 次调用）。
    // 重试只重发 LLM 请求，OCR/图片提取已完成，不会从头再来。
    for attempt in 0..5 {
        // 取消检查：已停止则不发起本次调用
        if ingest_cancelled(pool, source_id).await {
            return Err("CANCELLED：用户已停止处理".into());
        }
        // 每尝试一次发进度，避免界面停留在 OCR 阶段无提示（用户曾误以为卡在 6/6）
        emit_wiki_progress(
            events,
            source_id,
            project_id,
            source_filename,
            "generating",
            18 + (attempt as f64 / 5.0 * 12.0) as u8,
            &format!("LLM 生成页面（尝试 {}/5）", attempt + 1),
        );
        let proxy_result = proxy::handle_request(
            db_repo,
            settings,
            channel_id,
            "Wiki Ingest",
            chat_request.clone(),
            false,
            Some(chat_request_str.clone()),
            Some(format!("wiki-ingest_{}", project_id)),
            None,
        )
        .await;

        match &proxy_result {
            Ok(result) => {
                llm_answer = Some(result.body.clone());
                break;
            }
            Err((code, _)) if attempt < 4 && (500..=599).contains(code) => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err((code, msg)) => {
                return Err(format!("LLM request failed ({}): {}", code, msg));
            }
        }
    }

    let response_body = match llm_answer {
        Some(body) => body,
        None => {
            return Err("LLM request failed: 重试 5 次后仍未成功".to_string());
        }
    };

    // Extract text from response
    let raw_text = response_body
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|t| t.as_str())
        .unwrap_or("");

    if raw_text.is_empty() {
        // Fallback: create a single summary page from the source
        return Ok(vec![GeneratedPage {
            path: format!("summaries/{}.md", sanitize_filename(source_filename)),
            title: source_filename.to_string(),
            page_type: "summary".to_string(),
            content: format!(
                "---\ntitle: {}\ntype: summary\ntags: []\nsource: {}\n---\n\n# {}\n\n{}",
                source_filename, source_filename, source_filename, truncated
            ),
        }]);
    }

    // Parse the LLM output into pages
    Ok(parse_generated_pages(raw_text, source_filename))
}

fn build_source_context(sections: &[ContentSection]) -> String {
    let combined: String = sections
        .iter()
        .map(|s| format!("## {}\n{}", s.heading, s.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    if combined.len() <= MAX_SOURCE_CONTEXT_BYTES {
        return combined;
    }

    let mut truncated = truncate_utf8(&combined, MAX_SOURCE_CONTEXT_BYTES).to_string();
    truncated.push_str(SOURCE_CONTEXT_TRUNCATION_MARKER);
    truncated
}

/// Parse LLM-generated pages from the response text.
fn parse_generated_pages(text: &str, source_filename: &str) -> Vec<GeneratedPage> {
    let mut pages = Vec::new();
    let mut current_path = String::new();
    let mut current_content = String::new();
    let mut in_content = false;

    let mut lines = text.lines().peekable();

    // Skip any preamble before first path
    while let Some(line) = lines.peek() {
        let trimmed = line.trim();
        if trimmed.ends_with(".md") || (trimmed.contains('/') && trimmed.ends_with(".md")) {
            break;
        }
        // Also check for path-like patterns
        if trimmed.contains(".md") && !trimmed.starts_with("#") {
            break;
        }
        lines.next();
    }

    for line in text.lines() {
        let trimmed = line.trim();

        // 新页面开始（含自定义 schema 的 "### FILE: path.md" 头与代码围栏包裹）。
        // 关键：遇到新路径行时先关闭上一页，否则多页输出会被合并成一页。
        // FILE 头是显式格式（正文不会出现），总是识别；纯路径行仅在未进入内容时识别，
        // 避免正文里引用 "xxx.md" 的行被误判为页面边界。
        if let Some((path, explicit)) = extract_path_from_line(trimmed) {
            if explicit || !in_content {
                if in_content && !current_content.is_empty() && !current_path.is_empty() {
                    pages.push(build_page(&current_path, &current_content, source_filename));
                }
                current_path = path;
                current_content.clear();
                in_content = true;
                continue;
            }
        }

        // Check for page delimiter（默认 prompt 的 ---PAGE---）
        if trimmed == "---PAGE---" {
            if in_content && !current_content.is_empty() && !current_path.is_empty() {
                pages.push(build_page(&current_path, &current_content, source_filename));
            }
            current_content.clear();
            current_path.clear();
            in_content = false;
            continue;
        }

        if in_content {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }

    // Handle last page
    if !current_content.is_empty() {
        if current_path.is_empty() {
            // Try to extract path from content
            if let Some(path) = extract_path_from_content(&current_content) {
                current_path = path;
            }
        }
        if !current_path.is_empty() {
            pages.push(build_page(&current_path, &current_content, source_filename));
        } else if let Some(page) = build_page_from_content(&current_content, source_filename) {
            pages.push(page);
        }
    }

    // Deduplicate by path
    let mut seen = HashSet::new();
    pages.retain(|p| {
        if seen.contains(&p.path) {
            false
        } else {
            seen.insert(p.path.clone());
            true
        }
    });

    if pages.is_empty() {
        // Fallback: create a summary page
        pages.push(GeneratedPage {
            path: format!("summaries/{}.md", sanitize_filename(source_filename)),
            title: source_filename.to_string(),
            page_type: "summary".to_string(),
            content: format!(
                "---\ntitle: {}\ntype: summary\ntags: []\nsource: {}\n---\n\n# {}\n\n{}",
                source_filename,
                source_filename,
                source_filename,
                text.chars().take(8000).collect::<String>()
            ),
        });
    }

    pages
}

fn extract_path_from_content(content: &str) -> Option<String> {
    // Look for first line that looks like a path
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with(".md") && trimmed.len() < 200 {
            // Clean up markdown code fences
            let clean = trimmed
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim();
            if clean.ends_with(".md") {
                return Some(clean.to_string());
            }
        }
    }
    None
}

/// 从一行里提取 wiki 页面路径。返回 `(路径, 是否显式 FILE 头)`。
/// 支持三种形态：
/// - `### FILE: path.md`（自定义 schema 的 FILE 头，显式页面边界；剥离前缀，否则路径含非法字符导致 os error 267）
/// - `path.md`（纯路径行，默认 prompt 输出；仅在未进入内容时由调用方识别）
/// - `` ```path.md `` / `` ``` `` 包裹的路径行
fn extract_path_from_line(trimmed: &str) -> Option<(String, bool)> {
    let mut s = trimmed.trim();
    let mut explicit = false;
    // 自定义 schema 的 "### FILE: path.md" 头
    if let Some(rest) = s.strip_prefix("### FILE:") {
        s = rest.trim();
        explicit = true;
    }
    // 代码围栏
    s = s.trim_start_matches("```").trim_end_matches("```").trim();
    // 路径必须是纯相对路径：以 .md 结尾、不含空格（避免把整句误判为路径）
    if s.ends_with(".md") && s.len() < 200 && !s.contains(' ') {
        // 规范化：schema 里常写 `wiki/concepts/x.md`，而 write_page 已以项目 wiki 目录为根，
        // 保留 `wiki/` 前缀会落盘成双层 `wiki/wiki/`（读取/检索路径随之错乱）→ 剥掉。
        let normalized = s.strip_prefix("wiki/").unwrap_or(s);
        Some((normalized.to_string(), explicit))
    } else {
        None
    }
}

/// 剥离 LLM 常见的"整页被 ```markdown 代码围栏包裹"。
/// 不剥离会导致：① 页面渲染成正文代码块；② build_page 误判"无 frontmatter"再补一层，
/// 产出双重 frontmatter（实测 step-1-login.md 即如此）；③ 污染检索与字数统计。
fn strip_wrapping_fence(content: &str) -> &str {
    let mut c = content.trim();
    if !c.starts_with("```") {
        return c;
    }
    // 去掉首行围栏（```markdown / ```md / ```）
    match c.find('\n') {
        Some(nl) => c = c[nl + 1..].trim_start(),
        None => return c,
    }
    // 去掉尾行围栏
    if c.ends_with("```") {
        c = c[..c.len() - 3].trim_end();
    }
    c
}

fn build_page(path: &str, raw_content: &str, source_filename: &str) -> GeneratedPage {
    // Remove path line from content if present
    let content = raw_content
        .lines()
        .filter(|l| {
            let t = l.trim();
            !(t.ends_with(".md") && t.len() < 200)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let content = strip_wrapping_fence(content.trim());

    // Extract title from frontmatter or first heading
    let title = extract_title_from_content(content, path);

    // Determine page type from frontmatter or path
    let page_type = if path.starts_with("entities/") {
        "entity"
    } else if path.starts_with("concepts/") {
        "concept"
    } else if path.starts_with("summaries/") {
        "summary"
    } else if path.ends_with("index.md") {
        "index"
    } else if path.ends_with("log.md") {
        "log"
    } else {
        "entity"
    };

    // Ensure content has frontmatter
    let final_content = if content.starts_with("---") {
        content.to_string()
    } else {
        format!(
            "---\ntitle: {}\ntype: {}\ntags: []\nsource: {}\n---\n\n{}",
            title, page_type, source_filename, content
        )
    };

    GeneratedPage {
        path: path.to_string(),
        title,
        page_type: page_type.to_string(),
        content: final_content,
    }
}

fn build_page_from_content(content: &str, _source_filename: &str) -> Option<GeneratedPage> {
    let title = extract_title_from_content(content, "");
    if title.is_empty() {
        return None;
    }
    let path = format!("entities/{}.md", sanitize_filename(&title));
    Some(GeneratedPage {
        path,
        title,
        page_type: "entity".to_string(),
        content: content.to_string(),
    })
}

pub fn extract_title_from_content(content: &str, fallback_path: &str) -> String {
    // Try frontmatter title
    if content.starts_with("---") {
        let end = content[3..].find("---");
        if let Some(e) = end {
            let frontmatter = &content[3..3 + e];
            for line in frontmatter.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("title:") {
                    let title = trimmed[6..].trim().trim_matches('"').trim_matches('\'');
                    if !title.is_empty() {
                        return title.to_string();
                    }
                }
            }
        }
    }
    // Try first heading
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") {
            return trimmed[2..].trim().to_string();
        }
        if trimmed.starts_with("## ") {
            return trimmed[3..].trim().to_string();
        }
    }
    // Fallback to filename from path
    if !fallback_path.is_empty() {
        return fallback_path
            .split('/')
            .last()
            .unwrap_or(fallback_path)
            .trim_end_matches(".md")
            .to_string();
    }
    "Untitled".to_string()
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn extract_wikilinks(content: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut start = 0;
    loop {
        if let Some(s) = content[start..].find("[[") {
            let s = start + s + 2;
            if let Some(e) = content[s..].find("]]") {
                let link = &content[s..s + e];
                if !link.is_empty() {
                    links.push(link.to_string());
                }
                start = s + e + 2;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    links
}

/// Rebuild graph edges for a project based on current page wikilinks.
/// Called after page save/delete to keep the knowledge graph up-to-date.
/// FIX-27：内存 wikilink 解析表——path/title 一次载入，逐链接解析不再
/// 每条打一次 `resolve_wikilink_to_path` 查询（N+1 消除）。
struct WikilinkIndex {
    /// LOWER(title) → path（与旧实现 LOWER(title)=LOWER(?) 语义一致）。
    titles: HashMap<String, String>,
    paths: HashSet<String>,
}

impl WikilinkIndex {
    fn resolve(&self, link: &str) -> String {
        let link = link.trim();
        // If it already looks like a path, use as-is
        if link.contains('/') && link.ends_with(".md") {
            return link.to_string();
        }
        if let Some(path) = self.titles.get(&link.to_lowercase()) {
            return path.clone();
        }
        normalize_wikilink(link)
    }

    fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }
}

/// 一次载入项目全部活跃页：解析索引 + 逐页 wikilinks 列表。
async fn load_active_pages(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<(Vec<(String, Vec<String>)>, WikilinkIndex), String> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT path, wikilinks, title FROM wiki_pages WHERE project_id = ? AND status = 'active'",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("DB error: {}", e))?;
    let index = WikilinkIndex {
        titles: rows
            .iter()
            .map(|(path, _, title)| (title.to_lowercase(), path.clone()))
            .collect(),
        paths: rows.iter().map(|(path, _, _)| path.clone()).collect(),
    };
    let pages = rows
        .into_iter()
        .map(|(path, wikilinks_json, _)| {
            let wikilinks: Vec<String> = serde_json::from_str(&wikilinks_json).unwrap_or_default();
            (path, wikilinks)
        })
        .collect();
    Ok((pages, index))
}

/// FIX-27：批量写边——单事务 + 分块多行 INSERT，替代逐行往返。
async fn replace_graph_edges(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    links: Vec<(String, String)>,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| format!("DB error: {}", e))?;
    sqlx::query("DELETE FROM wiki_graph_edges WHERE project_id = ?")
        .bind(project_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
    let now = chrono::Utc::now().to_rfc3339();
    for chunk in links.chunks(500) {
        let mut sql = String::from(
            "INSERT OR IGNORE INTO wiki_graph_edges (id, project_id, source_page, target_page, edge_type, weight, created_at) VALUES ",
        );
        let groups: Vec<&str> = chunk
            .iter()
            .map(|_| "(?, ?, ?, ?, 'wikilink', 1.0, ?)")
            .collect();
        sql.push_str(&groups.join(", "));
        let mut query = sqlx::query(&sql);
        for (source, target) in chunk {
            query = query
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(project_id)
                .bind(source)
                .bind(target)
                .bind(&now);
        }
        query
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("DB error: {}", e))?;
    }
    tx.commit().await.map_err(|e| format!("DB error: {}", e))?;
    Ok(())
}

pub async fn rebuild_graph_edges(pool: &sqlx::SqlitePool, project_id: &str) -> Result<(), String> {
    let (pages, index) = load_active_pages(pool, project_id).await?;
    let mut all_links: Vec<(String, String)> = Vec::new();
    for (path, links) in &pages {
        for link in links {
            let target = index.resolve(link);
            if index.contains(&target) {
                all_links.push((path.clone(), target));
            }
        }
    }
    replace_graph_edges(pool, project_id, all_links).await
}

/// Update graph_edges table based on wikilinks in pages.
///
/// FIX-27：新页面在第 7 步已 upsert 入库，这里统一从 DB 一次载入全部
/// 活跃页（含新页）做内存解析 + 批量写入，不再逐链接/逐行打查询。
async fn update_graph_edges(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    pages: &[WrittenPage],
) -> Result<(), String> {
    let _ = pages; // 兼容旧签名保留参数（页面已在库中）
    let (db_pages, index) = load_active_pages(pool, project_id).await?;
    let mut all_links: Vec<(String, String)> = Vec::new();
    for (path, links) in &db_pages {
        for link in links {
            let target = index.resolve(link);
            if index.contains(&target) {
                all_links.push((path.clone(), target));
            }
        }
    }
    replace_graph_edges(pool, project_id, all_links).await
}

/// Extract tags from YAML frontmatter of a wiki page.
pub fn extract_tags_from_frontmatter(content: &str) -> Vec<String> {
    if !content.starts_with("---") {
        return vec![];
    }
    let rest = &content[3..];
    let end = match rest.find("---") {
        Some(e) => e,
        None => return vec![],
    };
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("tags:") {
            let tags_part = trimmed[5..].trim();
            // Parse array format: [tag1, tag2] or ["tag1", "tag2"]
            let cleaned = tags_part
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim();
            if cleaned.is_empty() {
                return vec![];
            }
            return cleaned
                .split(',')
                .map(|t| {
                    t.trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .trim()
                        .to_string()
                })
                .filter(|t| !t.is_empty())
                .collect();
        }
    }
    vec![]
}

fn normalize_wikilink(link: &str) -> String {
    let link = link.trim();
    // If it already looks like a path, use as-is
    if link.contains('/') && link.ends_with(".md") {
        return link.to_string();
    }
    // Otherwise, assume it's an entity name
    let slug: String = link
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    format!("entities/{}.md", slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_step_headings_splits_numbered_steps_into_headings() {
        let input = "新版本共享网站操作手册\n第一步：打开网站 http://x 登录管理员\n第二步：选择数据源管理，点击测试。\n补充说明：以上步骤需网络策略。\n第五步：进入资源维护。";
        let out = mark_step_headings(input);
        assert!(out.contains("## 第一步"), "第一步应成为标题: {out}");
        assert!(out.contains("## 第二步"), "第二步应成为标题: {out}");
        assert!(out.contains("## 第五步"), "第五步应成为标题: {out}");
        // 冒号前不是"第X步"的句子保持原样，不被误伤
        assert!(out.contains("补充说明：以上步骤需网络策略。"));
        // 步骤正文（冒号后）与标题分开
        assert!(out.contains("打开网站 http://x 登录管理员"));
        // 单行"第一步：xx"应被拆分，不再以整行出现
        assert!(!out
            .lines()
            .any(|l| l == "第一步：打开网站 http://x 登录管理员"));
        // 无步骤的普通文本原样返回
        assert_eq!(
            mark_step_headings("普通文本，无步骤。"),
            "普通文本，无步骤。"
        );
    }

    /// FIX-LATEST：用户自定义 schema 用 `### FILE: path.md` 头输出多页。
    /// 解析器必须：①剥离 FILE 前缀（否则路径含非法字符，Windows 写盘 os error 267）；
    /// ②遇到新 FILE 行即关闭上一页（否则多页被合并成一页）。
    #[test]
    fn parse_generated_pages_supports_file_headers_and_multi_page_output() {
        // 自定义 schema 的 ### FILE: 头格式（带 ```markdown 代码围栏）
        let text = r#"### FILE: wiki/index.md
```markdown
# Index
- [[entities/foo]]
```
### FILE: wiki/log.md
```markdown
# Log
- 六步已保留
```
### FILE: wiki/entities/foo.md
```markdown
---
title: Foo
type: entity
---
# Foo
第一步：打开网站
```
"#;
        let pages = parse_generated_pages(text, "manual.pdf");
        let paths: Vec<_> = pages.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["index.md", "log.md", "entities/foo.md"],
            "FILE 头多页应全部解析且剥掉 wiki/ 前缀（避免落盘成双层 wiki/wiki），当前: {paths:?}"
        );
        assert!(
            !pages[0].path.contains("### FILE:"),
            "路径不得含 FILE 头前缀: {}",
            pages[0].path
        );
        assert!(pages[2].content.contains("第一步：打开网站"));
        // 围栏必须被剥离：否则页面渲染成正文代码块，且 build_page 会再补一层 frontmatter
        for p in &pages {
            assert!(
                !p.content.contains("```markdown") && !p.content.contains("```"),
                "内容不应残留代码围栏: {}",
                p.content
            );
            assert_eq!(
                p.content.matches("---").count(),
                2,
                "应只有一对 frontmatter，实际: {}",
                p.content
            );
        }

        // 默认 ---PAGE--- 格式仍兼容
        let legacy = "entities/a.md\n---\ntitle: A\n---\n# A\ncontent\n---PAGE---\nentities/b.md\n---\ntitle: B\n---\n# B\ncontent";
        let pages = parse_generated_pages(legacy, "s.pdf");
        assert_eq!(pages.len(), 2, "---PAGE--- 格式应拆两页");

        // 正文中的 .md 引用行不误拆（纯路径行仅在未进入内容时识别）
        let single =
            "entities/a.md\n---\ntitle: A\n---\n# A\n参考 [[b]]，详细见 entities/b.md 的说明\n结尾";
        let pages = parse_generated_pages(single, "s.pdf");
        assert_eq!(
            pages.len(),
            1,
            "正文引用 .md 不得拆分页面: {:?}",
            pages.iter().map(|p| p.path.as_str()).collect::<Vec<_>>()
        );
    }

    /// FIX-22：摄入失败时任务与来源状态收敛为 failed，不再永久 running。
    /// 用不存在的来源触发第一步失败（无渠道/LLM 依赖，稳定可控）。
    #[tokio::test]
    async fn ingest_failure_converges_task_and_source_to_failed() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let events = crate::server::event_bridge::EventSink::headless(event_tx);
        let settings = crate::settings_store::SettingsStore::file(
            std::env::temp_dir().join(format!("waliapi-ingest-test-{}.json", uuid::Uuid::new_v4())),
        );

        // 先建真实项目（wiki_ingest_queue 对 project_id 有外键约束）
        sqlx::query(
            "INSERT INTO wiki_projects (id, name, wiki_dir, created_at, updated_at) VALUES ('proj-fk', 't', '/tmp/w', ?, ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let err = ingest_source(&events, &settings, &pool, "proj-fk", "src-none").await;
        assert!(err.is_err(), "不存在的来源必须报错");

        // 任务行存在且已收敛为 failed（带错误信息），不是 running
        let task: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT status, error_message FROM wiki_ingest_queue ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        let (status, error) = task.expect("task row must exist");
        assert_eq!(status, "failed", "任务状态必须收敛 failed");
        assert!(error.is_some(), "失败原因必须落库");
    }

    /// FIX-27：批量图谱重建——标题解析（含大小写不敏感）与边落库正确。
    #[tokio::test]
    async fn rebuild_graph_edges_resolves_titles_and_writes_edges() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        // wiki_pages 对 project_id 有外键约束：先建项目
        sqlx::query(
            "INSERT INTO wiki_projects (id, name, wiki_dir, created_at, updated_at) VALUES ('p1', 't', '/tmp/w', ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(&pool)
        .await
        .unwrap();
        for (path, title, wikilinks) in [
            ("a.md", "Alpha", r#"["Beta"]"#),
            ("b.md", "Beta", r#"["ghost"]"#),
            ("docs/c.md", "Gamma", r#"["docs/c.md"]"#),
        ] {
            sqlx::query(
                "INSERT INTO wiki_pages (id, project_id, path, title, page_type, content_hash, token_count, wikilinks, frontmatter, tags, status, created_at, updated_at)
                 VALUES (?, 'p1', ?, ?, 'note', 'h', 1, ?, '{}', '[]', 'active', ?, ?)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(path)
            .bind(title)
            .bind(wikilinks)
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .unwrap();
        }

        rebuild_graph_edges(&pool, "p1").await.unwrap();

        let edges: Vec<(String, String)> = sqlx::query_as(
            "SELECT source_page, target_page FROM wiki_graph_edges WHERE project_id = 'p1' ORDER BY source_page",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        // Alpha→Beta（标题解析）；Gamma→自身（路径直通）；Beta→ghost 不存在 → 无边
        assert!(
            edges.contains(&("a.md".to_string(), "b.md".to_string())),
            "edges: {edges:?}"
        );
        assert!(
            edges.contains(&("docs/c.md".to_string(), "docs/c.md".to_string())),
            "edges: {edges:?}"
        );
        assert_eq!(edges.len(), 2, "幽灵链接不得建边: {edges:?}");
    }

    #[test]
    fn source_context_truncates_cjk_at_utf8_boundary() {
        let sections = vec![ContentSection {
            heading: String::new(),
            content: "中".repeat(9_000),
        }];

        let context = build_source_context(&sections);
        let content = context
            .strip_suffix(SOURCE_CONTEXT_TRUNCATION_MARKER)
            .unwrap();

        assert!(content.len() <= MAX_SOURCE_CONTEXT_BYTES);
        assert!(content.is_char_boundary(content.len()));
        assert!(context.ends_with(SOURCE_CONTEXT_TRUNCATION_MARKER));
    }

    #[test]
    fn source_context_truncates_emoji_at_utf8_boundary() {
        let sections = vec![ContentSection {
            heading: "Emoji".to_string(),
            content: "😀".repeat(7_000),
        }];

        let context = build_source_context(&sections);
        let content = context
            .strip_suffix(SOURCE_CONTEXT_TRUNCATION_MARKER)
            .unwrap();

        assert!(content.len() <= MAX_SOURCE_CONTEXT_BYTES);
        assert!(content.is_char_boundary(content.len()));
    }

    #[test]
    fn source_context_preserves_content_within_limit() {
        let sections = vec![ContentSection {
            heading: "Overview".to_string(),
            content: "WaLiAPI Wiki".to_string(),
        }];

        assert_eq!(build_source_context(&sections), "## Overview\nWaLiAPI Wiki");
    }

    #[test]
    fn source_context_handles_ascii_at_and_above_byte_limit() {
        let exact = vec![ContentSection {
            heading: "A".to_string(),
            content: "x".repeat(MAX_SOURCE_CONTEXT_BYTES - "## A\n".len()),
        }];
        let over = vec![ContentSection {
            heading: "A".to_string(),
            content: "x".repeat(MAX_SOURCE_CONTEXT_BYTES - "## A\n".len() + 1),
        }];

        let exact_context = build_source_context(&exact);
        assert_eq!(exact_context.len(), MAX_SOURCE_CONTEXT_BYTES);
        assert!(!exact_context.ends_with(SOURCE_CONTEXT_TRUNCATION_MARKER));

        let over_context = build_source_context(&over);
        let over_content = over_context
            .strip_suffix(SOURCE_CONTEXT_TRUNCATION_MARKER)
            .unwrap();
        assert_eq!(over_content.len(), MAX_SOURCE_CONTEXT_BYTES);
    }

    #[test]
    fn source_context_truncates_combined_sections_at_utf8_boundary() {
        let sections = vec![
            ContentSection {
                heading: "Overview".to_string(),
                content: "a".repeat(12_000),
            },
            ContentSection {
                heading: "中文".to_string(),
                content: "界".repeat(5_000),
            },
        ];

        let context = build_source_context(&sections);
        let content = context
            .strip_suffix(SOURCE_CONTEXT_TRUNCATION_MARKER)
            .unwrap();

        assert!(content.len() <= MAX_SOURCE_CONTEXT_BYTES);
        assert!(content.is_char_boundary(content.len()));
        assert!(content.contains("## 中文"));
    }

    /// 回归测试（issue #55，FIX-03 移植适配）：摄入 >24KB 纯中文文档时，
    /// parse_content → 上下文组装的真实崩溃路径不得因字节切片 panic。
    /// 上游 PR #58 的用例直接构造 sections，本用例补齐从原始文档进出的端到端覆盖。
    #[test]
    fn source_context_large_cjk_document_via_parse_content_does_not_panic() {
        // 48000 字节纯中文（单行，保证切点落在多字节字符中间），超过上限。
        let doc = "知识库摄入测试。".repeat(2000);
        assert!(doc.len() > MAX_SOURCE_CONTEXT_BYTES);
        let sections = parse_content(&doc, "txt");
        assert!(!sections.is_empty());
        let context = build_source_context(&sections);
        let content = context
            .strip_suffix(SOURCE_CONTEXT_TRUNCATION_MARKER)
            .unwrap();
        assert!(content.len() <= MAX_SOURCE_CONTEXT_BYTES);
        assert!(content.is_char_boundary(content.len()));
        assert!(std::str::from_utf8(content.as_bytes()).is_ok());
    }
}
