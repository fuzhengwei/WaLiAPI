use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Json, IntoResponse, Response},
};
use futures_util::StreamExt;
use super::router::SharedState;
use crate::core::proxy;
use crate::db::repository::Repository;
use crate::adaptor::{get_adaptor, ProxyRequest};
use crate::core::dispatcher::Dispatcher;
use crate::security;

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

    // Extract Wali-Trace-Id from request headers
    let trace_id = headers.get("Wali-Trace-Id").and_then(|h| h.to_str().ok()).map(|s| s.to_string());

    // Store full request body for logging (no truncation — let frontend handle display)
    let request_body_str = serde_json::to_string(&json).unwrap_or_default();

    if is_stream {
        handle_stream(shared, json, key_record.id, key_record.name, request_body_str, trace_id).await
    } else {
        match proxy::handle_request(&repo, &shared.app, &key_record.id, &key_record.name, json, false, Some(request_body_str), trace_id).await {
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

/// Parse token usage from an SSE chunk's data line.
/// Looks for `usage` field in the JSON payload of `data: {...}` lines.
fn parse_usage_from_chunk(text: &str) -> Option<(i64, i64, i64)> {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data_str = trimmed.trim_start_matches("data:").trim();
        if data_str == "[DONE]" || data_str.is_empty() {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) {
            if let Some(usage) = json.get("usage") {
                let prompt = usage.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                let completion = usage.get("completion_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                let total = usage.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                if total > 0 || prompt > 0 || completion > 0 {
                    return Some((prompt, completion, total));
                }
            }
        }
    }
    None
}

async fn handle_stream(
    shared: SharedState,
    json: serde_json::Value,
    api_key_id: String,
    api_key_name: String,
    request_body: String,
    trace_id: Option<String>,
) -> Response {
    let model = json.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let security_settings = security::get_security_settings(&shared.app);
    let security_result = security::scan_request(&json, &security_settings);

    // Real redaction: if redact mode is active, sanitize the request body before forwarding
    let (forward_json, was_redacted) = if matches!(security_result.action, security::SecurityAction::Redact) || security_settings.redact_secrets {
        security::redact_request_body(&json, &security_settings)
    } else {
        (json.clone(), false)
    };
    let mut security_result = security_result;
    if was_redacted {
        security_result.sanitized = true;
    }

    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));

    if matches!(security_result.action, security::SecurityAction::Block) {
        let log = crate::db::models::RequestLog {
            response_choices: None,
            id: crate::utils::id::new_id(),
            seq: None,
            api_key_id: Some(api_key_id),
            api_key_name: Some(api_key_name),
            channel_id: None,
            channel_name: None,
            model: model.clone(),
            upstream_model: None,
            mode: "chat".to_string(),
            status_code: 451,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            duration_ms: 0,
            error_message: security_result.blocked_reason.clone(),
            is_stream: 1,
            is_retry: 0,
            created_at: crate::utils::time::now_iso(),
            request_body: Some(request_body),
            risk_level: security_result.risk_level.as_str().to_string(),
            risk_score: security_result.risk_score as i64,
            risk_summary: Some(security_result.summary.clone()),
            security_action: security_result.action.as_str().to_string(),
            sanitized: if security_result.sanitized { 1 } else { 0 },
            blocked_reason: security_result.blocked_reason.clone(),
            trace_id: trace_id.clone(),
        };
        let log_id = log.id.clone();
        let _ = repo.create_log(&log).await;
        let _ = repo.create_security_findings(&log_id, &security_result.findings, security_result.action.as_str()).await;
        let err_body = serde_json::json!({"error": {"message": security_result.summary, "type": "security_blocked", "code": "security.blocked"}});
        return (StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS, Json(err_body)).into_response();
    }
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
        body: forward_json.clone(),
        stream: true,
    };

    let (retry_enabled, retry_times) = proxy::get_retry_settings(&shared.app);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(selected_channels.len())
    } else {
        1
    };

    let mut last_error = None;

    for (attempt, channel) in selected_channels.into_iter().take(max_attempts).enumerate() {
        let config = Dispatcher::channel_to_config(&channel);
        let adaptor = get_adaptor(&channel.channel_type);

        // Compute the actual upstream model after mapping
        let upstream_model = {
            let mapping = &config.model_mapping;
            if let Some(mapped) = mapping.get(model.as_str()).and_then(|v| v.as_str()) {
                mapped.to_string()
            } else {
                model.clone()
            }
        };

        match adaptor.forward_stream(&request, &config).await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body_str = resp.text().await.unwrap_or_default();
                    last_error = Some(format!("{}: {}", channel.name, body_str));
                    continue;
                }

                let start = std::time::Instant::now();
                let channel_id = channel.id.clone();
                let channel_name = channel.name.clone();
                let repo_clone = repo.clone();
                let api_key_id_clone = api_key_id.clone();
                let api_key_name_clone = api_key_name.clone();
                let model_clone = model.clone();
                let upstream_model_clone = upstream_model.clone();
                let request_body_clone = request_body.clone();
                let security_result_clone = security_result.clone();
                let trace_id_clone = trace_id.clone();
                let is_retry = if attempt > 0 { 1 } else { 0 };

                // ── Raw byte passthrough with usage parsing ───────────────
                // Forward upstream SSE bytes directly as the response body.
                // While passing through, scan data lines for `usage` to record
                // token consumption in the log.
                let upstream_stream = resp.bytes_stream();

                let passthrough_stream = async_stream::stream! {
                    tokio::pin!(upstream_stream);

                    // Accumulate token usage and response content from SSE chunks
                    let mut usage_prompt: i64 = 0;
                    let mut usage_completion: i64 = 0;
                    let mut usage_total: i64 = 0;
                    let mut had_error = false;
                    let mut accumulated_content = String::new();
                    let mut accumulated_reasoning = String::new();
                    let mut response_role: Option<String> = None;
                    let mut finish_reason: Option<String> = None;
                    // Accumulate tool_calls by index (streaming chunks may contain partial tool_calls)
                    let mut tool_calls_map: std::collections::BTreeMap<i64, serde_json::Value> = std::collections::BTreeMap::new();

                    while let Some(chunk_result) = upstream_stream.next().await {
                        match chunk_result {
                            Ok(bytes) => {
                                // Try to parse usage and content from this chunk
                                if let Ok(text) = std::str::from_utf8(&bytes) {
                                    if let Some((p, c, t)) = parse_usage_from_chunk(text) {
                                        usage_prompt = p;
                                        usage_completion = c;
                                        usage_total = t;
                                    }
                                    // Accumulate delta content from SSE chunks
                                    for line in text.lines() {
                                        let trimmed = line.trim();
                                        if !trimmed.starts_with("data:") {
                                            continue;
                                        }
                                        let data_str = trimmed.trim_start_matches("data:").trim();
                                        if data_str == "[DONE]" || data_str.is_empty() {
                                            continue;
                                        }
                                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(data_str) {
                                            if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                                                if let Some(choice) = choices.first() {
                                                    if let Some(delta) = choice.get("delta") {
                                                        // Accumulate regular content
                                                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                                                            accumulated_content.push_str(content);
                                                        }
                                                        // Accumulate reasoning/thinking content (DeepSeek R1, OpenAI o1/o3, etc.)
                                                        if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
                                                            accumulated_reasoning.push_str(reasoning);
                                                        }
                                                        if response_role.is_none() {
                                                            if let Some(role) = delta.get("role").and_then(|r| r.as_str()) {
                                                                response_role = Some(role.to_string());
                                                            }
                                                        }
                                                        // Accumulate tool_calls by index
                                                        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                                                            for tc in tcs {
                                                                let idx = tc.get("index").and_then(|i| i.as_i64()).unwrap_or(0);
                                                                let entry = tool_calls_map.entry(idx).or_insert_with(|| {
                                                                    serde_json::json!({
                                                                        "id": "",
                                                                        "type": "function",
                                                                        "function": {
                                                                            "name": "",
                                                                            "arguments": ""
                                                                        }
                                                                    })
                                                                });
                                                                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                                                    if !id.is_empty() {
                                                                        entry["id"] = serde_json::json!(id);
                                                                    }
                                                                }
                                                                if let Some(t) = tc.get("type").and_then(|v| v.as_str()) {
                                                                    if !t.is_empty() {
                                                                        entry["type"] = serde_json::json!(t);
                                                                    }
                                                                }
                                                                if let Some(func) = tc.get("function") {
                                                                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                                                        if !name.is_empty() {
                                                                            entry["function"]["name"] = serde_json::json!(name);
                                                                        }
                                                                    }
                                                                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                                                        let existing = entry["function"]["arguments"].as_str().unwrap_or("");
                                                                        entry["function"]["arguments"] = serde_json::json!(format!("{}{}", existing, args));
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    if finish_reason.is_none() {
                                                        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                                                            if !reason.is_empty() && reason != "null" {
                                                                finish_reason = Some(reason.to_string());
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                yield Ok::<_, std::io::Error>(bytes);
                            }
                            Err(e) => {
                                had_error = true;
                                let err_chunk = format!(
                                    "data: {{\"error\":{{\"message\":\"Stream connection interrupted: {}\",\"type\":\"server_error\"}}}}\n\n",
                                    e
                                );
                                yield Ok::<_, std::io::Error>(err_chunk.into_bytes().into());
                                yield Ok::<_, std::io::Error>(b"data: [DONE]\n\n".to_vec().into());
                                break;
                            }
                        }
                    }

                    // Build response_choices from accumulated streaming content
                    let has_content = !accumulated_content.is_empty() || !accumulated_reasoning.is_empty() || !tool_calls_map.is_empty();
                    let response_choices = if has_content {
                        let mut message = serde_json::json!({
                            "role": response_role.unwrap_or_else(|| "assistant".to_string()),
                        });
                        // Only include content if there is any
                        if !accumulated_content.is_empty() {
                            message["content"] = serde_json::json!(accumulated_content);
                        }
                        // Include reasoning_content if present
                        if !accumulated_reasoning.is_empty() {
                            message["reasoning_content"] = serde_json::json!(accumulated_reasoning);
                        }
                        // Include tool_calls if present
                        if !tool_calls_map.is_empty() {
                            let tcs: Vec<serde_json::Value> = tool_calls_map.into_values().collect();
                            message["tool_calls"] = serde_json::json!(tcs);
                        }
                        Some(serde_json::to_string(&vec![serde_json::json!({
                            "index": 0,
                            "message": message,
                            "finish_reason": finish_reason,
                        })]).unwrap_or_default())
                    } else {
                        None
                    };

                    // Log after stream completes
                    let quota_to_add = usage_total;
                    let key_id_for_quota = api_key_id_clone.clone();
                    let log = crate::db::models::RequestLog {
                        id: crate::utils::id::new_id(),
                        seq: None,
                        api_key_id: Some(api_key_id_clone),
                        api_key_name: Some(api_key_name_clone),
                        channel_id: Some(channel_id),
                        channel_name: Some(channel_name),
                        model: model_clone.clone(),
                        upstream_model: Some(upstream_model_clone),
                        mode: "chat".to_string(),
                        status_code: if had_error { 502 } else { 200 },
                        prompt_tokens: usage_prompt,
                        completion_tokens: usage_completion,
                        total_tokens: usage_total,
                        duration_ms: start.elapsed().as_millis() as i64,
                        error_message: if had_error { Some("Stream interrupted".to_string()) } else { None },
                        is_stream: 1,
                        is_retry,
                        created_at: crate::utils::time::now_iso(),
                        request_body: Some(request_body_clone),
                        response_choices,
                        risk_level: security_result_clone.risk_level.as_str().to_string(),
                        risk_score: security_result_clone.risk_score as i64,
                        risk_summary: Some(security_result_clone.summary.clone()),
                        security_action: security_result_clone.action.as_str().to_string(),
                        sanitized: if security_result_clone.sanitized { 1 } else { 0 },
                        blocked_reason: security_result_clone.blocked_reason.clone(),
                        trace_id: trace_id_clone,
                    };
                    let log_id = log.id.clone();
                    let _ = repo_clone.create_log(&log).await;
                    let _ = repo_clone.create_security_findings(&log_id, &security_result_clone.findings, security_result_clone.action.as_str()).await;

                    // Increment quota if we got token counts
                    if quota_to_add > 0 {
                        let _ = repo_clone.increment_quota(&key_id_for_quota, quota_to_add).await;
                    }
                };

                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header(header::CACHE_CONTROL, "no-cache")
                    .header(header::CONNECTION, "keep-alive")
                    .body(Body::from_stream(passthrough_stream))
                    .unwrap();
            }
            Err(e) => {
                let error_message = e.to_string();
                let log = crate::db::models::RequestLog {
                    id: crate::utils::id::new_id(),
                    seq: None,
                    api_key_id: Some(api_key_id.clone()),
                    api_key_name: Some(api_key_name.clone()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(upstream_model.clone()),
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
                    request_body: Some(request_body.clone()),
                    response_choices: None,
                    risk_level: security_result.risk_level.as_str().to_string(),
                    risk_score: security_result.risk_score as i64,
                    risk_summary: Some(security_result.summary.clone()),
                    security_action: security_result.action.as_str().to_string(),
                    sanitized: if security_result.sanitized { 1 } else { 0 },
                    blocked_reason: security_result.blocked_reason.clone(),
                    trace_id: trace_id.clone(),
                };
                let log_id = log.id.clone();
                let _ = repo.create_log(&log).await;
                let _ = repo.create_security_findings(&log_id, &security_result.findings, security_result.action.as_str()).await;
                last_error = Some(format!("{}: {}", channel.name, error_message));
            }
        }
    }

    let err_body = serde_json::json!({
        "error": {
            "message": format!(
                "All stream channels failed for model {} after {} attempt(s): {}",
                model,
                max_attempts,
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
                // Also expose mapped model names (mapping keys)
                let mapping: serde_json::Value =
                    serde_json::from_str(&ch.model_mapping).unwrap_or(serde_json::Value::Object(Default::default()));
                if let Some(obj) = mapping.as_object() {
                    for key in obj.keys() {
                        if seen.insert(key.clone()) {
                            models.push(serde_json::json!({
                                "id": key, "object": "model",
                                "created": chrono::Utc::now().timestamp(),
                                "owned_by": ch.channel_type,
                            }));
                        }
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
