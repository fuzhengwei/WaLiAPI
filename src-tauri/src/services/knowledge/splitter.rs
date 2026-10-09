use serde::{Deserialize, Serialize};

use super::code_parser::Symbol;

#[derive(Debug, Clone)]
pub struct SplitConfig {
    pub chunk_size: usize,
    pub chunk_overlap: usize,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self {
            chunk_size: 512,
            chunk_overlap: 64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    pub token_count: usize,
    pub metadata: ChunkMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChunkMetadata {
    pub file_path: Option<String>,
    pub line_start: usize,
    pub line_end: usize,
    pub heading: Option<String>,
    pub language: Option<String>,
    // ── tree-sitter 符号感知字段 ──
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub signature: Option<String>,
    /// PDF 来源页码（普通文字层与 OCR 均按页分块）；旧数据默认为 None
    #[serde(default)]
    pub page_no: Option<u32>,
}

/// Split text into chunks by approximate token count.
/// Uses a simple heuristic: ~4 chars per token for English/code.
/// This avoids heavy tokenizer dependencies.
pub fn split_text(content: &str, config: &SplitConfig, metadata: &ChunkMetadata) -> Vec<Chunk> {
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();

    let _target_chars = config.chunk_size * 4; // ~4 chars per token
    let overlap_chars = config.chunk_overlap * 4;

    let mut current = String::new();
    let mut current_tokens = 0usize;
    let mut chunk_start_line = 0usize;

    for (line_idx, line) in lines.iter().enumerate() {
        let line_with_newline = format!("{}\n", line);
        let line_chars = line_with_newline.chars().count();
        let line_tokens = (line_chars + 3) / 4; // approximate

        // If adding this line would exceed target and we have content, flush
        if current_tokens + line_tokens > config.chunk_size && !current.is_empty() {
            let chunk_content = current.trim().to_string();
            if !chunk_content.is_empty() {
                chunks.push(Chunk {
                    content: chunk_content,
                    token_count: current_tokens,
                    metadata: ChunkMetadata {
                        line_start: chunk_start_line,
                        line_end: line_idx,
                        ..metadata.clone()
                    },
                });
            }

            // Overlap: keep last overlap_chars
            if overlap_chars > 0 && current.chars().count() > overlap_chars {
                let char_count = current.chars().count();
                let overlap_start_char_index = char_count - overlap_chars;
                let overlap_text: String = current.chars().skip(overlap_start_char_index).collect();
                current_tokens = (overlap_text.chars().count() + 3) / 4;
                current = overlap_text;
            } else {
                current = String::new();
                current_tokens = 0;
            }
            chunk_start_line = line_idx;
        }

        current.push_str(&line_with_newline);
        current_tokens += line_tokens;
    }

    // Flush remaining
    let chunk_content = current.trim().to_string();
    if !chunk_content.is_empty() {
        chunks.push(Chunk {
            content: chunk_content,
            token_count: current_tokens,
            metadata: ChunkMetadata {
                line_start: chunk_start_line,
                line_end: lines.len(),
                ..metadata.clone()
            },
        });
    }

    chunks
}

/// Split markdown by headers first, then by size
pub fn split_markdown(content: &str, config: &SplitConfig, metadata: &ChunkMetadata) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current_section = String::new();
    let mut current_heading: Option<String> = None;
    let mut line_start = 0usize;

    for (idx, line) in content.lines().enumerate() {
        // Check for markdown headers
        if line.starts_with('#') {
            // Flush current section if it's large enough
            let section_tokens = (current_section.chars().count() + 3) / 4;
            if section_tokens > config.chunk_size / 2 {
                let chunk_content = current_section.trim().to_string();
                if !chunk_content.is_empty() {
                    let mut meta = metadata.clone();
                    if let Some(ref h) = current_heading {
                        meta.heading = Some(h.clone());
                    }
                    meta.line_start = line_start;
                    meta.line_end = idx;
                    chunks.push(Chunk {
                        content: chunk_content,
                        token_count: section_tokens,
                        metadata: meta,
                    });
                }
                current_section = String::new();
                line_start = idx;
            }

            // Parse heading
            let trimmed = line.trim_start_matches('#');
            let _current_level = line.chars().take_while(|c| *c == '#').count();
            current_heading = Some(trimmed.trim().to_string());
        }

        current_section.push_str(line);
        current_section.push('\n');
    }

    // Flush remaining
    let section_tokens = (current_section.chars().count() + 3) / 4;
    if section_tokens > 0 {
        let chunk_content = current_section.trim().to_string();
        if !chunk_content.is_empty() {
            let mut meta = metadata.clone();
            if let Some(ref h) = current_heading {
                meta.heading = Some(h.clone());
            }
            meta.line_start = line_start;
            meta.line_end = content.lines().count();
            chunks.push(Chunk {
                content: chunk_content,
                token_count: section_tokens,
                metadata: meta,
            });
        }
    }

    // If any section is still too large, split it further
    let oversized: Vec<usize> = chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| c.token_count > config.chunk_size * 2)
        .map(|(idx, _)| idx)
        .collect();

    if oversized.is_empty() {
        return chunks;
    }

