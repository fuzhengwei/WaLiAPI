import { invoke } from "./runtime";
import type {
  Channel, CreateChannelInput, UpdateChannelInput, TestChannelResult,
  ChannelKey,
  ApiKey, CreateApiKeyInput, ApiKeyStats,
  RequestLog, LogStats, SecurityFinding,
  DashboardStats,
  ModelStats,
  TokenTrendPoint,
  Settings,
  ServerStatus,
  BuiltinRule, CustomRule, CreateCustomRuleInput, UpdateBuiltinRuleInput,
  ChannelProtocolPresetGroup,
  DraftChannelTestInput, DraftChannelTestResult,
  UpstreamModelsResult,
  AuthAccount, AuthLoginSessionStatus, AuthLoginStart, AuthMutationResult, AuthLogoutResult, AuthExportResult,
  AuthQuotaStatus, AuthUpdateInput,
  AuthProviderInfo, AuthLoginMethod,
  PromptTemplate,
} from "../types";

/**
 * 保存前草稿连通性测试（T07）。后端 `test_channel_draft` 命令已接入。
 *
 * 不落库：不创建/更新渠道、不计数配额、不写生产 request log；仅执行每个已选
 * 端点的最小非流推理探测（可能产生极少上游费用）并返回逐端点结果 + 草稿指纹。
 */
export async function testChannelDraft(input: DraftChannelTestInput): Promise<DraftChannelTestResult> {
  return invoke<DraftChannelTestResult>("test_channel_draft", { input });
}

// Channel stats
export interface ChannelStats {
  channel_id: string;
  total_calls: number;
  success_calls: number;
  failed_calls: number;
  total_tokens: number;
  prompt_tokens: number;
  completion_tokens: number;
  avg_latency_ms: number;
  last_call_at: string | null;
}

// Channel commands
export const channelApi = {
  getAll: () => invoke<Channel[]>("get_channels"),
  get: (id: string) => invoke<Channel>("get_channel", { id }),
  getApiKey: (id: string) => invoke<string>("get_channel_api_key", { id }),
  create: (input: CreateChannelInput) => invoke<Channel>("create_channel", { input }),
  update: (input: UpdateChannelInput) => invoke<Channel>("update_channel", { input }),
  toggle: (id: string, status: number) => invoke<void>("toggle_channel", { id, status }),
  delete: (id: string) => invoke<void>("delete_channel", { id }),
  test: (id: string) => invoke<TestChannelResult>("test_channel", { id }),
  getStats: () => invoke<ChannelStats[]>("get_channel_stats"),
  reorder: (orderedIds: string[]) => invoke<void>("reorder_channels", { orderedIds }),
  /** 获取全部协议及其提供商模板（只读；`presets[0]` 恒为 custom option）。 */
  getPresets: () => invoke<ChannelProtocolPresetGroup[]>("get_channel_presets"),
  /** 保存前草稿连通性测试（T07，真实后端命令，不落库）。 */
  testDraft: (input: DraftChannelTestInput) => testChannelDraft(input),
  /** 拉取上游模型列表（T14）。绝不写库：不覆盖已有模型列表，返回结果供弹窗勾选合并。 */
  syncUpstreamModels: (input: DraftChannelTestInput) =>
    invoke<UpstreamModelsResult>("sync_upstream_models", { input }),
  /** 获取渠道的额外 API Keys（masked）。 */
  getExtraKeys: (id: string) => invoke<ChannelKey[]>("get_channel_extra_keys", { id }),
  /** 获取单个额外 Key 的完整值（unmasked）。 */
  getExtraKeyValue: (keyId: string) => invoke<string>("get_channel_extra_key_value", { keyId }),
  /** 启用/禁用一个额外 Key。 */
  toggleExtraKey: (keyId: string, status: number) => invoke<void>("toggle_channel_extra_key", { keyId, status }),
  /** 删除一个额外 Key。 */
  deleteExtraKey: (keyId: string) => invoke<void>("delete_channel_extra_key", { keyId }),
};

