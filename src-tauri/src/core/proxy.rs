use crate::adaptor::{get_adaptor, ProxyRequest, TokenUsage};
use crate::core::attempt::{upstream_failover_decision, FailoverDecision};
use crate::core::dispatcher::Dispatcher;
use crate::db::models::{Channel, RequestLog};
use crate::db::repository::Repository;
use crate::security;
use crate::settings_store::SettingsStore;
use crate::utils;
use rand::Rng;
use std::sync::Arc;
use std::time::Instant;

/// Multi-key load balancing for the legacy proxy path.  Selects a random
/// enabled key from the channel's extra keys, weighted by `weight`.  The
/// primary `api_key` participates with the channel-level `weight`.
async fn select_key_for_channel(channel: &Channel, repo: &Arc<Repository>) -> Channel {
    let extra_keys = match repo.get_channel_api_keys(&channel.id).await {
        Ok(keys) => keys
            .into_iter()
            .filter(|k| k.status == 1)
            .collect::<Vec<_>>(),
        Err(_) => return channel.clone(),
    };
    if extra_keys.is_empty() {
        return channel.clone();
    }
    let mut pool: Vec<(String, i64)> = Vec::new();
    if !channel.api_key.is_empty() {
        pool.push((channel.api_key.clone(), channel.weight.max(1)));
    }
    for k in &extra_keys {
        pool.push((k.api_key.clone(), k.weight.max(1)));
    }
    if pool.is_empty() {
        return channel.clone();
    }
    let total: i64 = pool.iter().map(|(_, w)| w).sum();
    if total <= 0 {
        return channel.clone();
    }
    let mut pick = rand::rng().random_range(0..total);
    let mut chosen = &pool[0].0;
    for (key, w) in &pool {
        pick -= w;
        if pick <= 0 {
            chosen = key;
            break;
        }
    }
    let mut ch = channel.clone();
    ch.api_key = chosen.clone();
    ch
}

#[allow(dead_code)]
pub struct ProxyResult {
    pub status: u16,
    pub body: serde_json::Value,
    pub usage: Option<TokenUsage>,
    pub channel: Channel,
    pub duration_ms: u64,
}

