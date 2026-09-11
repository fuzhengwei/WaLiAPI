//! KB 文档上传的路径安全：库标识白名单校验 + 落盘文件名服务端生成。
//!
//! REST（`handlers::upload_document`）与 Tauri 命令（`commands::knowledge_base::upload_kb_document`）
//! 两条入口共用。原始文件名只入库作展示元数据，绝不参与落盘路径拼接——
//! 携带 `../`、绝对路径、反斜杠、UNC 形态的文件名都无法写出库目录。

use std::path::{Component, Path, PathBuf};

/// 库标识白名单：非空、仅 ASCII 字母/数字/`-`/`_`、长度 ≤ 64。
/// 正常业务里 kb_id 是 UUID，其它形态按非法拒绝（阻断 `..`、路径分隔符、UNC 注入）。
pub fn validate_kb_id(kb_id: &str) -> Result<(), String> {
    if kb_id.is_empty() {
        return Err("知识库标识不能为空".to_string());
    }
    if kb_id.len() > 64 {
        return Err("知识库标识过长（≤64 字符）".to_string());
    }
    if !kb_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("知识库标识含非法字符（仅允许字母、数字、-、_）".to_string());
    }
    Ok(())
}

/// 落盘扩展名白名单（小写），与 `parser::get_file_type` 支持的类型一致；
/// 未命中时回退 `bin`——扩展名只影响展示与解析提示，不影响内容安全。
const ALLOWED_EXTENSIONS: &[&str] = &[
    "md", "markdown", "txt", "pdf", "json", "yaml", "yml", "toml", "sql", "csv", "rs", "py", "ts",
    "tsx", "js", "jsx", "go", "java", "c", "cpp", "h", "hpp", "sh", "bash", "html", "xml", "svg",
    "css", "scss", "less",
];

/// 生成落盘文件名：`<doc_id>.<白名单扩展名>`。
///
/// 只取原始文件名最后一个 `.` 之后的扩展名（小写 + 白名单校验），其余部分全部丢弃，
/// 结果必然是单个 Normal 路径组件——无法穿越、无法指向绝对路径/UNC。
pub fn storage_file_name(doc_id: &str, original_filename: &str) -> String {
    let ext = sanitized_extension(original_filename).unwrap_or_else(|| "bin".to_string());
    format!("{doc_id}.{ext}")
}

/// 提取并校验扩展名：取最后一个 `.` 之后的部分，必须整体为 ASCII 字母/数字
/// （≤8 字符）且在白名单内；无 `.` 或不满足时返回 None。
fn sanitized_extension(original_filename: &str) -> Option<String> {
    let dot = original_filename.rfind('.')?;
    let ext = &original_filename[dot + 1..];
    if ext.is_empty() || ext.len() > 8 || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let lowered = ext.to_ascii_lowercase();
    ALLOWED_EXTENSIONS
        .contains(&lowered.as_str())
        .then_some(lowered)
}

/// 计算上传文件的落盘路径：`<data_dir>/kb_files/<kb_id>/<doc_id>.<ext>`。
///
/// kb_id 先过白名单，文件名由服务端生成；`starts_with` 前缀校验作为纵深防御，
/// 保证（即使未来有人改动拼接逻辑）结果仍限定在 kb_files 目录内。
pub fn storage_path(
    data_dir: &Path,
    kb_id: &str,
    doc_id: &str,
    original_filename: &str,
) -> Result<PathBuf, String> {
    validate_kb_id(kb_id)?;
    let name = storage_file_name(doc_id, original_filename);
    let kb_files_root = data_dir.join("kb_files");
    let path = kb_files_root.join(kb_id).join(&name);
    if !path.starts_with(&kb_files_root)
        || !Path::new(&name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!("落盘路径越界: {path:?}"));
    }
    Ok(path)
}

