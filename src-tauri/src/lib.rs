mod commands;
mod core;
mod adaptor;
mod server;
mod db;
mod utils;

use std::sync::Arc;
use tokio::sync::RwLock;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};

pub struct AppState {
    pub db: Arc<db::Database>,
    pub server_port: Arc<RwLock<u16>>,
    pub server_running: Arc<std::sync::atomic::AtomicBool>,
    pub server_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations(
                    "sqlite:xapi.db",
                    vec![tauri_plugin_sql::Migration {
                        version: 1,
                        description: "init database",
                        sql: include_str!("../migrations/001_init.sql"),
                        kind: tauri_plugin_sql::MigrationKind::Up,
                    }],
                )
                .build(),
        )
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ))
        .setup(|app| {
            // Setup tray
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("xapi - LLM API Gateway")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            // Initialize database
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

                // Start HTTP server
                let handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = server::start_server(handle, state).await;
                });
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Channel commands
            commands::channel::get_channels,
            commands::channel::get_channel,
            commands::channel::create_channel,
            commands::channel::update_channel,
            commands::channel::delete_channel,
            commands::channel::test_channel,
            // API Key commands
            commands::api_key::get_api_keys,
            commands::api_key::create_api_key,
            commands::api_key::update_api_key,
            commands::api_key::delete_api_key,
            // Log commands
            commands::log::get_logs,
            commands::log::get_log_stats,
            // Stats commands
            commands::stats::get_dashboard_stats,
            // Settings commands
            commands::settings::get_settings,
            commands::settings::save_settings,
            // Server commands
            commands::server::get_server_status,
            commands::server::restart_server,
        ])
        .run(tauri::generate_context!())
        .expect("error while running xapi");
}
