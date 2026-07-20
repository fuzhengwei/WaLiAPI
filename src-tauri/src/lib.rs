// WaLiAPI - 本地 LLM API 网关
// 第2-2节：安全审计引擎
mod adaptor;

use tauri::Manager;
mod commands;
mod core;
mod db;
mod security;
mod server;
mod utils;

use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AppState {
    pub db: Arc<db::Database>,
    pub server_port: Arc<RwLock<u16>>,
    pub server_running: Arc<std::sync::atomic::AtomicBool>,
    pub server_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! WaLiAPI 工程已就绪。", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                let db = db::Database::new(&app_handle).await;
                let state = Arc::new(AppState {
                    db: Arc::new(db),
                    server_port: Arc::new(RwLock::new(0)),
                    server_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    server_handle: Arc::new(RwLock::new(None)),
                });
                app_handle.manage(state.clone());

                let handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = server::start_server(handle, state).await;
                });
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            commands::api_key::get_api_keys,
            commands::api_key::create_api_key,
            commands::api_key::update_api_key,
            commands::api_key::delete_api_key,
            commands::channel::get_channels,
            commands::channel::get_channel,
            commands::channel::create_channel,
            commands::channel::update_channel,
            commands::channel::toggle_channel,
            commands::channel::delete_channel,
            commands::channel::test_channel,
            commands::log::get_logs,
            commands::log::get_log,
            commands::log::get_log_security_findings,
            commands::log::delete_log,
            commands::log::delete_logs_before,
            commands::log::delete_all_logs,
            commands::log::get_log_stats,
            commands::stats::get_dashboard_stats,
            commands::server::get_server_status,
            commands::server::restart_server,
        ])
        .run(tauri::generate_context!())
        .expect("error while running WaLiAPI");
}