// API Key commands
export const apiKeyApi = {
  getKnowledgeAccess: (id: string) => invoke<string[]>("get_api_key_knowledge_access", { id }),
  setKnowledgeAccess: (id: string, kbIds: string[]) => invoke<void>("set_api_key_knowledge_access", { id, kbIds }),
  testKnowledgeAccess: (id: string, kbId: string) => invoke<{ rest_status: number; rest_ok: boolean; mcp_status: number; mcp_ok: boolean }>("test_api_key_knowledge_access", { id, kbId }),
  getAll: () => invoke<ApiKey[]>("get_api_keys"),
  // FIX-13：列表只回掩码，复制/示例代码等显式动作经此按需取全量。
  getFull: (id: string) => invoke<string>("get_api_key_full", { id }),
  create: (input: CreateApiKeyInput) => invoke<ApiKey>("create_api_key", { input }),
  update: (input: { id: string; name?: string; quota_limit?: number; status?: number; allowed_models?: string[]; allowed_channels?: string[]; denied_models?: string[]; denied_channels?: string[] }) => invoke<void>("update_api_key", { input }),
  delete: (id: string) => invoke<void>("delete_api_key", { id }),
  getStats: () => invoke<ApiKeyStats[]>("get_api_key_stats"),
};

export interface GetLogsInput {
  limit?: number;
  offset?: number;
  keyword?: string;
  api_key_name?: string;
  channel_name?: string;
  model?: string;
  date_from?: string;
  date_to?: string;
  trace_id?: string;
  upstream_type?: "channel" | "auth_account";
}

// Log commands
export const logApi = {
  /** 列表接口只返回摘要；正文由 get 按日志展开时懒加载。 */
  getAll: (input?: GetLogsInput) => invoke<RequestLog[]>("get_logs", { input: input || {} }),
  /** 与 getAll 相同过滤条件下的日志总数，用于分页页码展示。 */
  count: (input?: GetLogsInput) => invoke<number>("count_logs", { input: input || {} }),
  get: (id: string) => invoke<RequestLog>("get_log", { id }),
  getSecurityFindings: (logId: string) => invoke<SecurityFinding[]>("get_log_security_findings", { logId }),
  /** 流式请求的已生成内容段（detailed 策略下落库；按 seq 升序拼接即完整内容）。 */
  getStreamSegments: (logId: string) => invoke<Array<{ seq: number; content: string }>>("get_log_stream_segments", { logId }),
  getStats: (days?: number) => invoke<LogStats[]>("get_log_stats", { days }),
  delete: (id: string) => invoke<void>("delete_log", { id }),
  deleteBefore: (beforeDate: string) => invoke<number>("delete_logs_before", { beforeDate }),
  deleteAll: () => invoke<number>("delete_all_logs"),
};

// Auth account commands. All result contracts are safe summaries; credential
// payloads remain inside the native command layer.
export const authApi = {
  accountsList: () => invoke<AuthAccount[]>("auth_accounts_list"),
  providersList: () => invoke<AuthProviderInfo[]>("auth_providers_list"),
  /**
   * @deprecated Synchronous login for Codex compatibility only.  Kimi (and any
   * DeviceCode provider) must use `loginStart`; the backend refuses `login`
   * for them before any network request.
   */
  login: (provider: string) => invoke<AuthMutationResult>("auth_login", { provider }),
  loginStart: (provider: string, replaceAccountId?: string, loginMethod?: AuthLoginMethod) =>
    invoke<AuthLoginStart>("auth_login_start", {
      provider,
      replaceAccountId: replaceAccountId ?? null,
      loginMethod: loginMethod ?? null,
    }),
  loginStatus: (sessionId: string) => invoke<AuthLoginSessionStatus>("auth_login_status", { sessionId }),
  loginCancel: (sessionId: string) => invoke<AuthLoginSessionStatus>("auth_login_cancel", { sessionId }),
  /** format: "codex"（默认）| "sub2api" | "cpa"，见 AuthFileFormat。 */
  loginImport: (provider?: string, path?: string, format?: string) =>
    invoke<AuthMutationResult>("auth_login_import", { provider, path, format }),
  /** Web 版：直接上传 auth 文件内容导入（无服务器文件路径）。 */
  loginImportContent: (provider: string, content: string, format?: string) =>
    invoke<AuthMutationResult>("auth_login_import_content", { provider, content, format }),
  defaultImportPath: () => invoke<string>("auth_default_import_path"),
  logout: (id: string) => invoke<AuthLogoutResult>("auth_logout", { id }),
  refreshToken: (id: string) => invoke<AuthAccount>("auth_refresh_token", { id }),
  refreshQuota: (id: string) => invoke<AuthAccount>("auth_refresh_quota", { id }),
  syncModels: (id: string) => invoke<AuthAccount>("auth_sync_models", { id }),
  exportJson: (id: string, path: string) => invoke<AuthExportResult>("auth_export_json", { id, path }),
  /** Web 版：导出 auth.json 内容，由浏览器触发下载。 */
  exportJsonContent: (id: string) => invoke<string>("auth_export_json_content", { id }),
  toggle: (id: string, disabled: boolean) => invoke<AuthAccount>("auth_toggle", { id, disabled }),
  quotaStatus: (id: string) => invoke<AuthQuotaStatus>("auth_quota_status", { id }),
  update: (input: AuthUpdateInput) => invoke<AuthAccount>("auth_update", { input }),
  /** 手动拖拽排序：按传入顺序重写 sort_order。 */
  reorder: (orderedIds: string[]) => invoke<void>("auth_reorder_accounts", { orderedIds }),
};

