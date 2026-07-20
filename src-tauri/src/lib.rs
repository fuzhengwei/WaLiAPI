// WaLiAPI - 本地 LLM API 网关
// 第1-6节：API转发代理核心
mod adaptor;

use tauri::Manager;
mod core;
mod security;
mod commands;
mod db;
mod utils;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! WaLiAPI 工程已就绪。", name)
}

pub struct AppState {
    pub db: std::sync::Arc<db::Database>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
                app_handle.manage(std::sync::Arc::new(AppState {
                    db: std::sync::Arc::new(db),
                }));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            commands::channel::get_channels,
            commands::channel::get_channel,
            commands::channel::create_channel,
            commands::channel::update_channel,
            commands::channel::toggle_channel,
            commands::channel::delete_channel,
            commands::channel::test_channel,
        ])
        .run(tauri::generate_context!())
        .expect("error while running WaLiAPI");
}