/// 只清理应用拥有的上传副本；目录/Git/URL 来源路径不属于应用。
pub fn remove_managed_file(
    data_dir: &Path,
    kb_id: &str,
    source_type: &str,
    file_path: Option<&str>,
) -> Result<(), String> {
    if source_type != "upload" {
        return Ok(());
    }
    let Some(file_path) = file_path else {
        return Ok(());
    };
    validate_kb_id(kb_id)?;
    let path = Path::new(file_path);
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("无法确认上传文件路径: {e}")),
    };
    let root = data_dir.canonicalize().map_err(|e| e.to_string())?;
    let managed = root.join("kb_files").join(kb_id);
    // 旧数据可能指向源文件，符号链接也不能使清理越过受管目录。
    if canonical.parent() != Some(managed.as_path()) {
        return Ok(());
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("删除上传副本失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC_ID: &str = "0f9c2a44-2b6f-4d0e-a1b7-3f1c2d9e8a7b";

    #[test]
    fn kb_ids_are_whitelist_validated() {
        // UUID（正常形态）与受限字符集通过
        assert!(validate_kb_id("3fa85f64-5717-4562-b3fc-2c963f66afa6").is_ok());
        assert!(validate_kb_id("kb_1-2").is_ok());
        // 穿越段、路径分隔符（正/反斜杠）、UNC、点开头、空串、超长、非 ASCII 全部拒绝
        for bad in [
            "",
            ".",
            "..",
            "../evil",
            "a/b",
            "a\\b",
            "\\\\server\\share",
            "C:\\evil",
            ".hidden",
            "id with space",
            "中文库",
            &"x".repeat(65),
        ] {
            assert!(validate_kb_id(bad).is_err(), "kb_id 应被拒绝: {bad:?}");
        }
    }

    #[test]
    fn storage_names_use_whitelisted_extension_only() {
        for (original, ext) in [
            ("年度报告.pdf", "pdf"),
            ("README.md", "md"),
            ("notes.TXT", "txt"),
            ("data.csv", "csv"),
            ("代码.rs", "rs"),
        ] {
            assert_eq!(
                storage_file_name(DOC_ID, original),
                format!("{DOC_ID}.{ext}"),
                "original: {original}"
            );
        }
        // 非白名单扩展名、无扩展名、可执行/压缩包形态 → 统一回退 bin
        for original in [
            "evil.exe",
            "shell.bat",
            "lib.dll",
            "archive.tar.gz",
            "noext",
            "pdf",
            "",
        ] {
            assert_eq!(
                storage_file_name(DOC_ID, original),
                format!("{DOC_ID}.bin"),
                "original: {original}"
            );
        }
    }

    #[test]
    fn malicious_filenames_cannot_escape_kb_directory() {
        let data_dir = Path::new("/waliapi-data");
        let kb_files_root = data_dir.join("kb_files");
        for original in [
            "../../../evil.pdf",
            "/etc/passwd.pdf",
            "C:\\Windows\\system32\\evil.dll",
            "\\\\server\\share\\evil.pdf",
            "..\\..\\..\\evil.pdf",
            "..",
            "con", // Windows 保留设备名（无扩展名回退 bin 后同样无害）
        ] {
            let path = storage_path(data_dir, "kb-ok", DOC_ID, original).unwrap();
            assert!(
                path.starts_with(&kb_files_root),
                "original {original:?} 逃出库目录: {path:?}"
            );
        }
        // 非法库标识直接拒绝，不触碰文件系统
        assert!(storage_path(data_dir, "..", DOC_ID, "a.pdf").is_err());
        assert!(storage_path(data_dir, "a/b", DOC_ID, "a.pdf").is_err());
        assert!(storage_path(data_dir, "", DOC_ID, "a.pdf").is_err());
    }
    #[test]
    fn removal_preserves_sources_and_only_cleans_owned_uploads() {
        let dir = std::env::temp_dir().join(format!("kb-delete-{}", uuid::Uuid::new_v4()));
        let managed = dir.join("kb_files/kb-test");
        std::fs::create_dir_all(&managed).unwrap();
        let original = dir.join("original.txt");
        std::fs::write(&original, "user source").unwrap();
        for source_type in ["local_dir", "git", "url", "upload"] {
            remove_managed_file(&dir, "kb-test", source_type, original.to_str()).unwrap();
            assert!(
                original.exists(),
                "must preserve original for {source_type}"
            );
        }
        let uploaded = managed.join("upload.txt");
        std::fs::write(&uploaded, "owned copy").unwrap();
        remove_managed_file(&dir, "kb-test", "local_dir", uploaded.to_str()).unwrap();
        assert!(
            uploaded.exists(),
            "source ownership matters even inside the data directory"
        );
        remove_managed_file(&dir, "kb-test", "upload", uploaded.to_str()).unwrap();
        assert!(!uploaded.exists());
        remove_managed_file(&dir, "kb-test", "upload", uploaded.to_str()).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&original, &uploaded).unwrap();
            remove_managed_file(&dir, "kb-test", "upload", uploaded.to_str()).unwrap();
            assert!(original.exists());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
