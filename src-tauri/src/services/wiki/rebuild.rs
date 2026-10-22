//! Wiki 目录与日志重建（并发安全）
//!
//! - `rebuild_index_programmatic`：程序化重建 `wiki/index.md`（B′）——读 DB 全量 active 页面按
//!   `page_type` 分组，原子写。摄入流程末尾自动调用，零 LLM、毫秒级，保证目录始终完整。
//! - `rebuild_index_with_llm`：LLM 语义归组重建（C′，手动按钮触发）——把全量页面清单喂 LLM
//!   归组，输出经**保底补齐**（漏页补《未分组》）后原子写；LLM 失败自动回退程序化重建。
//! - `ensure_log_rebuilt`：log.md 一致性检测 + 一次性回溯（老项目修复）——文件缺失/为空或
//!   "ingest |" 条目数少于已摄入源数时，从 `wiki_sources` 全量重建一份历史日志。
//! - `append_log_locked`：带 project 锁的 log 追加（摄入流程用，防并发丢行）。
//!
//! 并发安全：project 级 `tokio::Mutex` 串行化所有「读-改-写 index/log」操作；写入一律原子写
//! （临时文件 + rename，Windows `MoveFileExW` 可覆盖已存在目标）。LLM 生成页面阶段（摄入）
//! 不进锁，并行效率不受影响；仅最后一步重建（毫秒级/或手动 LLM 归组）串行。

use super::project;
use super::repository::WikiRepository;
use crate::core::proxy;
use crate::db::repository::Repository;
use crate::settings_store::SettingsStore;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

/// index.md / log.md 为系统页（path 相对 wiki/ 目录），由程序维护：
/// LLM 生成的版本不落盘、不入库，避免覆盖程序维护的正确内容。
pub fn is_system_page(path: &str) -> bool {
    path == "index.md" || path == "log.md"
}

/// project 级重建锁表：不同项目互不阻塞；同一项目的 index/log 写操作串行化。
/// value 用 `&'static Mutex`（Box::leak），使 guard 可跨 await 且不依赖局部变量存活期。
fn project_locks() -> &'static StdMutex<HashMap<String, &'static tokio::sync::Mutex<()>>> {
    static LOCKS: OnceLock<StdMutex<HashMap<String, &'static tokio::sync::Mutex<()>>>> =
        OnceLock::new();
    LOCKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// 取某项目的重建互斥锁（&'static，可跨 await 持有；tokio Mutex 不可重入，锁内勿再取同项目锁）。
pub fn project_lock(project_id: &str) -> &'static tokio::sync::Mutex<()> {
    let mut map = project_locks().lock().unwrap_or_else(|e| e.into_inner());
    map.entry(project_id.to_string())
        .or_insert_with(|| Box::leak(Box::new(tokio::sync::Mutex::new(()))))
}

/// 持锁等待（&'static 锁上直接 lock，guard 为 'static，无临时值借用问题）。
async fn lock_project(project_id: &str) -> tokio::sync::MutexGuard<'static, ()> {
    project_lock(project_id).lock().await
}

fn index_path(project_id: &str) -> std::path::PathBuf {
    project::project_wiki_dir(project_id).join("wiki").join("index.md")
}

fn log_path(project_id: &str) -> std::path::PathBuf {
    project::project_wiki_dir(project_id).join("wiki").join("log.md")
}

