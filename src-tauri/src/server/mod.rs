pub mod admin;
pub mod admin_auth;
pub mod admin_routes;
pub mod event_bridge;
pub mod handlers;
pub mod knowledge_access;
pub mod request_id;
pub mod router;
pub mod static_assets;

use crate::settings_store::SettingsStore;
use crate::AppState;
use std::sync::Arc;
use tauri::AppHandle;

/// 启动内嵌 HTTP 服务（LLM 网关 + Web 管理面板）。
/// `desktop_app` 仅桌面端传入（用于对话框等桌面专属命令）；headless 传 None。
pub async fn start_server(
    state: Arc<AppState>,
    desktop_app: Option<AppHandle>,
) -> Result<(), anyhow::Error> {
    let host = get_server_host(&state.settings);
    let port = get_server_port(&state.settings);

    // 服务端点 token（KB/Wiki REST / MCP）配置核对：
    // - 非回环绑定 + 缺 token：醒目多行告警，说明哪些端点已关闭、哪些面仍暴露；
    // - token 过短 / 两个 token 相同：配置告警；
    // - 回环绑定 + 缺 token：info 提示端点已关闭（桌面默认形态，不算暴露）。
    let service_tokens = admin::ServiceTokens::from_env();
    for warning in service_tokens.config_warnings() {
        tracing::warn!("[服务端点鉴权] {warning}");
    }
    if let Some(exposure) = service_tokens.exposure_warning(&host) {
        tracing::warn!("[服务端点鉴权] 非回环绑定且缺少服务 token：\n{exposure}");
    } else {
        let disabled = service_tokens.disabled_endpoints();
        if !disabled.is_empty() {
            tracing::info!(
                "[服务端点鉴权] 未配置 WALIAPI_ADMIN_TOKEN / WALIAPI_MCP_TOKEN，\
                 {} 已关闭（一律返回 401）。设置上述环境变量以启用。",
                disabled.join("、")
            );
        }
    }

    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;
    let actual_port = local_addr.port();

    *state.server_port.write().await = actual_port;
    state
        .server_running
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let router = {
        #[cfg(feature = "desktop-ui")]
        {
            router::create_router(state.clone(), desktop_app)
        }
        #[cfg(not(feature = "desktop-ui"))]
        {
            let _ = &desktop_app; // headless 不使用 AppHandle
            router::create_router(state.clone())
        }
    };

    state.events.emit(
        "server-started",
        serde_json::json!({
            "port": actual_port,
            "url": format!("http://{}:{}", host, actual_port)
        }),
    );

    tracing::info!(
        "WaLiAPI server listening on http://{}:{}",
        host,
        actual_port
    );

    axum::serve(listener, router).await?;

    state
        .server_running
        .store(false, std::sync::atomic::Ordering::SeqCst);

    Ok(())
}

fn get_server_host(settings: &SettingsStore) -> String {
    if let Ok(host) = std::env::var("WALIAPI_SERVER_HOST") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let host = settings.get_str("server.host", "");
    if !host.trim().is_empty() {
        return host.trim().to_string();
    }
    "127.0.0.1".to_string()
}

fn get_server_port(settings: &SettingsStore) -> u16 {
    if let Ok(port) = std::env::var("WALIAPI_SERVER_PORT") {
        if let Ok(value) = port.trim().parse::<u16>() {
            if value != 0 {
                return value;
            }
        }
    }

    let port = settings.get_u64("server.port", 8777) as u16;
    if port == 0 {
        return 8777;
    }
    port
}