// Stats commands
export const statsApi = {
  getDashboard: () => invoke<DashboardStats>("get_dashboard_stats"),
  getModelStats: () => invoke<ModelStats[]>("get_model_stats"),
  getTokenTrend: (hours?: number) => invoke<TokenTrendPoint[]>("get_token_trend", { hours: hours ?? 24 }),
};

// Settings commands
export interface FeatureFlagsDto {
  new_routeplan: boolean;
  cross_protocol_codec: boolean;
  native_responses: boolean;
  ollama_native: boolean;
  prefer_auth_accounts: boolean;
  prefer_same_protocol: boolean;
}

export const settingsApi = {
  get: () => invoke<Settings>("get_settings"),
  save: (settings: Settings) => invoke<void>("save_settings", { settings }),
  applyTheme: (theme: string) => invoke<void>("apply_theme", { theme }),
  setAutoStart: (enabled: boolean) => invoke<void>("set_auto_start", { enabled }),
  getFeatureFlags: () => invoke<FeatureFlagsDto>("get_feature_flags"),
};

// OCR 缓存管理（LLM OCR 页级结果缓存）
export interface OcrCacheInfo {
  total_bytes: number;
  doc_count: number;
}

export const ocrApi = {
  getCacheInfo: () => invoke<OcrCacheInfo>("get_ocr_cache_info"),
  clearCache: () => invoke<void>("clear_ocr_cache"),
};

// Server commands
export const serverApi = {
  getStatus: () => invoke<ServerStatus>("get_server_status"),
  restart: () => invoke<void>("restart_server"),
};

// Import / Export
export interface ImportResult {
  imported: number;
  skipped: number;
  errors: string[];
}

export interface ScannedSource {
  source: string;
  name: string;
  base_url: string;
  api_key: string;
  models: string[];
  api_format: string;
  raw: Record<string, unknown>;
}

export interface ScanResult {
  sources: ScannedSource[];
}

export const importExportApi = {
  exportChannels: () => invoke<string>("export_channels"),
  importWalicodeBackup: (content: string) => invoke<ImportResult>("import_walicode_backup", { content }),
  importWaliapiExport: (content: string) => invoke<ImportResult>("import_waliapi_export", { content }),
  scanLocalAiConfigs: () => invoke<ScanResult>("scan_local_ai_configs"),
  importScannedSources: (sources: ScannedSource[]) => invoke<ImportResult>("import_scanned_sources", { sources }),
  pickImportFile: () => invoke<string | null>("pick_import_file"),
  saveExportFile: (content: string, defaultName: string) => invoke<boolean>("save_export_file", { content, defaultName }),
};

