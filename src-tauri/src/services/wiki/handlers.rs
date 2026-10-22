use super::ingest;
use super::models::*;
use super::project;
use super::repository::WikiRepository;
use crate::core::proxy;
use crate::db::repository::Repository;
use crate::server::router::SharedState;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

fn default_top_k() -> usize {
    10
}

// ── Project handlers ──

pub async fn list_projects(State(shared): State<SharedState>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.list_projects().await {
        Ok(projects) => Json(serde_json::json!({ "data": projects })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn create_project(
    State(shared): State<SharedState>,
    Json(input): Json<CreateProjectInput>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    let project_id = project::new_uuid();
    let schema = input
        .schema_text
        .clone()
        .unwrap_or_else(|| super::repository::DEFAULT_SCHEMA.to_string());

    // Create directory structure
    if let Err(e) = project::init_project_dir(&project_id, &schema).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }

    let dir = project::project_wiki_dir(&project_id);
    let wiki_dir = dir.to_string_lossy().to_string();

    match repo.create_project(&input, &wiki_dir).await {
        Ok(p) => (StatusCode::CREATED, Json(p)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn get_project(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.get_project(&id).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

pub async fn update_project(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateProjectInput>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());

    // If schema_text changed, write to disk
    if let Some(ref schema) = input.schema_text {
        let dir = project::project_wiki_dir(&id);
        let schema_path = dir.join("schema").join("CLAUDE.md");
        let _ = tokio::fs::write(schema_path, schema).await;
    }

    match repo.update_project(&id, &input).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn delete_project(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    if let Err(e) = repo.delete_project(&id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    if let Err(e) = project::remove_project_dir(&id).await {
        tracing::warn!("Failed to remove project dir: {}", e);
    }
    (StatusCode::NO_CONTENT, "").into_response()
}

pub async fn get_project_stats(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.get_stats(&id).await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ── Source handlers ──

pub async fn list_sources(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.list_sources(&id).await {
        Ok(sources) => Json(serde_json::json!({ "data": sources })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn add_source(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
    Json(input): Json<AddSourceInput>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());

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

    // If content provided, write to disk
    if let Some(ref content) = content_bytes {
        if let Err(e) = project::write_source_file(&id, &input.filename, content).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    }

    match repo
        .add_source(&id, &input, content_hash.as_deref(), file_size)
        .await
    {
        Ok(s) => (StatusCode::CREATED, Json(s)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn delete_source(
    State(shared): State<SharedState>,
    Path((_id, sid)): Path<(String, String)>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    if let Err(e) = repo.delete_source(&sid).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    (StatusCode::NO_CONTENT, "").into_response()
}

pub async fn ingest_source(
    State(shared): State<SharedState>,
    Path((id, sid)): Path<(String, String)>,
) -> Response {
    let events = shared.state.events.clone();
    let settings = shared.state.settings.clone();
    let pool = shared.state.db.pool.clone();

    // FIX-22：真后台化——摄入含 LLM 生成，可能跑数十秒到数分钟，此前
    // 注释写后台但实际 await 全程，HTTP 连接被占满。现在立即返回 202，
    // 摄入在后台执行；任务/来源状态由 ingest::ingest_source 统一收敛
    // （失败落 failed），进度经 `wiki-source-progress` 事件流下发。
    let source_id = sid.clone();
    tokio::spawn(async move {
        if let Err(e) = ingest::ingest_source(&events, &settings, &pool, &id, &sid).await {
            tracing::warn!(project = %id, source = %sid, error = %e, "wiki ingest failed (状态已收敛为 failed)");
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "started",
            "source_id": source_id,
        })),
    )
        .into_response()
}

pub async fn rescan_sources(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let events = shared.state.events.clone();
    let settings = shared.state.settings.clone();
    let pool = shared.state.db.pool.clone();
    let repo = WikiRepository::new(pool.clone());

    // Get all pending sources and ingest them
    let sources = match repo.list_sources(&id).await {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    let pending: Vec<_> = sources.iter().filter(|s| s.status == "pending").collect();
    let mut results = Vec::new();

    for source in &pending {
        match ingest::ingest_source(&events, &settings, &pool, &id, &source.id).await {
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

    Json(serde_json::json!({
        "status": "done",
        "processed": pending.len(),
        "results": results,
    }))
    .into_response()
}

// ── Page handlers ──

pub async fn list_pages(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.list_pages(&id).await {
        Ok(pages) => Json(serde_json::json!({ "data": pages })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn get_page(
    State(shared): State<SharedState>,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());

    // Try DB first
    if let Ok(Some(page)) = repo.get_page(&id, &path).await {
        // Also read file content
        if let Ok(content) = project::read_page(&id, &path).await {
            return Json(serde_json::json!({
                "id": page.id,
                "project_id": page.project_id,
                "path": page.path,
                "title": page.title,
                "page_type": page.page_type,
                "content_hash": page.content_hash,
                "token_count": page.token_count,
                "wikilinks": page.wikilinks,
                "frontmatter": page.frontmatter,
                "status": page.status,
                "content": content,
                "created_at": page.created_at,
                "updated_at": page.updated_at,
            }))
            .into_response();
        }
    }

    // Try reading file directly from disk
    match project::read_page(&id, &path).await {
        Ok(content) => {
            let title = path
                .split('/')
                .last()
                .unwrap_or(&path)
                .trim_end_matches(".md")
                .to_string();
            Json(serde_json::json!({
                "path": path,
                "title": title,
                "content": content,
                "page_type": "unknown",
            }))
            .into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

pub async fn update_page(
    State(shared): State<SharedState>,
    Path((id, path)): Path<(String, String)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let content = body.get("content").and_then(|c| c.as_str()).unwrap_or("");

    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match update_page_inner(&shared.state.db.pool, &repo, &id, &path, content).await {
        Ok(()) => Json(serde_json::json!({ "ok": true, "path": path })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Inner logic for saving a wiki page — shared by HTTP handler and MCP handler.
pub async fn update_page_inner(
    pool: &sqlx::SqlitePool,
    repo: &WikiRepository,
    id: &str,
    path: &str,
    content: &str,
) -> Result<(), String> {
    if let Err(e) = project::write_page(id, path, content).await {
        return Err(e);
    }

    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let title = ingest::extract_title_from_content(content, path);
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

    let token_count = (content.len() / 4) as i64; // rough estimate

    // Extract wikilinks
    let wikilinks: Vec<String> = extract_wikilinks(content);
    let wikilinks_json = serde_json::to_string(&wikilinks).unwrap_or("[]".to_string());

    // Extract tags from frontmatter
    let tags = crate::services::wiki::ingest::extract_tags_from_frontmatter(content);
    let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());

    repo.upsert_page(
        id,
        path,
        &title,
        page_type,
        &hash,
        token_count,
        &wikilinks_json,
        "{}",
        &tags_json,
    )
    .await?;

    // Rebuild knowledge graph edges based on updated wikilinks
    let _ = ingest::rebuild_graph_edges(pool, id).await;

    // Append log
    let _ = project::append_log(id, &format!("update | {}", path)).await;

    Ok(())
}

pub async fn delete_page(
    State(shared): State<SharedState>,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    if let Err(e) = repo.delete_page(&id, &path).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    let _ = project::delete_page_file(&id, &path).await;
    // Rebuild graph edges after page deletion
    let _ = ingest::rebuild_graph_edges(&shared.state.db.pool, &id).await;
    (StatusCode::NO_CONTENT, "").into_response()
}

// ── Search & Ask ──

pub async fn search(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
    Query(params): Query<SearchQuery>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.search_pages(&id, &params.q, params.top_k).await {
        Ok(results) => {
            Json(serde_json::json!({ "data": results, "query": params.q })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn ask(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<WikiAskInput>,
) -> Response {
    let top_k = input.top_k.unwrap_or(5);
    let model = input.model.as_deref();
    let base_url = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| format!("http://{h}"));

    match ask_inner(
        &shared,
        &id,
        &input.question,
        top_k,
        model,
        base_url.as_deref(),
        None,
        None,
    )
    .await
    {
        Ok(json) => Json(json).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// 从页面内容里收集资产映射：`filename → /api/wiki/projects/<pid>/assets/<hash>/<file>`。
/// 供回答后处理使用：LLM 常只写裸文件名（`page-1-1.jpg`）或表格列举文件名，
/// 服务端据此把文件名还原成可渲染的完整图片地址。
fn collect_asset_map(text: &str, project_id: &str, out: &mut Vec<(String, String)>) {
    let marker = "assets/";
    let mut rest = text;
    while let Some(pos) = rest.find(marker) {
        let after = &rest[pos + marker.len()..];
        let hash_len = after.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        if hash_len == 32 && after.as_bytes().get(32) == Some(&b'/') {
            let filename: String = after[33..]
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, ')' | ']' | '"' | '\''))
                .collect();
            if !filename.is_empty() {
                let url = format!(
                    "/api/wiki/projects/{}/assets/{}/{}",
                    project_id,
                    &after[..32],
                    filename
                );
                if !out.iter().any(|(f, _)| f == &filename) {
                    out.push((filename, url));
                }
            }
            let advance = 33.min(after.len());
            rest = &after[advance..];
        } else {
            rest = &after[hash_len.min(after.len())..];
        }
        if rest.is_empty() {
            break;
        }
    }
}

/// 把回答里的图片引用还原成完整 markdown 图片（不依赖 LLM 输出形态）。
/// 覆盖 LLM 常见写法（以 `page-1-1.jpg`、hash `cc6f0b49.../page-1-1.jpg` 为例）：
///   - `<hash>/<file>`（LLM 最常用：省略 `/api/wiki/projects/<pid>/assets/` 前缀）
///   - `assets/<hash>/<file>` 与 `.../assets/<hash>/<file>`（省略号缩写）
///   - `.../<file>`、裸 `<file>`
/// 已在回答里出现的完整相对/绝对地址先被保护，替换完成后统一交给
/// `absolutize_asset_urls` 补 `http://host:port`。
fn expand_image_refs(text: &str, assets: &[(String, String)], base_url: Option<&str>) -> String {
    let base = base_url.filter(|b| !b.is_empty()).unwrap_or("");
    let mut out = text.to_string();
    let mut guards: Vec<(String, String)> = Vec::new();

    // 0) 先整体保护"已是完整绝对地址"的图片 URL（http(s)://…/api/wiki/projects/<pid>/assets/<32hex>/<file>）。
    //    不依赖 asset_map —— LLM 可能引用 snippet 未收集到的图片；否则后续把"裸文件名"替换成
    //    markdown 图片时，会连 URL 内部的文件名也误替换，产生嵌套损坏（实测：
    //    …/assets/ea2d…/![page-2-1.jpg](http://…)）。
    let marker = "/api/wiki/projects/";
    let mut scan = 0usize;
    while scan < out.len() {
        let Some(pos) = out[scan..].find(marker) else {
            break;
        };
        let abs_pos = scan + pos;
        let head = &out[..abs_pos];
        let scheme_start = ["http://", "https://"]
            .iter()
            .filter_map(|s| head.rfind(s))
            .max();
        if let Some(ss) = scheme_start {
            let marker_end = abs_pos + marker.len();
            let after = &out[marker_end..];
            let end = after
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '\'' | '"'))
                .unwrap_or(after.len());
            let url_end = marker_end + end;
            let abs_url = out[ss..url_end].to_string();
            let guard = format!("\u{0}ABS{}X\u{0}", guards.len());
            out = format!("{}{}{}", &out[..ss], guard, &out[url_end..]);
            guards.push((guard.clone(), abs_url));
            scan = ss + guard.len();
        } else {
            scan = abs_pos + marker.len();
        }
    }

    // 1) 保护已含完整地址的引用（避免二次改写为嵌套结构）
    for (i, (_, rel)) in assets.iter().enumerate() {
        let abs = format!("{base}{rel}");
        let guard = format!("\u{0}IMG{i}X\u{0}");
        if out.contains(abs.as_str()) {
            out = out.replace(abs.as_str(), &guard);
            guards.push((guard, abs));
        } else if out.contains(rel.as_str()) {
            out = out.replace(rel.as_str(), &guard);
            guards.push((guard, format!("{base}{rel}")));
        }
    }
    // 2) 把省略形态替换为完整 markdown 图片（每个文件名取最具体命中的形态替换一次）
    for (filename, rel) in assets.iter() {
        let absolute = format!("{base}{rel}");
        let segments: Vec<&str> = rel.split('/').collect();
        let hash = if segments.len() >= 2 {
            segments[segments.len() - 2]
        } else {
            ""
        };
        let mut patterns: Vec<String> = Vec::new();
        if !hash.is_empty() {
            patterns.push(format!("assets/{hash}/{filename}"));
            patterns.push(format!("{hash}/{filename}"));
        }
        patterns.push(format!(".../{filename}"));
        patterns.push(format!("…/{filename}"));
        patterns.push(filename.clone());
        for pattern in patterns {
            if out.contains(&pattern) {
                out = out.replace(&pattern, &format!("![{}]({})", filename, absolute));
                break;
            }
        }
    }
    // 3) 恢复被保护的引用
    for (guard, value) in guards {
        out = out.replace(&guard, &value);
    }
    out
}

/// 把文本中的图像 URL 统一成 `base_url + /api/wiki/...`。
/// 关键：LLM 常**自行编造 host 前缀**（`https://<wiki-host>/api/wiki/...`、
/// `http://localhost:8777/api/wiki/...`、错误端口等），这类占位/错误前缀必须被
/// **替换**为真实 base，而不是"已含 :// 就放过"——否则客户端拿到不可用地址。
/// base_url 为空时原样返回。
pub fn absolutize_asset_urls(text: &str, base_url: Option<&str>) -> String {
    let marker = "/api/wiki/projects/";
    if !text.contains(marker) {
        return text.to_string();
    }
    let Some(base) = base_url.filter(|b| !b.is_empty()) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(text.len() + 64);
    let mut rest = text;
    while let Some(pos) = rest.find(marker) {
        let before = &rest[..pos];
        match url_prefix_segment_start(before) {
            // marker 前存在一段 URL 前缀（真实或占位）→ 整段换成 base（幂等）
            Some(start) => {
                out.push_str(&before[..start]);
                out.push_str(base);
            }
            None => {
                out.push_str(before);
                out.push_str(base);
            }
        }
        out.push_str(marker);
        rest = &rest[pos + marker.len()..];
    }
    out.push_str(rest);
    out
}

/// 找出 `before` 末尾那段 URL 前缀（`scheme://host[:port]`）的起始字节位置。
/// 返回 None 表示 marker 前没有 URL 前缀段（纯 root-relative，需要补 base）。
fn url_prefix_segment_start(before: &str) -> Option<usize> {
    let scheme_pos = before.rfind("://")?;
    // 段起点 = scheme 之前最近的分隔符之后；分隔符不含 < > （占位符 <wiki-host> 属于同一段）
    let head = &before[..scheme_pos];
    let start = head
        .char_indices()
        .rev()
        .find(|(_, c)| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\''
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '|'
                        | ','
                        | ';'
                        | '、'
                        | '，'
                        | '：'
                        | '。'
                        | '；'
                        | '（'
                        | '）'
                )
        })
        // 用该 char 的 UTF-8 长度前进：中文标点（：，。等）为 3 字节，
        // 旧实现 `p + 1` 会切在字符中间 → panic: byte index is not a char boundary。
        .map(|(p, c)| p + c.len_utf8())
        .unwrap_or(0);
    // 段内若含空白则不是同一 URL 段（例如上一句里的 http:// 与本 URL 无关）
    let segment = &before[start..];
    if segment.is_empty() || segment.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(start)
}

/// 补全 LLM 回答里被省略前缀的图片片段。
/// 模型常把 URL 写成 `.../assets/<32hex>/page-1.jpg` 或在末尾才给出相对前缀——
/// 本函数扫描 `assets/<32hex>/<file>`，凡未带 `projects/<pid>/` 完整前缀的，
/// 统一补成相对完整路径 `/api/wiki/projects/<pid>/assets/<hash>/<file>`，
/// 再交给 `absolutize_asset_urls` 补上 `http://host:port`。
pub fn complete_asset_urls(text: &str, project_id: &str) -> String {
    if project_id.is_empty() {
        return text.to_string();
    }
    let full_prefix = format!("/api/wiki/projects/{}/assets/", project_id);
    let already_suffix = format!("projects/{}/", project_id);
    let marker = "assets/";
    let mut out = String::with_capacity(text.len() + 64);
    let mut rest = text;
    while let Some(pos) = rest.find(marker) {
        let before = &rest[..pos];
        let after = &rest[pos + marker.len()..];
        // after 应形如 <32位hex>/<文件名>
        let hash_len = after.chars().take_while(|c| c.is_ascii_hexdigit()).count();
        if hash_len == 32 && after.as_bytes().get(32) == Some(&b'/') {
            let filename: String = after[33..]
                .chars()
                .take_while(|c| {
                    !c.is_whitespace()
                        && !matches!(c, ')' | '、' | ',' | '，' | ']' | '>' | '"' | '\'')
                })
                .collect();
            let is_image = filename.ends_with(".jpg")
                || filename.ends_with(".jpeg")
                || filename.ends_with(".png");
            if is_image && !filename.is_empty() {
                if before.ends_with(&already_suffix) {
                    // 已带 projects/<pid>/ 前缀：原样保留
                    out.push_str(before);
                    out.push_str(marker);
                } else {
                    // 省略了前缀（含 "..." / "/" 残片）：裁掉省略号后补完整相对路径
                    let trimmed = before
                        .trim_end_matches(|c: char| c == '.' || c == '…' || c == '/' || c == ' ');
                    out.push_str(trimmed);
                    out.push_str(&full_prefix);
                }
                let end = 33 + filename.len();
                out.push_str(&after[..end]);
                rest = &after[end..];
                continue;
            }
        }
        out.push_str(before);
        out.push_str(marker);
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Inner logic for Wiki Q&A — shared by HTTP handler and MCP handler.
pub async fn ask_inner(
    shared: &SharedState,
    id: &str,
    question: &str,
    top_k: usize,
    model_override: Option<&str>,
    base_url: Option<&str>,
    // 调用方 API Key 身份（MCP ask_wiki 传真实 key，审计日志按此归因；
    // HTTP 面板路径为 None，回退到渠道 ID + "Wiki Chat"）。
    client_key_id: Option<&str>,
    client_key_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    let pool = &shared.state.db.pool;
    let repo = WikiRepository::new(pool.clone());
    let db_repo = Arc::new(Repository::new(pool.clone()));
    let settings = shared.state.settings.clone();

    // 图片 URL 绝对化兜底：未从请求头取到 Host（如某些 MCP 通道）时，
    // 用本机监听端口构造 http://127.0.0.1:{port}，保证回答里是完整可渲染地址。
    // 方案 B：三层兜底 —— 运行时端口 → 设置中心端口（一定有值）→ 默认 8777，
    // 确保 MCP/HTTP 路径 base 永不空（历史多轮"LLM 编占位 host"的根因就是 base 取不到）。
    let base_url = match base_url.filter(|b| !b.is_empty()) {
        Some(b) => Some(b.to_string()),
        None => {
            let runtime_port = *shared.state.server_port.read().await;
            let port = if runtime_port > 0 {
                runtime_port
            } else {
                settings.get_u64("server.port", 8777) as u16
            };
            Some(format!("http://127.0.0.1:{port}"))
        }
    };

    // Search relevant pages
    let results = repo
        .search_pages(id, question, top_k)
        .await
        .unwrap_or_default();

    // Read page contents（页面内 root-relative 图像 URL 在此按请求地址绝对化，客户端可直接渲染）
    let mut contexts = Vec::new();
    // 资产映射：filename → 完整相对 URL（供回答后处理把 LLM 的裸文件名还原成图片）
    let mut asset_map: Vec<(String, String)> = Vec::new();
    for r in &results {
        if let Ok(content) = project::read_page(id, &r.path).await {
            let snippet: String = content.chars().take(2000).collect();
            collect_asset_map(&snippet, id, &mut asset_map);
            contexts.push(format!(
                "## {} ({})\n{}",
                r.title,
                r.path,
                absolutize_asset_urls(&snippet, base_url.as_deref())
            ));
        }
    }

    if contexts.is_empty() {
        return Ok(serde_json::json!({
            "answer": "No relevant wiki pages found for your question. Please ingest some documents first.",
            "sources": []
        }));
    }

    let context_text = contexts.join("\n\n---\n\n");

    // Get project config
    let proj = repo.get_project(id).await?;

    let chat_model = model_override
        .or(proj.chat_model.as_deref())
        .unwrap_or("gpt-4o");
    let chat_channel_id = match proj.chat_channel_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => {
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM channels WHERE status = 1 ORDER BY priority DESC LIMIT 1",
            )
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("DB error: {}", e))?;
            match row.map(|(id,)| id) {
                Some(id) => id,
                None => return Err("No active channel configured. Please create a channel first or set chat_channel_id in Wiki project settings.".to_string()),
            }
        }
    };

    let system_prompt = "You are a Wiki knowledge assistant. Answer questions based on the provided wiki pages. Be concise and cite source pages using [[wikilinks]] format. If a wiki page contains image links (markdown image URLs), you MUST reproduce them as COMPLETE markdown image links ![description](full-url) using the full absolute URL exactly as given (including http://host:port and the whole /api/wiki/... path). Every image MUST appear as a standalone markdown image ![](url) in your answer. NEVER merely list image file names (e.g. page-1-1.jpg) in a table or text without the full markdown syntax, and NEVER abbreviate, shorten, truncate, or replace the URL prefix with \"...\" or any placeholder.";
    let user_prompt = format!(
        "Based on the following wiki pages, answer the question.\n\nWiki pages:\n{}\n\nQuestion: {}\n\nAnswer:",
        context_text, question
    );

    // Save user message
    let _ = repo.add_session(id, "user", question, None, None).await;

    let mut chat_request = serde_json::json!({
        "model": chat_model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_prompt}
        ],
        "stream": false,
        "temperature": proj.temperature.unwrap_or(0.4)
    });
    // 项目配置的推理档位（Wiki 设置→推理档位）；未配置或 default 时不设，跟随模型默认
    if let Some(effort) = proj
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "default")
    {
        chat_request["reasoning_effort"] = serde_json::json!(effort);
        // 同步显式 thinking：启用类→enabled，none/off→disabled（用户要求档位与 thinking 联动）
        match effort {
            "none" | "off" => {
                chat_request["thinking"] = serde_json::json!({"type": "disabled"});
            }
            _ => {
                chat_request["thinking"] = serde_json::json!({"type": "enabled"});
            }
        }
    }
    let chat_request_str: String = serde_json::to_string(&chat_request).unwrap_or_default();

    // 审计归因：有真实调用方 key 用 key；否则回退到渠道 ID + "Wiki Chat"（历史行为）。
    let log_api_key_id: &str = client_key_id.unwrap_or(&chat_channel_id);
    let log_api_key_name: &str = client_key_name.unwrap_or("Wiki Chat");

    let proxy_result = proxy::handle_request(
        &db_repo,
        &settings,
        log_api_key_id,
        log_api_key_name,
        chat_request,
        false,
        Some(chat_request_str),
        Some(format!("wiki-chat_{}", id)),
        None,
    )
    .await;

    let (answer, usage) = match proxy_result {
        Ok(result) => {
            let answer_text = result
                .body
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|t| t.as_str())
                .unwrap_or("Failed to generate answer.");

            let usage = result.body.get("usage").map(|u| serde_json::json!({
                "prompt_tokens": u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                "completion_tokens": u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                "total_tokens": u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            }));

            (answer_text.to_string(), usage)
        }
        Err((code, msg)) => {
            let err_answer = format!("LLM request failed ({}): {}", code, msg);
            (err_answer, None)
        }
    };

    let sources: Vec<WikiAnswerSource> = results
        .iter()
        .map(|r| WikiAnswerSource {
            path: r.path.clone(),
            title: r.title.clone(),
            score: r.score,
            snippet: r.snippet.clone(),
        })
        .collect();

    // 最后一道保险：LLM 可能把图片写成裸文件名（`page-1-1.jpg`）、省略前缀的相对路径、
    // 或完整 URL。三步依次处理，保证客户端拿到的一定是完整可渲染的 markdown 图片：
    //   ① 补全被省略的 `assets/<hash>/<file>` 片段
    //   ② 把裸文件名还原成 `![file](完整URL)`（映射来自检索页面）
    //   ③ 统一补上 http://host:port
    let answer = complete_asset_urls(&answer, id);
    let answer = expand_image_refs(&answer, &asset_map, base_url.as_deref());
    let answer = absolutize_asset_urls(&answer, base_url.as_deref());

    // Save assistant message
    let _ = repo
        .add_session(
            id,
            "assistant",
            &answer,
            Some(&serde_json::to_string(&sources).unwrap_or_default()),
            Some(chat_model),
        )
        .await;

    Ok(serde_json::json!({
        "answer": answer,
        "sources": sources,
        "usage": usage,
    }))
}

// ── Graph ──

pub async fn get_graph(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.get_graph(&id).await {
        Ok(graph) => Json(graph).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ── Sessions ──

pub async fn list_sessions(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.list_sessions(&id).await {
        Ok(sessions) => Json(serde_json::json!({ "data": sessions })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub async fn clear_sessions(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    if let Err(e) = repo.clear_sessions(&id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
    }
    (StatusCode::NO_CONTENT, "").into_response()
}

// ── Queue ──

pub async fn get_queue_status(
    State(shared): State<SharedState>,
    Path(id): Path<String>,
) -> Response {
    let repo = WikiRepository::new(shared.state.db.pool.clone());
    match repo.list_tasks(&id).await {
        Ok(tasks) => Json(serde_json::json!({ "data": tasks })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ── Helpers ──

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

/// Wiki 页面图像资源（PDF 整页原稿等）的只读取回端点。
///
/// 安全设计（用户已确认「内部资料 / 直连」档位）：
/// - 免 Bearer 鉴权：路径中的 `token` 即访问凭证（摄入时逐页生成的随机 128bit 段，猜不到即读不到）；
/// - 不写访问日志：避免图片 URL 落进网关日志形成可长期转发的凭证；
/// - 三段路径全部白名单字符校验（拒绝 `..`/绝对路径/反斜杠），只可能命中 `raw/assets/<token>/<file>`；
/// - 文件不存在一律 404（不区分 403，避免凭证枚举）。
pub async fn get_asset(
    State(_shared): State<SharedState>,
    Path((project_id, token, file)): Path<(String, String, String)>,
) -> Response {
    fn safe_segment(s: &str) -> bool {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    }
    if !safe_segment(&project_id) || !safe_segment(&token) || !safe_segment(&file) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = project::project_wiki_dir(&project_id)
        .join("raw")
        .join("assets")
        .join(&token)
        .join(&file);
    match tokio::fs::read(&path).await {
        Ok(bytes) => axum::http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "private, max-age=86400")
            .body(Body::from(bytes))
            .unwrap()
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{absolutize_asset_urls, collect_asset_map, complete_asset_urls, expand_image_refs};

    /// 用户实测场景（截图证据）：LLM 在表格里写 `<hash>/page-1-1.jpg`（省略 `/api/wiki/projects/<pid>/assets/`），
    /// 服务端必须据检索页面还原成完整可渲染的 markdown 图片。
    #[test]
    fn hash_prefixed_and_bare_filenames_become_full_markdown_images() {
        let pid = "f821cc2e-daf1-4994-827c-51b93bd29bc7";
        let hash = "cc6f0b49c762489c812795f36dab5f9f";
        let page =
            format!("![page 1 screenshot 1](/api/wiki/projects/{pid}/assets/{hash}/page-1-1.jpg)");
        let mut map = Vec::new();
        collect_asset_map(&page, pid, &mut map);
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].0, "page-1-1.jpg");

        let base = "http://127.0.0.1:1133";
        let expected =
            format!("![page-1-1.jpg]({base}/api/wiki/projects/{pid}/assets/{hash}/page-1-1.jpg)");

        // ① 用户截图形态：<hash>/<file>
        let answer = format!("| 第1页 | {hash}/page-1-1.jpg |");
        let out = expand_image_refs(&answer, &map, Some(base));
        assert!(out.contains(&expected), "hash/file 形态未还原: {out}");

        // ② 裸文件名
        let out2 = expand_image_refs("步骤1（登录） | page-1-1.jpg", &map, Some(base));
        assert!(out2.contains(&expected), "裸文件名未还原: {out2}");

        // ③ assets/<hash>/<file>（省略号缩写）
        let out3 = expand_image_refs(&format!(".../assets/{hash}/page-1-1.jpg"), &map, Some(base));
        assert!(out3.contains(&expected), "省略号形态未还原: {out3}");

        // ④ 已是完整相对路径 → 幂等（只补 host，不重复套 markdown）
        let rel = format!("/api/wiki/projects/{pid}/assets/{hash}/page-1-1.jpg");
        let out4 = absolutize_asset_urls(&expand_image_refs(&rel, &map, Some(base)), Some(base));
        assert_eq!(out4, format!("{base}{rel}"));
    }

    /// 用户实测场景（CherryStudio 截图）：LLM 自行编造 host 占位符
    /// `https://<wiki-host>/api/wiki/...`，服务端必须换成真实 base，否则客户端拿到不可用地址。
    #[test]
    fn fabricated_host_prefix_is_replaced_with_real_base() {
        let base = "http://127.0.0.1:1133";
        let rel = "/api/wiki/projects/p1/assets/cc6f0b49c762489c812795f36dab5f9f/page-1-1.jpg";

        // ① 占位符 host
        let text = format!("![封面](https://<wiki-host>{rel})");
        assert_eq!(
            absolutize_asset_urls(&text, Some(base)),
            format!("![封面]({base}{rel})")
        );

        // ② 错误 host/端口也被纠正
        let wrong = format!("![x](http://localhost:8777{rel})");
        assert_eq!(
            absolutize_asset_urls(&wrong, Some(base)),
            format!("![x]({base}{rel})")
        );

        // ③ 已正确 → 幂等
        let ok = format!("![x]({base}{rel})");
        assert_eq!(absolutize_asset_urls(&ok, Some(base)), ok);

        // ④ 同一段里多个 URL（含占位符）全部归一
        let multi = format!("见 {base}{rel} 与 https://<host>{rel}");
        let out = absolutize_asset_urls(&multi, Some(base));
        assert_eq!(out.matches(rel).count(), 2, "{out}");
        assert!(!out.contains("<host>"), "{out}");
        assert!(!out.contains("127.0.0.1:1133http"), "{out}");

        // ⑤ 无 base 时原样（不误改）
        assert_eq!(absolutize_asset_urls(&text, None), text);

        // ⑥ 中文标点作分隔符 + 同段含真实 URL + 图片 marker → 不得 panic（真实页面场景）
        // 「湖南省政务数据共享网站」页含 `门户地址：` + `http://59.232.3.14:18080/portal/home`
        // 与图片链接；marker 前最近分隔符是中文冒号 `：`（UTF-8 3 字节）。
        // 旧实现用 `p + 1` 前进 1 字节，会切在 `：` 中间 → panic: byte index is not a char boundary
        let real = format!("**门户地址**：`http://59.232.3.14:18080/portal/home`\n\n![x]({rel})");
        let out = absolutize_asset_urls(&real, Some(base));
        assert!(out.contains(rel), "{out}");
        assert!(out.contains(&format!("{base}{rel}")), "{out}");
    }

    /// 用户实测场景：LLM 在回答里省略了图片 URL 前缀（`.../assets/<hash>/file`）。
    /// 服务端必须补全成完整路径，否则对话客户端（CherryStudio）无法渲染。
    #[test]
    fn complete_asset_urls_fills_omitted_prefix_then_absolutize() {
        let pid = "p";
        let text = "步骤1：.../assets/60053bcba08e425d90344c8a83bc565a/page-1-1.jpg、/assets/736a68b4b3e140fb86737921dd324aa4/page-1-2.jpg";
        let filled = complete_asset_urls(text, pid);
        assert!(
            filled.contains(
                "/api/wiki/projects/p/assets/60053bcba08e425d90344c8a83bc565a/page-1-1.jpg"
            ),
            "{filled}"
        );
        assert!(
            filled.contains(
                "/api/wiki/projects/p/assets/736a68b4b3e140fb86737921dd324aa4/page-1-2.jpg"
            ),
            "{filled}"
        );
        assert!(!filled.contains("..."), "省略号应被清除: {filled}");
        // 再绝对化 → 客户端直接可渲染
        let full = absolutize_asset_urls(&filled, Some("http://127.0.0.11:1133"));
        assert!(
            full.contains("http://127.0.0.11:1133/api/wiki/projects/p/assets/60053bcba08e425d90344c8a83bc565a/page-1-1.jpg"),
            "{full}"
        );
        // 已带完整前缀的不重复补全
        let once = complete_asset_urls(&full, pid);
        assert_eq!(
            once.matches("assets/").count(),
            full.matches("assets/").count()
        );
        assert!(!once.contains("projects/p/assets/projects/"));
    }

    /// 回归：完整绝对 URL 的图片引用**绝不嵌套**（即使 asset_map 包含该文件）。
    /// 实测曾在 `…/assets/<32hex>/![page-x.jpg](…)` 处嵌套损坏：expand_image_refs 的
    /// "裸文件名"替换把完整 URL 内部的文件名也换成了 markdown 图片。
    #[test]
    fn expand_image_refs_never_nests_full_absolute_urls() {
        let base = "http://127.0.0.1:1133";
        let h = "0".repeat(32);
        let rel = format!("/api/wiki/projects/p/assets/{h}/page-2-1.jpg");
        let text = format!("![page 2 screenshot 1]({base}{rel})  后文继续");
        // asset_map 恰好含该文件：若"完整 URL 保护"失效，step2 会把 URL 内部文件名
        // 再替换成 markdown 图片 → 嵌套损坏。
        let assets = vec![("page-2-1.jpg".to_string(), rel.clone())];
        let out = expand_image_refs(&text, &assets, Some(base));
        assert!(!out.contains("![page-2-1.jpg]"), "嵌套损坏: {out}");
        assert!(
            out.contains("![page 2 screenshot 1]("),
            "完整图片被破坏: {out}"
        );
        // 裸文件名（非 URL 内）仍应被补全
        let bare = format!("见附件 {base}{rel} 或直接查看 page-2-1.jpg");
        let out2 = expand_image_refs(&bare, &assets, Some(base));
        assert!(out2.contains("![page-2-1.jpg]("), "裸文件名应补全: {out2}");
    }

    #[test]
    fn asset_urls_are_absolutized_with_host_and_left_alone_without() {
        let md = "![第 2 页原稿](/api/wiki/projects/p1/assets/abc123/page-2.jpg)";
        assert_eq!(
            absolutize_asset_urls(md, None),
            md,
            "无 host 时原样保留 root-relative 路径"
        );
        assert_eq!(
            absolutize_asset_urls(md, Some("http://10.55.1.101:8777")),
            "![第 2 页原稿](http://10.55.1.101:8777/api/wiki/projects/p1/assets/abc123/page-2.jpg)"
        );
        // 多个引用按同一 host 补全
        let multi =
            "![](/api/wiki/projects/p1/assets/a/1.jpg) x ![](/api/wiki/projects/p1/assets/b/2.jpg)";
        let out = absolutize_asset_urls(multi, Some("http://127.0.0.1:8777"));
        assert_eq!(
            out,
            "![](http://127.0.0.1:8777/api/wiki/projects/p1/assets/a/1.jpg) x ![](http://127.0.0.1:8777/api/wiki/projects/p1/assets/b/2.jpg)"
        );
    }
}
