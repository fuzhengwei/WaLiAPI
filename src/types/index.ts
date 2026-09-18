// Channel types

/** A single API key entry for a channel (multi-key load balancing). */
export interface ChannelKey {
  id: string;
  api_key: string;
  weight: number;
  status: number;
  created_at: string;
  updated_at: string;
}

/** Input for creating/updating a channel API key. */
export interface ChannelKeyInput {
  api_key: string;
  weight?: number;
  status?: number;
}

/** 渠道级自定义上游请求头。status=1 启用，status=0 禁用。 */
export interface ChannelRequestHeader {
  id: string;
  name: string;
  value: string;
  status: number;
}

export interface ChannelRequestHeaderInput {
  name: string;
  value: string;
  status?: number;
}

export interface Channel {
  id: string;
  name: string;
  type: string;
  base_url: string;
  api_key: string;
  models: string[];
  status: number;
  priority: number;
  weight: number;
  config: Record<string, unknown>;
  model_mapping: Record<string, string | string[]>;
  timeout_secs: number;
  // --- T02 normalized protocol identity (output DTO always returns these) ---
  protocol: "openai" | "anthropic" | "ollama" | string;
  provider: string;
  native_base_url: string;
  native_endpoints: string[];
  identity_revision: number;
  preset_revision?: string | null;
  legacy_executor_override?: string | null;
  executor_kind: string;
  created_at: string;
  updated_at: string;
  last_test_at: string | null;
  last_test_ok: number | null;
  /** 主动健康探测（迁移 033）：探测列优先展示，无探测数据回退手动测试列。 */
  last_probe_at: string | null;
  last_probe_ok: number | null;
  probe_latency_ms: number | null;
  /** Multi-key: extra API keys (masked in DTO, use getChannelExtraKeys for full). */
  extra_keys: ChannelKey[];
  /** 渠道级自定义上游请求头。 */
  request_headers: ChannelRequestHeader[];
}

export interface CreateChannelInput {
  name: string;
  type: string;
  base_url: string;
  api_key: string;
  models: string[];
  priority?: number;
  weight?: number;
  config?: Record<string, unknown>;
  model_mapping?: Record<string, string | string[]>;
  timeout_secs?: number;
  // --- T02 optional new identity fields (missing => legacy inference) ---
  protocol?: "openai" | "anthropic" | "ollama" | string;
  provider?: string;
  native_base_url?: string;
  native_endpoints?: string[];
  preset_revision?: string;
  legacy_executor_override?: string;
  // --- T07 draft-test receipt. Backend validates these against the current
  // draft when present; force_save saves despite failed/skipped tests as long
  // as the same draft was tested at least once. ---
  test_run_id?: string;
  draft_fingerprint?: string;
  force_save?: boolean;
  /** Multi-key: additional API keys for load balancing. */
  extra_keys?: ChannelKeyInput[];
  /** 渠道级自定义上游请求头。 */
  request_headers?: ChannelRequestHeaderInput[];
}

export interface UpdateChannelInput {
  id: string;
  name?: string;
  type?: string;
  base_url?: string;
  api_key?: string;
  models?: string[];
  status?: number;
  priority?: number;
  weight?: number;
  config?: Record<string, unknown>;
  model_mapping?: Record<string, string | string[]>;
  timeout_secs?: number;
  // --- T02 optional new identity fields. None = keep; explicit empty
  // native_endpoints is rejected by the backend. ---
  protocol?: "openai" | "anthropic" | "ollama" | string;
  provider?: string;
  native_base_url?: string;
  native_endpoints?: string[];
  preset_revision?: string;
  legacy_executor_override?: string;
  /** Distinguish "edit leave-blank = keep key" from explicit clear (Ollama). */
  clear_api_key?: boolean;
  // --- T07 draft-test receipt (see CreateChannelInput). ---
  test_run_id?: string;
  draft_fingerprint?: string;
  force_save?: boolean;
  /** Multi-key: replacement for extra keys (full replace semantics). */
  extra_keys?: ChannelKeyInput[];
  /** 渠道级自定义上游请求头，整体替换。 */
  request_headers?: ChannelRequestHeaderInput[];
}

export interface TestChannelResult {
  success: boolean;
  message: string;
  latency_ms: number;
}

