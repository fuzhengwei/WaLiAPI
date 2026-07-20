use crate::adaptor::{get_adaptor, ProxyRequest, TokenUsage};
use crate::core::dispatcher::Dispatcher;
use crate::db::models::{Channel, RequestLog};
use crate::db::repository::Repository;
use crate::utils;
use crate::security;
use std::sync::Arc;
use std::time::Instant;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub struct ProxyResult {
    pub status: u16,
    pub body: serde_json::Value,
    pub usage: Option<TokenUsage>,
    pub channel: Channel,
    pub duration_ms: u64,
}

pub async fn handle_request(
    repo: &Arc<Repository>,
    app: &AppHandle,
    api_key_id: &str,
    api_key_name: &str,
    body: serde_json::Value,
    is_stream: bool,
    request_body: Option<String>,
) -> Result<ProxyResult, (u16, String)> {
    let start = Instant::now();
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let security_settings = security::get_security_settings(app);
    let security_result = security::scan_request(&body, &security_settings);

    // Real redaction: if redact mode is active, sanitize the request body before forwarding
    let (forward_body, was_redacted) = if matches!(security_result.action, security::SecurityAction::Redact) || security_settings.redact_secrets {
        security::redact_request_body(&body, &security_settings)
    } else {
        (body.clone(), false)
    };
    let mut security_result = security_result;
    if was_redacted {
        security_result.sanitized = true;
    }

    if matches!(security_result.action, security::SecurityAction::Block) {
        let log = RequestLog {
            id: utils::id::new_id(),
            seq: None,
            api_key_id: Some(api_key_id.to_string()),
            api_key_name: Some(api_key_name.to_string()),
            channel_id: None,
            channel_name: None,
            model: model.clone(),
            upstream_model: None,
            mode: "chat".to_string(),
            status_code: 451,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            duration_ms: start.elapsed().as_millis() as i64,
            error_message: security_result.blocked_reason.clone(),
            is_stream: if is_stream { 1 } else { 0 },
            is_retry: 0,
            created_at: utils::time::now_iso(),
            request_body: request_body.clone(),
            risk_level: security_result.risk_level.as_str().to_string(),
            risk_score: security_result.risk_score as i64,
            risk_summary: Some(security_result.summary.clone()),
            security_action: security_result.action.as_str().to_string(),
            sanitized: if security_result.sanitized { 1 } else { 0 },
            blocked_reason: security_result.blocked_reason.clone(),
        };
        let log_id = log.id.clone();
        let _ = repo.create_log(&log).await;
        let _ = repo.create_security_findings(&log_id, &security_result.findings, security_result.action.as_str()).await;
        return Err((451, security_result.summary));
    }

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
        body: forward_body.clone(),
        stream: is_stream,
    };

    let (retry_enabled, retry_times) = get_retry_settings(app);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(selected_channels.len())
    } else {
        1
    };

    let mut last_error = None;

    for (attempt, channel) in selected_channels.into_iter().take(max_attempts).enumerate() {
        let config = Dispatcher::channel_to_config(&channel);
        let adaptor = get_adaptor(&channel.channel_type);
        let attempt_start = Instant::now();
        let result = adaptor.forward(&request, &config).await;
        let duration_ms = attempt_start.elapsed().as_millis() as u64;
        let is_retry = if attempt > 0 { 1 } else { 0 };

        // Compute the actual upstream model after mapping
        let upstream_model = {
            let mapping = &config.model_mapping;
            if let Some(mapped) = mapping.get(model.as_str()).and_then(|v| v.as_str()) {
                mapped.to_string()
            } else {
                model.clone()
            }
        };

        match result {
            Ok((status, resp_body, usage)) => {
                // Scan response for risks
                let resp_security = security::scan_response(&resp_body, &security_settings);
                let resp_findings_count = resp_security.findings.len();
                if resp_findings_count > 0 {
                    // Merge response findings into request findings for logging
                    security_result.findings.extend(resp_security.findings);
                    if resp_security.risk_level.rank() > security_result.risk_level.rank() {
                        security_result.risk_level = resp_security.risk_level;
                        security_result.risk_score = security_result.risk_score.max(resp_security.risk_score);
                        security_result.summary = format!("{} | 响应侧: {}", security_result.summary, resp_security.summary);
                    }
                }

                let log = RequestLog {
                    id: utils::id::new_id(),
                    seq: None,
                    api_key_id: Some(api_key_id.to_string()),
                    api_key_name: Some(api_key_name.to_string()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(upstream_model.clone()),
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
                    request_body: request_body.clone(),
                    risk_level: security_result.risk_level.as_str().to_string(),
                    risk_score: security_result.risk_score as i64,
                    risk_summary: Some(security_result.summary.clone()),
                    security_action: security_result.action.as_str().to_string(),
                    sanitized: if security_result.sanitized { 1 } else { 0 },
                    blocked_reason: security_result.blocked_reason.clone(),
                };
                let log_id = log.id.clone();
                let _ = repo.create_log(&log).await;
                let _ = repo.create_security_findings(&log_id, &security_result.findings, security_result.action.as_str()).await;

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
                    seq: None,
                    api_key_id: Some(api_key_id.to_string()),
                    api_key_name: Some(api_key_name.to_string()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(upstream_model.clone()),
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
                    request_body: request_body.clone(),
                    risk_level: security_result.risk_level.as_str().to_string(),
                    risk_score: security_result.risk_score as i64,
                    risk_summary: Some(security_result.summary.clone()),
                    security_action: security_result.action.as_str().to_string(),
                    sanitized: if security_result.sanitized { 1 } else { 0 },
                    blocked_reason: security_result.blocked_reason.clone(),
                };
                let log_id = log.id.clone();
                let _ = repo.create_log(&log).await;
                let _ = repo.create_security_findings(&log_id, &security_result.findings, security_result.action.as_str()).await;
                last_error = Some(error_message);
            }
        }
    }

    Err((
        502,
        format!(
            "All channels failed for model {} after {} attempt(s): {}",
            model,
            max_attempts,
            last_error.unwrap_or_else(|| "unknown upstream error".to_string())
        ),
    ))
}

pub fn get_retry_settings(app: &AppHandle) -> (bool, i32) {
    if let Ok(store) = app.store("settings.json") {
        let enabled = store
            .get("retry.enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let times = store
            .get("retry.times")
            .and_then(|v| v.as_i64())
            .unwrap_or(2) as i32;
        return (enabled, times);
    }
    (true, 2)
}