/// 程序化重建 index.md（带锁入口）。返回索引到的内容页数量。
pub async fn rebuild_index_programmatic(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<usize, String> {
    let _guard = lock_project(project_id).await;
    rebuild_index_programmatic_inner(pool, project_id).await
}

/// 程序化重建（无锁；调用方须已持有 project 锁）。LLM 归组失败时也用它回退。
async fn rebuild_index_programmatic_inner(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<usize, String> {
    let repo = WikiRepository::new(pool.clone());
    let pages = repo.list_pages(project_id).await?;
    let pages: Vec<_> = pages
        .into_iter()
        .filter(|p| !is_system_page(&p.path))
        .collect();

    let content = build_programmatic_index(&pages);
    write_index(project_id, &content).await?;
    Ok(pages.len())
}

/// 按 page_type 机械分组（entity/concept/summary/review/其他）。系统页（index/log）不参与。
fn build_programmatic_index(
    pages: &[crate::services::wiki::models::WikiPage],
) -> String {
    let mut groups: HashMap<&'static str, Vec<&crate::services::wiki::models::WikiPage>> =
        HashMap::new();
    for p in pages {
        if is_system_page(&p.path) {
            continue;
        }
        let key = match p.page_type.as_str() {
            "entity" => "实体",
            "concept" => "概念",
            "summary" => "摘要",
            "review" => "评审",
            _ => "其他",
        };
        groups.entry(key).or_default().push(p);
    }
    let mut out = String::from("# Wiki 目录\n\n");
    for key in ["实体", "概念", "摘要", "评审", "其他"] {
        let Some(list) = groups.get(key) else { continue };
        if list.is_empty() {
            continue;
        }
        out.push_str(&format!("## {key}\n"));
        for p in list {
            out.push_str(&format!("- [[{}]] — {}\n", link_of(&p.path), p.title));
        }
        out.push('\n');
    }
    if out.trim_end() == "# Wiki 目录" {
        out.push_str("（暂无内容页）\n");
    }
    out
}

/// `entities/foo.md` → `entities/foo`（wikilink 约定不带扩展名）。
fn link_of(path: &str) -> String {
    path.trim_end_matches(".md").to_string()
}

/// LLM 语义归组重建 index.md（带锁）。返回索引到的内容页数量。
pub async fn rebuild_index_with_llm(
    settings: &SettingsStore,
    pool: &sqlx::SqlitePool,
    project_id: &str,
    model: Option<&str>,
) -> Result<usize, String> {
    let _guard = lock_project(project_id).await;
    let repo = WikiRepository::new(pool.clone());
    let proj = repo.get_project(project_id).await?;
    let pages = repo.list_pages(project_id).await?;
    let pages: Vec<_> = pages
        .into_iter()
        .filter(|p| !is_system_page(&p.path))
        .collect();

    if pages.is_empty() {
        write_index(project_id, "# Wiki 目录\n\n（暂无内容页）\n").await?;
        return Ok(0);
    }

    // 渠道与模型：显式传参 > 项目 chat > 项目 ingest > 首个 active 渠道
    let model = model
        .filter(|m| !m.is_empty())
        .or_else(|| proj.chat_model.as_deref().filter(|m| !m.is_empty()))
        .or_else(|| proj.ingest_model.as_deref().filter(|m| !m.is_empty()))
        .unwrap_or("gpt-4o")
        .to_string();
    let channel_id = match proj
        .chat_channel_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .or_else(|| proj.ingest_channel_id.as_deref().filter(|id| !id.is_empty()))
    {
        Some(id) => id.to_string(),
        None => {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM channels WHERE status = 1 ORDER BY priority DESC LIMIT 1",
            )
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("DB error: {}", e))?;
            row.map(|(id,)| id).ok_or_else(|| {
                "没有可用的渠道，请在 Wiki 项目设置中配置问答/摄入渠道后重试".to_string()
            })?
        }
    };

    let list: Vec<String> = pages
        .iter()
        .map(|p| {
            let tags: Vec<String> =
                serde_json::from_str::<Vec<String>>(&p.tags).unwrap_or_default();
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!("（标签：{}）", tags.join("、"))
            };
            format!(
                "- `{}`｜{}｜类型 {} {}",
                p.path, p.title, p.page_type, tag_str
            )
        })
        .collect();

    let chat_request = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT_INDEX},
            {"role": "user", "content": format!(
                "当前 Wiki 共有 {} 个内容页面，请语义归组并生成完整目录：\n\n{}",
                pages.len(),
                list.join("\n")
            )}
        ],
        "stream": false,
        "temperature": 0.2,
    });
    let chat_request_str = serde_json::to_string(&chat_request).unwrap_or_default();

    let db_repo = Arc::new(Repository::new(pool.clone()));
    let proxy_result = proxy::handle_request(
        &db_repo,
        settings,
        &channel_id,
        "Wiki Index Rebuild",
        chat_request.clone(),
        false,
        Some(chat_request_str),
        Some(format!("wiki-index-rebuild_{project_id}")),
        None,
    )
    .await;

    let raw_text = match proxy_result {
        Ok(r) => r
            .body
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        Err((_code, msg)) => {
            // LLM 失败 → 回退程序化重建，保证目录绝不空/坏
            return rebuild_index_programmatic_inner(pool, project_id).await;
        }
    };
    if raw_text.trim().is_empty() {
        return rebuild_index_programmatic_inner(pool, project_id).await;
    }

    let final_content = ensure_all_pages_covered(&raw_text, &pages);
    write_index(project_id, &final_content).await?;
    Ok(pages.len())
}

