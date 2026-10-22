//! Wiki 项目备份：导出为 zip（磁盘目录 + 核心 DB 记录），导入时完整恢复。
//!
//! 设计要点（与用户确认）：
//! - 备份内容 = 项目磁盘目录全文（`files/`，含 `raw/assets/<token>/` 图片与 token 凭证、
//!   `raw/sources/` 源文件、`wiki/*.md` 页面、`schema/CLAUDE.md`）
//!   + 核心数据库记录（`db.json`：项目配置 / 页面元数据 / 源记录 / 图谱边）+ `meta.json` 元信息。
//! - 问答历史、摄入队列、评审为运行时派生数据，不随备份（核心数据范围）。
//! - API Key 授权关系不恢复（换机/重装后密钥 ID 对不上，用户确认不恢复）。
//! - 导入总是生成全新项目 ID：绝不覆盖现有项目；页面 MD 内容中内嵌的旧
//!   `/api/wiki/projects/<old>/assets/` 图片 URL 会同步替换为新 ID，图片 token 原样保留。
//! - 解压路径逐段白名单校验，拒绝路径穿越（`..` / 绝对路径 / 反斜杠）。

use super::models::{WikiPage, WikiProject, WikiSource};
use super::project;
use super::repository::WikiRepository;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tokio::fs;
use uuid::Uuid;

/// 备份格式版本：不兼容变更时递增，导入时校验。
pub const BACKUP_FORMAT_VERSION: u32 = 1;

/// zip 内磁盘文件的前缀目录。
const FILES_PREFIX: &str = "files/";

// ── 对外结果类型 ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct WikiExportResult {
    pub path: String,
    pub project_id: String,
    pub project_name: String,
    pub page_count: usize,
    pub source_count: usize,
    pub file_count: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WikiImportResult {
    pub project_id: String,
    pub project_name: String,
    pub page_count: usize,
    pub source_count: usize,
    pub file_count: usize,
}

