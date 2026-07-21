// Channel types
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
  model_mapping: Record<string, string>;
  created_at: string;
  updated_at: string;
  last_test_at: string | null;
  last_test_ok: number | null;
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
  model_mapping?: Record<string, string>;
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
  model_mapping?: Record<string, string>;
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
  quota_limit: number;
  quota_used: number;
  expires_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateApiKeyInput {
  name: string;
  allowed_models?: string[];
  allowed_channels?: string[];
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
  active_channels: number;
  avg_latency_ms: number;
  total_channels: number;
  total_api_keys: number;
  total_requests: number;
  total_tokens: number;
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
