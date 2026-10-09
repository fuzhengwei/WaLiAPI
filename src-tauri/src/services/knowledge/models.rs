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
    /// PDF 实际页数；未重新导入的旧文档可能为 0。
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

/// 通用检索结果；旧请求仍只返回 data，新能力仅在显式请求时附加元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub data: Vec<SearchResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RagDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagAnswer {
    pub answer: String,
    pub sources: Vec<SourceInfo>,
    pub usage: Option<UsageInfo>,
    #[serde(default)]
    pub retrieval_details: Option<Vec<RetrievalDetail>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RagDiagnostics>,
    /// 显式选择检索策略或实际部分成功时返回检索模式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
    /// 只确认已请求思考档位，不表示上游接受或执行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<RagReasoning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagReasoning {
    pub requested: String,
    pub status: ReasoningStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningStatus {
    Requested,
    NotSent,
}

/// 仅显式诊断请求返回阶段结果，不包含提示词、渠道地址或凭据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagDiagnostics {
    pub request_id: String,
    pub stages: Vec<RagDiagnosticStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagDiagnosticStage {
    pub stage: String,
    pub status: String,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// stage / channel / request；仅失败的截止或取消阶段提供。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_scope: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_no: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet_start: Option<usize>,
    /// 显式候选请求提供最终上下文原文，供任意客户端按 Unicode 码点核对引用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_text: Option<String>,
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
    /// 启用严格健康检测：必须有检索片段、有效答案和来源。
    #[serde(default)]
    pub diagnostics: bool,
    /// 整次 RAG 共用时间预算，服务端限制为 100..=120000 毫秒；省略保持历史行为。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// 授权通过后的可恢复向量查询失败，允许使用关键词检索。
    #[serde(default)]
    pub allow_keyword_fallback: bool,
    /// hybrid 的可恢复关键词检索失败，允许使用已授权的向量结果；默认关闭。
    #[serde(default)]
    pub allow_vector_fallback: bool,
    /// 禁用历史本地分支隐式部分成功；仍允许显式授权的降级方向。
    #[serde(default)]
    pub strict_retrieval: bool,
    /// 最终截断前的候选池；省略时保持普通请求原有行为。
    #[serde(default)]
    pub candidate_k: Option<usize>,
    /// 省略或 default 不覆盖网关 / 模型默认；其他档位使用通用协议参数。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
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