// Security rules
export const securityApi = {
  getBuiltinRules: () => invoke<BuiltinRule[]>("get_builtin_security_rules"),
  updateBuiltinRule: (id: string, input: UpdateBuiltinRuleInput) => invoke<void>("update_builtin_security_rule", { id, input }),
  deleteBuiltinRule: (id: string) => invoke<void>("delete_builtin_security_rule", { id }),
  resetBuiltinRules: () => invoke<BuiltinRule[]>("reset_builtin_security_rules"),
  getCustomRules: () => invoke<CustomRule[]>("get_custom_security_rules"),
  createCustomRule: (input: CreateCustomRuleInput) => invoke<CustomRule>("create_custom_security_rule", { input }),
  toggleCustomRule: (id: string, enabled: boolean) => invoke<void>("toggle_custom_security_rule", { id, enabled }),
  deleteCustomRule: (id: string) => invoke<void>("delete_custom_security_rule", { id }),
};

// Knowledge Base types
export interface KnowledgeBase {
    id: string;
    name: string;
    description: string | null;
    status: number;
    doc_count: number;
    chunk_count: number;
    total_tokens: number;
    embedding_model: string | null;
    embedding_channel_id: string | null;
    ocr_model: string | null;
    mcp_enabled: number;
    chunk_size: number;
    chunk_overlap: number;
    excluded_dirs: string;
    excluded_files: string;
    included_files: string;
    embedding_dim: number;
    index_status: string;
    embedding_batch_size: number;
    created_at: string;
    updated_at: string;
}

export interface KbDocument {
  id: string;
  kb_id: string;
  filename: string;
  file_path: string | null;
  file_type: string;
  file_size: number;
  content_hash: string;
  chunk_count: number;
  token_count: number;
  status: string;
  error_message: string | null;
  source_type: string;
  source_url: string | null;
  source_path: string | null;
  doc_meta: string;
  ocr_engine: string | null;
  page_count: number;
  /** JSON 数组字符串，如 "[3,7]"，前端自行 JSON.parse */
  ocr_failed_pages: string;
  created_at: string;
  updated_at: string;
}

export interface KbConversation {
  id: string;
  kb_id: string;
  role: string;
  content: string;
  sources: string | null;
  model: string | null;
  tokens_used: number;
  created_at: string;
}

export interface KbSource {
  id: string;
  kb_id: string;
  source_type: string;
  source_url: string | null;
  source_path: string | null;
  branch: string | null;
  status: string;
  file_count: number;
  error: string | null;
  created_at: string;
  updated_at: string;
}

export interface KbIndexMeta {
  kb_id: string;
  index_type: string;
  embedding_dim: number;
  chunk_count: number;
  index_path: string | null;
  built_at: string | null;
  status: string;
}

export interface ConversationMessage {
  role: string;
  content: string;
}

export interface KbSearchResult {
  chunk_id: string;
  doc_id: string;
  filename: string;
  content: string;
  score: number;
  metadata: Record<string, unknown>;
}

export interface KbRetrievalDetail {
  chunk_id: string;
  filename: string;
  score: number;
  vector_score: number | null;
  keyword_score: number | null;
  snippet: string;
  symbol_name: string | null;
  symbol_kind: string | null;
}

export interface KbRagAnswer {
  answer: string;
  sources: Array<{
    filename: string;
    score: number;
    snippet: string;
  }>;
  usage: { prompt_tokens: number; completion_tokens: number; total_tokens: number } | null;
  retrieval_details: KbRetrievalDetail[] | null;
}

export interface KbTag {
  word: string;
  count: number;
}