// ── zip 内部结构 ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct MetaInfo {
    format_version: u32,
    project_id: String,
    project_name: String,
    exported_at: String,
    page_count: usize,
    source_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct DbSnapshot {
    project: WikiProject,
    pages: Vec<WikiPage>,
    sources: Vec<WikiSource>,
    graph_edges: Vec<GraphEdgeRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphEdgeRow {
    source_page: String,
    target_page: String,
    edge_type: String,
    weight: f64,
}

/// 解压后暂存磁盘文件的条目（相对 `files/` 的路径 + 字节）。
struct ExtractedFile {
    rel_path: String,
    bytes: Vec<u8>,
}

/// 解析并校验后的备份内容。
struct ParsedBackup {
    meta: MetaInfo,
    snapshot: DbSnapshot,
    files: Vec<ExtractedFile>,
}

// ── 导出 ────────────────────────────────────────────────────────────────────

/// 把单个 Wiki 项目导出为 zip 备份到 `target_dir`。
/// `target_dir` 必须存在（前端对话框/输入框给定）；文件名自动生成
/// `wiki-backup-<项目名>-<YYYYMMDD-HHMMSS>.zip`。
pub async fn export_project(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    target_dir: &str,
) -> Result<WikiExportResult, String> {
    let repo = WikiRepository::new(pool.clone());
    let project = repo.get_project(project_id).await?;
    let pages = repo.list_pages(project_id).await?;
    let sources = repo.list_sources(project_id).await?;
    let graph_edges = load_graph_edges(pool, project_id).await?;

    let root = project::project_wiki_dir(project_id);
    let target = PathBuf::from(target_dir);
    if !target.is_dir() {
        return Err(format!("导出目录不存在: {}", target_dir));
    }

    let meta = MetaInfo {
        format_version: BACKUP_FORMAT_VERSION,
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        exported_at: Utc::now().to_rfc3339(),
        page_count: pages.len(),
        source_count: sources.len(),
    };
    let snapshot = DbSnapshot {
        project: project.clone(),
        pages,
        sources,
        graph_edges,
    };

    let safe_name = sanitize_file_name(&project.name);
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let filename = format!("wiki-backup-{safe_name}-{timestamp}.zip");
    let out_path = target.join(&filename);

    // 打包在 blocking 线程执行：zip 写入为同步 IO（含 Seek），避免阻塞 tokio 事件循环
    let root_for_blocking = root.clone();
    let meta_json =
        serde_json::to_string_pretty(&meta).map_err(|e| format!("序列化 meta 失败: {e}"))?;
    let db_json =
        serde_json::to_string_pretty(&snapshot).map_err(|e| format!("序列化 db 失败: {e}"))?;
    let out_path_clone = out_path.clone();

    let (file_count, size_bytes) = tokio::task::spawn_blocking(move || {
        write_zip(&out_path_clone, &root_for_blocking, &meta_json, &db_json)
    })
    .await
    .map_err(|e| format!("备份任务失败: {e}"))??;

    Ok(WikiExportResult {
        path: out_path.to_string_lossy().to_string(),
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        page_count: meta.page_count,
        source_count: meta.source_count,
        file_count,
        size_bytes,
    })
}

/// 同步打包：收集项目目录全部文件并写入 zip（阻塞线程内调用）。
fn write_zip(
    out_path: &Path,
    root: &Path,
    meta_json: &str,
    db_json: &str,
) -> Result<(usize, u64), String> {
    let files = collect_files(root)?;

    let file = std::fs::File::create(out_path)
        .map_err(|e| format!("创建备份文件失败 {}: {e}", out_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);

    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // meta.json
    zip.start_file("meta.json", options)
        .map_err(|e| format!("写入 meta.json 失败: {e}"))?;
    zip.write_all(meta_json.as_bytes())
        .map_err(|e| format!("写入 meta.json 失败: {e}"))?;

    // db.json
    zip.start_file("db.json", options)
        .map_err(|e| format!("写入 db.json 失败: {e}"))?;
    zip.write_all(db_json.as_bytes())
        .map_err(|e| format!("写入 db.json 失败: {e}"))?;

    // files/**
    let mut file_count = 0usize;
    for (rel, abs) in &files {
        let entry_name = format!("{}{}", FILES_PREFIX, rel);
        zip.start_file(&entry_name, options)
            .map_err(|e| format!("写入 {entry_name} 失败: {e}"))?;
        let bytes = std::fs::read(abs).map_err(|e| format!("读取 {} 失败: {e}", abs.display()))?;
        zip.write_all(&bytes)
            .map_err(|e| format!("写入 {entry_name} 失败: {e}"))?;
        file_count += 1;
    }

    let file = zip.finish().map_err(|e| format!("完成备份文件失败: {e}"))?;
    let size_bytes = file.metadata().map(|m| m.len()).unwrap_or_default();
    Ok((file_count, size_bytes))
}

/// 递归收集目录下所有文件，返回 (相对路径, 绝对路径)；相对路径统一用 `/` 分隔。
fn collect_files(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("读取目录失败 {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("读取目录项失败: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(|_| "备份路径异常".to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, path));
            }
        }
    }
    // 稳定排序，保证备份内容确定性
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

// ── 导入 ────────────────────────────────────────────────────────────────────

/// 从本地 zip 路径导入备份。
pub async fn import_backup_from_path(
    pool: &sqlx::SqlitePool,
    path: &str,
) -> Result<WikiImportResult, String> {
    let bytes = fs::read(path)
        .await
        .map_err(|e| format!("读取备份文件失败 {path}: {e}"))?;
    import_backup_from_bytes(pool, &bytes).await
}

/// 从 zip 原始字节导入备份（Web 端浏览器选中文件经 base64 传入）。
pub async fn import_backup_from_bytes(
    pool: &sqlx::SqlitePool,
    bytes: &[u8],
) -> Result<WikiImportResult, String> {
    if bytes.is_empty() {
        return Err("备份文件为空".to_string());
    }

    // 1) blocking：解析 zip、校验结构、提取文件字节到内存
    let backup = tokio::task::spawn_blocking({
        let bytes = bytes.to_vec();
        move || parse_backup(&bytes)
    })
    .await
    .map_err(|e| format!("解析备份任务失败: {e}"))??;

    // 2) 生成全新项目 ID，绝不覆盖现有项目
    let new_project_id = Uuid::new_v4().to_string();

    // 3) 落盘：写入项目目录 + 页面 MD 内嵌旧 project_id 替换
    let root = project::project_wiki_dir(&new_project_id);
    write_project_files(&root, &backup, &new_project_id).await?;

    // 4) DB 事务恢复；任一环节失败回滚并清理已写目录
    match restore_db(pool, &backup, &new_project_id, &root).await {
        Ok(r) => Ok(r),
        Err(e) => {
            let _ = fs::remove_dir_all(&root).await;
            Err(format!("恢复数据库失败，已清理未完成的导入: {e}"))
        }
    }
}