pub async fn handle_request(
    repo: &Arc<Repository>,
    settings: &SettingsStore,
    api_key_id: &str,
    api_key_name: &str,
    body: serde_json::Value,
    is_stream: bool,
    sanitized_request_body: Option<String>,
    trace_id: Option<String>,
    // Authoritative security audit from the gate (audited the ORIGINAL
    // protocol JSON full tree).  When `Some`, the body is NOT re-scanned here:
    // the gate's verdict is used for log fields and the safety-net block.
    // `None` falls back to an internal scan and is used only by legacy paths
    // (e.g. RAG) that do not pass the gate yet.
    audit: Option<&security::SecurityScanResult>,
) -> Result<ProxyResult, (u16, String)> {
    let start: Instant = Instant::now();
    let model: String = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let security_settings = security::get_security_settings(settings);
    let custom_rules = if security_settings.enabled {
        security::rules::CustomRuleRepository::get_enabled(repo.pool())
            .await
            .unwrap_or_default()
    } else {
        vec![]
    };
    // The gate already audited the ORIGINAL protocol JSON at the handler.
    // Re-scanning the (possibly converted) Chat JSON here would be a redundant,
    // competing non-authoritative audit — only do that when the caller had no
    // gate (legacy RAG path).
    let mut security_result = match audit {
        Some(result) => result.clone(),
        None => security::scan_request(&body, &security_settings, &custom_rules),
    };

    // Real redaction: if redact mode is active, sanitize the request body before forwarding
    let (forward_body, was_redacted) =
        if matches!(security_result.action, security::SecurityAction::Redact)
            || security_settings.redact_secrets
        {
            security::redact_request_body(&body, &security_settings)
        } else {
            (body.clone(), false)
        };
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
            request_body: sanitized_request_body.clone(),
            response_choices: None,
            risk_level: security_result.risk_level.as_str().to_string(),
            risk_score: security_result.risk_score as i64,
            risk_summary: Some(security_result.summary.clone()),
            security_action: security_result.action.as_str().to_string(),
            sanitized: if security_result.sanitized { 1 } else { 0 },
            blocked_reason: security_result.blocked_reason.clone(),
            trace_id: trace_id.clone(),
            // T09: blocked log — no route/upstream context (channel unknown).
            ..Default::default()
        };
        let log_id = log.id.clone();
        if let Err(e) = repo.create_log(&log).await {
            eprintln!("[WARN] create_log failed: {}", e);
        }
        if let Err(e) = repo
            .create_security_findings(
                &log_id,
                &security_result.findings,
                security_result.action.as_str(),
            )
            .await
        {
            eprintln!("[WARN] create_security_findings failed: {}", e);
        }
        return Err((451, security_result.summary));
    }

    let channels = repo
        .get_enabled_channels()
        .await
        .map_err(|e| (500, format!("DB error: {}", e)))?;
    if channels.is_empty() {
        return Err((503, "No available channels".to_string()));
    }

    let selected_channels = Dispatcher::select_channels(&channels, &model);
    if selected_channels.is_empty() {
        return Err((503, format!("No channel available for model: {}", model)));
    }

    let (retry_enabled, retry_times) = get_retry_settings(settings);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(selected_channels.len())
    } else {
        1
    };

    let mut last_error = None;

    for (attempt, channel) in selected_channels.into_iter().take(max_attempts).enumerate() {
        // Multi-key load balancing: select a key from extra keys if available.
        let channel = select_key_for_channel(&channel, &repo).await;
        let config = Dispatcher::channel_to_config(&channel);
        let adaptor = get_adaptor(&channel.channel_type);
        let attempt_start = Instant::now();

        // T05: array model-mapping sampling moved OUT of this loop into the
        // shared planner helper.  It is resolved EXACTLY ONCE per attempt and
        // pre-baked into the forwarded body, so the actual request model, the
        // adaptor's own apply_model_mapping (now a no-op) and the log all share
        // the same upstream_model (design 11.4).
        // ThreadRng is not Send, so scope it tightly: it must be dropped before
        // the awaited upstream call below.
        let upstream_model = {
            let mut rng = rand::rng();
            crate::core::route_plan::resolve_upstream_model(&config.model_mapping, &model, &mut rng)
        };
        let mut attempt_body = forward_body.clone();
        if let Some(obj) = attempt_body.as_object_mut() {
            obj.insert(
                "model".into(),
                serde_json::Value::String(upstream_model.clone()),
            );
        }
        let request = ProxyRequest {
            model: model.clone(),
            body: attempt_body,
            stream: is_stream,
        };

        let result = adaptor.forward(&request, &config).await;
        let duration_ms = attempt_start.elapsed().as_millis() as u64;
        let is_retry = if attempt > 0 { 1 } else { 0 };

        match result {
            Ok((status, resp_body, usage)) => {
                // P0 fix: check HTTP status code — non-success codes should trigger failover
                if status >= 400 {
                    let error_message = format!("{}: HTTP {}", channel.name, status);
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
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                        duration_ms: duration_ms as i64,
                        error_message: Some(error_message.clone()),
                        is_stream: if is_stream { 1 } else { 0 },
                        is_retry,
                        created_at: utils::time::now_iso(),
                        request_body: sanitized_request_body.clone(),
                        response_choices: None,
                        risk_level: security_result.risk_level.as_str().to_string(),
                        risk_score: security_result.risk_score as i64,
                        risk_summary: Some(security_result.summary.clone()),
                        security_action: security_result.action.as_str().to_string(),
                        sanitized: if security_result.sanitized { 1 } else { 0 },
                        blocked_reason: security_result.blocked_reason.clone(),
                        trace_id: trace_id.clone(),
                        // T09: observability fields on the facade path (T06).
                        ..Default::default()
                    };
                    let log_id = log.id.clone();
                    if let Err(e) = repo.create_log(&log).await {
                        eprintln!("[WARN] create_log failed: {}", e);
                    }
                    if let Err(e) = repo
                        .create_security_findings(
                            &log_id,
                            &security_result.findings,
                            security_result.action.as_str(),
                        )
                        .await
                    {
                        eprintln!("[WARN] create_security_findings failed: {}", e);
                    }
                    last_error = Some(error_message);
                    // Terminal upstream status: the same request would fail
                    // on every other channel too, so stop cycling and answer
                    // with the decision's downstream status (an upstream
                    // 401/403 is masked to 502 — the log above keeps the
                    // real status).
                    match upstream_failover_decision(status) {
                        FailoverDecision::Failover => continue,
                        FailoverDecision::Stop { downstream_status } => {
                            return Ok(ProxyResult {
                                status: downstream_status,
                                body: resp_body,
                                usage: None,
                                channel,
                                duration_ms: duration_ms as u64,
                            });
                        }
                    }
                }

                // Extract and log choices
                let response_choices = resp_body
                    .get("choices")
                    .and_then(|c| serde_json::to_string(c).ok());
                if response_choices.is_some() {
                    // choices logging disabled
                }

                // Scan response for risks
                let resp_security =
                    security::scan_response(&resp_body, &security_settings, &custom_rules);
                let resp_findings_count = resp_security.findings.len();
                if resp_findings_count > 0 {
                    // Merge response findings into request findings for logging
                    security_result.findings.extend(resp_security.findings);
                    if resp_security.risk_level.rank() > security_result.risk_level.rank() {
                        security_result.risk_level = resp_security.risk_level;
                        security_result.risk_score =
                            security_result.risk_score.max(resp_security.risk_score);
                        security_result.summary = format!(
                            "{} | 响应侧: {}",
                            security_result.summary, resp_security.summary
                        );
                    }
                }

                let (prompt_tokens, completion_tokens, total_tokens) = {
                    let (p, c, t) = (
                        usage.as_ref().map(|u| u.prompt_tokens as i64).unwrap_or(0),
                        usage
                            .as_ref()
                            .map(|u| u.completion_tokens as i64)
                            .unwrap_or(0),
                        usage.as_ref().map(|u| u.total_tokens as i64).unwrap_or(0),
                    );
                    // Fallback: estimate tokens when upstream didn't return usage.
                    if t == 0 && p == 0 && c == 0 && status >= 200 && status < 300 {
                        let req_body: serde_json::Value =
                            serde_json::from_str(sanitized_request_body.as_deref().unwrap_or("{}"))
                                .unwrap_or(serde_json::Value::Null);
                        let resp_text = response_choices.as_deref().unwrap_or("");
                        let (ep, ec, et) = crate::endpoint_executor::estimate_usage::estimate_usage(
                            &req_body,
                            Some(resp_text),
                            &model,
                        );
                        if et > 0 {
                            eprintln!("[INFO] token usage estimated (proxy.rs): prompt={}, completion={}, total={}", ep, ec, et);
                            (ep, ec, et)
                        } else {
                            (p, c, t)
                        }
                    } else {
                        (p, c, t)
                    }
                };

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
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    duration_ms: duration_ms as i64,
                    error_message: None,
                    is_stream: if is_stream { 1 } else { 0 },
                    is_retry,
                    created_at: utils::time::now_iso(),
                    request_body: sanitized_request_body.clone(),
                    response_choices: response_choices.clone(),
                    risk_level: security_result.risk_level.as_str().to_string(),
                    risk_score: security_result.risk_score as i64,
                    risk_summary: Some(security_result.summary.clone()),
                    security_action: security_result.action.as_str().to_string(),
                    sanitized: if security_result.sanitized { 1 } else { 0 },
                    blocked_reason: security_result.blocked_reason.clone(),
                    trace_id: trace_id.clone(),
                    // T09: observability fields (route_group / upstream
                    // protocol/endpoint / provider / codec / failure class /
                    // identity revision) are populated on the facade path (T06);
                    // this legacy loop keeps them NULL for now.
                    ..Default::default()
                };
                let log_id = log.id.clone();
                if let Err(e) = repo.create_log(&log).await {
                    eprintln!("[WARN] create_log failed: {}", e);
                }
                if let Err(e) = repo
                    .create_security_findings(
                        &log_id,
                        &security_result.findings,
                        security_result.action.as_str(),
                    )
                    .await
                {
                    eprintln!("[WARN] create_security_findings failed: {}", e);
                }

                if let Some(ref u) = usage {
                    if let Err(e) = repo
                        .increment_quota(api_key_id, u.total_tokens as i64)
                        .await
                    {
                        eprintln!("[WARN] increment_quota failed: {}", e);
                    }
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
                    request_body: sanitized_request_body.clone(),
                    response_choices: None,
                    risk_level: security_result.risk_level.as_str().to_string(),
                    risk_score: security_result.risk_score as i64,
                    risk_summary: Some(security_result.summary.clone()),
                    security_action: security_result.action.as_str().to_string(),
                    sanitized: if security_result.sanitized { 1 } else { 0 },
                    blocked_reason: security_result.blocked_reason.clone(),
                    trace_id: trace_id.clone(),
                    // T09: observability fields (route_group / upstream
                    // protocol/endpoint / provider / codec / failure class /
                    // identity revision) are populated on the facade path (T06);
                    // this legacy loop keeps them NULL for now.
                    ..Default::default()
                };
                let log_id = log.id.clone();
                if let Err(e) = repo.create_log(&log).await {
                    eprintln!("[WARN] create_log failed: {}", e);
                }
                if let Err(e) = repo
                    .create_security_findings(
                        &log_id,
                        &security_result.findings,
                        security_result.action.as_str(),
                    )
                    .await
                {
                    eprintln!("[WARN] create_security_findings failed: {}", e);
                }
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

pub fn get_retry_settings(settings: &SettingsStore) -> (bool, i32) {
    let enabled = settings.get_bool("retry.enabled", true);
    let times = settings.get_u64("retry.times", 2) as i32;
    (enabled, times)
}
