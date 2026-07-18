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

// Log types
export interface RequestLog {
  id: string;
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