    // Replace oversized chunks with sub-chunks
    let mut result = Vec::new();
    for (_i, chunk) in chunks.into_iter().enumerate() {
        if chunk.token_count > config.chunk_size * 2 {
            let sub_chunks = split_text(&chunk.content, config, &chunk.metadata);
            result.extend(sub_chunks);
        } else {
            result.push(chunk);
        }
    }

    result
}

/// Split code by symbol boundaries (function/class/method).
/// Falls back to `split_text` for unsupported languages or empty symbol lists.
pub fn split_code_by_symbols(
    content: &str,
    symbols: &[Symbol],
    config: &SplitConfig,
    metadata: &ChunkMetadata,
) -> Vec<Chunk> {
    if symbols.is_empty() {
        return split_text(content, config, metadata);
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut covered_lines = std::collections::HashSet::new();

    for sym in symbols {
        let start = sym.start_line.min(lines.len().saturating_sub(1));
        let end = sym.end_line.min(lines.len().saturating_sub(1));
        if end < start {
            continue;
        }

        let chunk_content: String = lines[start..=end].join("\n");
        let token_count = estimate_tokens(&chunk_content);

        // 超大符号内部再切分
        if token_count > config.chunk_size * 3 {
            let sub_meta = ChunkMetadata {
                line_start: start,
                line_end: end,
                heading: Some(format!("{}: {}", sym.kind.as_str(), sym.name)),
                language: metadata.language.clone(),
                file_path: metadata.file_path.clone(),
                symbol_name: Some(sym.name.clone()),
                symbol_kind: Some(sym.kind.as_str().to_string()),
                signature: sym.signature.clone(),
                page_no: None,
            };
            let sub_chunks = split_text(&chunk_content, config, &sub_meta);
            chunks.extend(sub_chunks);
        } else {
            chunks.push(Chunk {
                content: chunk_content,
                token_count,
                metadata: ChunkMetadata {
                    line_start: start,
                    line_end: end,
                    heading: Some(format!("{}: {}", sym.kind.as_str(), sym.name)),
                    language: metadata.language.clone(),
                    file_path: metadata.file_path.clone(),
                    symbol_name: Some(sym.name.clone()),
                    symbol_kind: Some(sym.kind.as_str().to_string()),
                    signature: sym.signature.clone(),
                    page_no: None,
                },
            });
        }

        for i in start..=end {
            covered_lines.insert(i);
        }
    }

    // 收集未被符号覆盖的行（import / 全局变量 / 注释）
    let mut orphan = String::new();
    let mut orphan_start: Option<usize> = None;
    for (idx, line) in lines.iter().enumerate() {
        if !covered_lines.contains(&idx) {
            if orphan_start.is_none() {
                orphan_start = Some(idx);
            }
            orphan.push_str(line);
            orphan.push('\n');
        } else if !orphan.is_empty() {
            let tokens = estimate_tokens(&orphan);
            if tokens > 10 {
                chunks.push(Chunk {
                    content: orphan.trim().to_string(),
                    token_count: tokens,
                    metadata: ChunkMetadata {
                        line_start: orphan_start.unwrap(),
                        line_end: idx,
                        language: metadata.language.clone(),
                        file_path: metadata.file_path.clone(),
                        ..Default::default()
                    },
                });
            }
            orphan.clear();
            orphan_start = None;
        }
    }
    if !orphan.is_empty() {
        let tokens = estimate_tokens(&orphan);
        if tokens > 10 {
            chunks.push(Chunk {
                content: orphan.trim().to_string(),
                token_count: tokens,
                metadata: ChunkMetadata {
                    line_start: orphan_start.unwrap_or(0),
                    line_end: lines.len(),
                    language: metadata.language.clone(),
                    file_path: metadata.file_path.clone(),
                    ..Default::default()
                },
            });
        }
    }

    // 按 line_start 排序
    chunks.sort_by_key(|c| c.metadata.line_start);
    chunks
}

/// Estimate token count from text (~4 chars per token)
fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

/// Main split dispatcher
pub fn split(
    content: &str,
    file_type: &str,
    config: &SplitConfig,
    metadata: &ChunkMetadata,
) -> Vec<Chunk> {
    match file_type {
        "markdown" => split_markdown(content, config, metadata),
        _ => split_text(content, config, metadata),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_metadata_page_no_defaults_to_none() {
        // 旧数据（无 page_no 字段）反序列化兼容
        let meta: ChunkMetadata = serde_json::from_str(r#"{"line_start":0,"line_end":5}"#)
            .expect("deserialize legacy metadata");
        assert_eq!(meta.page_no, None);

        // OCR 文档的 page_no 正常往返
        let meta = ChunkMetadata {
            page_no: Some(3),
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: ChunkMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.page_no, Some(3));
    }

    #[test]
    fn split_text_propagates_page_no() {
        // OCR 路径按页调用 split，页码需透传到每个 chunk 的 metadata
        let config = SplitConfig::default();
        let metadata = ChunkMetadata {
            page_no: Some(7),
            ..Default::default()
        };
        let chunks = split_text("第一行\n第二行\n第三行", &config, &metadata);
        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|c| c.metadata.page_no == Some(7)));
    }
}