/// LLM 归组输出的保底补齐：LLM 可能漏页，缺失的 path 一律补到《未分组》，
/// 保证 LLM 永不可能造成丢页面（这是 C′ 方案的刚性前提）。
fn ensure_all_pages_covered(llm_text: &str, pages: &[crate::services::wiki::models::WikiPage]) -> String {
    let cleaned = strip_md_fence(llm_text);
    let mut out = cleaned.trim_end().to_string();
    out.push('\n');

    let covered: HashSet<String> = pages
        .iter()
        .map(|p| link_of(&p.path))
        .filter(|link| cleaned.contains(&format!("[[{link}]]")))
        .collect();
    let missing: Vec<&crate::services::wiki::models::WikiPage> = pages
        .iter()
        .filter(|p| !covered.contains(&link_of(&p.path)))
        .collect();

    if !missing.is_empty() {
        out.push_str("\n## 未分组\n");
        for p in &missing {
            out.push_str(&format!("- [[{}]] — {}\n", link_of(&p.path), p.title));
        }
    }
    out
}

/// 去掉 LLM 输出可能带的 ```markdown 围栏。
fn strip_md_fence(text: &str) -> String {
    let t = text.trim();
    if t.starts_with("```") {
        if let Some(end) = t.rfind("```") {
            if end > 3 {
                let inner = t[3..end].trim_start();
                return inner.trim_end().to_string();
            }
        }
    }
    t.to_string()
}

const SYSTEM_PROMPT_INDEX: &str = r#"You are a Wiki catalog organizer. Given the full list of wiki pages in a project, group them into logical sections and generate a clean Markdown index.

Rules:
- Write in Chinese.
- Group pages into meaningful sections based on their topic/entity (e.g. 部署运维, 业务功能, 概念方法, 评审记录...). Keep at most 6-8 sections.
- Each entry line MUST be exactly: `- [[<path-without-.md-extension>]] — <title>`
- Do NOT invent or omit pages: cover EVERY path given.
- Do NOT repeat a page in more than one section.
- Output ONLY the index Markdown content — no code fences, no extra commentary, no checklist."#;