// Knowledge Base commands
export const kbApi = {
  getAll: () => invoke<KnowledgeBase[]>("get_knowledge_bases"),
  create: (input: { name: string; description?: string; embedding_model?: string; ocr_model?: string | null }) =>
    invoke<KnowledgeBase>("create_knowledge_base", { input }),
  update: (id: string, input: Partial<{ name: string; description: string; embedding_model: string; embedding_channel_id: string; ocr_model: string | null; status: number; mcp_enabled: number; chunk_size: number; chunk_overlap: number; excluded_dirs: string; excluded_files: string; included_files: string; embedding_batch_size: number }>) =>
        invoke<KnowledgeBase>("update_knowledge_base", { id, input }),
  delete: (id: string) => invoke<void>("delete_knowledge_base", { id }),
  getDocuments: (kbId: string) => invoke<KbDocument[]>("get_kb_documents", { kbId }),
  uploadDocument: (input: { kb_id: string; filename: string; content: string }) =>
    invoke<KbDocument>("upload_kb_document", { input }),
  deleteDocument: (docId: string, kbId: string) =>
    invoke<void>("delete_kb_document", { docId, kbId }),
  reindexDocument: (docId: string) =>
    invoke<void>("reindex_kb_document", { docId }),
  search: (input: { query: string; kb_id?: string; top_k?: number; vector_weight?: number; keyword_weight?: number; search_mode?: string }) =>
    invoke<KbSearchResult[]>("search_knowledge_base", { input }),
  ask: (input: { question: string; kb_id?: string; top_k?: number; model?: string; history?: ConversationMessage[]; deep_research?: boolean; max_rounds?: number; vector_weight?: number; keyword_weight?: number; search_mode?: string }) =>
    invoke<KbRagAnswer>("ask_knowledge_base", { input }),
  getStats: (kbId: string) => invoke<Record<string, unknown>>("get_kb_stats", { kbId }),
  // Conversation history
  getConversations: (kbId: string) => invoke<KbConversation[]>("get_kb_conversations", { kbId }),
  clearConversations: (kbId: string) => invoke<void>("clear_kb_conversations", { kbId }),
  // Sources (multi-source import)
  getSources: (kbId: string) => invoke<KbSource[]>("get_kb_sources", { kbId }),
  deleteSource: (sourceId: string, kbId: string) => invoke<void>("delete_kb_source", { sourceId, kbId }),
  importSource: (kbId: string, input: { source_type: string; repo_url?: string; branch?: string; token?: string; url?: string; dir_path?: string; excluded_dirs?: string[]; included_files?: string[]; max_file_size?: number }) =>
    invoke<KbSource>("import_kb_source", { kbId, input }),
  // Index management
  getIndexStatus: (kbId: string) => invoke<KbIndexMeta | null>("get_kb_index_status", { kbId }),
  buildIndex: (kbId: string) => invoke<void>("build_kb_index", { kbId }),
  dropIndex: (kbId: string) => invoke<void>("drop_kb_index", { kbId }),
  getTags: (kbId: string, limit?: number) => invoke<KbTag[]>("get_kb_tags", { kbId, limit }),
};

// ── Wiki types ──
export interface WikiProject {
    id: string;
    name: string;
    description: string | null;
    status: number;
    schema_text: string | null;
    wiki_dir: string;
    ingest_model: string | null;
    chat_model: string | null;
    ingest_channel_id: string | null;
    chat_channel_id: string | null;
    mcp_enabled: number;
    source_count: number;
    page_count: number;
    last_ingest_at: string | null;
    last_lint_at: string | null;
    created_at: string;
    updated_at: string;
}

export interface CreateWikiProjectInput {
    name: string;
    description?: string;
    ingest_model?: string;
    chat_model?: string;
    ingest_channel_id?: string;
    chat_channel_id?: string;
    schema_text?: string;
}

export interface UpdateWikiProjectInput {
    name?: string;
    description?: string;
    status?: number;
    schema_text?: string;
    ingest_model?: string;
    chat_model?: string;
    ingest_channel_id?: string;
    chat_channel_id?: string;
    mcp_enabled?: number;
}

export interface WikiPage {
    id: string;
    project_id: string;
    path: string;
    title: string;
    page_type: string;
    content_hash: string;
    token_count: number;
    wikilinks: string;
    frontmatter: string;
    tags: string;
    status: string;
    content?: string;
    created_at: string;
    updated_at: string;
}

export interface WikiSource {
    id: string;
    project_id: string;
    source_type: string;
    filename: string;
    file_path: string | null;
    source_url: string | null;
    content_hash: string | null;
    file_size: number;
    status: string;
    page_count: number;
    error_message: string | null;
    created_at: string;
    ingested_at: string | null;
}

export interface WikiSearchResult {
    page_id: string;
    path: string;
    title: string;
    score: number;
    snippet: string;
    page_type: string;
}

export interface WikiGraphData {
    nodes: Array<{
        id: string;
        label: string;
        path: string | null;
        node_type: string;
        link_count: number;
    }>;
    edges: Array<{
        source: string;
        target: string;
        edge_type: string;
        weight: number;
    }>;
}

