pub mod router;
pub mod handlers;

use crate::AppState;
use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;

pub async fn start_server(app: AppHandle, state: std::sync::Arc<AppState>) -> Result<(), anyhow::Error> {
    let host = get_server_host(&app);
    let port = get_server_port(&app);

    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;
    let actual_port = local_addr.port();

    *state.server_port.write().await = actual_port;
    state.server_running.store(true, std::sync::atomic::Ordering::SeqCst);

    let router = router::create_router(app.clone(), state.clone());

    app.emit(
        "server-started",
        serde_json::json!({
            "port": actual_port,
            "url": format!("http://{}:{}", host, actual_port)
        }),
    )
    .ok();

    tracing::info!("xapi server listening on http://{}:{}", host, actual_port);

    axum::serve(listener, router).await?;

    state.server_running.store(false, std::sync::atomic::Ordering::SeqCst);

    Ok(())
}

fn get_server_host(app: &AppHandle) -> String {
    if let Ok(store) = app.store("settings.json") {
        if let Some(host) = store.get("server.host") {
            if let Some(value) = host.as_str() {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    "127.0.0.1".to_string()
}

fn get_server_port(app: &AppHandle) -> u16 {
    if let Ok(store) = app.store("settings.json") {
        if let Some(port) = store.get("server.port") {
            if let Some(value) = port.as_u64() {
                return value as u16;
            }
        }
    }
    8777
}
