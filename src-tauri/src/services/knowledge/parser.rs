use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub enum ParsedContent {
    PlainText(String),
    Markdown { text: String },
    Code { text: String, language: String },
    Structured(String),
    Pdf(ParsedPdf),
}

#[derive(Debug, Clone)]
pub struct ParsedPdf {
    pub pages: Vec<String>,
    pub extraction: PdfTextExtraction,
}

/// 仅描述文字层提取结果，不推断页面内容或资料是否完整，不包含原文与库错误详情。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdfTextExtraction {
    pub version: u32,
    pub page_count: usize,
    pub status: String,
    pub pages: Vec<PdfPageExtraction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PdfPageExtraction {
    pub page_no: u32,
    pub char_count: usize,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl PdfTextExtraction {
    /// 无法加载页面树时页数未知，不能把 0 描述为文档真实页数。
    pub fn load_failure(error: &str) -> Self {
        let code = ["PDF_LOAD_FAILED", "PDF_PASSWORD_REQUIRED", "PDF_NO_PAGES"]
            .into_iter()
            .find(|code| error.starts_with(code))
            .unwrap_or("PDF_LOAD_FAILED");
        Self {
            version: 1,
            page_count: 0,
            status: "failed".into(),
            pages: Vec::new(),
            error_code: Some(code.into()),
        }
    }
}

fn parse_pdf(content: &[u8]) -> Result<ParsedPdf, String> {
    // pdf-extract 的 by_pages API 遇到第一页错误即停止，故完整枚举页面树并逐页提取。
    let mut doc = std::panic::catch_unwind(|| pdf_extract::Document::load_mem(content))
        .map_err(|_| "PDF_LOAD_FAILED: PDF 页面结构解析失败".to_string())?
        .map_err(|_| "PDF_LOAD_FAILED: 无法读取 PDF 文件结构".to_string())?;
    if doc.is_encrypted() {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| doc.decrypt("")))
            .map_err(|_| "PDF_LOAD_FAILED: PDF 加密结构解析失败".to_string())?
            .map_err(|_| "PDF_PASSWORD_REQUIRED: PDF 需要有效密码，未提取文字层".to_string())?;
    }
    let page_ids = std::panic::catch_unwind(|| doc.get_pages())
        .map_err(|_| "PDF_LOAD_FAILED: PDF 页面结构解析失败".to_string())?;
    if page_ids.is_empty() {
        return Err("PDF_NO_PAGES: PDF 中没有可读取的页面".into());
    }
    let mut pages = Vec::with_capacity(page_ids.len());
    let mut page_info = Vec::with_capacity(page_ids.len());
    for page_no in page_ids.keys().copied() {
        let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut text = String::new();
            let mut output = pdf_extract::PlainTextOutput::new(&mut text);
            pdf_extract::output_doc_page(&doc, &mut output, page_no)?;
            Ok::<_, pdf_extract::OutputError>(text)
        }));
        let (text, error_code) = match extracted {
            Ok(Ok(text)) => (super::text::normalize_radicals(&text), None),
            Ok(Err(_)) => (String::new(), Some("PDF_PAGE_EXTRACT_FAILED".to_string())),
            Err(_) => (String::new(), Some("PDF_PAGE_PARSE_FAILED".to_string())),
        };
        let visible_text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let char_count = visible_text.chars().count();
        let status = if error_code.is_some() {
            "failed"
        } else if super::ocr::is_scanned_pdf(&visible_text, 1) {
            // 复用现有 OCR 的字符阈值，仅报告不足；不会启用 OCR 或调用模型。
            "insufficient"
        } else {
            "extracted"
        };
        page_info.push(PdfPageExtraction {
            page_no,
            char_count,
            status: status.into(),
            error_code,
        });
        pages.push(text);
    }
    let status = if page_info.iter().all(|p| p.char_count == 0) {
        "failed"
    } else if page_info.iter().any(|p| p.status != "extracted") {
        "partial"
    } else {
        "complete"
    };
    Ok(ParsedPdf {
        extraction: PdfTextExtraction {
            version: 1,
            page_count: page_info.len(),
            status: status.into(),
            pages: page_info,
            error_code: (status == "failed").then(|| "PDF_TEXT_LAYER_EMPTY".into()),
        },
        pages,
    })
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
        "pdf" => parse_pdf(content).map(ParsedContent::Pdf),
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

