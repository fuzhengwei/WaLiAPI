use chrono::Utc;
use std::path::{Component, Path, PathBuf};
use tokio::fs;
use uuid::Uuid;

/// Get the base wiki directory for all projects.
pub fn wiki_base_dir() -> PathBuf {
    let data_dir = std::env::var_os("WALIAPI_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|path| path.join("waliapi")))
        .unwrap_or_else(|| PathBuf::from("./waliapi-data"));
    let base = data_dir.join("wiki");
    let _ = std::fs::create_dir_all(&base);
    base
}

fn project_component(project_id: &str) -> String {
    if !project_id.is_empty()
        && project_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return project_id.to_owned();
    }
    // Project IDs are UUIDs in normal operation. Encoding unexpected input
    // keeps the infallible legacy helper inside the Wiki root without silently
    // treating `../other-project` as a path.
    let encoded = project_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("invalid-{encoded}")
}

pub(crate) fn safe_relative_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.contains('\\') {
        return Err("Invalid relative path".to_owned());
    }
    let candidate = Path::new(path);
    if candidate
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(candidate.to_path_buf())
    } else {
        Err("Path traversal detected".to_owned())
    }
}

fn safe_file_name(name: &str) -> Result<PathBuf, String> {
    let path = safe_relative_path(name)?;
    if path.components().count() == 1 {
        Ok(path)
    } else {
        Err("Invalid file name".to_owned())
    }
}

fn reject_symlink_components(root: &Path, relative: &Path) -> Result<(), String> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err("Path traversal detected".to_owned());
        };
        current.push(component);
        if let Ok(metadata) = std::fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err("Symbolic links are not allowed in Wiki paths".to_owned());
            }
        }
    }
    Ok(())
}

fn wiki_page_path(project_id: &str, path: &str) -> Result<PathBuf, String> {
    let root = project_wiki_dir(project_id).join("wiki");
    let relative = safe_relative_path(path)?;
    reject_symlink_components(&root, &relative)?;
    Ok(root.join(relative))
}

/// Get a project's wiki directory.
pub fn project_wiki_dir(project_id: &str) -> PathBuf {
    let dir = wiki_base_dir()
        .join("projects")
        .join(project_component(project_id));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::create_dir_all(dir.join("raw").join("sources"));
    let _ = std::fs::create_dir_all(dir.join("raw").join("assets"));
    let _ = std::fs::create_dir_all(dir.join("wiki").join("entities"));
    let _ = std::fs::create_dir_all(dir.join("wiki").join("concepts"));
    let _ = std::fs::create_dir_all(dir.join("wiki").join("summaries"));
    let _ = std::fs::create_dir_all(dir.join("schema"));
    dir
}

/// Initialize a new wiki project directory structure.
pub async fn init_project_dir(project_id: &str, schema_text: &str) -> Result<PathBuf, String> {
    let dir = project_wiki_dir(project_id);

    // Write schema/CLAUDE.md
    let schema_path = dir.join("schema").join("CLAUDE.md");
    fs::write(&schema_path, schema_text)
        .await
        .map_err(|e| format!("Failed to write schema: {}", e))?;

    // Write wiki/index.md
    let index_path = dir.join("wiki").join("index.md");
    if !index_path.exists() {
        fs::write(&index_path, "# Wiki Index\n\n<!-- Add pages below -->\n")
            .await
            .map_err(|e| format!("Failed to write index: {}", e))?;
    }

    // Write wiki/log.md
    let log_path = dir.join("wiki").join("log.md");
    if !log_path.exists() {
        let now = Utc::now().to_rfc3339();
        fs::write(
            &log_path,
            &format!("# Wiki Log\n\n## [{}] init | Project created\n", now),
        )
        .await
        .map_err(|e| format!("Failed to write log: {}", e))?;
    }

    // Write .meta.json
    let meta_path = dir.join(".meta.json");
    let meta = serde_json::json!({
        "project_id": project_id,
        "created_at": Utc::now().to_rfc3339(),
    });
    fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap())
        .await
        .map_err(|e| format!("Failed to write meta: {}", e))?;

    Ok(dir)
}

