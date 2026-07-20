// WaLiAPI - 本地 LLM API 网关
// 第1-2节：数据库设计与初始化
mod db;

use tauri::Manager;
mod utils;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! WaLiAPI 工程已就绪。", name)
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
            // 初始化数据库：连接池 + 执行 migrations
            let app_handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                let db = db::Database::new(&app_handle).await;
                app_handle.manage(std::sync::Arc::new(db));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running WaLiAPI");
}
