import { invoke } from "@tauri-apps/api/core";
import type {
  Channel, CreateChannelInput, UpdateChannelInput, TestChannelResult,
  ApiKey, CreateApiKeyInput,
  RequestLog, LogStats,
  DashboardStats,
  Settings,
  ServerStatus,
} from "../types";

// Channel commands
export const channelApi = {
  getAll: () => invoke<Channel[]>("get_channels"),
  get: (id: string) => invoke<Channel>("get_channel", { id }),
  create: (input: CreateChannelInput) => invoke<Channel>("create_channel", { input }),
  update: (input: UpdateChannelInput) => invoke<Channel>("update_channel", { input }),
  delete: (id: string) => invoke<void>("delete_channel", { id }),
  test: (id: string) => invoke<TestChannelResult>("test_channel", { id }),
};

// API Key commands
export const apiKeyApi = {
  getAll: () => invoke<ApiKey[]>("get_api_keys"),
  create: (input: CreateApiKeyInput) => invoke<ApiKey>("create_api_key", { input }),
  update: (id: string, status?: number) => invoke<void>("update_api_key", { input: { id, status } }),
  delete: (id: string) => invoke<void>("delete_api_key", { id }),
};

// Log commands
export const logApi = {
  getAll: (limit?: number, offset?: number) => invoke<RequestLog[]>("get_logs", { limit, offset }),
  get: (id: string) => invoke<RequestLog>("get_log", { id }),
  getStats: (days?: number) => invoke<LogStats[]>("get_log_stats", { days }),
  delete: (id: string) => invoke<void>("delete_log", { id }),
  deleteBefore: (beforeDate: string) => invoke<number>("delete_logs_before", { beforeDate }),
  deleteAll: () => invoke<number>("delete_all_logs"),
};

// Stats commands
export const statsApi = {
  getDashboard: () => invoke<DashboardStats>("get_dashboard_stats"),
};

// Settings commands
export const settingsApi = {
  get: () => invoke<Settings>("get_settings"),
  save: (settings: Settings) => invoke<void>("save_settings", { settings }),
  applyTheme: (theme: string) => invoke<void>("apply_theme", { theme }),
  setAutoStart: (enabled: boolean) => invoke<void>("set_auto_start", { enabled }),
};

// Server commands
export const serverApi = {
  getStatus: () => invoke<ServerStatus>("get_server_status"),
  restart: () => invoke<void>("restart_server"),
};
