use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub enum ParsedContent {
    PlainText(String),
    Markdown { text: String },
    Code { text: String, language: String },
    Structured(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownSection {
    pub heading: String,
    pub level: u8,
    pub content: String,
    pub line_start: usize,
    pub line_end: usize,
}

/// Parse file by extension
pub fn parse_file(filename: &str, content: &[u8]) -> Result<ParsedContent, String> {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();

    match ext.as_str() {
        "md" | "markdown" => {
            let text = String::from_utf8_lossy(content).to_string();
            Ok(ParsedContent::Markdown { text })
        }
        // Code files
        "rs" | "go" | "py" | "ts" | "tsx" | "js" | "jsx" | "java" | "c" | "cpp" | "h" | "hpp"
        | "cs" | "php" | "swift" | "kt" | "rb" | "scala" | "clj" | "sh" | "bash" | "vue"
        | "svelte" | "sql" | "proto" | "gradle" => {
            let text = String::from_utf8_lossy(content).to_string();
            Ok(ParsedContent::Code {
                text,
                language: ext.clone(),
            })
        }
        // Structured
        "json" | "yaml" | "yml" | "toml" | "xml" | "html" | "csv" => {
            let text = String::from_utf8_lossy(content).to_string();
            Ok(ParsedContent::Structured(text))
        }
        // Text
        "txt" | "rst" | "log" | "env" | "ini" | "conf" | "cfg" | "svg" => {
            let text = String::from_utf8_lossy(content).to_string();
            Ok(ParsedContent::PlainText(text))
        }
        // PDF
        "pdf" => {
            let content_vec = content.to_vec(); // owned copy for catch_unwind
                                                // 捕获 PDF 解析过程中的代码崩溃（panic）
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pdf_extract::extract_text_from_mem(&content_vec)
            }))
            .map_err(|e| {
                let msg = e
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| e.downcast_ref::<String>().map(|s| s.as_str()))
                    .unwrap_or("Unknown PDF parse panic");
                format!("PDF parse panic: {}", msg)
            })?;
            let text = text.map_err(|e| format!("PDF parse error: {}", e))?;
            Ok(ParsedContent::PlainText(super::text::normalize_radicals(
                &text,
            )))
        }
        _ => {
            // Try to decode as UTF-8, fall back to lossy
            let text = String::from_utf8_lossy(content).to_string();
            Ok(ParsedContent::PlainText(text))
        }
    }
}

/// Get file type label from extension
pub fn get_file_type(filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "md" | "markdown" => "markdown",
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "go" => "go",
        "java" => "java",
        "c" | "cpp" | "h" | "hpp" => "cpp",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        "sh" | "bash" => "shell",
        "html" | "xml" | "svg" => "markup",
        "css" | "scss" | "less" => "style",
        "pdf" => "pdf",
        _ => "text",
    }
    .to_string()
}

// Note: PDF files are parsed by pdf-extract crate, returning plain text.
// Binary formats like .docx/.xlsx are not supported yet.
