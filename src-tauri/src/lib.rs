mod adaptor;
#[cfg(test)]
mod auth_integration_tests;
pub mod auth_provider;
mod channel_presets;
pub mod commands;
pub mod core;
pub mod db;
mod endpoint_executor;
mod protocol;
#[cfg(test)]
mod rollout_integration_tests;
pub mod security;
pub mod server;
pub mod settings_store;
pub mod services;
pub mod utils;
pub mod web_server;

/// 应用标识符（与 tauri.conf.json 一致），用于 headless 数据目录解析。
pub const APP_IDENTIFIER: &str = "waliapi.xiaofuge.cn";

use std::sync::Arc;
#[cfg(feature = "desktop-ui")]
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_store::StoreExt;
use tokio::sync::RwLock;

/// 桌面端设置后端：tauri-plugin-store（settings.json）。
struct TauriSettingsBackend(AppHandle);

impl settings_store::SettingsBackend for TauriSettingsBackend {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0.store("settings.json").ok()?.get(key)
    }

    fn set_many(&self, entries: &[(String, serde_json::Value)]) -> Result<(), String> {
        let store = self.0.store("settings.json").map_err(|e| e.to_string())?;
        for (key, value) in entries {
            store.set(key.clone(), value.clone());
        }
        store.save().map_err(|e| e.to_string())
    }
}

pub struct AppState {
    pub db: Arc<db::Database>,
    pub auth_service: Arc<auth_provider::service::AuthService>,
    pub login_sessions: Arc<commands::auth::LoginSessions>,
    pub server_port: Arc<RwLock<u16>>,
    pub server_running: Arc<std::sync::atomic::AtomicBool>,
    pub server_handle: Arc<RwLock<Option<tauri::async_runtime::JoinHandle<()>>>>,
    /// T07: short-lived, in-process test-run receipt store used to validate
    /// `test_run_id + draft_fingerprint + force_save` at channel save time.
    /// Process restart clears it → every receipt expires → re-test required.
    pub test_receipts: Arc<crate::services::channel_test::TestReceiptStore>,
    /// Web 管理面板：管理员会话（内存存储，重启失效）。
    pub admin_sessions: Arc<server::admin_auth::SessionStore>,
    /// 统一事件出口（桌面 Webview + Web SSE 桥）。
    pub events: server::event_bridge::EventSink,
    /// 设置存储（桌面 tauri-plugin-store / headless JSON 文件）。
    pub settings: settings_store::SettingsStore,
    /// 应用数据目录（KB 文件等落盘位置）。
    pub data_dir: std::path::PathBuf,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 获取可执行文件所在目录
    let exe_dir = std::env::current_exe()
        .map(|path| path.parent().map(|p| p.to_path_buf()).unwrap_or(std::path::PathBuf::from(".")))
        .unwrap_or(std::path::PathBuf::from("."));

    // 创建日志目录
    let log_dir = exe_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();

    // 按天滚动日志：文件名前缀 waliapi.log（如 waliapi.log.2026-08-25），最多保留 7 个文件
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("waliapi.log")
        .max_log_files(7)
        .build(&log_dir)
        .ok();

