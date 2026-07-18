use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub server_port: u16,
    #[serde(default = "default_host")]
    pub server_host: String,
    #[serde(default = "default_theme")]
    pub ui_theme: String,
    #[serde(default = "default_language")]
    pub ui_language: String,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default = "default_false")]
    pub auto_start: bool,
    #[serde(default = "default_retry_enabled")]
    pub retry_enabled: bool,
    #[serde(default = "default_retry_times")]
    pub retry_times: i32,
}

fn default_port() -> u16 { 8777 }
fn default_host() -> String { "127.0.0.1".to_string() }
fn default_theme() -> String { "dark".to_string() }
fn default_language() -> String { "zh-CN".to_string() }
fn default_true() -> bool { true }
fn default_false() -> bool { false }
fn default_retry_enabled() -> bool { true }
fn default_retry_times() -> i32 { 2 }

impl Default for Settings {
    fn default() -> Self {
        Settings {
            server_port: default_port(),
            server_host: default_host(),
            ui_theme: default_theme(),
            ui_language: default_language(),
            minimize_to_tray: default_true(),
            close_to_tray: default_true(),
            auto_start: default_false(),
            retry_enabled: default_retry_enabled(),
            retry_times: default_retry_times(),
        }
    }
}

fn get_str(store: &tauri_plugin_store::Store<tauri::Wry>, key: &str, default: &str) -> String {
    store.get(key)
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| default.to_string())
}

fn get_u64(store: &tauri_plugin_store::Store<tauri::Wry>, key: &str, default: u64) -> u64 {
    store.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

fn get_bool(store: &tauri_plugin_store::Store<tauri::Wry>, key: &str, default: bool) -> bool {
    store.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let settings = Settings {
        server_port: get_u64(&store, "server.port", 8777) as u16,
        server_host: get_str(&store, "server.host", "127.0.0.1"),
        ui_theme: get_str(&store, "ui.theme", "dark"),
        ui_language: get_str(&store, "ui.language", "zh-CN"),
        minimize_to_tray: get_bool(&store, "general.minimize_to_tray", true),
        close_to_tray: get_bool(&store, "general.close_to_tray", true),
        auto_start: get_bool(&store, "general.auto_start", false),
        retry_enabled: get_bool(&store, "retry.enabled", true),
        retry_times: get_u64(&store, "retry.times", 2) as i32,
    };
    Ok(settings)
}

#[tauri::command]
pub async fn save_settings(settings: Settings, app: AppHandle) -> Result<(), String> {
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    store.set("server.port", serde_json::json!(settings.server_port));
    store.set("server.host", serde_json::json!(settings.server_host));
    store.set("ui.theme", serde_json::json!(settings.ui_theme));
    store.set("ui.language", serde_json::json!(settings.ui_language));
    store.set("general.minimize_to_tray", settings.minimize_to_tray);
    store.set("general.close_to_tray", settings.close_to_tray);
    store.set("general.auto_start", settings.auto_start);
    store.set("retry.enabled", settings.retry_enabled);
    store.set("retry.times", settings.retry_times);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}
