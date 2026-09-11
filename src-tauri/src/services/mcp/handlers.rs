use crate::db::repository::Repository;
use crate::server::router::SharedState;
use crate::services::knowledge::{embedder, rag, repository::KbRepository, retriever};
use crate::services::wiki::{
    handlers as wiki_handlers, ingest as wiki_ingest, project as wiki_project,
    repository::WikiRepository,
};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// MCP server instructions — agent 首次连接时注入 system prompt
const MCP_INSTRUCTIONS: &str = r#"# WaLiAPI RAG — 本地向量检索

RAG 已预建索引：文档已解析、分块、向量化并存入本地 SQLite + HNSW 索引。
所有检索都是本地操作，亚秒级响应。

## 工具使用优先级

1. **ask_knowledge_base** — 首选。直接提问，返回 AI 生成的回答 + 来源引用。
   适合：任何问题、概念理解、代码含义、流程梳理。

2. **search_knowledge_base** — 当需要看原始文本片段，或 ask_knowledge_base 回答不够时使用。
   返回匹配的 chunk 原文 + 相似度分数。

3. **list_knowledge_bases** — 首次使用时调用一次，获取可用 RAG ID。
   之后无需重复调用。

4. **其他工具** — 按需使用（上传文档、管理索引等）。

## 反模式

- ❌ 不要先 search 再自己总结 — 直接用 ask_knowledge_base，它内部已做 RAG
- ❌ 不要每次都调 list_knowledge_bases — 缓存第一次的结果
- ❌ 不要对同一问题反复 search 不同关键词 — 一次 ask_knowledge_base 通常足够

## 代码文件

RAG 中的代码文件按符号边界分块（函数/类/方法），每个 chunk 是完整符号。
chunk metadata 包含 symbol_name、symbol_kind、signature，可用于精确过滤。"#;

// ── Session management for SSE transport ──────────────────────────
// Each SSE client gets a unique session_id. The POST handler uses
// the session_id to push JSON-RPC responses back through the SSE stream.

/// FIX-23：有界通道发送端（容量见 MCP_SSE_CHANNEL_CAPACITY）。
type SessionSender = mpsc::Sender<String>;

fn sse_sessions() -> &'static Arc<RwLock<HashMap<String, SessionSender>>> {
    static SESSIONS: std::sync::OnceLock<Arc<RwLock<HashMap<String, SessionSender>>>> =
        std::sync::OnceLock::new();
    SESSIONS.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

// ── MCP JSON-RPC types ────────────────────────────────────────────

/// MCP JSON-RPC 2.0 request
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct McpRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// MCP JSON-RPC 2.0 response
#[derive(Debug, Serialize)]
pub struct McpResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<McpError>,
}

#[derive(Debug, Serialize)]
pub struct McpError {
    code: i32,
    message: String,
}

impl McpResponse {
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<serde_json::Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(McpError { code, message }),
        }
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

// ── MCP tool definitions ──────────────────────────────────────────