// PDF 按页面提取文字层，保留来源页码；二进制 .docx/.xlsx 暂未支持。

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pdf_extract::content::{Content, Operation};
    use pdf_extract::{dictionary, Document, Object, Stream};

    /// 内存生成的公开 PDF；空文字页绘制合成位图，broken 页使用无效 MediaBox。
    pub(crate) fn synthetic_pdf(pages: &[(&str, bool)]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let image = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
            },
            vec![0, 0, 0],
        ));
        let resources = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font },
            "XObject" => dictionary! { "Scan" => image },
        });
        let mut kids = Vec::new();
        for (text, broken) in pages {
            let mut content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 12.into()]),
                    Operation::new("Td", vec![30.into(), 700.into()]),
                    Operation::new("Tj", vec![Object::string_literal(*text)]),
                    Operation::new("ET", vec![]),
                ],
            };
            if text.trim().is_empty() {
                content.operations.extend([
                    Operation::new("q", vec![]),
                    Operation::new(
                        "cm",
                        vec![
                            200.into(),
                            0.into(),
                            0.into(),
                            200.into(),
                            30.into(),
                            300.into(),
                        ],
                    ),
                    Operation::new("Do", vec![Object::Name(b"Scan".to_vec())]),
                    Operation::new("Q", vec![]),
                ]);
            }
            let stream = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let mut page = dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => stream,
            };
            if *broken {
                page.set("MediaBox", vec![Object::Integer(0), Object::Integer(0)]);
            }
            kids.push(doc.add_object(page).into());
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => pages.len() as i64,
                "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    fn parsed_pdf(pages: &[(&str, bool)]) -> ParsedPdf {
        match parse_file("synthetic.pdf", &synthetic_pdf(pages)).unwrap() {
            ParsedContent::Pdf(pdf) => pdf,
            _ => panic!("expected page-aware PDF"),
        }
    }

    #[test]
    fn ordinary_pdf_has_real_page_count_and_preserves_final_page() {
        let first = "Public synthetic first page with sufficient visible text and source markers.";
        let last = "Public synthetic final page with sufficient visible text and a final marker.";
        let pdf = parsed_pdf(&[(first, false), (last, false)]);
        assert_eq!(pdf.extraction.page_count, 2);
        assert_eq!(pdf.extraction.status, "complete");
        assert_eq!(pdf.extraction.pages[1].page_no, 2);
        assert!(pdf.pages[0].contains(first));
        assert!(pdf.pages[1].contains(last));
        let json = serde_json::to_string(&pdf.extraction).unwrap();
        assert!(!json.contains("source markers"));
        assert!(!json.contains("final marker"));
    }

    #[test]
    fn mixed_text_layer_reports_short_and_empty_pages_without_removing_short_text() {
        let pdf = parsed_pdf(&[
            (
                "Public synthetic text page with sufficient visible characters for the text layer.",
                false,
            ),
            ("Short caption", false),
            ("   ", false),
        ]);
        assert_eq!(pdf.extraction.page_count, 3);
        assert_eq!(pdf.extraction.status, "partial");
        assert_eq!(pdf.extraction.pages[1].status, "insufficient");
        assert_eq!(pdf.extraction.pages[2].char_count, 0);
        assert!(pdf.pages[1].contains("Short caption"));
        assert!(pdf.extraction.pages.iter().all(|p| p.error_code.is_none()));
    }

    #[test]
    fn failed_middle_page_does_not_hide_later_pages_or_leak_library_errors() {
        let pdf = parsed_pdf(&[
            ("Public synthetic first page with enough visible characters for reliable extraction.", false),
            ("PrivateMarker must never appear in extraction metadata", true),
            ("Public synthetic third page with enough visible characters and a retained final marker.", false),
        ]);
        assert_eq!(pdf.extraction.page_count, 3);
        assert_eq!(pdf.extraction.status, "partial");
        assert_eq!(pdf.extraction.pages[1].status, "failed");
        assert_eq!(
            pdf.extraction.pages[1].error_code.as_deref(),
            Some("PDF_PAGE_PARSE_FAILED")
        );
        assert!(pdf.pages[1].is_empty());
        assert!(pdf.pages[2].contains("retained final marker"));
        assert!(!serde_json::to_string(&pdf.extraction)
            .unwrap()
            .contains("PrivateMarker"));
    }

    #[test]
    fn invalid_pdf_error_is_safe_and_page_count_is_unknown() {
        let error = parse_file("synthetic.pdf", b"PrivateMarker invalid PDF bytes").unwrap_err();
        assert!(error.starts_with("PDF_LOAD_FAILED:"));
        assert!(!error.contains("PrivateMarker"));
        let info = PdfTextExtraction::load_failure(&error);
        assert_eq!(info.page_count, 0);
        assert!(info.pages.is_empty());
        assert_eq!(info.error_code.as_deref(), Some("PDF_LOAD_FAILED"));
    }

    #[test]
    fn empty_pdf_text_layer_is_not_reported_as_complete() {
        let pdf = parsed_pdf(&[("", false), ("", false)]);
        assert_eq!(pdf.extraction.page_count, 2);
        assert_eq!(pdf.extraction.status, "failed");
        assert_eq!(
            pdf.extraction.error_code.as_deref(),
            Some("PDF_TEXT_LAYER_EMPTY")
        );
        assert!(pdf
            .extraction
            .pages
            .iter()
            .all(|p| p.status == "insufficient"));
    }
}