/// 解析 zip：校验 meta/db 结构、路径安全，提取全部文件字节。
fn parse_backup(bytes: &[u8]) -> Result<ParsedBackup, String> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("不是有效的 zip 备份文件: {e}"))?;

    // 1) meta.json
    let meta: MetaInfo = {
        let mut file = archive
            .by_name("meta.json")
            .map_err(|_| "备份缺少 meta.json，不是 WaLiAPI Wiki 备份".to_string())?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut file, &mut text)
            .map_err(|e| format!("读取 meta.json 失败: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("meta.json 解析失败: {e}"))?
    };
    if meta.format_version != BACKUP_FORMAT_VERSION {
        return Err(format!(
            "备份格式版本不受支持（当前 {}, 备份 {}），请升级 WaLiAPI 后重试",
            BACKUP_FORMAT_VERSION, meta.format_version
        ));
    }

    // 2) db.json
    let snapshot: DbSnapshot = {
        let mut file = archive
            .by_name("db.json")
            .map_err(|_| "备份缺少 db.json".to_string())?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut file, &mut text)
            .map_err(|e| format!("读取 db.json 失败: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("db.json 解析失败: {e}"))?
    };

    // 3) files/**
    let mut files = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取备份条目失败: {e}"))?;
        let name = entry.name().to_string();
        if !name.starts_with(FILES_PREFIX) {
            continue; // meta.json / db.json 已单独解析
        }
        let rel = name[FILES_PREFIX.len()..].to_string();
        let safe_rel = safe_zip_relative(&rel)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|e| format!("读取备份文件 {rel} 失败: {e}"))?;
        files.push(ExtractedFile {
            rel_path: safe_rel,
            bytes,
        });
    }
    if files.is_empty() {
        return Err("备份中没有项目文件（files/ 为空）".to_string());
    }

    Ok(ParsedBackup {
        meta,
        snapshot,
        files,
    })
}

/// 校验 zip 内相对路径：拒绝空、`\`、绝对路径、`..` / `.` / 空段（`//`）。
fn safe_zip_relative(rel: &str) -> Result<String, String> {
    if rel.is_empty() || rel.contains('\\') || rel.starts_with('/') {
        return Err(format!("备份包含非法路径: {rel}"));
    }
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(format!("备份包含非法路径: {rel}"));
        }
    }
    Ok(rel.to_string())
}

/// 把备份文件写入项目目录；`wiki/**/*.md` 内容里内嵌的旧 project_id 替换为新 ID。
async fn write_project_files(
    root: &Path,
    backup: &ParsedBackup,
    new_project_id: &str,
) -> Result<(), String> {
    let old_marker = format!("/api/wiki/projects/{}/", backup.meta.project_id);
    let new_marker = format!("/api/wiki/projects/{}/", new_project_id);

    for file in &backup.files {
        let target = root.join(&file.rel_path);
        // 父目录逐段创建（rel_path 已过白名单校验，不会逃逸 root）
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("创建目录失败 {}: {e}", parent.display()))?;
        }
        let is_page_md = file.rel_path.starts_with("wiki/") && file.rel_path.ends_with(".md");
        let bytes = if is_page_md && old_marker != new_marker {
            // 页面内容里 LLM 生成的完整图片 URL 内嵌旧 project_id，导入换新 ID 后必须同步改写
            let text = String::from_utf8_lossy(&file.bytes).replace(&old_marker, &new_marker);
            text.into_bytes()
        } else {
            file.bytes.clone()
        };
        fs::write(&target, &bytes)
            .await
            .map_err(|e| format!("写入文件失败 {}: {e}", target.display()))?;
    }

    // 重写 .meta.json（若有）：project_id 换成新 ID
    let meta_path = root.join(".meta.json");
    if meta_path.exists() {
        if let Ok(text) = fs::read_to_string(&meta_path).await {
            let rewritten = text.replace(&old_marker, &new_marker);
            let _ = fs::write(&meta_path, rewritten).await;
        }
    }
    Ok(())
}