pub(crate) fn get_tools() -> Vec<serde_json::Value> {
    vec![
        // ── Read-only tools (existing) ───────────────────────────
        serde_json::json!({
            "name": "search_knowledge_base",
            "description": "Search across a local knowledge base using hybrid (vector + keyword), vector-only, or keyword-only retrieval. Returns matching text chunks with similarity scores and per-component score breakdowns. Searches a specific KB if kb_id provided, otherwise searches all MCP-enabled KBs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language search query. Will be embedded and matched against document chunks."
                    },
                    "kb_id": {
                        "type": "string",
                        "description": "Specific knowledge base ID to search. If omitted, searches all MCP-enabled KBs."
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 5)",
                        "default": 5
                    },
                    "search_mode": {
                        "type": "string",
                        "enum": ["hybrid", "vector", "keyword"],
                        "description": "Retrieval mode: hybrid (vector+keyword, default), vector (semantic only), keyword (FTS5 only). CJK bigram tokenization is used for Chinese queries.",
                        "default": "hybrid"
                    },
                    "vector_weight": {
                        "type": "number",
                        "description": "Weight for vector similarity score in hybrid mode (0.0-1.0, default: 0.7). Only effective when search_mode=hybrid.",
                        "default": 0.7
                    },
                    "keyword_weight": {
                        "type": "number",
                        "description": "Weight for keyword (FTS5) score in hybrid mode (0.0-1.0, default: 0.3). Only effective when search_mode=hybrid.",
                        "default": 0.3
                    }
                },
                "required": ["query"]
            }
        }),
        serde_json::json!({
            "name": "list_knowledge_bases",
            "description": "List all MCP-enabled knowledge bases. Returns KB ID, name, document count, chunk count, and description. Use this first to discover available knowledge bases before searching or asking questions.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        serde_json::json!({
            "name": "read_document",
            "description": "Read the full content of a specific document in a knowledge base. Use after search_knowledge_base to get the complete text of a matched chunk's source document.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Knowledge base ID" },
                    "doc_id": { "type": "string", "description": "Document ID (from search results)" }
                },
                "required": ["kb_id", "doc_id"]
            }
        }),
        serde_json::json!({
            "name": "ask_knowledge_base",
            "description": "Ask a question to the knowledge base and get an AI-generated answer based on retrieved context (RAG). Uses the configured LLM channel to generate a response grounded in the KB content. Returns the answer, source citations, and per-chunk retrieval score breakdowns (vector score, keyword score, symbol info).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "The question to ask. The answer will be grounded in retrieved document chunks." },
                    "kb_id": { "type": "string", "description": "Knowledge base ID. If omitted, uses all MCP-enabled KBs." },
                    "top_k": { "type": "integer", "description": "Number of chunks to retrieve as context (default: 5)", "default": 5 },
                    "model": { "type": "string", "description": "LLM model to use for answer generation (default: uses channel default)" },
                    "search_mode": {
                        "type": "string",
                        "enum": ["hybrid", "vector", "keyword"],
                        "description": "Retrieval mode: hybrid (vector+keyword, default), vector (semantic only), keyword (FTS5 only). CJK bigram tokenization is used for Chinese queries.",
                        "default": "hybrid"
                    },
                    "vector_weight": {
                        "type": "number",
                        "description": "Weight for vector similarity in hybrid mode (0.0-1.0, default: 0.7). Only effective when search_mode=hybrid.",
                        "default": 0.7
                    },
                    "keyword_weight": {
                        "type": "number",
                        "description": "Weight for keyword (FTS5) in hybrid mode (0.0-1.0, default: 0.3). Only effective when search_mode=hybrid.",
                        "default": 0.3
                    }
                },
                "required": ["question"]
            }
        }),
        serde_json::json!({
            "name": "get_knowledge_base_stats",
            "description": "Get detailed statistics about a knowledge base: document count, ready documents, chunk count, total tokens, embedding model, and index status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Knowledge base ID" }
                },
                "required": ["kb_id"]
            }
        }),
        // ── Write tools: Knowledge Base lifecycle ──────────────────
        serde_json::json!({
            "name": "create_knowledge_base",
            "description": "创建新 RAG 知识库。⚠️ 使用前请先调用 list_knowledge_bases 查看已有知识库，避免重复创建。仅当用户明确要求创建新库，或现有库都不适用时才创建。创建后可通过 upload_document 或 import_source 添加内容。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "RAG 名称（1-100字符）" },
                    "description": { "type": "string", "description": "RAG 用途描述（可选）" },
                    "embedding_model": { "type": "string", "description": "嵌入模型（默认: text-embedding-3-small）" },
                    "embedding_channel_id": { "type": "string", "description": "自定义嵌入渠道 ID（可选）" }
                },
                "required": ["name"]
            }
        }),
        serde_json::json!({
            "name": "update_knowledge_base",
            "description": "Update knowledge base configuration: name, description, embedding model, chunk size, MCP enabled status, etc. Only provided fields will be updated.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Knowledge base ID" },
                    "name": { "type": "string", "description": "New name" },
                    "description": { "type": "string", "description": "New description" },
                    "embedding_model": { "type": "string", "description": "New embedding model (changing this requires re-indexing all documents)" },
                    "embedding_channel_id": { "type": "string", "description": "New embedding channel ID" },
                    "mcp_enabled": { "type": "integer", "description": "1 to enable MCP access, 0 to disable" },
                    "chunk_size": { "type": "integer", "description": "Chunk size in tokens (default: 512)" },
                    "chunk_overlap": { "type": "integer", "description": "Chunk overlap in tokens (default: 64)" }
                },
                "required": ["kb_id"]
            }
        }),
        serde_json::json!({
            "name": "delete_knowledge_base",
            "description": "Permanently delete a knowledge base and all its documents, chunks, and index. This action cannot be undone.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Knowledge base ID to delete" }
                },
                "required": ["kb_id"]
            }
        }),
        // ── Write tools: Document management ───────────────────────
        serde_json::json!({
            "name": "upload_document",
            "description": "上传文档到 RAG。⚠️ 如果未指定 kb_id，将返回已有知识库列表供选择——请先调用 list_knowledge_bases 让用户选择目标库，或确认创建新库。文档上传后会自动解析、分块、向量化并建立索引。支持格式: .txt .md .pdf .docx .doc .pptx .xlsx .csv .json .html .rs .py .js .ts .go .java .c .cpp .h .sh .yaml .yml .toml",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "目标 RAG ID。如果未提供，将返回已有知识库列表供用户选择" },
                    "filename": { "type": "string", "description": "文档文件名（含扩展名，如 'report.pdf'）" },
                    "content": { "type": "string", "description": "Base64 编码的文件内容" }
                },
                "required": ["filename", "content"]
            }
        }),
        serde_json::json!({
            "name": "delete_document",
            "description": "Delete a document from a knowledge base. This removes the document, its chunks, and its embeddings. The HNSW index will be rebuilt automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Knowledge base ID" },
                    "doc_id": { "type": "string", "description": "Document ID to delete" }
                },
                "required": ["kb_id", "doc_id"]
            }
        }),
        serde_json::json!({
            "name": "list_documents",
            "description": "List all documents in a knowledge base with their status (pending, processing, ready, failed), chunk count, and token count.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Knowledge base ID" }
                },
                "required": ["kb_id"]
            }
        }),
        // ── Write tools: Index management ──────────────────────────
        serde_json::json!({
            "name": "build_index",
            "description": "Build or rebuild the HNSW vector index for a knowledge base. This should be called after uploading multiple documents to optimize search performance. The build runs asynchronously.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Knowledge base ID" }
                },
                "required": ["kb_id"]
            }
        }),
        // ── Write tools: Source import ─────────────────────────────
        serde_json::json!({
            "name": "import_source",
            "description": "Import documents from an external source (Git repo, URL, or local directory) into a knowledge base. The import runs asynchronously — use list_documents or get_knowledge_base_stats to check progress.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kb_id": { "type": "string", "description": "Target knowledge base ID" },
                    "source_type": { "type": "string", "enum": ["git", "url", "local_dir"], "description": "Type of source to import from" },
                    "repo_url": { "type": "string", "description": "Git repository URL (for source_type=git)" },
                    "branch": { "type": "string", "description": "Git branch to clone (for source_type=git, default: main)" },
                    "token": { "type": "string", "description": "Git access token for private repos (for source_type=git)" },
                    "url": { "type": "string", "description": "URL to fetch content from (for source_type=url)" },
                    "dir_path": { "type": "string", "description": "Local directory path (for source_type=local_dir)" },
                    "excluded_dirs": { "type": "array", "items": { "type": "string" }, "description": "Directory names to exclude (e.g. ['node_modules', '.git'])" },
                    "included_files": { "type": "array", "items": { "type": "string" }, "description": "File extensions to include (e.g. ['.md', '.txt'])" },
                    "max_file_size": { "type": "integer", "description": "Max file size in bytes (default: 1MB)" }
                },
                "required": ["kb_id", "source_type"]
            }
        }),
        // ── Wiki tools: Project management ──────────────────────────
        serde_json::json!({
            "name": "list_wiki_projects",
            "description": "列出所有 Wiki 项目。返回项目 ID、名称、页面数、源数、描述。Wiki 是结构化知识库，页面按 frontmatter 组织，支持标签、图谱、问答。",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        serde_json::json!({
            "name": "get_wiki_project",
            "description": "获取 Wiki 项目详情：统计信息、标签、页面概览。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" }
                },
                "required": ["project_id"]
            }
        }),
        // ── Wiki tools: Pages ───────────────────────────────────────
        serde_json::json!({
            "name": "list_wiki_pages",
            "description": "列出 Wiki 项目的所有页面，返回路径、标题、类型、标签。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" }
                },
                "required": ["project_id"]
            }
        }),
        serde_json::json!({
            "name": "get_wiki_page",
            "description": "读取 Wiki 页面的完整 Markdown 内容。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "path": { "type": "string", "description": "页面路径（如 'index.md' 或 'guides/setup.md'）" }
                },
                "required": ["project_id", "path"]
            }
        }),
        serde_json::json!({
            "name": "save_wiki_page",
            "description": "创建或更新 Wiki 页面。传入 Markdown 内容，自动提取 frontmatter 标签和 wikilinks。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "path": { "type": "string", "description": "页面路径（如 'guides/api.md'）" },
                    "content": { "type": "string", "description": "页面 Markdown 内容" }
                },
                "required": ["project_id", "path", "content"]
            }
        }),
        // ── Wiki tools: Search & Ask ───────────────────────────────
        serde_json::json!({
            "name": "search_wiki",
            "description": "搜索 Wiki 页面。按标题、路径模糊匹配，并搜索页面内容。返回匹配页面的标题、路径、摘要片段。适合结构化知识检索，比 RAG chunk 更精确。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "query": { "type": "string", "description": "搜索关键词" },
                    "top_k": { "type": "integer", "description": "最大返回结果数（默认 10）", "default": 10 }
                },
                "required": ["project_id", "query"]
            }
        }),
        serde_json::json!({
            "name": "ask_wiki",
            "description": "向 Wiki 提问，获取基于 Wiki 页面的 AI 回答。检索相关页面 → LLM 生成回答 → 返回回答 + 来源引用。与 RAG ask 类似，但基于完整 Wiki 页面而非 chunk。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "question": { "type": "string", "description": "问题" },
                    "top_k": { "type": "integer", "description": "检索页面数（默认 5）", "default": 5 },
                    "model": { "type": "string", "description": "LLM 模型（默认用项目配置）" }
                },
                "required": ["project_id", "question"]
            }
        }),
        // ── Wiki tools: Tags & Graph ───────────────────────────────
        serde_json::json!({
            "name": "get_wiki_tags",
            "description": "获取 Wiki 项目的标签列表（从页面 frontmatter 自动提取），按频率排序。可用于快速了解 Wiki 覆盖的主题。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "limit": { "type": "integer", "description": "返回标签数（默认 15）", "default": 15 }
                },
                "required": ["project_id"]
            }
        }),
        serde_json::json!({
            "name": "get_wiki_graph",
            "description": "获取 Wiki 知识图谱：页面（节点）和 wikilinks（边）。用于可视化知识关联。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" }
                },
                "required": ["project_id"]
            }
        }),
        // ── Wiki tools: Sources ─────────────────────────────────────
        serde_json::json!({
            "name": "list_wiki_sources",
            "description": "列出 Wiki 项目的源资料及其摄入状态。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" }
                },
                "required": ["project_id"]
            }
        }),
        serde_json::json!({
            "name": "ingest_wiki_source",
            "description": "触发 Wiki 源资料的摄入：自动解析文档 → 生成结构化 Wiki 页面 → 提取标签和 wikilinks。异步执行，可用 list_wiki_pages 检查进度。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "source_id": { "type": "string", "description": "源资料 ID" }
                },
                "required": ["project_id", "source_id"]
            }
        }),
        // ── Wiki tools: Project lifecycle ───────────────────────────
        serde_json::json!({
            "name": "create_wiki_project",
            "description": "创建新 Wiki 项目。Wiki 是结构化知识库，页面按 frontmatter 组织，支持标签、图谱、问答。创建后可通过 save_wiki_page 添加页面或 add_wiki_source 添加源资料。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Wiki 项目名称（1-100字符）" },
                    "description": { "type": "string", "description": "项目描述（可选）" },
                    "ingest_model": { "type": "string", "description": "摄入模型（用于自动生成页面的 LLM，可选）" },
                    "chat_model": { "type": "string", "description": "问答模型（用于 Wiki 问答的 LLM，可选）" },
                    "ingest_channel_id": { "type": "string", "description": "摄入渠道 ID（可选，默认自动选择）" },
                    "chat_channel_id": { "type": "string", "description": "问答渠道 ID（可选，默认自动选择）" },
                    "schema_text": { "type": "string", "description": "自定义 Wiki schema（CLAUDE.md 内容，可选）。定义页面结构规范、标签约定等。" }
                },
                "required": ["name"]
            }
        }),
        serde_json::json!({
            "name": "delete_wiki_project",
            "description": "永久删除 Wiki 项目及其所有页面、源资料和目录。此操作不可恢复。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" }
                },
                "required": ["project_id"]
            }
        }),
        // ── Wiki tools: Page deletion ───────────────────────────────
        serde_json::json!({
            "name": "delete_wiki_page",
            "description": "删除 Wiki 项目中的指定页面。同时删除数据库记录和磁盘文件。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "path": { "type": "string", "description": "页面路径（如 'guides/old-page.md'）" }
                },
                "required": ["project_id", "path"]
            }
        }),
        // ── Wiki tools: Source management ──────────────────────────
        serde_json::json!({
            "name": "add_wiki_source",
            "description": "添加源资料到 Wiki 项目。可传入文件内容，自动保存到磁盘。添加后可用 ingest_wiki_source 触发摄入，自动生成结构化 Wiki 页面。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "filename": { "type": "string", "description": "源资料文件名（如 'design.md' 或 'service.java'）" },
                    "source_type": { "type": "string", "description": "源类型（如 'md', 'java', 'py', 'txt' 等）" },
                    "content": { "type": "string", "description": "文件内容（纯文本，非 base64）。如果提供，会自动保存到磁盘。" },
                    "file_path": { "type": "string", "description": "已有文件路径（可选，如果不提供 content 则用此路径）" },
                    "source_url": { "type": "string", "description": "来源 URL（可选）" }
                },
                "required": ["project_id", "filename", "source_type"]
            }
        }),
        serde_json::json!({
            "name": "delete_wiki_source",
            "description": "删除 Wiki 项目中的指定源资料。仅删除源记录，不影响已通过该源生成的 Wiki 页面。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_id": { "type": "string", "description": "Wiki 项目 ID" },
                    "source_id": { "type": "string", "description": "源资料 ID" }
                },
                "required": ["project_id", "source_id"]
            }
        }),
    ]
}