/// Read a wiki page from disk.
pub async fn read_page(project_id: &str, path: &str) -> Result<String, String> {
    let full_path = wiki_page_path(project_id, path)?;
    fs::read_to_string(&full_path)
        .await
        .map_err(|e| format!("Failed to read {}: {}", path, e))
}

/// Write a wiki page to disk.
pub async fn write_page(project_id: &str, path: &str, content: &str) -> Result<(), String> {
    let full_path = wiki_page_path(project_id, path)?;
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create dir: {}", e))?;
    }
    fs::write(&full_path, content)
        .await
        .map_err(|e| format!("Failed to write {}: {}", path, e))
}

/// 原子写：先写同目录临时文件，再 rename 覆盖目标（Windows `MoveFileExW` 支持覆盖已存在文件）。
/// 避免 index.md / log.md 被并发或中断写成半个文件。
pub async fn write_file_atomic(full_path: &std::path::Path, content: &str) -> Result<(), String> {
    let file_name = full_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("wiki-file");
    let tmp_path = full_path.with_file_name(format!(
        ".{}.tmp-{}",
        file_name,
        std::process::id()
    ));
    fs::write(&tmp_path, content)
        .await
        .map_err(|e| format!("Failed to write temp file: {}", e))?;
    fs::rename(&tmp_path, full_path)
        .await
        .map_err(|e| format!("Failed to replace target file: {}", e))
}

/// Delete a wiki page from disk.
pub async fn delete_page_file(project_id: &str, path: &str) -> Result<(), String> {
    let full_path = wiki_page_path(project_id, path)?;
    if full_path.exists() {
        fs::remove_file(&full_path)
            .await
            .map_err(|e| format!("Failed to delete {}: {}", path, e))?;
    }
    Ok(())
}

/// Append to log.md
pub async fn append_log(project_id: &str, entry: &str) -> Result<(), String> {
    let dir = project_wiki_dir(project_id);
    let log_path = dir.join("wiki").join("log.md");
    let now = Utc::now().format("%Y-%m-%d %H:%M").to_string();
    let line = format!("\n## [{}] {}\n", now, entry);
    let mut content = fs::read_to_string(&log_path).await.unwrap_or_default();
    content.push_str(&line);
    fs::write(&log_path, &content)
        .await
        .map_err(|e| format!("Failed to append log: {}", e))
}

/// Update index.md with a new entry.
pub async fn update_index(project_id: &str, entries: &[IndexEntry]) -> Result<(), String> {
    let dir = project_wiki_dir(project_id);
    let index_path = dir.join("wiki").join("index.md");
    let mut content = String::from("# Wiki Index\n\n");
    for entry in entries {
        content.push_str(&format!("- [[{}]] — {}\n", entry.path, entry.summary));
    }
    fs::write(&index_path, &content)
        .await
        .map_err(|e| format!("Failed to write index: {}", e))
}

pub struct IndexEntry {
    pub path: String,
    pub summary: String,
}

/// List all wiki page files on disk.
pub async fn list_page_files(project_id: &str) -> Result<Vec<PageFileInfo>, String> {
    let dir = project_wiki_dir(project_id);
    let wiki_dir = dir.join("wiki");
    let mut results = Vec::new();
    let mut stack = vec![wiki_dir.clone()];
    while let Some(current) = stack.pop() {
        let mut entries = match fs::read_dir(&current).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(ext) = path.extension() {
                if ext == "md" {
                    let rel_path = path
                        .strip_prefix(&wiki_dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    let name = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    results.push(PageFileInfo {
                        path: rel_path,
                        title: name,
                    });
                }
            }
        }
    }
    Ok(results)
}

pub struct PageFileInfo {
    pub path: String,
    pub title: String,
}