// API Key types
export interface ApiKey {
  id: string;
  name: string;
  key: string;
  status: number;
  allowed_models: string[];
  allowed_channels: string[];
  denied_models: string[];
  denied_channels: string[];
  quota_limit: number;
  quota_used: number;
  expires_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateApiKeyInput {
  name: string;
  /** 可选自定义密钥。留空则自动生成 sk-waliapi-<uuid>。 */
  key?: string;
  allowed_models?: string[];
  allowed_channels?: string[];
  denied_models?: string[];
  denied_channels?: string[];
  quota_limit?: number;
  expires_at?: string;
}

export interface ApiKeyStats {
  api_key_id: string;
  total_calls: number;
  success_calls: number;
  failed_calls: number;
  total_tokens: number;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  avg_latency_ms: number;
  last_call_at: string | null;
}

// Log types
export interface RequestLog {
  id: string;
  seq: number | null;
  api_key_name: string | null;
  channel_name: string | null;
  model: string;
  upstream_model: string | null;
  mode: string;
  status_code: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cached_tokens: number;
  duration_ms: number;
  error_message: string | null;
  is_stream: boolean;
  is_retry: boolean;
  created_at: string;
  request_body: string | null;
  response_choices: string | null;
  risk_level: string;
  risk_score: number;
  risk_summary: string | null;
  security_action: string;
  sanitized: boolean;
  blocked_reason: string | null;
  trace_id: string | null;
  reasoning_effort: string | null;
  // --- T09 observability fields (nullable; legacy rows are null) ---
  downstream_protocol: string | null;
  downstream_endpoint: string | null;
  route_group: string | null;
  upstream_protocol: string | null;
  upstream_endpoint: string | null;
  provider: string | null;
  codec_version: string | null;
  failure_class: string | null;
  identity_revision: number | null;
  client_cancelled: boolean | null;
  stream_committed: boolean | null;
  upstream_type: "channel" | "auth_account" | string;
  /** 日志记录级别；摘要列表也会返回该字段。 */
  detail_level?: "basic" | "brief" | "detailed" | string;
  /** 基本日志不提供正文，详细日志可按需加载。 */
  detail_available?: boolean;
  started_at?: string | null;
}

// Auth account contracts intentionally expose only renderer-safe account
// summaries. Credential payloads never enter this TypeScript boundary.
export interface AuthModelState {
  id: string;
  status: string;
  unavailable: boolean;
  next_retry_after: string | null;
  last_error: string | null;
}

export interface AuthQuotaWindow {
  used_percent: number | null;
  window_minutes: number | null;
  reset_at: string | null;
}

export interface AuthQuotaLimit {
  limit_id: string;
  limit_name: string | null;
  primary: AuthQuotaWindow | null;
  secondary: AuthQuotaWindow | null;
  credits: number | null;
}

export interface AuthQuotaState {
  version: number;
  exceeded: boolean;
  reason: string | null;
  next_recover_at: string | null;
  backoff_level: number;
  limits: AuthQuotaLimit[];
}

export interface AuthAccount {
  id: string;
  provider: string;
  label: string;
  account_id: string;
  status: string;
  disabled: boolean;
  priority: number;
  weight: number;
  /** 手动排序权重（拖拽排序），默认 0 表示未手动排序。 */
  sort_order: number;
  email: string | null;
  plan_type: string | null;
  /** Stable, non-secret reason the account was marked invalid (e.g. "payment_required"). */
  invalidation_reason: string | null;
  models: AuthModelState[];
  quota: AuthQuotaState | null;
  model_mapping?: Record<string, string | string[]>;
  expires_at: string | null;
  hasRefreshToken: boolean;
  last_refreshed_at: string | null;
  last_models_sync_at: string | null;
  next_refresh_after: string | null;
  next_retry_after: string | null;
  created_at: string;
  updated_at: string;
}

export interface AuthMutationResult {
  account: AuthAccount;
  /** Batch-imported accounts; login flows contain the primary account only. */
  imported_accounts?: AuthAccount[];
  warning: string | null;
  notice: string | null;
}

export interface AuthLoginStart {
  sessionId: string;
}

export type AuthProviderId = "codex" | "kimi" | "grok" | (string & {});
export type AuthLoginMethod = "browser_callback" | "device_code";

export interface AuthProviderInfo {
  id: AuthProviderId;
  displayName: string;
  iconKey: string;
  loginMode: "browser_callback" | "device_code" | (string & {});
  loginMethods: AuthLoginMethod[];
  supportsImport: boolean;
  supportsExport: boolean;
  supportsQuota: boolean;
}

export interface DeviceVerification {
  url: string;
  userCode: string;
  expiresAt: string | null;
}

export interface AuthLoginSessionStatus {
  sessionId: string;
  provider: string;
  state: "pending" | "saving" | "syncing" | "succeeded" | "cancelled" | "failed";
  step:
    | "preparing"
    | "authorizing"
    | "waiting"
    | "exchanging"
    | "saving"
    | "syncing"
    | null;
  verification: DeviceVerification | null;
  result: AuthMutationResult | null;
  errorCode:
    | "cancelled"
    | "timeout"
    | "browser_open"
    | "callback_state"
    | "device_authorization"
    | "authorization_denied"
    | "token_exchange"
    | "login_failed"
    | null;
  error: string | null;
}

export interface AuthLogoutResult {
  deleted: boolean;
}

export interface AuthExportResult {
  path: string;
  backup_path: string | null;
}

export interface AuthQuotaStatus {
  quota: AuthQuotaState | null;
  available: boolean;
}

export interface AuthUpdateInput {
  id: string;
  label: string;
  priority: number;
  weight: number;
  model_mapping?: Record<string, string | string[]>;
}

export interface SecurityFinding {
  id: string;
  log_id: string;
  phase: string;
  category: string;
  rule_id: string;
  severity: string;
  title: string;
  description: string | null;
  location: string | null;
  evidence_masked: string | null;
  action: string | null;
  created_at: string;
}

export interface LogStats {
  date: string;
  count: number;
  total_tokens: number;
}

// Stats types
export interface DashboardStats {
  today_requests: number;
  today_total_tokens: number;
  today_cached_tokens: number;
  today_prompt_tokens: number;
  total_cached_tokens: number;
  total_prompt_tokens: number;
  active_channels: number;
  avg_latency_ms: number;
  total_channels: number;
  active_auth_accounts: number;
  total_auth_accounts: number;
  total_api_keys: number;
  total_requests: number;
  total_tokens: number;
  total_knowledge_bases: number;
  total_kb_documents: number;
  total_kb_chunks: number;
  total_wiki_projects: number;
  total_wiki_pages: number;
}

export interface ModelStats {
  model: string;
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
  total_tokens: number;
  success_rate: number;
  avg_latency_ms: number;
}

export interface TokenTrendPoint {
  hour: string;
  model: string;
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
  total_tokens: number;
  request_count: number;
}

// Settings types
export interface Settings {
  server_port: number;
  server_host: string;
  ui_theme: string;
  ui_language: string;
  minimize_to_tray: boolean;
  close_to_tray: boolean;
  auto_start: boolean;
  retry_enabled: boolean;
  retry_times: number;
  security_enabled: boolean;
  security_mode: string;
  security_scan_unicode: boolean;
  security_scan_tools: boolean;
  security_scan_network: boolean;
  security_scan_response: boolean;
  security_redact_secrets: boolean;
  security_block_on_critical: boolean;
  routing_prefer_auth_accounts: boolean;
  routing_prefer_same_protocol: boolean;
  // LLM OCR（扫描版 PDF 识别）全局配置
  ocr_enabled: boolean;
  ocr_max_pages: number;
  ocr_concurrency: number;
  ocr_dpi: number;
  log_detail_level: "basic" | "detailed" | string;
  log_retention_days: number;
  // OTLP 导出（request_log → OTLP/HTTP JSON span，默认关闭）
  otlp_enabled: boolean;
  otlp_endpoint: string;
  otlp_headers: string;
  otlp_interval_secs: number;
  otlp_batch_size: number;
  // 渠道主动健康探测
  probe_enabled: boolean;
  probe_interval_secs: number;
  // 语义缓存（C-02，默认关闭）
  cache_enabled: boolean;
  cache_ttl_secs: number;
  cache_threshold_percent: number;
  cache_embedding_model: string;
}

// Security rule types
export interface BuiltinRule {
  id: string;
  rule_id: string;
  category: string;
  severity: string;
  title: string;
  description: string | null;
  toggle_key: string | null;
  enabled: boolean;
  created_at: string;
}

export interface UpdateBuiltinRuleInput {
  severity?: string;
  title?: string;
  description?: string;
  enabled?: boolean;
}

export interface CustomRule {
  id: string;
  rule_type: string;  // blacklist | whitelist
  category: string;   // domain | tool | path | keyword
  pattern: string;
  severity: string;
  action: string;
  enabled: boolean;
  description: string | null;
  created_at: string;
}

export interface CreateCustomRuleInput {
  rule_type: string;
  category: string;
  pattern: string;
  severity?: string;
  action?: string;
  description?: string;
}

// Server status
export interface ServerStatus {
  running: boolean;
  port: number;
  url: string;
}

// Channel type info
export interface ChannelTypeInfo {
  value: string;
  label: string;
  category: string;
  default_base_url: string;
  models: string[];
}

// ── Provider preset DTO（T01，与 src-tauri/src/channel_presets.rs 保持一致）──
// 序列化字符串必须与 Rust 枚举的 serde 输出完全一致。

export type ChannelProtocol = "openai" | "anthropic" | "ollama";

export type ChannelProvider =
  | "openai"
  | "google"
  | "deepseek"
  | "qwen"
  | "zhipu"
  | "doubao"
  | "doubao_coding_plan"
  | "moonshot"
  | "anthropic"
  | "ollama"
  | "custom";

export type ChannelEndpoint =
  | "chat_completions"
  | "responses"
  | "messages"
  | "count_tokens"
  | "embeddings"
  | "api_chat";

export type ChannelAuthScheme =
  | "bearer"
  | "x_api_key"
  | "query_key"
  | "optional_bearer";

export type ChannelRegionGroup =
  | "custom"
  | "international"
  | "domestic"
  | "local";

export type ChannelModelEnumStrategy = "static_only" | "static_plus_sync" | "sync_only";

export type ChannelEndpointTestStrategy = "probe_first_model" | "list_models";

export interface ChannelModelSuggestion {
  id: string;
  verified_at: string;
  source_url: string;
}

/** 渠道提供商模板（T01）。URL/模型/能力唯一真相在后端 registry。 */
export interface ChannelPreset {
  id: string;
  protocol: ChannelProtocol;
  provider: ChannelProvider;
  display_name: string;
  region: ChannelRegionGroup;
  description: string;
  icon_key: string;
  native_base_url: string;
  legacy_base_url: string;
  legacy_type: string;
  native_endpoints: ChannelEndpoint[];
  default_checked_endpoints: ChannelEndpoint[];
  auth_scheme: ChannelAuthScheme;
  model_suggestions: ChannelModelSuggestion[];
  model_enum_strategy: ChannelModelEnumStrategy;
  endpoint_test_strategy: ChannelEndpointTestStrategy;
  preset_revision: string;
}

/** 每个协议一组；`presets[0]` 恒为固定 custom option。 */
export interface ChannelProtocolPresetGroup {
  protocol: ChannelProtocol;
  presets: ChannelPreset[];
}

// ── 草稿连通性测试（T07）─────────────────────────────────────────────────────
// 字段名与设计 5.2 及 T07 API 契约逐字一致，不得改名。

export type DraftEndpointTestStatus = "passed" | "failed" | "skipped";

export type DraftEndpointTestFailureCategory =
  | "network"
  | "timeout"
  | "authentication"
  | "endpoint_unsupported"
  | "model"
  | "request"
  | "protocol"
  | "unknown";

export interface DraftEndpointTestResult {
  endpoint: ChannelEndpoint;
  status: DraftEndpointTestStatus;
  category?: DraftEndpointTestFailureCategory;
  /** 已脱敏 message（绝不包含 API Key 或完整请求体）。 */
  message: string;
  latency_ms: number;
  /** 本次测试实际探测的模型。 */
  tested_model: string | null;
  /** 该端点的验证是否可能产生极少上游费用。 */
  cost_possible: boolean;
}

export interface DraftChannelTestResult {
  /** 覆盖 protocol/provider/规范 URL/模型/端点/timeout/Key 的后端不可逆指纹。 */
  draft_fingerprint: string;
  tested_at: string;
  test_run_id: string;
  results: DraftEndpointTestResult[];
}

/** `test_channel_draft` 的输入：完整未保存草稿（T07 API 契约）。 */
export interface DraftChannelTestInput {
  /** 编辑场景提供已保存渠道 id，供后端在 API Key 留空时读取现有 Key（T07）。 */
  id?: string;
  name: string;
  type: string;
  base_url: string;
  api_key: string;
  /** 显式清除已保存 Key（T02）：为 true 时后端把留空的 Key 解析为空串，而非沿用已存 Key。 */
  clear_api_key?: boolean;
  models: string[];
  priority?: number;
  weight?: number;
  config?: Record<string, unknown>;
  model_mapping?: Record<string, string | string[]>;
  timeout_secs?: number;
  protocol?: ChannelProtocol | string;
  provider?: ChannelProvider | string;
  native_base_url?: string;
  native_endpoints?: ChannelEndpoint[];
  preset_revision?: string;
  legacy_executor_override?: string;
  /** 渠道级自定义上游请求头。 */
  request_headers?: ChannelRequestHeaderInput[];
}

// ── 上游模型同步（T14）─────────────────────────────────────────────────────────
// `sync_upstream_models` 命令的输出：绝不写库，仅返回拉取结果供弹窗勾选合并。

export interface UpstreamModelsResult {
  /** 上游返回的模型 ID 列表（openai `data[].id` / ollama `models[].name`）。 */
  models: string[];
  /** 判定出的上游协议：`openai` / `anthropic` / `ollama`。 */
  protocol: ChannelProtocol | string;
  /** 拉取时使用的根 URL（便于展示/排障）。 */
  base_url: string;
}

// Prompt 模板（C-07 版本化管理）
export interface PromptTemplate {
  id: string;
  template_key: string;
  version: number;
  content: string;
  active: boolean;
  created_at: string;
}