// ── Core JSON-RPC dispatch ────────────────────────────────────────

/// Main MCP JSON-RPC handler — async dispatch
async fn dispatch_jsonrpc_async(shared: &SharedState, req: &McpRequest) -> McpResponse {
    match req.method.as_str() {
        "initialize" => McpResponse::success(
            req.id.clone(),
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "WaLiAPI Knowledge Base",
                    "version": "0.1.0"
                },
                "instructions": MCP_INSTRUCTIONS
            }),
        ),
        "notifications/initialized" => McpResponse::success(req.id.clone(), serde_json::json!({})),
        "tools/list" => McpResponse::success(
            req.id.clone(),
            serde_json::json!({
                "tools": get_tools()
            }),
        ),
        "tools/call" => {
            let tool_name = req
                .params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("");

            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));

            match handle_tool_call(shared, tool_name, &args).await {
                Ok(result) => McpResponse::success(req.id.clone(), result),
                Err(e) => McpResponse::error(req.id.clone(), -32603, e),
            }
        }
        "ping" => McpResponse::success(req.id.clone(), serde_json::json!({})),
        _ => McpResponse::error(
            req.id.clone(),
            -32601,
            format!("Unknown method: {}", req.method),
        ),
    }
}

// ── SSE endpoint: GET /mcp/sse ────────────────────────────────────
// Standard MCP SSE transport:
// 1. Client opens SSE connection
// 2. Server sends `endpoint` event with POST URL (includes session_id)
// 3. Client POSTs JSON-RPC requests to that URL
// 4. Server pushes responses back through the SSE stream

/// FIX-23：SSE 会话消息通道容量上限。慢客户端不再无界积压——通道满时
/// `try_send` 失败即丢该条响应（MPClient 靠 JSON-RPC id 匹配，超时自行处理）。
const MCP_SSE_CHANNEL_CAPACITY: usize = 64;

/// FIX-23：会话清理守卫——持有于 SSE 生成器内，客户端断开使流被 Drop
/// 时触发，立即移除会话条目（替代旧的 1 小时定时清扫，断连不再滞留）。
struct SseSessionGuard {
    session_id: String,
}

impl Drop for SseSessionGuard {
    fn drop(&mut self) {
        let sessions = sse_sessions().clone();
        let id = std::mem::take(&mut self.session_id);
        // Drop 内不能 await：转交 tokio 执行；runtime 已关停则无事可做。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                sessions.write().await.remove(&id);
            });
        }
    }
}