/// Write a source file to raw/sources/.
pub async fn write_source_file(
    project_id: &str,
    filename: &str,
    content: &[u8],
) -> Result<PathBuf, String> {
    let dir = project_wiki_dir(project_id);
    let sources_dir = dir.join("raw").join("sources");
    fs::create_dir_all(&sources_dir)
        .await
        .map_err(|e| format!("Failed to create sources dir: {}", e))?;
    let relative = safe_file_name(filename)?;
    reject_symlink_components(&sources_dir, &relative)?;
    let file_path = sources_dir.join(relative);
    fs::write(&file_path, content)
        .await
        .map_err(|e| format!("Failed to write source file: {}", e))?;
    Ok(file_path)
}

/// Resolve source content bytes from an uploaded `content` string.
/// PDF 源资料走后端 base64 传输（前端 `fileToBase64`），此处解码回原始字节；
/// 非 PDF（或解码失败的历史数据）按 UTF-8 文本字节处理。
pub fn resolve_source_content(filename: &str, content: &str) -> Vec<u8> {
    let is_pdf = filename
        .rsplit('.')
        .next()
        .map(|ext| ext.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false);
    if is_pdf {
        if let Ok(bytes) =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, content)
        {
            return bytes;
        }
    }
    content.as_bytes().to_vec()
}

/// Read a source file from raw/sources/ or raw/.
pub async fn read_source_file(project_id: &str, path: &str) -> Result<Vec<u8>, String> {
    let dir = project_wiki_dir(project_id);
    let relative = safe_relative_path(path)?;
    let relative = if relative.starts_with(Path::new("raw/sources"))
        || relative.starts_with(Path::new("wiki"))
    {
        relative
    } else {
        PathBuf::from("raw/sources").join(relative)
    };
    reject_symlink_components(&dir, &relative)?;
    let safe_path = dir.join(relative);

    fs::read(&safe_path)
        .await
        .map_err(|e| format!("Failed to read file: {}", e))
}

/// Remove a project directory entirely.
pub async fn remove_project_dir(project_id: &str) -> Result<(), String> {
    let dir = project_wiki_dir(project_id);
    if dir.exists() {
        fs::remove_dir_all(&dir)
            .await
            .map_err(|e| format!("Failed to remove project dir: {}", e))?;
    }
    Ok(())
}

pub fn new_uuid() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_paths_reject_traversal_absolute_and_mixed_separators() {
        for path in [
            "",
            "../settings.json",
            "/etc/passwd",
            "wiki/../../secret",
            "..\\secret",
        ] {
            assert!(safe_relative_path(path).is_err(), "path: {path}");
        }
        assert_eq!(
            safe_relative_path("concepts/routing.md").unwrap(),
            PathBuf::from("concepts/routing.md")
        );
    }

    #[test]
    fn project_ids_cannot_escape_the_projects_directory() {
        assert_eq!(project_component("project-123"), "project-123");
        assert!(project_component("../../outside").starts_with("invalid-"));
        assert!(!project_component("../../outside").contains('/'));
    }

    #[test]
    fn resolve_source_content_decodes_pdf_base64_and_keeps_text_bytes() {
        // PDF：base64 按字节还原（含二进制内容）
        let raw = vec![0x25u8, 0x50, 0x44, 0x46, 0x00, 0xff, 0x80]; // "%PDF\x00\xff\x80"
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &raw);
        assert_eq!(resolve_source_content("手册.pdf", &b64), raw);
        assert_eq!(resolve_source_content("guide.PDF", &b64), raw);
        // 非 PDF：按 UTF-8 文本字节处理
        assert_eq!(
            resolve_source_content("notes.md", "hello 中文"),
            "hello 中文".as_bytes()
        );
        // PDF 但内容不是合法 base64（历史数据）→ 回退文本字节
        assert_eq!(
            resolve_source_content("broken.pdf", "not-base64!!!"),
            "not-base64!!!".as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn wiki_paths_reject_symbolic_link_components() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("waliapi-wiki-path-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        symlink(std::env::temp_dir(), root.join("escape")).unwrap();
        assert!(reject_symlink_components(&root, Path::new("escape/file.md")).is_err());
    }
}
