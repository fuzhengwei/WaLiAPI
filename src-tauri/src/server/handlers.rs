use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Json, IntoResponse, Response, sse::{Sse, Event as SseEvent}},
};
use futures_util::StreamExt;
use std::convert::Infallible;
use super::router::SharedState;
use crate::core::proxy;
use crate::db::repository::Repository;
use crate::adaptor::{get_adaptor, ProxyRequest};
use crate::core::dispatcher::Dispatcher;

pub async fn handle_chat_completions(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let body_str = String::from_utf8_lossy(&body);
    let json: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(j) => j,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)).into_response(),
    };

    let is_stream = json.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    let auth_header = headers.get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("");
    let api_key = auth_header.strip_prefix("Bearer ").unwrap_or("").trim();

    if api_key.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Missing API key").into_response();
    }

    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));
    let key_record = match repo.get_api_key_by_key(api_key).await {
        Ok(k) => k,
        Err(_) => return (StatusCode::UNAUTHORIZED, "Invalid API key").into_response(),
    };

    if key_record.quota_limit > 0 && key_record.quota_used >= key_record.quota_limit {
        return (StatusCode::TOO_MANY_REQUESTS, "Quota exceeded").into_response();
    }

    if is_stream {
        handle_stream(shared, json, key_record.id, key_record.name).await
    } else {
        match proxy::handle_request(&repo, &key_record.id, &key_record.name, json, false).await {
            Ok(result) => (StatusCode::OK, Json(result.body)).into_response(),
            Err((code, msg)) => {
                let err_body = serde_json::json!({
                    "error": { "message": msg, "type": "upstream_error", "code": code }
                });
                (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY), Json(err_body)).into_response()
            }
        }
    }
}

async fn handle_stream(
    shared: SharedState,
    json: serde_json::Value,
    api_key_id: String,
    api_key_name: String,
) -> Response {
    let model = json.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();

    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));
    let channels = match repo.get_enabled_channels().await {
        Ok(c) => c,
        Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, "No channels available").into_response(),
    };

    let selected_channels = Dispatcher::select_channels(&channels, &model);
    if selected_channels.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "No channel for model").into_response();
    }

    let request = ProxyRequest {
        model: model.clone(),
        body: json.clone(),
        stream: true,
    };

    let mut last_error = None;

    for (attempt, channel) in selected_channels.into_iter().enumerate() {
        let config = Dispatcher::channel_to_config(&channel);
        let adaptor = get_adaptor(&channel.channel_type);

        match adaptor.forward_stream(&request, &config).await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body_str = resp.text().await.unwrap_or_default();
                    last_error = Some(format!("{}: {}", channel.name, body_str));
                    continue;
                }

                let stream = resp.bytes_stream().map(|result| {
                    match result {
                        Ok(bytes) => Ok::<_, Infallible>(SseEvent::default().data(String::from_utf8_lossy(&bytes))),
                        Err(_) => Ok(SseEvent::default().data("[DONE]")),
                    }
                });

                let start = std::time::Instant::now();
                let channel_id = channel.id.clone();
                let channel_name = channel.name.clone();
                let repo_clone = repo.clone();
                let api_key_id_clone = api_key_id.clone();
                let api_key_name_clone = api_key_name.clone();
                let model_clone = model.clone();
                let is_retry = if attempt > 0 { 1 } else { 0 };

                let log_stream = async_stream::stream! {
                    tokio::pin!(stream);
                    while let Some(event) = stream.next().await {
                        let sse_event = match event {
                            Ok(sse_event) => sse_event,
                            Err(_) => continue,
                        };
                        yield Ok::<_, Infallible>(sse_event);
                    }
                    let _ = repo_clone.create_log(&crate::db::models::RequestLog {
                        id: crate::utils::id::new_id(),
                        api_key_id: Some(api_key_id_clone),
                        api_key_name: Some(api_key_name_clone),
                        channel_id: Some(channel_id),
                        channel_name: Some(channel_name),
                        model: model_clone.clone(),
                        upstream_model: Some(model_clone),
                        mode: "chat".to_string(),
                        status_code: 200,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                        duration_ms: start.elapsed().as_millis() as i64,
                        error_message: None,
                        is_stream: 1,
                        is_retry,
                        created_at: crate::utils::time::now_iso(),
                    }).await;
                };

                return Sse::new(log_stream).into_response();
            }
            Err(e) => {
                let error_message = e.to_string();
                let _ = repo.create_log(&crate::db::models::RequestLog {
                    id: crate::utils::id::new_id(),
                    api_key_id: Some(api_key_id.clone()),
                    api_key_name: Some(api_key_name.clone()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(model.clone()),
                    mode: "chat".to_string(),
                    status_code: 502,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    duration_ms: 0,
                    error_message: Some(error_message.clone()),
                    is_stream: 1,
                    is_retry: if attempt > 0 { 1 } else { 0 },
                    created_at: crate::utils::time::now_iso(),
                }).await;
                last_error = Some(format!("{}: {}", channel.name, error_message));
            }
        }
    }

    let err_body = serde_json::json!({
        "error": {
            "message": format!(
                "All stream channels failed for model {}: {}",
                model,
                last_error.unwrap_or_else(|| "unknown upstream error".to_string())
            ),
            "type": "upstream_error"
        }
    });
    (StatusCode::BAD_GATEWAY, Json(err_body)).into_response()
}

pub async fn handle_completions(State(_shared): State<SharedState>) -> Response {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented yet").into_response()
}

pub async fn handle_embeddings(State(_shared): State<SharedState>) -> Response {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented yet").into_response()
}

pub async fn handle_list_models(State(shared): State<SharedState>) -> Response {
    let repo = Repository::new(shared.state.db.pool.clone());
    match repo.get_enabled_channels().await {
        Ok(channels) => {
            let mut models: Vec<serde_json::Value> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for ch in &channels {
                let ch_models: Vec<String> = serde_json::from_str(&ch.models).unwrap_or_default();
                for m in ch_models {
                    if seen.insert(m.clone()) {
                        models.push(serde_json::json!({
                            "id": m, "object": "model",
                            "created": chrono::Utc::now().timestamp(),
                            "owned_by": ch.channel_type,
                        }));
                    }
                }
            }
            Json(serde_json::json!({ "object": "list", "data": models })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    }
}

pub async fn handle_images(State(_shared): State<SharedState>) -> Response {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented yet").into_response()
}

pub async fn handle_audio_transcriptions(State(_shared): State<SharedState>) -> Response {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented yet").into_response()
}

pub async fn handle_audio_speech(State(_shared): State<SharedState>) -> Response {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented yet").into_response()
}

pub async fn handle_health(State(shared): State<SharedState>) -> Response {
    let port = shared.state.server_port.read().await.clone();
    let running = shared.state.server_running.load(std::sync::atomic::Ordering::SeqCst);
    Json(serde_json::json!({
        "status": "ok",
        "running": running,
        "port": port,
        "url": format!("http://127.0.0.1:{}", port),
    })).into_response()
}