pub async fn handle_mcp_sse(State(_shared): State<SharedState>) -> Response {
    // Generate unique session ID
    let session_id = uuid::Uuid::new_v4().to_string();

    // Create channel for this session (bounded, FIX-23)
    let (tx, mut rx) = mpsc::channel::<String>(MCP_SSE_CHANNEL_CAPACITY);

    // Register session
    sse_sessions().write().await.insert(session_id.clone(), tx);

    // Build SSE stream
    let session_id_clone = session_id.clone();
    let stream = async_stream::stream! {
        // 断连即清（FIX-23）：守卫随生成器 Drop 而触发。
        let _session_guard = SseSessionGuard { session_id: session_id_clone.clone() };
        // 1. Send endpoint event — tells client where to POST JSON-RPC
        let endpoint_url = format!("/mcp?session_id={}", session_id_clone);
        let endpoint_event = format!(
            "event: endpoint\ndata: {}\n\n",
            endpoint_url
        );
        yield Ok::<_, std::io::Error>(endpoint_event.into_bytes());

        // 2. Keep-alive loop + forward JSON-RPC responses
        let mut keepalive_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        keepalive_interval.tick().await; // first tick is immediate

        loop {
            tokio::select! {
                // Forward JSON-RPC responses to client
                msg = rx.recv() => {
                    match msg {
                        Some(msg) => {
                            let sse_data = format!("data: {}\n\n", msg);
                            yield Ok::<_, std::io::Error>(sse_data.into_bytes());
                        }
                        // 发送端全部被清理（守卫之外的异常路径）→ 结束流。
                        None => break,
                    }
                }
                // Keepalive
                _ = keepalive_interval.tick() => {
                    yield Ok::<_, std::io::Error>(b": keepalive\n\n".to_vec());
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
}

// ── POST endpoint: POST /mcp?session_id=xxx ───────────────────────
// Receives JSON-RPC requests and pushes responses through the SSE stream

#[derive(Debug, Deserialize)]
pub struct McpQueryParams {
    #[serde(default)]
    pub session_id: Option<String>,
}

pub async fn handle_mcp(
    State(shared): State<SharedState>,
    Query(params): Query<McpQueryParams>,
    body: axum::body::Bytes,
) -> Response {
    let body_str = String::from_utf8_lossy(&body);

    // Parse JSON-RPC request
    let req: McpRequest = match serde_json::from_str(&body_str) {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)).into_response();
        }
    };

    // Check if this is a notification (no id → no response)
    let is_notification = req.id.is_none();

    let response = dispatch_jsonrpc_async(&shared, &req).await;

    // If session_id is provided, push response through SSE stream
    if let Some(session_id) = &params.session_id {
        let sessions = sse_sessions().read().await;
        if let Some(tx) = sessions.get(session_id) {
            // FIX-23：有界通道 + try_send——慢客户端积压满 64 条时丢弃
            // 本条响应（不再无界膨胀内存），同时检测接收端已断开。
            if let Err(e) = tx.try_send(response.to_json_string()) {
                tracing::warn!("MCP SSE session {session_id} push dropped: {e}");
            }
        }
    }

    // For SSE transport: return 202 Accepted (response goes through SSE)
    // For direct POST (no session_id): return JSON response directly
    if params.session_id.is_some() {
        if is_notification {
            return StatusCode::ACCEPTED.into_response();
        }
        // Response is sent via SSE, but also return 202
        return StatusCode::ACCEPTED.into_response();
    }

    // No session_id — notifications get 202 with no body
    if is_notification {
        return StatusCode::ACCEPTED.into_response();
    }

    // Direct POST: return JSON response
    Json(response).into_response()
}

// ── Tool call handlers ────────────────────────────────────────────

async fn handle_tool_call(
    shared: &SharedState,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let pool = &shared.state.db.pool;

    match tool_name {
        "search_knowledge_base" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or("Missing query")?;
            let kb_id = args.get("kb_id").and_then(|k| k.as_str()).unwrap_or("");
            let top_k = args.get("top_k").and_then(|t| t.as_u64()).unwrap_or(5) as usize;
            let search_mode = args
                .get("search_mode")
                .and_then(|s| s.as_str())
                .unwrap_or("hybrid");
            let vector_weight = args
                .get("vector_weight")
                .and_then(|w| w.as_f64())
                .unwrap_or(0.7) as f32;
            let keyword_weight = args
                .get("keyword_weight")
                .and_then(|w| w.as_f64())
                .unwrap_or(0.3) as f32;

            let emb_model = if !kb_id.is_empty() {
                let kb_repo = KbRepository::new(pool.clone());
                kb_repo
                    .get_kb(kb_id)
                    .await
                    .ok()
                    .and_then(|kb| kb.embedding_model)
                    .unwrap_or_else(|| "text-embedding-3-small".to_string())
            } else {
                "text-embedding-3-small".to_string()
            };

            let repo = Repository::new(pool.clone());

            // Keyword-only mode: no embedding needed
            if search_mode == "keyword" && !kb_id.is_empty() {
                let results = retriever::keyword_only_search(pool, kb_id, query, top_k).await?;
                let content: Vec<serde_json::Value> = results.iter().map(|r| {
                    serde_json::json!({
                        "type": "text",
                        "text": format!("[{}] (score: {:.2}) [keyword]\n{}", r.filename, r.score, r.content)
                    })
                }).collect();
                return Ok(serde_json::json!({ "content": content, "isError": false }));
            }

            let embeddings = embedder::embed(&[query.to_string()], &emb_model, &repo).await?;
            if embeddings.is_empty() {
                return Err("Failed to embed query".to_string());
            }

            if kb_id.is_empty() {
                // Cross-KB search: always hybrid (search_all doesn't support mode selection)
                let results = retriever::search_all(pool, &embeddings[0], top_k, true).await?;
                let content: Vec<serde_json::Value> = results.iter().map(|r| {
                    serde_json::json!({
                        "type": "text",
                        "text": format!("[{}] (score: {:.2})\n{}", r.filename, r.score, r.content)
                    })
                }).collect();
                Ok(serde_json::json!({ "content": content, "isError": false }))
            } else {
                // Single-KB search with details
                let scored = retriever::hybrid_search_with_details(
                    pool,
                    kb_id,
                    query,
                    &embeddings[0],
                    top_k,
                    vector_weight,
                    keyword_weight,
                    // MCP 工具路径用默认融合模式（RRF）；面板路径按 kb.fusion_mode 设置
                    retriever::FusionMode::Rrf,
                )
                .await?;

                let content: Vec<serde_json::Value> = scored
                    .iter()
                    .map(|s| {
                        let r = &s.result;
                        let mut line = format!("[{}] (score: {:.2}", r.filename, r.score);
                        if let Some(vs) = s.vector_score {
                            line.push_str(&format!(", vec: {:.2}", vs));
                        }
                        if let Some(ks) = s.keyword_score {
                            line.push_str(&format!(", kw: {:.2}", ks));
                        }
                        line.push_str(")\n");
                        line.push_str(&r.content);
                        serde_json::json!({ "type": "text", "text": line })
                    })
                    .collect();

                Ok(serde_json::json!({ "content": content, "isError": false }))
            }
        }

        "list_knowledge_bases" => {
            let kb_repo = KbRepository::new(pool.clone());
            let kbs = kb_repo.get_all_kbs().await.map_err(|e| e.to_string())?;

            // Only expose KBs with mcp_enabled = 1
            let exposed: Vec<_> = kbs.iter().filter(|kb| kb.mcp_enabled == 1).collect();

            let content: Vec<serde_json::Value> = exposed.iter().map(|kb| {
                serde_json::json!({
                    "type": "text",
                    "text": format!("ID: {}\nName: {}\nDocuments: {}\nChunks: {}\nDescription: {}",
                        kb.id, kb.name, kb.doc_count, kb.chunk_count,
                        kb.description.as_deref().unwrap_or("N/A"))
                })
            }).collect();

            Ok(serde_json::json!({
                "content": content,
                "isError": false
            }))
        }

        "read_document" => {
            let _kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;
            let doc_id = args
                .get("doc_id")
                .and_then(|d| d.as_str())
                .ok_or("Missing doc_id")?;

            let kb_repo = KbRepository::new(pool.clone());
            let doc = kb_repo
                .get_document(doc_id)
                .await
                .map_err(|e| e.to_string())?;

            let content = if let Some(path) = &doc.file_path {
                std::fs::read_to_string(path).unwrap_or_else(|_| "Failed to read file".to_string())
            } else {
                "No file path available".to_string()
            };

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("File: {}\n\n{}", doc.filename, content)
                }],
                "isError": false
            }))
        }

        "ask_knowledge_base" => {
            let question = args
                .get("question")
                .and_then(|q| q.as_str())
                .ok_or("Missing question")?;
            let kb_id = args.get("kb_id").and_then(|k| k.as_str()).unwrap_or("");
            let top_k = args.get("top_k").and_then(|t| t.as_u64()).unwrap_or(5) as usize;
            let search_mode = args
                .get("search_mode")
                .and_then(|s| s.as_str())
                .unwrap_or("hybrid");
            let vector_weight = args
                .get("vector_weight")
                .and_then(|w| w.as_f64())
                .unwrap_or(0.7) as f32;
            let keyword_weight = args
                .get("keyword_weight")
                .and_then(|w| w.as_f64())
                .unwrap_or(0.3) as f32;

            let emb_model = if !kb_id.is_empty() {
                let kb_repo = KbRepository::new(pool.clone());
                kb_repo
                    .get_kb(kb_id)
                    .await
                    .ok()
                    .and_then(|kb| kb.embedding_model)
                    .unwrap_or_else(|| "text-embedding-3-small".to_string())
            } else {
                "text-embedding-3-small".to_string()
            };

            // Auto-select chat model from available channels if not specified
            let chat_model = if let Some(m) = args.get("model").and_then(|m| m.as_str()) {
                m.to_string()
            } else {
                let main_repo = Repository::new(pool.clone());
                let channels = main_repo.get_enabled_channels().await.unwrap_or_default();
                let embedding_models = [
                    "text-embedding-3-small",
                    "text-embedding-3-large",
                    "text-embedding-ada-002",
                    "bge-large-zh",
                    "bge-small-zh",
                ];
                let mut picked: Option<String> = None;
                for ch in &channels {
                    let models: Vec<String> = serde_json::from_str(&ch.models).unwrap_or_default();
                    if let Some(m) = models
                        .iter()
                        .find(|m| !embedding_models.contains(&m.as_str()))
                    {
                        picked = Some(m.clone());
                        break;
                    }
                }
                picked.unwrap_or_else(|| "gpt-4o".to_string())
            };

            let answer = rag::ask_with_config(
                pool,
                kb_id,
                question,
                &emb_model,
                &chat_model,
                top_k,
                true,
                &[],
                &shared.state.settings,
                vector_weight,
                keyword_weight,
                search_mode,
            )
            .await?;

            let mut content = vec![serde_json::json!({
                "type": "text",
                "text": answer.answer
            })];

            // Source citations
            for source in &answer.sources {
                content.push(serde_json::json!({
                    "type": "text",
                    "text": format!("Source: {} (score: {:.2})\n{}", source.filename, source.score, source.snippet)
                }));
            }

            // Retrieval details: per-chunk score breakdown
            if let Some(details) = &answer.retrieval_details {
                let mut detail_lines = String::from("\n--- Retrieval Details ---\n");
                for d in details {
                    let mut line = format!("• {} (score: {:.2}", d.filename, d.score);
                    if let Some(vs) = d.vector_score {
                        line.push_str(&format!(", vec: {:.2}", vs));
                    }
                    if let Some(ks) = d.keyword_score {
                        line.push_str(&format!(", kw: {:.2}", ks));
                    }
                    if let Some(sym) = &d.symbol_name {
                        line.push_str(&format!(", symbol: {}", sym));
                        if let Some(kind) = &d.symbol_kind {
                            line.push_str(&format!(" ({})", kind));
                        }
                    }
                    line.push_str(")");
                    detail_lines.push_str(&line);
                    detail_lines.push('\n');
                }
                content.push(serde_json::json!({
                    "type": "text",
                    "text": detail_lines
                }));
            }

            Ok(serde_json::json!({
                "content": content,
                "isError": false
            }))
        }

        "get_knowledge_base_stats" => {
            let kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;

            let kb_repo = KbRepository::new(pool.clone());
            let kb = kb_repo.get_kb(kb_id).await.map_err(|e| e.to_string())?;
            let docs = kb_repo
                .get_documents(kb_id)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Knowledge Base: {}\nDocuments: {} (ready: {})\nChunks: {}\nTotal Tokens: {}",
                        kb.name,
                        kb.doc_count,
                        docs.iter().filter(|d| d.status == "ready").count(),
                        kb.chunk_count,
                        kb.total_tokens
                    )
                }],
                "isError": false
            }))
        }

        // ── Write tools: Knowledge Base lifecycle ──────────────────
        "create_knowledge_base" => {
            let name = args
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or("Missing name")?;
            let description = args.get("description").and_then(|d| d.as_str());
            let embedding_model = args.get("embedding_model").and_then(|m| m.as_str());
            let embedding_channel_id = args.get("embedding_channel_id").and_then(|c| c.as_str());

            let input = crate::services::knowledge::models::CreateKbInput {
                name: name.to_string(),
                description: description.map(|s| s.to_string()),
                embedding_model: embedding_model.map(|s| s.to_string()),
                embedding_channel_id: embedding_channel_id.map(|s| s.to_string()),
                // MCP 工具不暴露 OCR 模型配置（在知识库设置表单中配置）
                ocr_model: None,
            };

            let kb_repo = KbRepository::new(pool.clone());
            let kb = kb_repo.create_kb(&input).await.map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Knowledge base created successfully.\nID: {}\nName: {}\nDescription: {}\nEmbedding model: {}\nMCP enabled: true",
                        kb.id,
                        kb.name,
                        kb.description.as_deref().unwrap_or("N/A"),
                        kb.embedding_model.as_deref().unwrap_or("text-embedding-3-small")
                    )
                }],
                "isError": false
            }))
        }

        "update_knowledge_base" => {
            let kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;

            let input = crate::services::knowledge::models::UpdateKbInput {
                name: args
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string()),
                description: args
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string()),
                embedding_model: args
                    .get("embedding_model")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string()),
                embedding_channel_id: args
                    .get("embedding_channel_id")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string()),
                status: args.get("status").and_then(|s| s.as_i64()),
                mcp_enabled: args.get("mcp_enabled").and_then(|m| m.as_i64()),
                chunk_size: args.get("chunk_size").and_then(|c| c.as_i64()),
                chunk_overlap: args.get("chunk_overlap").and_then(|c| c.as_i64()),
                excluded_dirs: args
                    .get("excluded_dirs")
                    .and_then(|e| e.as_str())
                    .map(|s| s.to_string()),
                excluded_files: args
                    .get("excluded_files")
                    .and_then(|e| e.as_str())
                    .map(|s| s.to_string()),
                included_files: args
                    .get("included_files")
                    .and_then(|i| i.as_str())
                    .map(|s| s.to_string()),
                embedding_batch_size: args.get("embedding_batch_size").and_then(|b| b.as_i64()),
                // MCP 工具不暴露 OCR 模型配置（在知识库设置表单中配置）
                ocr_model: None,
            };

            let kb_repo = KbRepository::new(pool.clone());
            let kb = kb_repo
                .update_kb(kb_id, &input)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Knowledge base updated.\nID: {}\nName: {}\nMCP enabled: {}\nChunk size: {}\nChunk overlap: {}",
                        kb.id, kb.name, kb.mcp_enabled, kb.chunk_size, kb.chunk_overlap
                    )
                }],
                "isError": false
            }))
        }

        "delete_knowledge_base" => {
            let kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;

            let kb_repo = KbRepository::new(pool.clone());
            let kb = kb_repo.get_kb(kb_id).await.map_err(|e| e.to_string())?;
            kb_repo.delete_kb(kb_id).await.map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Knowledge base '{}' ({}) has been permanently deleted.", kb.name, kb_id)
                }],
                "isError": false
            }))
        }

        // ── Write tools: Document management ───────────────────────
        "upload_document" => {
            let filename = args
                .get("filename")
                .and_then(|f| f.as_str())
                .ok_or("Missing filename")?;
            let content_b64 = args
                .get("content")
                .and_then(|c| c.as_str())
                .ok_or("Missing content")?;

            let kb_repo = KbRepository::new(pool.clone());

            // If kb_id not provided, return available KBs for user to choose
            let kb_id = match args.get("kb_id").and_then(|k| k.as_str()) {
                Some(id) if !id.is_empty() => id.to_string(),
                _ => {
                    let kbs = kb_repo.get_all_kbs().await.map_err(|e| e.to_string())?;
                    let exposed: Vec<_> = kbs.iter().filter(|kb| kb.mcp_enabled == 1).collect();

                    if exposed.is_empty() {
                        return Ok(serde_json::json!({
                            "content": [{
                                "type": "text",
                                "text": "⚠️ 未指定 RAG，且当前没有任何可用的知识库。\n\n请先调用 create_knowledge_base 创建一个 RAG，然后再上传文档。"
                            }],
                            "isError": false
                        }));
                    }

                    let mut lines = vec!["⚠️ 未指定目标 RAG。请选择一个已有知识库，或确认创建新库。\n\n已有知识库列表:".to_string()];
                    for (i, kb) in exposed.iter().enumerate() {
                        lines.push(format!(
                            "\n[{}] ID: {}\n    名称: {}\n    文档数: {} | 切片数: {} | Tokens: {}\n    描述: {}",
                            i + 1,
                            kb.id,
                            kb.name,
                            kb.doc_count,
                            kb.chunk_count,
                            kb.total_tokens,
                            kb.description.as_deref().unwrap_or("无")
                        ));
                    }
                    lines.push(
                        "\n\n请告诉 AI 你要上传到哪个 RAG（提供 ID 或名称），或者要求创建新 RAG。"
                            .to_string(),
                    );

                    return Ok(serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": lines.join("\n")
                        }],
                        "isError": false
                    }));
                }
            };

            let filename = filename.to_string();

            let content = {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(content_b64)
                    .map_err(|e| format!("Invalid base64: {}", e))?
            };

            use sha2::Digest;
            let hash = sha2::Sha256::digest(&content);
            let hash_hex = hex::encode(hash);

            let kb_repo = KbRepository::new(pool.clone());

            // Check duplicate
            if let Ok(Some(_)) = kb_repo.find_document_by_hash(&kb_id, &hash_hex).await {
                return Ok(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Document '{}' already exists in this knowledge base (same content hash).", filename)
                    }],
                    "isError": false
                }));
            }

            let file_type = crate::services::knowledge::parser::get_file_type(&filename);
            let file_size = content.len() as i64;

            // Save file to disk
            let kb_dir = shared.state.data_dir.join("kb_files").join(&kb_id);
            std::fs::create_dir_all(&kb_dir).ok();
            let doc_id = uuid::Uuid::new_v4().to_string();
            let file_path = kb_dir.join(format!("{}_{}", &doc_id, &filename));
            std::fs::write(&file_path, &content).ok();
            let file_path_str = file_path.to_string_lossy().to_string();

            let doc = kb_repo
                .create_document(
                    &kb_id,
                    &filename,
                    Some(&file_path_str),
                    &file_type,
                    file_size,
                    &hash_hex,
                )
                .await
                .map_err(|e| e.to_string())?;

            let kb = kb_repo.get_kb(&kb_id).await.map_err(|e| e.to_string())?;
            let emb_model = kb.embedding_model.clone();

            // Spawn async processing
            let pool_clone = pool.clone();
            let events_clone = shared.state.events.clone();
            let doc_id_clone = doc.id.clone();
            let filename_clone = filename.clone();
            let kb_id_clone = kb_id.clone();
            let settings_clone = shared.state.settings.clone();
            let data_dir_clone = shared.state.data_dir.clone();

            tokio::spawn(async move {
                if let Err(e) = crate::services::knowledge::processor::process_document(
                    &pool_clone,
                    &events_clone,
                    &kb_id_clone,
                    &doc_id_clone,
                    &filename_clone,
                    &content,
                    emb_model.as_deref(),
                    &settings_clone,
                    &data_dir_clone,
                )
                .await
                {
                    tracing::error!("Document processing failed: {}", e);
                }
            });

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Document '{}' uploaded to knowledge base.\nDoc ID: {}\nFile type: {}\nSize: {} bytes\nStatus: processing (will be automatically chunked, embedded, and indexed)",
                        filename, doc.id, file_type, file_size
                    )
                }],
                "isError": false
            }))
        }

        "delete_document" => {
            let kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;
            let doc_id = args
                .get("doc_id")
                .and_then(|d| d.as_str())
                .ok_or("Missing doc_id")?;

            let kb_repo = KbRepository::new(pool.clone());
            let doc = kb_repo
                .get_document(doc_id)
                .await
                .map_err(|e| e.to_string())?;

            // Delete file from disk
            crate::services::knowledge::upload::remove_managed_file(
                &shared.state.data_dir,
                &doc.kb_id,
                &doc.source_type,
                doc.file_path.as_deref(),
            )?;
            // 级联删除 OCR 页级缓存（以内容哈希为键）
            crate::services::knowledge::ocr::cache::remove_cache(
                &shared.state.data_dir,
                &doc.content_hash,
            );

            // Delete chunks and document record
            kb_repo.delete_chunks_by_doc(doc_id).await.ok();
            kb_repo
                .delete_document(doc_id)
                .await
                .map_err(|e| e.to_string())?;
            kb_repo
                .update_kb_counts(kb_id)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Document '{}' ({}) has been deleted from the knowledge base.", doc.filename, doc_id)
                }],
                "isError": false
            }))
        }

        "list_documents" => {
            let kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;

            let kb_repo = KbRepository::new(pool.clone());
            let docs = kb_repo
                .get_documents(kb_id)
                .await
                .map_err(|e| e.to_string())?;

            if docs.is_empty() {
                return Ok(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": "No documents in this knowledge base yet."
                    }],
                    "isError": false
                }));
            }

            let lines: Vec<String> = docs
                .iter()
                .map(|d| {
                    format!(
                        "- {} | ID: {} | Status: {} | Chunks: {} | Tokens: {} | Size: {} bytes",
                        d.filename, d.id, d.status, d.chunk_count, d.token_count, d.file_size
                    )
                })
                .collect();

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Documents in knowledge base ({} total):\n{}", docs.len(), lines.join("\n"))
                }],
                "isError": false
            }))
        }

        // ── Write tools: Index management ──────────────────────────
        "build_index" => {
            let kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;

            let kb_repo = KbRepository::new(pool.clone());
            kb_repo.update_kb_index_status(kb_id, "building").await.ok();

            let pool_clone = pool.clone();
            let events_clone = shared.state.events.clone();
            let kb_id_clone = kb_id.to_string();

            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async {
                    events_clone.emit(
                        "kb-index-progress",
                        serde_json::json!({
                            "kb_id": &kb_id_clone,
                            "status": "building",
                            "message": "Building HNSW index…"
                        }),
                    );

                    match crate::services::knowledge::retriever::build_index(
                        &pool_clone,
                        &kb_id_clone,
                        &events_clone,
                    )
                    .await
                    {
                        Ok(()) => {
                            tracing::info!("HNSW index built for KB {}", kb_id_clone);
                            events_clone.emit(
                                "kb-index-progress",
                                serde_json::json!({
                                    "kb_id": &kb_id_clone,
                                    "status": "ready",
                                    "message": "Index build complete"
                                }),
                            );
                        }
                        Err(e) => {
                            tracing::error!("Index build failed for KB {}: {}", kb_id_clone, e);
                            let repo = KbRepository::new(pool_clone.clone());
                            repo.update_kb_index_status(&kb_id_clone, "error")
                                .await
                                .ok();
                            events_clone.emit(
                                "kb-index-progress",
                                serde_json::json!({
                                    "kb_id": &kb_id_clone,
                                    "status": "error",
                                    "message": format!("Index build failed: {}", e)
                                }),
                            );
                        }
                    }
                });
            });

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Index build started for knowledge base {}. Use get_knowledge_base_stats to check progress.", kb_id)
                }],
                "isError": false
            }))
        }

        // ── Write tools: Source import ─────────────────────────────
        "import_source" => {
            let kb_id = args
                .get("kb_id")
                .and_then(|k| k.as_str())
                .ok_or("Missing kb_id")?;
            let source_type = args
                .get("source_type")
                .and_then(|s| s.as_str())
                .ok_or("Missing source_type")?;
            let kb_id = kb_id.to_string();

            let input =
                crate::services::knowledge::models::ImportSourceInput {
                    source_type: source_type.to_string(),
                    repo_url: args
                        .get("repo_url")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string()),
                    branch: args
                        .get("branch")
                        .and_then(|b| b.as_str())
                        .map(|s| s.to_string()),
                    token: args
                        .get("token")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string()),
                    url: args
                        .get("url")
                        .and_then(|u| u.as_str())
                        .map(|s| s.to_string()),
                    dir_path: args
                        .get("dir_path")
                        .and_then(|d| d.as_str())
                        .map(|s| s.to_string()),
                    excluded_dirs: args.get("excluded_dirs").and_then(|e| e.as_array()).map(
                        |arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        },
                    ),
                    included_files: args.get("included_files").and_then(|i| i.as_array()).map(
                        |arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        },
                    ),
                    max_file_size: args
                        .get("max_file_size")
                        .and_then(|m| m.as_u64())
                        .map(|v| v as usize),
                };

            let kb_repo = KbRepository::new(pool.clone());
            let source = kb_repo
                .create_source(
                    &kb_id,
                    &input.source_type,
                    input.repo_url.as_deref().or(input.url.as_deref()),
                    input.dir_path.as_deref(),
                    input.branch.as_deref(),
                )
                .await
                .map_err(|e| e.to_string())?;

            let source_id = source.id.clone();
            let source_type_clone = input.source_type.clone();
            let pool_clone = pool.clone();
            let events_clone = shared.state.events.clone();
            let kb_id_clone = kb_id.clone();
            let settings_clone = shared.state.settings.clone();
            let data_dir_clone = shared.state.data_dir.clone();

            tokio::spawn(async move {
                let result = if source_type_clone == "git" {
                    crate::services::knowledge::importer::import_git_repo(
                        &pool_clone,
                        &events_clone,
                        &kb_id_clone,
                        &source_id,
                        &input,
                        &settings_clone,
                        &data_dir_clone,
                    )
                    .await
                } else if source_type_clone == "url" {
                    crate::services::knowledge::importer::import_url(
                        &pool_clone,
                        &events_clone,
                        &kb_id_clone,
                        &source_id,
                        &input,
                        &settings_clone,
                        &data_dir_clone,
                    )
                    .await
                } else if source_type_clone == "local_dir" {
                    crate::services::knowledge::importer::import_local_dir(
                        &pool_clone,
                        &events_clone,
                        &kb_id_clone,
                        &source_id,
                        &input,
                        &settings_clone,
                        &data_dir_clone,
                    )
                    .await
                } else {
                    Err(format!("Unknown source type: {}", source_type_clone))
                };

                let repo = KbRepository::new(pool_clone.clone());
                match result {
                    Ok(count) => {
                        repo.update_source_status(&source_id, "done", count as i64, None)
                            .await
                            .ok();
                    }
                    Err(e) => {
                        repo.update_source_status(&source_id, "error", 0, Some(&e))
                            .await
                            .ok();
                        tracing::error!("Import failed: {}", e);
                    }
                }
            });

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Import started.\nSource ID: {}\nType: {}\nKnowledge base: {}\nThe import runs asynchronously. Use list_documents or get_knowledge_base_stats to check progress.",
                        source.id, source_type, kb_id
                    )
                }],
                "isError": false
            }))
        }

        // ── Wiki tools ─────────────────────────────────────────────
        "list_wiki_projects" => {
            let wiki_repo = WikiRepository::new(pool.clone());
            let projects = wiki_repo.list_projects().await.map_err(|e| e.to_string())?;

            let content: Vec<serde_json::Value> = projects.iter().map(|p| {
                serde_json::json!({
                    "type": "text",
                    "text": format!("ID: {}\nName: {}\nPages: {} | Sources: {}\nDescription: {}",
                        p.id, p.name, p.page_count, p.source_count, p.description.as_deref().unwrap_or("N/A"))
                })
            }).collect();

            Ok(serde_json::json!({ "content": content, "isError": false }))
        }

        "get_wiki_project" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let wiki_repo = WikiRepository::new(pool.clone());
            let proj = wiki_repo
                .get_project(project_id)
                .await
                .map_err(|e| e.to_string())?;
            let stats = wiki_repo
                .get_stats(project_id)
                .await
                .unwrap_or(serde_json::json!({}));

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "ID: {}\nName: {}\nPages: {} | Sources: {}\nDescription: {}\nStats: {}",
                        proj.id, proj.name, proj.page_count, proj.source_count,
                        proj.description.as_deref().unwrap_or("N/A"),
                        serde_json::to_string_pretty(&stats).unwrap_or_default()
                    )
                }],
                "isError": false
            }))
        }

        "list_wiki_pages" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let wiki_repo = WikiRepository::new(pool.clone());
            let pages = wiki_repo
                .list_pages(project_id)
                .await
                .map_err(|e| e.to_string())?;

            if pages.is_empty() {
                return Ok(serde_json::json!({
                    "content": [{ "type": "text", "text": "No wiki pages yet." }],
                    "isError": false
                }));
            }

            let lines: Vec<String> = pages
                .iter()
                .map(|p| format!("- {} ({}) | {}", p.title, p.path, p.page_type))
                .collect();

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Wiki pages ({} total):\n{}", pages.len(), lines.join("\n"))
                }],
                "isError": false
            }))
        }

        "get_wiki_page" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let path = args
                .get("path")
                .and_then(|s| s.as_str())
                .ok_or("Missing path")?;

            let content = wiki_project::read_page(project_id, path)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": content
                }],
                "isError": false
            }))
        }

        "save_wiki_page" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let path = args
                .get("path")
                .and_then(|s| s.as_str())
                .ok_or("Missing path")?;
            let content = args
                .get("content")
                .and_then(|s| s.as_str())
                .ok_or("Missing content")?;

            // Call the wiki update_page handler logic
            let wiki_repo = WikiRepository::new(pool.clone());
            let result =
                wiki_handlers::update_page_inner(pool, &wiki_repo, project_id, path, content).await;

            match result {
                Ok(()) => Ok(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Wiki page '{}' saved successfully.", path)
                    }],
                    "isError": false
                })),
                Err(e) => Err(e),
            }
        }

        "search_wiki" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let query = args
                .get("query")
                .and_then(|s| s.as_str())
                .ok_or("Missing query")?;
            let top_k = args.get("top_k").and_then(|t| t.as_u64()).unwrap_or(10) as usize;

            let wiki_repo = WikiRepository::new(pool.clone());
            let results = wiki_repo
                .search_pages(project_id, query, top_k)
                .await
                .map_err(|e| e.to_string())?;

            if results.is_empty() {
                return Ok(serde_json::json!({
                    "content": [{ "type": "text", "text": "No matching wiki pages found." }],
                    "isError": false
                }));
            }

            let lines: Vec<String> = results
                .iter()
                .map(|r| {
                    let mut line = format!("- {} ({})", r.title, r.path);
                    if !r.snippet.is_empty() {
                        line.push_str(&format!("\n  {}", r.snippet));
                    }
                    line
                })
                .collect();

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Wiki search results ({} found):\n{}", results.len(), lines.join("\n"))
                }],
                "isError": false
            }))
        }

        "ask_wiki" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let question = args
                .get("question")
                .and_then(|s| s.as_str())
                .ok_or("Missing question")?;
            let top_k = args.get("top_k").and_then(|t| t.as_u64()).unwrap_or(5) as usize;
            let model = args.get("model").and_then(|m| m.as_str());

            let result = wiki_handlers::ask_inner(shared, project_id, question, top_k, model).await;

            match result {
                Ok(json) => Ok(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&json).unwrap_or_default() }],
                    "isError": false
                })),
                Err(e) => Err(e),
            }
        }

        "get_wiki_tags" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(15) as usize;

            let wiki_repo = WikiRepository::new(pool.clone());
            let tags = wiki_repo
                .get_tags(project_id, limit)
                .await
                .map_err(|e| e.to_string())?;

            if tags.is_empty() {
                return Ok(serde_json::json!({
                    "content": [{ "type": "text", "text": "No tags found. Tags are auto-extracted from page frontmatter." }],
                    "isError": false
                }));
            }

            let lines: Vec<String> = tags
                .iter()
                .map(|t| format!("- {} ({})", t.word, t.count))
                .collect();

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Wiki tags:\n{}", lines.join("\n"))
                }],
                "isError": false
            }))
        }

        "get_wiki_graph" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;

            let wiki_repo = WikiRepository::new(pool.clone());
            let graph = wiki_repo
                .get_graph(project_id)
                .await
                .map_err(|e| e.to_string())?;

            let lines: Vec<String> = graph
                .nodes
                .iter()
                .map(|n| {
                    format!(
                        "- {} ({}){}",
                        n.label,
                        n.node_type,
                        n.path
                            .as_deref()
                            .map(|p| format!(" [{}]", p))
                            .unwrap_or_default()
                    )
                })
                .collect();

            let edge_lines: Vec<String> = graph
                .edges
                .iter()
                .map(|e| format!("  {} --{}--> {}", e.source, e.edge_type, e.target))
                .collect();

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Graph: {} nodes, {} edges\n\nNodes:\n{}\n\nEdges:\n{}",
                        graph.nodes.len(), graph.edges.len(),
                        lines.join("\n"), edge_lines.join("\n"))
                }],
                "isError": false
            }))
        }

        "list_wiki_sources" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;

            let wiki_repo = WikiRepository::new(pool.clone());
            let sources = wiki_repo
                .list_sources(project_id)
                .await
                .map_err(|e| e.to_string())?;

            if sources.is_empty() {
                return Ok(serde_json::json!({
                    "content": [{ "type": "text", "text": "No wiki sources yet." }],
                    "isError": false
                }));
            }

            let lines: Vec<String> = sources
                .iter()
                .map(|s| {
                    format!(
                        "- {} | ID: {} | Type: {} | Status: {} | Pages: {}",
                        s.filename, s.id, s.source_type, s.status, s.page_count
                    )
                })
                .collect();

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Wiki sources ({} total):\n{}", sources.len(), lines.join("\n"))
                }],
                "isError": false
            }))
        }

        "ingest_wiki_source" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let source_id = args
                .get("source_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing source_id")?;

            let result = wiki_ingest::ingest_source(
                &shared.state.events,
                &shared.state.settings,
                pool,
                project_id,
                source_id,
            )
            .await
            .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Ingest complete. Pages created: {}",
                        result.pages_created)
                }],
                "isError": false
            }))
        }

        // ── Wiki tools: Project lifecycle ───────────────────────────
        "create_wiki_project" => {
            let name = args
                .get("name")
                .and_then(|s| s.as_str())
                .ok_or("Missing name")?;

            let input = crate::services::wiki::models::CreateProjectInput {
                name: name.to_string(),
                description: args
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string()),
                ingest_model: args
                    .get("ingest_model")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string()),
                chat_model: args
                    .get("chat_model")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string()),
                ingest_channel_id: args
                    .get("ingest_channel_id")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string()),
                chat_channel_id: args
                    .get("chat_channel_id")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string()),
                schema_text: args
                    .get("schema_text")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string()),
            };

            let project_id = wiki_project::new_uuid();
            let schema = input
                .schema_text
                .clone()
                .unwrap_or_else(|| crate::services::wiki::repository::DEFAULT_SCHEMA.to_string());

            // Create directory structure
            wiki_project::init_project_dir(&project_id, &schema)
                .await
                .map_err(|e| e.to_string())?;

            let dir = wiki_project::project_wiki_dir(&project_id);
            let wiki_dir = dir.to_string_lossy().to_string();

            let wiki_repo = WikiRepository::new(pool.clone());
            let project = wiki_repo
                .create_project_with_id(&project_id, &input, &wiki_dir)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Wiki project created successfully.\nID: {}\nName: {}\nDescription: {}\nIngest model: {}\nChat model: {}",
                        project.id,
                        project.name,
                        project.description.as_deref().unwrap_or("N/A"),
                        project.ingest_model.as_deref().unwrap_or("default"),
                        project.chat_model.as_deref().unwrap_or("default")
                    )
                }],
                "isError": false
            }))
        }

        "delete_wiki_project" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;

            let wiki_repo = WikiRepository::new(pool.clone());
            let project = wiki_repo
                .get_project(project_id)
                .await
                .map_err(|e| e.to_string())?;

            wiki_repo
                .delete_project(project_id)
                .await
                .map_err(|e| e.to_string())?;

            // Remove directory
            if let Err(e) = wiki_project::remove_project_dir(project_id).await {
                tracing::warn!("Failed to remove project dir: {}", e);
            }

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Wiki project '{}' ({}) has been permanently deleted.", project.name, project_id)
                }],
                "isError": false
            }))
        }

        // ── Wiki tools: Page deletion ───────────────────────────────
        "delete_wiki_page" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let path = args
                .get("path")
                .and_then(|s| s.as_str())
                .ok_or("Missing path")?;

            let wiki_repo = WikiRepository::new(pool.clone());
            wiki_repo
                .delete_page(project_id, path)
                .await
                .map_err(|e| e.to_string())?;

            // Delete file from disk
            let _ = wiki_project::delete_page_file(project_id, path).await;

            // Rebuild graph edges after page deletion
            let _ = crate::services::wiki::ingest::rebuild_graph_edges(&pool, project_id).await;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Wiki page '{}' has been deleted.", path)
                }],
                "isError": false
            }))
        }

        // ── Wiki tools: Source management ──────────────────────────
        "add_wiki_source" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let filename = args
                .get("filename")
                .and_then(|s| s.as_str())
                .ok_or("Missing filename")?;
            let source_type = args
                .get("source_type")
                .and_then(|s| s.as_str())
                .ok_or("Missing source_type")?;

            let content = args.get("content").and_then(|c| c.as_str());
            let file_path = args
                .get("file_path")
                .and_then(|f| f.as_str())
                .map(|s| s.to_string());
            let source_url = args
                .get("source_url")
                .and_then(|u| u.as_str())
                .map(|s| s.to_string());

            // Compute hash and size if content provided
            let (content_hash, file_size) = if let Some(ref content) = content {
                use sha2::Digest;
                let mut hasher = sha2::Sha256::new();
                hasher.update(content.as_bytes());
                let hash = format!("{:x}", hasher.finalize());
                (Some(hash), content.len() as i64)
            } else {
                (None, 0i64)
            };

            // Write content to disk if provided
            if let Some(ref content) = content {
                wiki_project::write_source_file(project_id, filename, content.as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
            }

            let input = crate::services::wiki::models::AddSourceInput {
                source_type: source_type.to_string(),
                filename: filename.to_string(),
                file_path,
                source_url,
                content: content.map(|s| s.to_string()),
            };

            let wiki_repo = WikiRepository::new(pool.clone());
            let source = wiki_repo
                .add_source(project_id, &input, content_hash.as_deref(), file_size)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Wiki source added successfully.\nID: {}\nFilename: {}\nType: {}\nStatus: pending\n\nUse ingest_wiki_source to generate structured pages from this source.",
                        source.id, source.filename, source.source_type
                    )
                }],
                "isError": false
            }))
        }

        "delete_wiki_source" => {
            let project_id = args
                .get("project_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing project_id")?;
            let source_id = args
                .get("source_id")
                .and_then(|s| s.as_str())
                .ok_or("Missing source_id")?;

            let wiki_repo = WikiRepository::new(pool.clone());

            // Get source info before deletion for the response message
            let sources = wiki_repo
                .list_sources(project_id)
                .await
                .map_err(|e| e.to_string())?;
            let source = sources.iter().find(|s| s.id == source_id).ok_or_else(|| {
                format!("Source {} not found in project {}", source_id, project_id)
            })?;

            let filename = source.filename.clone();

            wiki_repo
                .delete_source(source_id)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Wiki source '{}' ({}) has been deleted.", filename, source_id)
                }],
                "isError": false
            }))
        }

        _ => Err(format!("Unknown tool: {}", tool_name)),
    }
}
