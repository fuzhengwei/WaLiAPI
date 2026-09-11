use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KbKnowledgeBase {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: i64,
    pub doc_count: i64,
    pub chunk_count: i64,
    pub total_tokens: i64,
    pub embedding_model: Option<String>,
    pub embedding_channel_id: Option<String>,
    pub mcp_enabled: i64,
    pub chunk_size: i64,
    pub chunk_overlap: i64,
    pub excluded_dirs: String,
    pub excluded_files: String,
    pub included_files: String,
    pub embedding_dim: i64,
    pub embedding_revision: i64,
    pub index_status: String,
    pub embedding_batch_size: i64,
    /// 知识库级 OCR 视觉模型（如 qwen-vl-max）；None/空 = 不启用。
    /// 仅在全局设置 ocr.enabled 开启后对扫描版 PDF 生效。
    pub ocr_model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKbInput {
    pub name: String,
    pub description: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_channel_id: Option<String>,
    pub ocr_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateKbInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_channel_id: Option<String>,
    pub status: Option<i64>,
    pub mcp_enabled: Option<i64>,
    pub chunk_size: Option<i64>,
    pub chunk_overlap: Option<i64>,
    pub excluded_dirs: Option<String>,
    pub excluded_files: Option<String>,
    pub included_files: Option<String>,
    pub embedding_batch_size: Option<i64>,
    pub ocr_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KbDocument {
    pub id: String,
    pub kb_id: String,
    pub filename: String,
    pub file_path: Option<String>,
    pub file_type: String,
    pub file_size: i64,
    pub content_hash: String,
    pub chunk_count: i64,
    pub token_count: i64,
    pub status: String,
    pub error_message: Option<String>,
    pub source_type: String,
    pub source_url: Option<String>,
    pub source_path: Option<String>,
    pub doc_meta: String,
    /// OCR 识别引擎（'vlm' / NULL = 未经过 OCR）
    pub ocr_engine: Option<String>,
    /// PDF 页数（仅 OCR 文档回填）
    pub page_count: i64,
    /// OCR 失败页码的 JSON 数组（如 "[3,7]"）
    pub ocr_failed_pages: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadDocInput {
    pub filename: String,
    pub file_path: Option<String>,
    pub content: String, // base64 encoded
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KbChunk {
    pub id: String,
    pub doc_id: String,
    pub kb_id: String,
    pub chunk_index: i64,
    pub content: String,
    pub token_count: i64,
    pub metadata: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk_id: String,
    pub doc_id: String,
    pub filename: String,
    pub content: String,
    pub score: f32,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagAnswer {
    pub answer: String,
    pub sources: Vec<SourceInfo>,
    pub usage: Option<UsageInfo>,
    #[serde(default)]
    pub retrieval_details: Option<Vec<RetrievalDetail>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalDetail {
    pub chunk_id: String,
    pub filename: String,
    pub score: f32,
    pub vector_score: Option<f32>,
    pub keyword_score: Option<f32>,
    pub snippet: String,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub filename: String,
    pub score: f32,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageInfo {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KbTask {
    pub id: String,
    pub kb_id: String,
    pub doc_id: Option<String>,
    pub task_type: String,
    pub status: String,
    pub progress: i64,
    pub total_items: i64,
    pub done_items: i64,
    pub error_message: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

// ════════════════════════════════════════════════════════
// New models for v2 upgrade
// ════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KbConversation {
    pub id: String,
    pub kb_id: String,
    pub role: String,
    pub content: String,
    pub sources: Option<String>,
    pub model: Option<String>,
    pub tokens_used: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskInput {
    pub question: String,
    pub kb_id: Option<String>,
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    #[serde(default = "default_chat_model")]
    pub model: String,
    pub history: Option<Vec<ConversationMessage>>,
    #[serde(default)]
    pub deep_research: bool,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: usize,
    #[serde(default)]
    pub vector_weight: Option<f32>,
    #[serde(default)]
    pub keyword_weight: Option<f32>,
    #[serde(default)]
    pub search_mode: Option<String>,
}

fn default_top_k() -> usize {
    5
}
fn default_chat_model() -> String {
    "gpt-4o".to_string()
}
fn default_max_rounds() -> usize {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KbSource {
    pub id: String,
    pub kb_id: String,
    pub source_type: String,
    pub source_url: Option<String>,
    pub source_path: Option<String>,
    pub branch: Option<String>,
    pub status: String,
    pub file_count: i64,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportSourceInput {
    pub source_type: String, // git | url | local_dir
    pub repo_url: Option<String>,
    pub branch: Option<String>,
    pub token: Option<String>,
    pub url: Option<String>,
    pub dir_path: Option<String>,
    pub excluded_dirs: Option<Vec<String>>,
    pub included_files: Option<Vec<String>>,
    pub max_file_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct KbIndexMeta {
    pub kb_id: String,
    pub index_type: String,
    pub embedding_dim: i64,
    pub chunk_count: i64,
    pub index_path: Option<String>,
    pub built_at: Option<String>,
    pub status: String,
}
