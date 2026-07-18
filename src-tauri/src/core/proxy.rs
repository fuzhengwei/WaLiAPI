use crate::adaptor::{get_adaptor, ProxyRequest, TokenUsage};
use crate::core::dispatcher::Dispatcher;
use crate::db::models::{Channel, RequestLog};
use crate::db::repository::Repository;
use crate::utils;
use std::sync::Arc;
use std::time::Instant;

pub struct ProxyResult {
    pub status: u16,
    pub body: serde_json::Value,
    pub usage: Option<TokenUsage>,
    pub channel: Channel,
    pub duration_ms: u64,
}

pub async fn handle_request(
    repo: &Arc<Repository>,
    api_key_id: &str,
    api_key_name: &str,
    body: serde_json::Value,
    is_stream: bool,
) -> Result<ProxyResult, (u16, String)> {
    let start = Instant::now();
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();

    let channels = repo.get_enabled_channels().await.map_err(|e| (500, format!("DB error: {}", e)))?;
    if channels.is_empty() {
        return Err((503, "No available channels".to_string()));
    }

    let selected_channels = Dispatcher::select_channels(&channels, &model);
    if selected_channels.is_empty() {
        return Err((503, format!("No channel available for model: {}", model)));
    }

    let request = ProxyRequest {
        model: model.clone(),
        body: body.clone(),
        stream: is_stream,
    };

    let mut last_error = None;

    for (attempt, channel) in selected_channels.into_iter().enumerate() {
        let config = Dispatcher::channel_to_config(&channel);
        let adaptor = get_adaptor(&channel.channel_type);
        let attempt_start = Instant::now();
        let result = adaptor.forward(&request, &config).await;
        let duration_ms = attempt_start.elapsed().as_millis() as u64;
        let is_retry = if attempt > 0 { 1 } else { 0 };

        match result {
            Ok((status, resp_body, usage)) => {
                let log = RequestLog {
                    id: utils::id::new_id(),
                    api_key_id: Some(api_key_id.to_string()),
                    api_key_name: Some(api_key_name.to_string()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(model.clone()),
                    mode: "chat".to_string(),
                    status_code: status as i64,
                    prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens as i64).unwrap_or(0),
                    completion_tokens: usage.as_ref().map(|u| u.completion_tokens as i64).unwrap_or(0),
                    total_tokens: usage.as_ref().map(|u| u.total_tokens as i64).unwrap_or(0),
                    duration_ms: duration_ms as i64,
                    error_message: None,
                    is_stream: if is_stream { 1 } else { 0 },
                    is_retry,
                    created_at: utils::time::now_iso(),
                };
                let _ = repo.create_log(&log).await;

                if let Some(ref u) = usage {
                    let _ = repo.increment_quota(api_key_id, u.total_tokens as i64).await;
                }

                return Ok(ProxyResult {
                    status,
                    body: resp_body,
                    usage,
                    channel,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            Err(e) => {
                let error_message = e.to_string();
                let log = RequestLog {
                    id: utils::id::new_id(),
                    api_key_id: Some(api_key_id.to_string()),
                    api_key_name: Some(api_key_name.to_string()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(model.clone()),
                    mode: "chat".to_string(),
                    status_code: 502,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    duration_ms: duration_ms as i64,
                    error_message: Some(error_message.clone()),
                    is_stream: if is_stream { 1 } else { 0 },
                    is_retry,
                    created_at: utils::time::now_iso(),
                };
                let _ = repo.create_log(&log).await;
                last_error = Some(error_message);
            }
        }
    }

    Err((
        502,
        format!(
            "All channels failed for model {}: {}",
            model,
            last_error.unwrap_or_else(|| "unknown upstream error".to_string())
        ),
    ))
}