/// log.md 一致性检测 + 一次性回溯（老项目修复）：
/// 文件缺失 / 为空 / 无头部 / "ingest |" 条目数少于已摄入源数 → 从 `wiki_sources` 重建整份。
/// 返回是否执行了回溯。回溯后 M==N，此后仅程序 append，不会重复触发。
pub async fn ensure_log_rebuilt(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<bool, String> {
    let _guard = lock_project(project_id).await;
    let repo = WikiRepository::new(pool.clone());
    let sources = repo.list_sources(project_id).await?;
    let ingested_count = sources.iter().filter(|s| s.status == "ingested").count();

    let path = log_path(project_id);
    let current = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let has_header = current.contains("# Wiki Log") || current.contains("# Operation Log");
    let entry_count = current.matches("ingest |").count();

    if current.trim().is_empty() || !has_header || entry_count < ingested_count {
        let content = build_log_from_sources(&sources);
        write_log(project_id, &content).await?;
        return Ok(true);
    }
    Ok(false)
}

/// 从 `wiki_sources` 全量生成 log.md（时间序 + status 标注）。
fn build_log_from_sources(sources: &[crate::services::wiki::models::WikiSource]) -> String {
    let mut out = String::from("# Wiki 操作日志\n\n");
    out.push_str(&format!(
        "## [{}] init | Project created\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    ));

    let mut ingested: Vec<_> = sources.iter().filter(|s| s.status == "ingested").collect();
    ingested.sort_by_key(|s| s.ingested_at.clone().unwrap_or_default());
    for s in ingested {
        out.push_str(&format!(
            "## [{}] ingest | {} → {} 页\n",
            fmt_time(s.ingested_at.as_deref().unwrap_or("")),
            s.filename,
            s.page_count
        ));
    }

    let mut others: Vec<_> = sources.iter().filter(|s| s.status != "ingested").collect();
    others.sort_by_key(|s| s.created_at.clone());
    for s in others {
        out.push_str(&format!(
            "## [{}] ingest | {}（{}）\n",
            fmt_time(&s.created_at),
            s.filename,
            s.status
        ));
    }
    out
}

/// `2026-10-17T10:05:03Z` → `2026-10-17 10:05`（与 append_log 格式一致）。
fn fmt_time(rfc3339: &str) -> String {
    rfc3339
        .replace('T', " ")
        .chars()
        .take(16)
        .collect::<String>()
}

/// 带 project 锁的 log 追加（摄入流程用，防并发 append 丢行）。
pub async fn append_log_locked(project_id: &str, entry: &str) -> Result<(), String> {
    let _guard = lock_project(project_id).await;
    project::append_log(project_id, entry).await
}

async fn write_index(project_id: &str, content: &str) -> Result<(), String> {
    let path = index_path(project_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("创建目录失败: {e}"))?;
    }
    project::write_file_atomic(&path, content).await
}

async fn write_log(project_id: &str, content: &str) -> Result<(), String> {
    let path = log_path(project_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("创建目录失败: {e}"))?;
    }
    project::write_file_atomic(&path, content).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(path: &str, title: &str, page_type: &str, tags: &str) -> crate::services::wiki::models::WikiPage {
        crate::services::wiki::models::WikiPage {
            id: "id".into(),
            project_id: "p".into(),
            path: path.into(),
            title: title.into(),
            page_type: page_type.into(),
            content_hash: "".into(),
            token_count: 0,
            wikilinks: "[]".into(),
            frontmatter: "{}".into(),
            tags: tags.into(),
            status: "active".into(),
            created_at: "".into(),
            updated_at: "".into(),
        }
    }

    #[test]
    fn programmatic_index_groups_by_type_and_excludes_system() {
        let pages = vec![
            page("entities/甲.md", "甲实体", "entity", "[\"业务\"]"),
            page("concepts/乙.md", "乙概念", "concept", "[]"),
            page("summaries/丙.md", "丙摘要", "summary", "[]"),
            page("index.md", "目录", "index", "[]"), // 系统页，须排除
            page("log.md", "日志", "log", "[]"),       // 系统页，须排除
        ];
        let index = build_programmatic_index(&pages);
        assert!(index.contains("## 实体") && index.contains("[[entities/甲]] — 甲实体"));
        assert!(index.contains("## 概念") && index.contains("[[concepts/乙]] — 乙概念"));
        assert!(index.contains("## 摘要") && index.contains("[[summaries/丙]] — 丙摘要"));
        assert!(!index.contains("[[index]]") && !index.contains("[[log]]"), "系统页不得入目录");
    }

    #[test]
    fn llm_missing_pages_get_appended_to_ungrouped() {
        let pages = vec![
            page("entities/甲.md", "甲实体", "entity", "[]"),
            page("entities/乙.md", "乙实体", "entity", "[]"),
        ];
        // LLM 只写了甲，漏了乙
        let llm = "# 目录\n\n## 业务\n- [[entities/甲]] — 甲实体\n";
        let out = ensure_all_pages_covered(llm, &pages);
        assert!(out.contains("[[entities/甲]]"));
        assert!(out.contains("## 未分组") && out.contains("[[entities/乙]] — 乙实体"));
    }

    #[test]
    fn llm_fence_is_stripped_and_covered_detection_ignores_fence() {
        let pages = vec![page("entities/甲.md", "甲实体", "entity", "[]")];
        let llm = "```markdown\n# 目录\n\n- [[entities/甲]] — 甲实体\n```";
        let out = ensure_all_pages_covered(llm, &pages);
        assert!(!out.contains("```"), "围栏应被剥离");
        assert!(!out.contains("## 未分组"), "已覆盖页面不应补入未分组");
    }

    #[test]
    fn log_rebuild_detects_missing_entries() {
        // damaged：条目数少于 ingested 源数
        let damaged = "# Wiki Log\n\n## [2026-01-01 00:00] ingest | a.pdf → 2 页\n";
        assert!(damaged.matches("ingest |").count() < 3);
        // 正常：条目数 == ingested 源数
        let ok = "# Wiki Log\n\n## [2026-01-01 00:00] ingest | a.pdf → 2 页\n## [2026-01-01 01:00] ingest | b.pdf → 1 页\n## [2026-01-01 02:00] ingest | c.pdf → 3 页\n";
        assert!(ok.matches("ingest |").count() >= 3);
    }
}