/// DB 事务恢复：项目 → 页面 → 源记录 → 图谱边（外键顺序）。
async fn restore_db(
    pool: &sqlx::SqlitePool,
    backup: &ParsedBackup,
    new_project_id: &str,
    root: &Path,
) -> Result<WikiImportResult, String> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("开启数据库事务失败: {e}"))?;

    let p = &backup.snapshot.project;
    let wiki_dir = root.to_string_lossy().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO wiki_projects (id, name, description, status, schema_text, wiki_dir, ingest_model, chat_model, ingest_channel_id, chat_channel_id, pdf_ocr_channel_id, pdf_ocr_model, reasoning_effort, temperature, mcp_enabled, source_count, page_count, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(new_project_id)
    .bind(&p.name)
    .bind(&p.description)
    .bind(p.status)
    .bind(&p.schema_text)
    .bind(&wiki_dir)
    .bind(&p.ingest_model)
    .bind(&p.chat_model)
    .bind(&p.ingest_channel_id)
    .bind(&p.chat_channel_id)
    .bind(&p.pdf_ocr_channel_id)
    .bind(&p.pdf_ocr_model)
    .bind(&p.reasoning_effort)
    .bind(p.temperature)
    .bind(p.mcp_enabled)
    .bind(p.source_count)
    .bind(p.page_count)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("写入项目记录失败: {e}"))?;

    for page in &backup.snapshot.pages {
        sqlx::query(
            "INSERT INTO wiki_pages (id, project_id, path, title, page_type, content_hash, token_count, wikilinks, frontmatter, tags, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        // 页面 id 是内部键（业务上按 project_id+path 定位），原项目共存时
        // 保留原 id 会撞 UNIQUE 约束，故重新生成
        .bind(Uuid::new_v4().to_string())
        .bind(new_project_id)
        .bind(&page.path)
        .bind(&page.title)
        .bind(&page.page_type)
        .bind(&page.content_hash)
        .bind(page.token_count)
        .bind(&page.wikilinks)
        .bind(&page.frontmatter)
        .bind(&page.tags)
        .bind(&page.status)
        .bind(&page.created_at)
        .bind(&page.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("写入页面记录 {} 失败: {e}", page.path))?;
    }

    for source in &backup.snapshot.sources {
        sqlx::query(
            "INSERT INTO wiki_sources (id, project_id, source_type, filename, file_path, source_url, content_hash, file_size, status, page_count, error_message, created_at, ingested_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        // 同页面：源 id 为内部键，原项目共存时需重新生成
        .bind(Uuid::new_v4().to_string())
        .bind(new_project_id)
        .bind(&source.source_type)
        .bind(&source.filename)
        // file_path 指向旧机器绝对路径，导入后不可用；统一置空，
        // 读取走 read_source_file(raw/sources/<filename>)，文件已随 files/ 恢复。
        .bind(Option::<String>::None)
        .bind(&source.source_url)
        .bind(&source.content_hash)
        .bind(source.file_size)
        .bind(&source.status)
        .bind(source.page_count)
        .bind(&source.error_message)
        .bind(&source.created_at)
        .bind(&source.ingested_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("写入源记录 {} 失败: {e}", source.filename))?;
    }

    for edge in &backup.snapshot.graph_edges {
        sqlx::query(
            "INSERT OR IGNORE INTO wiki_graph_edges (id, project_id, source_page, target_page, edge_type, weight, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(Uuid::new_v4().to_string())
        .bind(new_project_id)
        .bind(&edge.source_page)
        .bind(&edge.target_page)
        .bind(&edge.edge_type)
        .bind(edge.weight)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("写入图谱边失败: {e}"))?;
    }

    tx.commit()
        .await
        .map_err(|e| format!("提交数据库事务失败: {e}"))?;

    Ok(WikiImportResult {
        project_id: new_project_id.to_string(),
        project_name: backup.snapshot.project.name.clone(),
        page_count: backup.snapshot.pages.len(),
        source_count: backup.snapshot.sources.len(),
        file_count: backup.files.len(),
    })
}

// ── 内部工具 ────────────────────────────────────────────────────────────────

/// 读取某项目的图谱边（供导出快照）。
async fn load_graph_edges(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<Vec<GraphEdgeRow>, String> {
    let rows = sqlx::query_as::<_, (String, String, String, f64)>(
        "SELECT source_page, target_page, edge_type, weight FROM wiki_graph_edges WHERE project_id = ? ORDER BY source_page",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("DB error: {e}"))?;
    Ok(rows
        .into_iter()
        .map(
            |(source_page, target_page, edge_type, weight)| GraphEdgeRow {
                source_page,
                target_page,
                edge_type,
                weight,
            },
        )
        .collect())
}

/// 项目名转文件名安全片段：非法字符替换为 `_`。
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.bytes().all(|b| b == b'_') {
        "project".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// WALIAPI_DATA_DIR 是进程级环境变量，cargo test 并行执行时多个测试互踩
    /// 会导致数据目录指向错乱（断言假失败）。凡涉及该 env 的测试统一持此锁串行化。
    static DATA_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_data_dir() -> std::sync::MutexGuard<'static, ()> {
        DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn safe_zip_relative_rejects_traversal_and_absolute() {
        for bad in [
            "",
            "..",
            "../x",
            "a/../../b",
            "\\evil",
            "/abs",
            "a/./b",
            "wiki//x",
        ] {
            assert!(safe_zip_relative(bad).is_err(), "must reject: {bad:?}");
        }
        for good in [
            "wiki/index.md",
            "raw/assets/abc123/page-1.jpg",
            "schema/CLAUDE.md",
        ] {
            assert_eq!(safe_zip_relative(good).unwrap(), good);
        }
    }

    #[test]
    fn sanitize_file_name_cleans_illegal_chars() {
        assert_eq!(sanitize_file_name("项目 Wiki A"), "___Wiki_A");
        assert_eq!(sanitize_file_name("a/b:c*d"), "a_b_c_d");
        assert_eq!(sanitize_file_name("..::"), "project");
        assert_eq!(sanitize_file_name("ok-1_2"), "ok-1_2");
    }

    /// 创建内存数据库（跑全量迁移），返回 1 连接的 pool。
    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    /// 完整往返：建项目（含页面/源/图片资产/图谱边）→ 导出 zip → 导入 → 校验内容一致、
    /// 项目 ID 换新、页面内嵌旧 ID 的图片 URL 被替换、图片 token 目录原样保留。
    #[tokio::test]
    async fn round_trip_export_import_preserves_everything() {
        let _data_guard = lock_data_dir();
        let pool = test_pool().await;

        // 数据目录指向独立临时目录，避免污染真实 wiki 数据
        let data_root =
            std::env::temp_dir().join(format!("waliapi-wiki-backup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_root).unwrap();
        std::env::set_var("WALIAPI_DATA_DIR", &data_root);

        // 1) 建项目
        let repo = WikiRepository::new(pool.clone());
        let project_id = Uuid::new_v4().to_string();
        let schema = "# Test Schema".to_string();
        let dir = project::init_project_dir(&project_id, &schema)
            .await
            .unwrap();
        let wiki_dir = dir.to_string_lossy().to_string();
        repo.create_project_with_id(
            &project_id,
            &crate::services::wiki::models::CreateProjectInput {
                name: "备份测试 Wiki".to_string(),
                description: Some("round trip".to_string()),
                ingest_model: None,
                chat_model: None,
                ingest_channel_id: None,
                chat_channel_id: None,
                pdf_ocr_channel_id: None,
                pdf_ocr_model: None,
                reasoning_effort: None,
                temperature: None,
                schema_text: Some(schema),
            },
            &wiki_dir,
        )
        .await
        .unwrap();

        // 2) 页面：内容里含内嵌旧 project_id 的完整图片 URL（LLM 摄入时的真实形态）
        let token = "cc6f0b49c762489c812795f36dab5f9f";
        let page_content = format!(
            "# 页面\n\n![截图](/api/wiki/projects/{project_id}/assets/{token}/page-1-1.jpg)\n\n[[其他页面]]"
        );
        project::write_page(&project_id, "entities/备份页面.md", &page_content)
            .await
            .unwrap();
        repo.upsert_page(
            &project_id,
            "entities/备份页面.md",
            "备份页面",
            "entity",
            "hash123",
            42,
            r#"["其他页面"]"#,
            "{}",
            r#"["测试"]"#,
        )
        .await
        .unwrap();

        // 3) 图片资产：raw/assets/<token>/page-1-1.jpg
        let asset_dir = project::project_wiki_dir(&project_id)
            .join("raw")
            .join("assets")
            .join(token);
        std::fs::create_dir_all(&asset_dir).unwrap();
        std::fs::write(asset_dir.join("page-1-1.jpg"), b"fake-jpeg-bytes").unwrap();

        // 4) 源记录
        project::write_source_file(&project_id, "手册.md", b"source content")
            .await
            .unwrap();
        repo.add_source(
            &project_id,
            &crate::services::wiki::models::AddSourceInput {
                source_type: "md".to_string(),
                filename: "手册.md".to_string(),
                file_path: None,
                source_url: None,
                content: None,
            },
            Some("srchash"),
            14,
        )
        .await
        .unwrap();

        // 5) 图谱边
        sqlx::query(
            "INSERT INTO wiki_graph_edges (id, project_id, source_page, target_page, edge_type, weight, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&project_id)
        .bind("entities/备份页面.md")
        .bind("entities/其他页面.md")
        .bind("wikilink")
        .bind(1.0f64)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        // 6) 导出
        let out_dir = data_root.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let export = export_project(&pool, &project_id, out_dir.to_string_lossy().as_ref())
            .await
            .expect("export must succeed");
        let zip_path = PathBuf::from(&export.path);
        assert!(zip_path.exists(), "zip 文件必须已生成");
        assert_eq!(export.page_count, 1);
        assert_eq!(export.source_count, 1);
        assert!(export.size_bytes > 0);

        // 7) 导入（生成全新 ID）
        let imported = import_backup_from_path(&pool, &zip_path.to_string_lossy())
            .await
            .expect("import must succeed");
        assert_ne!(imported.project_id, project_id, "导入必须使用全新项目 ID");

        // 8) 校验 DB
        let projects = repo.list_projects().await.unwrap();
        assert_eq!(projects.len(), 2, "原项目 + 导入项目共存");
        let new_project = projects
            .iter()
            .find(|p| p.id == imported.project_id)
            .expect("导入的项目必须存在");
        assert_eq!(new_project.name, "备份测试 Wiki");
        assert_eq!(new_project.status, 1);
        assert_eq!(new_project.mcp_enabled, 1);
        assert_eq!(new_project.schema_text.as_deref(), Some("# Test Schema"));

        let new_pages = repo.list_pages(&imported.project_id).await.unwrap();
        assert_eq!(new_pages.len(), 1);
        let page = &new_pages[0];
        assert_eq!(page.path, "entities/备份页面.md");
        assert_eq!(page.token_count, 42);
        assert_eq!(page.wikilinks, r#"["其他页面"]"#);
        assert_eq!(page.tags, r#"["测试"]"#);

        let new_sources = repo.list_sources(&imported.project_id).await.unwrap();
        assert_eq!(new_sources.len(), 1);
        assert_eq!(new_sources[0].filename, "手册.md");
        assert_eq!(new_sources[0].file_size, 14);
        assert!(new_sources[0].file_path.is_none(), "旧绝对路径必须置空");

        let edge_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wiki_graph_edges WHERE project_id = ?")
                .bind(&imported.project_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(edge_count, 1);

        // 9) 校验磁盘：页面内容里旧 project_id 图片 URL 已替换为新 ID，token 原样保留
        let new_root = project::project_wiki_dir(&imported.project_id);
        let new_content = tokio::fs::read_to_string(new_root.join("wiki/entities/备份页面.md"))
            .await
            .unwrap();
        let old_marker = format!("/api/wiki/projects/{project_id}/assets/{token}/");
        let new_marker = format!("/api/wiki/projects/{}/assets/{token}/", imported.project_id);
        assert!(
            !new_content.contains(&old_marker),
            "旧 project_id 必须被替换"
        );
        assert!(
            new_content.contains(&new_marker),
            "新 project_id 必须写入图片 URL"
        );

        // 10) 图片资产文件原样恢复
        let asset_bytes =
            tokio::fs::read(new_root.join(format!("raw/assets/{token}/page-1-1.jpg")))
                .await
                .unwrap();
        assert_eq!(asset_bytes, b"fake-jpeg-bytes");

        // 11) 原项目不受影响
        let orig_content = tokio::fs::read_to_string(
            project::project_wiki_dir(&project_id).join("wiki/entities/备份页面.md"),
        )
        .await
        .unwrap();
        assert!(orig_content.contains(&old_marker), "原项目内容不得被改写");

        // 清理
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// 非法备份文件必须给出明确错误，而不是 panic / 静默成功。
    #[tokio::test]
    async fn import_rejects_invalid_inputs() {
        let _data_guard = lock_data_dir();
        let pool = test_pool().await;

        let err = import_backup_from_bytes(&pool, b"not-a-zip")
            .await
            .unwrap_err();
        assert!(err.contains("zip"), "错误应说明非 zip: {err}");

        // 伪造 zip：只含 files/，缺 meta.json
        let data_root =
            std::env::temp_dir().join(format!("waliapi-wiki-backup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_root).unwrap();
        std::env::set_var("WALIAPI_DATA_DIR", &data_root);
        let fake = data_root.join("fake.zip");
        let file = std::fs::File::create(&fake).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("files/x.md", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        drop(zip);

        let err = import_backup_from_path(&pool, fake.to_string_lossy().as_ref())
            .await
            .unwrap_err();
        assert!(err.contains("meta.json"), "缺 meta.json 必须报错: {err}");

        let _ = std::fs::remove_dir_all(&data_root);
    }
}