export interface WikiTag {
  word: string;
  count: number;
}

export interface AddWikiSourceInput {
    source_type: string;
    filename: string;
    file_path?: string;
    source_url?: string;
    content?: string;
}

// Wiki commands
export const wikiApi = {
    getProjects: () => invoke<WikiProject[]>("get_wiki_projects"),
    createProject: (input: CreateWikiProjectInput) => invoke<WikiProject>("create_wiki_project", { input }),
    getProject: (id: string) => invoke<WikiProject>("get_wiki_project", { id }),
    updateProject: (id: string, input: UpdateWikiProjectInput) => invoke<WikiProject>("update_wiki_project", { id, input }),
    deleteProject: (id: string) => invoke<void>("delete_wiki_project", { id }),
    getPages: (projectId: string) => invoke<WikiPage[]>("get_wiki_pages", { projectId }),
    getPage: (projectId: string, path: string) => invoke<WikiPage & { content: string }>("get_wiki_page", { projectId, path }),
    savePage: (projectId: string, path: string, content: string) => invoke<void>("save_wiki_page", { projectId, path, content }),
    getSources: (projectId: string) => invoke<WikiSource[]>("get_wiki_sources", { projectId }),
    addSource: (projectId: string, input: AddWikiSourceInput) => invoke<WikiSource>("add_wiki_source", { projectId, input }),
    deleteSource: (sourceId: string) => invoke<void>("delete_wiki_source", { sourceId }),
    search: (projectId: string, query: string, topK?: number) => invoke<WikiSearchResult[]>("search_wiki", { projectId, query, topK }),
    getGraph: (projectId: string) => invoke<WikiGraphData>("get_wiki_graph", { projectId }),
    getStats: (projectId: string) => invoke<Record<string, unknown>>("get_wiki_stats", { projectId }),
    ingestSource: (projectId: string, sourceId: string) => invoke<{ status: string; pages_created: number; page_paths: string[] }>("ingest_wiki_source", { projectId, sourceId }),
    rescanSources: (projectId: string) => invoke<{ status: string; processed: number; results: unknown[] }>("rescan_wiki_sources", { projectId }),
    getTags: (projectId: string, limit?: number) => invoke<WikiTag[]>("get_wiki_tags", { projectId, limit }),
};

// Service status
export interface ServiceStatus {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  running: boolean;
  stats: Record<string, unknown>;
}

export const serviceApi = {
  getStatuses: () => invoke<ServiceStatus[]>("get_service_statuses"),
};

// ── App Config (应用配置) ──
export interface AppInfo {
  name: string;
  label: string;
  icon: string;
  description: string;
  config_path: string;
  config_format: string;
  available: boolean;
  applied: boolean;
  download_url: string;
}

export interface ApplyResult {
  success: boolean;
  message: string;
  /** Codex 专用：切回后检测到 auth.json 处于 API Key 模式 */
  authWarning?: string | null;
}

export interface ConfigContent {
  exists: boolean;
  content: string;
  error: string | null;
}

export const appConfigApi = {
  getApps: () => invoke<AppInfo[]>("get_app_configs"),
  apply: (appName: string, apiKey: string, model: string) => invoke<ApplyResult>("apply_app_config", { appName, apiKey, model }),
  clear: (appName: string) => invoke<ApplyResult>("clear_app_config", { appName }),
  resetCodexAuth: () => invoke<ApplyResult>("reset_codex_auth"),
  getContent: (appName: string) => invoke<ConfigContent>("get_app_config_content", { appName }),
  openFolder: (appName: string) => invoke<void>("open_config_folder", { appName }),
};

// Prompt template commands（C-07 模板版本化）
export const promptTemplateApi = {
  list: () => invoke<PromptTemplate[]>("list_prompt_templates"),
  create: (templateKey: string, content: string) => invoke<number>("create_prompt_template", { templateKey, content }),
  activate: (templateKey: string, version: number) => invoke<void>("activate_prompt_template", { templateKey, version }),
};

// 语义缓存管理命令（C-02）
export const semanticCacheApi = {
  clear: (model?: string) => invoke<number>("clear_semantic_cache", { model: model ?? null }),
};
