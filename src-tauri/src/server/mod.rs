pub mod router;
pub mod handlers;

use crate::AppState;
use tauri::{AppHandle, Emitter};

pub async fn start_server(app: AppHandle, state: std::sync::Arc<AppState>) -> Result<(), anyhow::Error> {
    let port = get_server_port(&app).await;

    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let actual_port = listener.local_addr()?.port();

    *state.server_port.write().await = actual_port;
    state.server_running.store(true, std::sync::atomic::Ordering::SeqCst);

    let router = router::create_router(app.clone(), state.clone());

    app.emit("server-started", serde_json::json!({ "port": actual_port, "url": format!("http://127.0.0.1:{}", actual_port) })).ok();

    tracing::info!("xapi server listening on http://127.0.0.1:{}", actual_port);

    axum::serve(listener, router).await?;

    state.server_running.store(false, std::sync::atomic::Ordering::SeqCst);

    Ok(())
}

async fn get_server_port(app: &AppHandle) -> u16 {
    use tauri_plugin_store::StoreExt;
    if let Ok(store) = app.store("settings.json") {
        if let Some(port) = store.get("server.port") {
            if let Some(p) = port.as_u64() {
                return p as u16;
            }
        }
    }
    8777
}