    // 统一输出到文件；构建失败时回退到标准输出
    let subscriber = tracing_subscriber::fmt().with_max_level(tracing::Level::INFO);
    if let Some(file_appender) = file_appender {
        subscriber.with_writer(file_appender).init();
    } else {
        subscriber.init();
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations(
                    "sqlite:waliapi.db",
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
            #[cfg(feature = "desktop-ui")]
            {
                let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
                let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

                TrayIconBuilder::with_id("main")
                    .icon(app.default_window_icon().unwrap().clone())
                    .menu(&menu)
                    .tooltip("WaLiAPI - Local LLM API Gateway")
                    .show_menu_on_left_click(false)
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            let _ = restore_main_window(tray.app_handle());
                        }
                    })
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "quit" => app.exit(0),
                        "show" => {
                            let _ = restore_main_window(app);
                        }
                        _ => {}
                    })
                    .build(app)?;
            }

            if let Some(window) = app.get_webview_window("main") {
                let app_handle = app.handle().clone();
                window.on_window_event(move |event| match event {
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        if should_close_to_tray(&app_handle) {
                            api.prevent_close();
                            if let Some(main_window) = app_handle.get_webview_window("main") {
                                let _ = main_window.hide();
                            }
                        }
                    }
                    _ => {}
                });

                if env_flag("WALIAPI_HIDE_WINDOW") {
                    let _ = window.hide();
                }
            }

            let app_handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                let data_dir = app_handle
                    .path()
                    .app_data_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("."));
                let db = db::Database::new(&app_handle).await;
                let db = Arc::new(db);
                if let Err(e) = server::admin_auth::ensure_initial_admin(&db.pool, &data_dir).await
                {
                    log::error!("初始化 Web 管理员账号失败: {e}");
                }
                let auth_service = Arc::new(auth_provider::service::AuthService::new(
                    Arc::new(db::repository::Repository::new(db.pool.clone())),
                    auth_provider::ProviderRegistry::new(),
                ));
                let (event_tx, _) = tokio::sync::broadcast::channel(
                    server::event_bridge::EVENT_CHANNEL_CAPACITY,
                );
                let emit_handle = app_handle.clone();
                let state = Arc::new(AppState {
                    db,
                    auth_service: auth_service.clone(),
                    login_sessions: Arc::new(commands::auth::LoginSessions::new()),
                    server_port: Arc::new(RwLock::new(0)),
                    server_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    server_handle: Arc::new(RwLock::new(None)),
                    test_receipts: Arc::new(crate::services::channel_test::TestReceiptStore::new(
                        std::time::Duration::from_secs(30 * 60),
                    )),
                    admin_sessions: server::admin_auth::SessionStore::new(),
                    events: server::event_bridge::EventSink::desktop(
                        move |event, payload| {
                            let _ = emit_handle.emit(event, payload);
                        },
                        event_tx,
                    ),
                    settings: settings_store::SettingsStore::new(Arc::new(TauriSettingsBackend(
                        app_handle.clone(),
                    ))),
                    data_dir,
                });
                app_handle.manage(state.clone());

                tauri::async_runtime::spawn(async move {
                    auth_provider::maintenance::run_maintenance_loop(auth_service).await;
                });

                // 桌面版启动即自动拉起内嵌服务（LLM 网关 + /admin/api）；
                // Web 管理面板 SPA 仅在 embed-web 构建（waliapi-web / Docker）中提供。
                let state_clone = state.clone();
                let app_clone = app_handle.clone();
                let handle = tauri::async_runtime::spawn(async move {
                    if let Err(e) = server::start_server(state_clone, Some(app_clone)).await {
                        log::error!("内嵌服务启动失败: {e}");
                    }
                });
                *state.server_handle.write().await = Some(handle);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::channel::get_channels,
            commands::channel::get_channel_presets,
            commands::channel::get_channel,
            commands::channel::get_channel_api_key,
            commands::channel::create_channel,
            commands::channel::update_channel,
            commands::channel::toggle_channel,
            commands::channel::delete_channel,
            commands::channel::test_channel,
            commands::channel::test_channel_draft,
            commands::channel::sync_upstream_models,
            commands::channel::get_channel_stats,
            commands::channel::reorder_channels,
            commands::channel::get_channel_extra_keys,
            commands::channel::get_channel_extra_key_value,
            commands::channel::toggle_channel_extra_key,
            commands::channel::delete_channel_extra_key,
            commands::api_key::get_api_keys,
            commands::api_key::create_api_key,
            commands::api_key::update_api_key,
            commands::api_key::delete_api_key,
            commands::api_key::get_api_key_stats,
            commands::auth::auth_accounts_list,
            commands::auth::auth_providers_list,
            commands::auth::auth_login,
            commands::auth::auth_login_start,
            commands::auth::auth_login_status,
            commands::auth::auth_login_cancel,
            commands::auth::auth_login_import,
            commands::auth::auth_login_import_content,
            commands::auth::auth_default_import_path,
            commands::auth::auth_logout,
            commands::auth::auth_refresh_token,
            commands::auth::auth_sync_models,
            commands::auth::auth_export_json,
            commands::auth::auth_export_json_content,
            commands::auth::auth_toggle,
            commands::auth::auth_quota_status,
            commands::auth::auth_update,
            commands::log::get_logs,
            commands::log::get_log,
            commands::log::get_log_security_findings,
            commands::log::delete_log,
            commands::log::delete_logs_before,
            commands::log::delete_all_logs,
            commands::log::get_log_stats,
            commands::log_repair::repair_stream_cancel_logs,
            commands::stats::get_dashboard_stats,
            commands::stats::get_model_stats,
            commands::stats::get_token_trend,
            commands::settings::get_settings,
            commands::settings::get_feature_flags,
            commands::settings::save_settings,
            commands::settings::apply_theme,
            commands::settings::set_auto_start,
            commands::server::get_server_status,
            commands::server::restart_server,
            commands::security::get_builtin_security_rules,
            commands::security::update_builtin_security_rule,
            commands::security::delete_builtin_security_rule,
            commands::security::reset_builtin_security_rules,
            commands::security::get_custom_security_rules,
            commands::security::create_custom_security_rule,
            commands::security::toggle_custom_security_rule,
            commands::security::delete_custom_security_rule,
            commands::import_export::export_channels,
            commands::import_export::import_walicode_backup,
            commands::import_export::import_waliapi_export,
            commands::import_export::scan_local_ai_configs,
            commands::import_export::import_scanned_sources,
            commands::import_export::pick_import_file,
            commands::import_export::save_export_file,
            // Knowledge Base
            commands::knowledge_base::get_knowledge_bases,
            commands::knowledge_base::create_knowledge_base,
            commands::knowledge_base::update_knowledge_base,
            commands::knowledge_base::delete_knowledge_base,
            commands::knowledge_base::get_kb_documents,
            commands::knowledge_base::delete_kb_document,
            commands::knowledge_base::reindex_kb_document,
            commands::knowledge_base::search_knowledge_base,
            commands::knowledge_base::ask_knowledge_base,
            commands::knowledge_base::get_kb_stats,
            commands::knowledge_base::upload_kb_document,
            commands::knowledge_base::get_kb_conversations,
            commands::knowledge_base::clear_kb_conversations,
            commands::knowledge_base::get_kb_sources,
            commands::knowledge_base::delete_kb_source,
            commands::knowledge_base::import_kb_source,
            commands::knowledge_base::get_kb_index_status,
            commands::knowledge_base::build_kb_index,
            commands::knowledge_base::drop_kb_index,
            commands::knowledge_base::get_kb_tags,
            commands::knowledge_base::get_ocr_cache_info,
            commands::knowledge_base::clear_ocr_cache,
            commands::services::get_service_statuses,
            // Wiki
            commands::wiki::get_wiki_projects,
            commands::wiki::create_wiki_project,
            commands::wiki::get_wiki_project,
            commands::wiki::update_wiki_project,
            commands::wiki::delete_wiki_project,
            commands::wiki::get_wiki_pages,
            commands::wiki::get_wiki_page,
            commands::wiki::save_wiki_page,
            commands::wiki::get_wiki_sources,
            commands::wiki::add_wiki_source,
            commands::wiki::delete_wiki_source,
            commands::wiki::search_wiki,
            commands::wiki::get_wiki_graph,
            commands::wiki::get_wiki_stats,
            commands::wiki::ingest_wiki_source,
            commands::wiki::rescan_wiki_sources,
            commands::wiki::get_wiki_tags,
            // App Config (应用配置)
            commands::app_config::get_app_configs,
            commands::app_config::apply_app_config,
            commands::app_config::clear_app_config,
            commands::app_config::reset_codex_auth,
            commands::app_config::get_app_config_content,
            commands::app_config::open_config_folder,
        ])
        .build(tauri::generate_context!())
        .expect("error while building WaLiAPI")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            {
                if let RunEvent::Reopen { .. } = event {
                    let _ = restore_main_window(app);
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (app, &event);
            }
        });
}

fn restore_main_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(target_os = "macos")]
        {
            let _ = app.show();
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    Ok(())
}

fn should_close_to_tray(app: &tauri::AppHandle) -> bool {
    app.store("settings.json")
        .ok()
        .and_then(|store| store.get("general.close_to_tray").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}
