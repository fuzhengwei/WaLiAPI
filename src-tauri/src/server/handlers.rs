use super::router::SharedState;
use crate::adaptor::{get_adaptor, ProxyRequest};
use crate::core::attempt::{upstream_failover_decision, FailoverDecision};
use crate::core::dispatcher::Dispatcher;
use crate::core::feature_flags;
use crate::core::proxy;
use crate::core::route_plan::{self, EndpointKind};
use crate::db::repository::Repository;
use crate::protocol;
use crate::security;
use axum::{
    body::Body,
    extract::{OriginalUri, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use futures_util::StreamExt;
use rand::SeedableRng;
use sqlx::sqlite::SqlitePool;

/// A key lookup failure means "invalid key" only when the row is genuinely
/// absent (or disabled); any other database error is a storage failure and
/// must surface as 5xx, not as an authentication error.
fn is_key_lookup_storage_error(err: &sqlx::Error) -> bool {
    !matches!(err, sqlx::Error::RowNotFound)
}

/// Run the unified security audit gate against the ORIGINAL downstream
/// protocol JSON full tree (never a converted Chat JSON).  Returns
/// `Ok(AuditedRequest)` for audit-allow / redact, or a ready HTTP response
/// for fail-closed errors (Confirm / budget / parse / internal) so the caller
/// returns immediately and never contacts upstream.
///
/// `Response` is the natural fail-closed error type here (the caller returns it
/// verbatim); boxing it would ripple through every handler call site for no
/// measurable gain, so the lint is scoped.
#[allow(clippy::result_large_err)]
async fn audit_original(
    protocol: security::gate::DownstreamProtocol,
    endpoint: &str,
    original_json: serde_json::Value,
    query: Option<String>,
    trace_id: Option<String>,
    shared: &SharedState,
) -> Result<security::gate::AuditedRequest, Response> {
    let model = original_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let stream = original_json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    let settings = security::get_security_settings(&shared.state.settings);
    let custom_rules = if settings.enabled {
        security::rules::CustomRuleRepository::get_enabled(&shared.state.db.pool)
            .await
            .unwrap_or_default()
    } else {
        vec![]
    };
    match security::gate::gate_original(
        protocol,
        endpoint,
        original_json,
        query,
        model,
        stream,
        trace_id,
        &settings,
        None,
        custom_rules,
    ) {
        Ok(audited) => Ok(audited),
        Err(security::gate::SecurityGateError::ApprovalRequired { message }) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": { "message": message, "type": "approval_required", "code": "approval_required" }
            })),
        )
            .into_response()),
        Err(security::gate::SecurityGateError::BudgetExceeded { message }) => Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": { "message": message, "type": "security_scan_budget_exceeded", "code": "security_scan_budget_exceeded" }
            })),
        )
            .into_response()),
        Err(security::gate::SecurityGateError::ParseFailed { message }) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": { "message": message, "type": "invalid_request_error", "code": "invalid_request_error" }
            })),
        )
            .into_response()),
        Err(security::gate::SecurityGateError::Internal { message }) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": { "message": message, "type": "api_error", "code": "api_error" }
            })),
        )
            .into_response()),
    }
}

/// T06 integration point: run the model-first RoutePlan facade for ANY routed
/// endpoint (Chat / Responses / Messages / CountTokens / Embeddings) on BOTH
/// stream and non-stream paths when the `new_routeplan` feature flag is ON.
///
/// Returns `Ok(None)` when the flag is OFF so the legacy flat path runs
/// unchanged (production default until rollout).  When ON, the plan is built via
/// `authorize_and_plan` and driven by the T06 executor + streaming driver, which
/// write the RequestLog + quota accounting (T05 handoff #3) on the facade path.
///
/// All 10 parameters are distinct immutable inputs threaded from the callers in
/// this file; folding them into a struct would be an interface change across a
/// frozen handler surface, so the lint is scoped here.
#[allow(clippy::too_many_arguments)]
async fn maybe_route_plan(
    shared: &SharedState,
    repo: &std::sync::Arc<Repository>,
    key: &crate::db::models::ApiKey,
    audited: &security::gate::AuditedRequest,
    endpoint: EndpointKind,
    is_stream: bool,
    mode: &str,
    safe_headers: &[(String, String)],
    sanitized_log_body: &str,
    trace_id: Option<String>,
) -> Result<Option<Response>, Response> {
    let flags = feature_flags::read_feature_flags(&shared.state.settings);
    // Auth accounts are request-scoped route candidates. They force the mixed
    // RoutePlan rollout while the global Channel rollout flag is off; absent a
    // usable account, retain the legacy pure-Channel path unchanged.
    let accounts = repo
        .list_route_accounts(&crate::utils::time::now_iso())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DB error: {}", e),
            )
                .into_response()
        })?;
    let has_request_scoped_auth_candidate =
        has_request_scoped_auth_candidate(key, endpoint, &audited.envelope.model, &accounts);
    if !auth_routeplan_rollout_enabled(&flags, has_request_scoped_auth_candidate) {
        return Ok(None);
    }
    let channels = repo
        .get_enabled_channels_for_mode(endpoint.as_str(), is_stream, &crate::utils::time::now_iso())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("DB error: {}", e),
            )
                .into_response()
        })?;
    let mut plan_rng = rand::rngs::StdRng::from_os_rng();
    let plan = match route_plan::authorize_and_plan_with_accounts(
        key,
        &audited.envelope.model,
        endpoint,
        &channels,
        &accounts,
        &flags,
        &audited.forward_json,
        &mut plan_rng,
    ) {
        Ok(plan) => plan,
        Err(e) => {
            let code = e.http_status();
            // I-3: a facade rejection (auth / no candidate / no endpoint)
            // must be observable in the RequestLog on BOTH paths, except for
            // Count Tokens which is deliberately excluded from request history.
            if crate::endpoint_executor::driver::should_write_request_log(endpoint) {
                crate::endpoint_executor::driver::write_stream_precommit_failure_log(
                    repo,
                    key,
                    audited,
                    mode,
                    is_stream,
                    code,
                    &e.message(),
                    sanitized_log_body,
                    trace_id.as_deref(),
                )
                .await;
            }
            return Ok(Some(
                (
                    StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                    Json(serde_json::json!({
                        "error": { "message": e.message(), "type": "route_plan_error", "code": code }
                    })),
                )
                    .into_response(),
            ));
        }
    };
    if is_stream {
        let resp = crate::endpoint_executor::driver::route_stream_plan_with_auth_service(
            plan,
            audited,
            key,
            safe_headers,
            mode,
            repo,
            sanitized_log_body,
            trace_id,
            shared.state.auth_service.clone(),
        )
        .await;
        Ok(Some(resp))
    } else {
        let resp = crate::endpoint_executor::driver::route_plan_response_with_auth_service(
            plan,
            audited,
            key,
            safe_headers,
            mode,
            repo,
            sanitized_log_body,
            trace_id,
            shared.state.auth_service.clone(),
        )
        .await;
        Ok(Some(resp))
    }
}

/// The auth rollout is deliberately request-scoped: Auth accounts turn on the
/// mixed planner for this request only, while an all-Channel request continues
/// to use the legacy path until the global flag is enabled.
fn auth_routeplan_rollout_enabled(
    flags: &crate::core::feature_flags::FeatureFlags,
    has_route_account: bool,
) -> bool {
    flags.new_routeplan || has_route_account
}

/// Auth must only force the RoutePlan for a request it can actually serve.  In
/// particular, an account with a model snapshot for another model must not
/// turn an otherwise legacy Responses/Messages request into a RoutePlan
/// rejection while all Channel rollout flags remain off.
fn has_request_scoped_auth_candidate(
    key: &crate::db::models::ApiKey,
    endpoint: EndpointKind,
    model: &str,
    accounts: &[crate::db::models::AuthAccount],
) -> bool {
    matches!(
        endpoint,
        EndpointKind::ChatCompletions | EndpointKind::Responses | EndpointKind::Messages
    ) && route_plan::resolve_route_candidates(&[], accounts, model, key)
        .iter()
        .any(|candidate| candidate.auth_account().is_some())
}

#[cfg(test)]
mod key_lookup_classification_tests {
    use super::is_key_lookup_storage_error;

    #[test]
    fn row_not_found_is_authentication_failure_not_storage_error() {
        let err = sqlx::Error::RowNotFound;
        assert!(!is_key_lookup_storage_error(&err));
    }

    #[test]
    fn database_errors_are_storage_failures() {
        assert!(is_key_lookup_storage_error(&sqlx::Error::PoolTimedOut));
        assert!(is_key_lookup_storage_error(&sqlx::Error::Io(
            std::io::Error::new(std::io::ErrorKind::Other, "disk")
        )));
    }
}

#[cfg(test)]
mod auth_routeplan_rollout_tests {
    use super::{auth_routeplan_rollout_enabled, has_request_scoped_auth_candidate};
    use crate::core::feature_flags::FeatureFlags;
    use crate::core::route_plan::EndpointKind;
    use crate::db::models::{ApiKey, AuthAccount};
    use serde_json::json;

    fn key() -> ApiKey {
        ApiKey {
            id: "key".into(),
            name: "key".into(),
            key: "sk-test".into(),
            status: 1,
            allowed_models: "[]".into(),
            allowed_channels: "[]".into(),
            denied_models: "[]".into(),
            denied_channels: "[]".into(),
            quota_limit: 0,
            quota_used: 0,
            expires_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn account(model: &str) -> AuthAccount {
        AuthAccount {
            id: "account-a".into(),
            provider: "codex".into(),
            label: "Account A".into(),
            account_id: "remote-a".into(),
            status: "active".into(),
            disabled: 0,
            priority: 1,
            weight: 1,
            quota_json: None,
            model_states_json: json!({
                "version": 1,
                "models": [{
                    "id": model,
                    "status": "available",
                    "unavailable": false,
                    "next_retry_after": null,
                    "last_error": null
                }]
            })
            .to_string(),
            model_mapping_json: "{}".into(),
            attributes_json: "{}".into(),
            payload_json: "{}".into(),
            last_refreshed_at: None,
            last_models_sync_at: None,
            next_refresh_after: None,
            next_retry_after: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn auth_routeplan_rollout_forces_only_requests_with_a_routeable_account() {
        let flags_off = FeatureFlags {
            new_routeplan: false,
            cross_protocol_codec: false,
            native_responses: false,
            ollama_native: false,
            prefer_auth_accounts: false,
            prefer_same_protocol: true,
        };
        assert!(auth_routeplan_rollout_enabled(&flags_off, true));
        assert!(!auth_routeplan_rollout_enabled(&flags_off, false));
        // The gate is not allowed to mutate ordinary Channel capability flags.
        assert!(!flags_off.cross_protocol_codec);
        assert!(!flags_off.native_responses);
    }

    #[test]
    fn flags_off_keep_unmatched_auth_models_on_legacy_responses_and_messages_paths() {
        let flags_off = FeatureFlags::all_off();
        let account = account("model-a");
        for endpoint in [EndpointKind::Responses, EndpointKind::Messages] {
            let eligible = has_request_scoped_auth_candidate(
                &key(),
                endpoint,
                "model-b",
                std::slice::from_ref(&account),
            );
            assert!(
                !eligible,
                "{endpoint:?} must not be forced into RoutePlan by an unrelated model snapshot"
            );
            assert!(
                !auth_routeplan_rollout_enabled(&flags_off, eligible),
                "{endpoint:?} with model-b must remain on its legacy handler"
            );
        }
    }

    #[test]
    fn flags_off_force_routeplan_only_for_matching_auth_endpoint_and_model() {
        let account = account("model-a");
        assert!(has_request_scoped_auth_candidate(
            &key(),
            EndpointKind::Responses,
            "model-a",
            std::slice::from_ref(&account),
        ));
        assert!(!has_request_scoped_auth_candidate(
            &key(),
            EndpointKind::CountTokens,
            "model-a",
            &[account],
        ));
    }
}

/// Persist a blocked/security-fail log using only the sanitized log body.
#[allow(clippy::too_many_arguments)]
async fn log_security_block(
    repo: &std::sync::Arc<Repository>,
    api_key_id: &str,
    api_key_name: &str,
    model: String,
    mode: &str,
    is_stream: bool,
    sanitized_log_json: &serde_json::Value,
    audit_result: &security::SecurityScanResult,
    trace_id: Option<String>,
) {
    let blocked = crate::db::models::RequestLog {
        id: crate::utils::id::new_id(),
        seq: None,
        api_key_id: Some(api_key_id.to_string()),
        api_key_name: Some(api_key_name.to_string()),
        channel_id: None,
        channel_name: None,
        model,
        upstream_model: None,
        mode: mode.to_string(),
        status_code: 451,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        duration_ms: 0,
        error_message: audit_result.blocked_reason.clone(),
        is_stream: if is_stream { 1 } else { 0 },
        is_retry: 0,
        created_at: crate::utils::time::now_iso(),
        request_body: serde_json::to_string(sanitized_log_json).ok(),
        response_choices: None,
        risk_level: audit_result.risk_level.as_str().to_string(),
        risk_score: audit_result.risk_score as i64,
        risk_summary: Some(audit_result.summary.clone()),
        security_action: audit_result.action.as_str().to_string(),
        sanitized: 1,
        blocked_reason: audit_result.blocked_reason.clone(),
        trace_id,
        // T09: blocked log — no route/upstream context (channel unknown).
        ..Default::default()
    };
    let log_id = blocked.id.clone();
    if let Err(e) = repo.create_log(&blocked).await {
        eprintln!("[WARN] create_log failed: {}", e);
    }
    if let Err(e) = repo
        .create_security_findings(
            &log_id,
            &audit_result.findings,
            audit_result.action.as_str(),
        )
        .await
    {
        eprintln!("[WARN] create_security_findings failed: {}", e);
    }
}

/// Serialize a JSON body for logging.  This is the ONLY sanctioned path from a
/// request body into the log layer: it always redacts secrets first, so raw
/// request bytes are never persisted (T03 spec).
#[allow(dead_code)]
fn sanitized_log_string(value: &serde_json::Value) -> String {
    serde_json::to_string(&security::redact::redact_json_for_logging(value)).unwrap_or_default()
}

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

    let is_stream = json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let api_key = auth_header.strip_prefix("Bearer ").unwrap_or("").trim();

    if api_key.is_empty() {
        return (StatusCode::UNAUTHORIZED, "Missing API key").into_response();
    }

    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));
    let key_record = match repo.get_api_key_by_key(api_key).await {
        Ok(k) => k,
        Err(e) if is_key_lookup_storage_error(&e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Key lookup failed").into_response()
        }
        Err(_) => return (StatusCode::UNAUTHORIZED, "Invalid API key").into_response(),
    };

    if key_record.quota_limit > 0 && key_record.quota_used >= key_record.quota_limit {
        return (StatusCode::TOO_MANY_REQUESTS, "Quota exceeded").into_response();
    }

    // Extract Wali-Trace-Id from request headers
    let trace_id = headers
        .get("Wali-Trace-Id")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    // Unified security audit gate — audits the ORIGINAL protocol JSON full
    // tree before any routing/codec.  Fail-closed (Confirm/budget) returns
    // before any upstream contact.
    let audited = match audit_original(
        security::gate::DownstreamProtocol::ChatCompletions,
        "/v1/chat/completions",
        json.clone(),
        None,
        trace_id.clone(),
        &shared,
    )
    .await
    {
        Ok(audited) => audited,
        Err(response) => return response,
    };
    let audit_result = audited.audit_result.clone();
    let forward_json = audited.forward_json.clone();

    // Never persist the raw request body.  Only the gate's sanitized log body
    // reaches the log layer; raw bytes are used solely for scanning, hashing
    // and length/parse forensics.
    let request_body_str = serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default();

    if matches!(audit_result.action, security::SecurityAction::Block) {
        log_security_block(
            &repo,
            &key_record.id,
            &key_record.name,
            audited.envelope.model.clone(),
            "chat",
            is_stream,
            &audited.sanitized_log_json,
            &audit_result,
            trace_id.clone(),
        )
        .await;
        let err_body = serde_json::json!({"error": {"message": audit_result.summary, "type": "security_blocked", "code": "security.blocked"}});
        return (StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS, Json(err_body)).into_response();
    }

    // T06: when `new_routeplan` is ON, route through the model-first RoutePlan
    // facade first (stream and non-stream both).  `Ok(None)` means the flag is
    // off and the legacy flat path runs unchanged.
    let safe_headers = crate::endpoint_executor::safe_request_headers(&headers);
    match maybe_route_plan(
        &shared,
        &repo,
        &key_record,
        &audited,
        EndpointKind::ChatCompletions,
        is_stream,
        "chat",
        &safe_headers,
        &request_body_str,
        trace_id.clone(),
    )
    .await
    {
        Ok(Some(resp)) => return resp,
        Ok(None) => {}
        Err(resp) => return resp,
    }

    if is_stream {
        handle_stream(
            shared,
            forward_json,
            key_record.id,
            key_record.name,
            request_body_str,
            audit_result,
            trace_id,
        )
        .await
    } else {
        match proxy::handle_request(
            &repo,
            &shared.state.settings,
            &key_record.id,
            &key_record.name,
            forward_json,
            false,
            Some(request_body_str),
            trace_id,
            Some(&audit_result),
        )
        .await
        {
            Ok(result) => (
                StatusCode::from_u16(result.status).unwrap_or(StatusCode::OK),
                Json(result.body),
            )
                .into_response(),
            Err((code, msg)) => {
                let err_body = serde_json::json!({
                    "error": { "message": msg, "type": "upstream_error", "code": code }
                });
                (
                    StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                    Json(err_body),
                )
                    .into_response()
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
                let prompt = usage
                    .get("prompt_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let completion = usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let total = usage
                    .get("total_tokens")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if total > 0 || prompt > 0 || completion > 0 {
                    return Some((prompt, completion, total));
                }
            }
        }
    }
    None
}

/// Accumulate token usage and response content from one complete OpenAI Chat
/// SSE record (`data: {json}\n\n`).  Shared by every upstream protocol: the
/// Anthropic bridge converts its records into this exact shape first.
fn accumulate_openai_chat_record(
    record: &str,
    usage_prompt: &mut i64,
    usage_completion: &mut i64,
    usage_total: &mut i64,
    accumulated_content: &mut String,
    accumulated_reasoning: &mut String,
    response_role: &mut Option<String>,
    finish_reason: &mut Option<String>,
    tool_calls_map: &mut std::collections::BTreeMap<i64, serde_json::Value>,
) {
    if let Some((p, c, t)) = parse_usage_from_chunk(record) {
        *usage_prompt = p;
        *usage_completion = c;
        *usage_total = t;
    }
    // Accumulate delta content from SSE records
    for line in record.lines() {
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
                        if let Some(reasoning) =
                            delta.get("reasoning_content").and_then(|c| c.as_str())
                        {
                            accumulated_reasoning.push_str(reasoning);
                        }
                        if response_role.is_none() {
                            if let Some(role) = delta.get("role").and_then(|r| r.as_str()) {
                                *response_role = Some(role.to_string());
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
                                    if let Some(args) =
                                        func.get("arguments").and_then(|v| v.as_str())
                                    {
                                        let existing =
                                            entry["function"]["arguments"].as_str().unwrap_or("");
                                        entry["function"]["arguments"] =
                                            serde_json::json!(format!("{}{}", existing, args));
                                    }
                                }
                            }
                        }
                    }
                    if finish_reason.is_none() {
                        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                            if !reason.is_empty() && reason != "null" {
                                *finish_reason = Some(reason.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn handle_stream(
    shared: SharedState,
    json: serde_json::Value,
    api_key_id: String,
    api_key_name: String,
    request_body: String,
    security_result: security::SecurityScanResult,
    trace_id: Option<String>,
) -> Response {
    let model = json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    // Security gate already ran on the ORIGINAL protocol JSON at the entry
    // handler.  `json` is the gate's redacted forward body and `request_body`
    // is the gate's sanitized log body (always safe to persist).
    let forward_json = json.clone();

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
        let err_body = serde_json::json!({"error": {"message": security_result.summary, "type": "security_blocked", "code": "security.blocked"}});
        return (StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS, Json(err_body)).into_response();
    }
    let channels = match repo.get_enabled_channels().await {
        Ok(c) => c,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "No channels available").into_response()
        }
    };

    let selected_channels = Dispatcher::select_channels(&channels, &model);
    if selected_channels.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "No channel for model").into_response();
    }

    let (retry_enabled, retry_times) = proxy::get_retry_settings(&shared.state.settings);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(selected_channels.len())
    } else {
        1
    };

    let mut last_error = None;
    // Set when an upstream returned a terminal (non-retryable) status before
    // any bytes were streamed, so the loop stops and the final response keeps
    // that status instead of a generic 502.
    let mut last_error_status: Option<StatusCode> = None;

    for (attempt, channel) in selected_channels.into_iter().take(max_attempts).enumerate() {
        let config = Dispatcher::channel_to_config(&channel);
        let adaptor = get_adaptor(&channel.channel_type);

        // Compute the actual upstream model after mapping and bake it into the
        // body ONCE so the actual request and the log share the same model
        // (design 11.4); apply_model_mapping no longer re-samples arrays.
        let upstream_model = resolve_mapped_model(&config.model_mapping, &model);
        let mut attempt_body = forward_json.clone();
        if let Some(obj) = attempt_body.as_object_mut() {
            obj.insert(
                "model".into(),
                serde_json::Value::String(upstream_model.clone()),
            );
        }
        let request = ProxyRequest {
            model: model.clone(),
            body: attempt_body,
            stream: true,
        };

        match adaptor.forward_stream(&request, &config).await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body_str = resp.text().await.unwrap_or_default();
                    last_error = Some(format!("{}: {}", channel.name, body_str));
                    match upstream_failover_decision(status.as_u16()) {
                        FailoverDecision::Failover => continue,
                        FailoverDecision::Stop { downstream_status } => {
                            // Nothing has been streamed yet — stop cycling
                            // channels; the tail returns this status (an
                            // upstream 401/403 is masked to 502; last_error
                            // above keeps the real response text).
                            last_error_status =
                                Some(StatusCode::from_u16(downstream_status).unwrap_or(status));
                            break;
                        }
                    }
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
                // Claude/Anthropic channels return Anthropic SSE from
                // forward_stream; bridge it to OpenAI SSE before relaying.
                let upstream_is_anthropic = crate::protocol::sse_bridge::is_anthropic_upstream(
                    &channel.channel_type,
                    channel.protocol.as_deref(),
                );

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
                    // Normalize fragmented / Anthropic upstream records into
                    // complete OpenAI SSE records before accumulation + relay.
                    let mut sse_bridge = crate::protocol::sse_bridge::UpstreamSseBridge::for_upstream(
                        upstream_is_anthropic,
                        &upstream_model_clone,
                    );

                    while let Some(chunk_result) = upstream_stream.next().await {
                        match chunk_result {
                            Ok(bytes) => {
                                // Normalize the chunk through the bridge, then
                                // accumulate usage/content and relay the records.
                                //
                                // Feed RAW bytes, never `str::from_utf8`-gated:
                                // a chunk boundary can split a multibyte UTF-8
                                // codepoint (CJK text is 3 bytes/char), and such
                                // a chunk previously fell into an else-branch
                                // that yielded the raw Anthropic frame straight
                                // to the OpenAI client (Opencode "Type validation
                                // failed ... expected array for `choices`").  The
                                // bridge buffers bytes and decodes only COMPLETE
                                // records, so a mid-codepoint split is held and
                                // reassembled across calls — never escaped raw.
                                match sse_bridge.push(&bytes) {
                                    Ok(records) => {
                                        for record in records {
                                            accumulate_openai_chat_record(
                                                &record,
                                                &mut usage_prompt,
                                                &mut usage_completion,
                                                &mut usage_total,
                                                &mut accumulated_content,
                                                &mut accumulated_reasoning,
                                                &mut response_role,
                                                &mut finish_reason,
                                                &mut tool_calls_map,
                                            );
                                            yield Ok::<_, std::io::Error>(bytes::Bytes::from(record.into_bytes()));
                                        }
                                    }
                                    Err(e) => {
                                        had_error = true;
                                        let err_chunk = format!(
                                            "data: {{\"error\":{{\"message\":\"Upstream conversion failed: {}\",\"type\":\"server_error\"}}}}\n\n",
                                            e
                                        );
                                        yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_chunk.into_bytes()));
                                        yield Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                had_error = true;
                                let err_chunk = format!(
                                    "data: {{\"error\":{{\"message\":\"Stream connection interrupted: {}\",\"type\":\"server_error\"}}}}\n\n",
                                    e
                                );
                                yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_chunk.into_bytes()));
                                yield Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
                                break;
                            }
                        }
                    }

                    // Stream ended. Flush any trailing record that terminated at
                    // EOF, and on Anthropic channels emit the exactly-once final
                    // sequence (finish_reason + usage frame, then [DONE]).
                    if !had_error {
                        match sse_bridge.finish() {
                            Ok(records) => {
                                for record in records {
                                    accumulate_openai_chat_record(
                                        &record,
                                        &mut usage_prompt,
                                        &mut usage_completion,
                                        &mut usage_total,
                                        &mut accumulated_content,
                                        &mut accumulated_reasoning,
                                        &mut response_role,
                                        &mut finish_reason,
                                        &mut tool_calls_map,
                                    );
                                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(record.into_bytes()));
                                }
                            }
                            Err(e) => {
                                had_error = true;
                                let err_chunk = format!(
                                    "data: {{\"error\":{{\"message\":\"Upstream conversion failed: {}\",\"type\":\"server_error\"}}}}\n\n",
                                    e
                                );
                                yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_chunk.into_bytes()));
                                yield Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
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
                    // Fallback: estimate tokens when upstream didn't return usage.
                    if usage_total == 0 && usage_prompt == 0 && usage_completion == 0 && !had_error {
                        let req_body: serde_json::Value = serde_json::from_str(&request_body_clone).unwrap_or(serde_json::Value::Null);
                        let (p, c, t) = crate::endpoint_executor::estimate_usage::estimate_usage(&req_body, Some(&accumulated_content), &model_clone);
                        usage_prompt = p;
                        usage_completion = c;
                        usage_total = t;
                        if usage_total > 0 {
                            eprintln!("[INFO] stream token usage estimated (handlers.rs chat): prompt={}, completion={}, total={}", usage_prompt, usage_completion, usage_total);
                        }
                    }
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
                    ..Default::default()
                    };
                    let log_id = log.id.clone();
                    if let Err(e) = repo_clone.create_log(&log).await { eprintln!("[WARN] create_log failed: {}", e); }
                    if let Err(e) = repo_clone.create_security_findings(&log_id, &security_result_clone.findings, security_result_clone.action.as_str()).await { eprintln!("[WARN] create_security_findings failed: {}", e); }

                    // Increment quota if we got token counts
                    if quota_to_add > 0 {
                        if let Err(e) = repo_clone.increment_quota(&key_id_for_quota, quota_to_add).await { eprintln!("[WARN] increment_quota failed: {}", e); }
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
    let status = last_error_status.unwrap_or(StatusCode::BAD_GATEWAY);
    (status, Json(err_body)).into_response()
}

// ─── Anthropic Messages API: POST /v1/messages ─────────────────────────────
// Accepts Anthropic-format requests and proxies to upstream channels.
// For Claude-type channels: forward natively (Anthropic format).
// For other channels: convert Anthropic → OpenAI → upstream → OpenAI → Anthropic.

fn anthropic_error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(serde_json::json!({"type":"error", "error":{"type":kind, "message":message.into()}})),
    )
        .into_response()
}

/// Resolve a model name through the mapping: supports both single string and
/// array of strings (array = load balancing, sampled once per call).
///
/// T09 (design 11.4): this now DELEGATES to the shared planner helper
/// (`route_plan::resolve_upstream_model`) so there is exactly ONE sampling
/// implementation in the codebase — the adaptor/handlers never re-sample.
fn resolve_mapped_model(mapping: &serde_json::Value, model: &str) -> String {
    let mut rng = rand::rng();
    crate::core::route_plan::resolve_upstream_model(mapping, model, &mut rng)
}

/// Map a body's model through the mapping and RETURN the resolved upstream
/// model alongside the mapped body (single sample for request + log, design
/// 11.4: "实际请求模型与日志模型一致").
fn mapped_anthropic_body(
    body: &serde_json::Value,
    mapping: &serde_json::Value,
) -> (serde_json::Value, String) {
    let mut body = body.clone();
    let mut resolved = String::new();
    if let Some(model) = body.get("model").and_then(|value| value.as_str()) {
        let mapped = resolve_mapped_model(mapping, model);
        resolved = mapped.clone();
        if mapped != model {
            body["model"] = serde_json::Value::String(mapped);
        }
    }
    (body, resolved)
}

/// T06: `is_native_anthropic_channel(type == "claude")` is REMOVED — the
/// production-selection duty moved to the identity-based
/// `executor::driver::is_native_anthropic` / `supports_count_tokens` (protocol
/// == "anthropic" AND the endpoint capability is declared, design 6.2).
fn is_unsafe_proxy_header(name: &str) -> bool {
    // RFC 9110 hop-by-hop fields and credentials belonging to the *client*
    // must never cross the gateway boundary.  Everything else is deliberately
    // forwarded so future Anthropic end-to-end headers keep working.
    matches!(
        name,
        "authorization"
            | "proxy-authorization"
            | "x-api-key"
            | "cookie"
            | "set-cookie"
            | "host"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "content-type"
            | "expect"
            | "accept-encoding"
            | "wali-trace-id"
    )
}

fn forwarded_anthropic_headers(
    headers: &HeaderMap,
) -> Vec<(axum::http::HeaderName, axum::http::HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| !is_unsafe_proxy_header(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn valuable_anthropic_response_headers(
    headers: &reqwest::header::HeaderMap,
) -> Vec<(axum::http::HeaderName, axum::http::HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| !is_unsafe_proxy_header(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

async fn native_anthropic_request(
    config: &crate::adaptor::ChannelConfig,
    headers: &HeaderMap,
    body: &serde_json::Value,
    count_tokens: bool,
    query: Option<&str>,
) -> Result<(reqwest::Response, Option<String>), reqwest::Error> {
    let path = if count_tokens {
        "messages/count_tokens"
    } else {
        "messages"
    };
    let url = native_anthropic_url(config, path, query);
    let (mapped_body, upstream_model) = mapped_anthropic_body(body, &config.model_mapping);
    // count_tokens is always non-streaming; native Anthropic Messages streams
    // through this same function, so use a streaming client (connect-timeout
    // only) to avoid cutting off long SSE generations.
    let client = crate::adaptor::streaming_client();
    let mut request = client
        .post(url)
        .header("x-api-key", &config.api_key)
        .header("content-type", "application/json");
    for (name, value) in forwarded_anthropic_headers(headers) {
        request = request.header(name, value);
    }
    let resp = request.json(&mapped_body).send().await?;
    // T09 (design 11.4): the SAME sampled model used for the request body is
    // returned so the caller's log records the real upstream model (the old
    // behavior always logged `None` for native Anthropic).
    let model = if upstream_model.is_empty() {
        None
    } else {
        Some(upstream_model)
    };
    Ok((resp, model))
}

fn native_anthropic_url(
    config: &crate::adaptor::ChannelConfig,
    path: &str,
    query: Option<&str>,
) -> String {
    let mut url = format!("{}/{}", config.base_url.trim_end_matches('/'), path);
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        url.push('?');
        url.push_str(query);
    }
    url
}

async fn openai_messages_request(
    config: &crate::adaptor::ChannelConfig,
    body: &serde_json::Value,
    is_stream: bool,
) -> Result<(reqwest::Response, Option<String>), reqwest::Error> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    // T09 (design 11.4): sample the array mapping EXACTLY ONCE here, bake the
    // resolved model into the body, and return it so the caller's log records
    // the SAME model the request actually used (the old path logged None).
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("");
    let upstream_model = if model.is_empty() {
        None
    } else {
        Some(resolve_mapped_model(&config.model_mapping, model))
    };
    let mut mapped_body = crate::adaptor::openai::apply_model_mapping(body, &config.model_mapping);
    if let (Some(um), Some(obj)) = (upstream_model.as_ref(), mapped_body.as_object_mut()) {
        obj.insert("model".into(), serde_json::Value::String(um.clone()));
    }
    let client = if is_stream {
        crate::adaptor::streaming_client()
    } else {
        crate::adaptor::blocking_client(config.timeout_secs)
    };
    let resp = client
        .post(url)
        .bearer_auth(&config.api_key)
        .header("content-type", "application/json")
        .json(&mapped_body)
        .send()
        .await?;
    Ok((resp, upstream_model))
}

#[derive(Clone)]
struct StreamLogContext {
    repo: std::sync::Arc<Repository>,
    key: crate::db::models::ApiKey,
    channel: crate::db::models::Channel,
    model: String,
    /// T09: the single-sampled upstream model (design 11.4) so the native
    /// Anthropic log records the real request model instead of always None.
    upstream_model: Option<String>,
    request: serde_json::Value,
    security: security::SecurityScanResult,
    is_stream: bool,
}

const MAX_NATIVE_SSE_RECORD_BYTES: usize = 64 * 1024;

/// Incrementally extracts the cumulative usage fields from a native Anthropic
/// SSE stream.  It deliberately retains at most one bounded record rather
/// than every byte forwarded to the client.
#[derive(Default)]
struct NativeSseUsageParser {
    pending: Vec<u8>,
    input: Option<i64>,
    output: Option<i64>,
    cached: Option<i64>,
    stopped: bool,
    malformed_or_oversized: bool,
}

impl NativeSseUsageParser {
    fn feed(&mut self, bytes: &[u8]) {
        if self.malformed_or_oversized {
            return;
        }
        self.pending.extend_from_slice(bytes);
        while let Some(end) = sse_record_end(&self.pending) {
            let record: Vec<u8> = self.pending.drain(..end).collect();
            self.consume_record(&record);
        }
        if self.pending.len() > MAX_NATIVE_SSE_RECORD_BYTES {
            self.pending.clear();
            self.malformed_or_oversized = true;
        }
    }

    fn consume_record(&mut self, record: &[u8]) {
        let Ok(text) = std::str::from_utf8(record) else {
            return;
        };
        let data = text
            .lines()
            .filter_map(|line| {
                line.trim_end_matches('\r')
                    .strip_prefix("data:")
                    .map(|value| value.trim_start())
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            return;
        };
        match value.get("type").and_then(|value| value.as_str()) {
            Some("message_start") => {
                self.input = value.pointer("/message/usage").map(anthropic_input_usage);
                self.cached = value
                    .pointer("/message/usage/cache_read_input_tokens")
                    .and_then(|v| v.as_i64());
            }
            Some("message_delta") => {
                self.output = value
                    .pointer("/usage/output_tokens")
                    .and_then(|value| value.as_i64())
            }
            Some("message_stop") => self.stopped = true,
            _ => {}
        }
    }

    fn finish(self) -> Option<(i64, i64, i64)> {
        (!self.malformed_or_oversized && self.stopped).then(|| {
            (
                self.input.unwrap_or(0),
                self.output.unwrap_or(0),
                self.cached.unwrap_or(0),
            )
        })
    }
}

fn sse_record_end(input: &[u8]) -> Option<usize> {
    let crlf = input
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4);
    let lf = input
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| index + 2);
    match (crlf, lf) {
        (Some(crlf), Some(lf)) => Some(crlf.min(lf)),
        (Some(end), None) | (None, Some(end)) => Some(end),
        (None, None) => None,
    }
}

fn native_usage(bytes: &[u8], is_sse: bool) -> Option<(i64, i64, i64)> {
    let text = std::str::from_utf8(bytes).ok()?;
    if !is_sse {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let usage = value.get("usage")?;
        let cached = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        return Some((
            anthropic_input_usage(usage),
            usage
                .get("output_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            cached,
        ));
    }
    let mut parser = NativeSseUsageParser::default();
    parser.feed(text.as_bytes());
    parser.finish()
}

fn anthropic_input_usage(usage: &serde_json::Value) -> i64 {
    usage
        .get("input_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or(0)
        + usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or(0)
        + usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or(0)
}

fn native_response(response: reqwest::Response, accounting: Option<StreamLogContext>) -> Response {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = valuable_anthropic_response_headers(response.headers());
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let is_sse = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let upstream = response.bytes_stream();
    let stream = async_stream::stream! {
        tokio::pin!(upstream);
        let mut usage_parser = NativeSseUsageParser::default();
        // A non-streaming Messages response is a small JSON object in normal
        // operation.  Keep a hard cap for accounting so a malicious upstream
        // can never turn the proxy into an unbounded collector.
        let mut non_sse_observed = Vec::new();
        let mut completed = true;
        while let Some(item) = upstream.next().await {
            match item {
                Ok(bytes) => {
                    if is_sse {
                        usage_parser.feed(&bytes);
                    } else if non_sse_observed.len().saturating_add(bytes.len()) <= MAX_NATIVE_SSE_RECORD_BYTES {
                        non_sse_observed.extend_from_slice(&bytes);
                    }
                    yield Ok::<_, std::io::Error>(bytes);
                }
                Err(error) => { completed = false; yield Err::<bytes::Bytes, _>(std::io::Error::other(error)); break; }
            }
        }
        if let Some(context) = accounting {
            if completed {
                let usage = if is_sse { usage_parser.finish() } else { native_usage(&non_sse_observed, false) };
                if let Some(usage) = usage {
                    record_anthropic_success(context.repo, &context.key, &context.channel, &context.model, context.upstream_model.clone(), &context.request, &context.security, context.is_stream, Some(usage)).await;
                }
            }
        }
    };
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::from_stream(stream)).unwrap_or_else(|_| {
        anthropic_error(
            StatusCode::BAD_GATEWAY,
            "api_error",
            "Unable to proxy native Anthropic response",
        )
    })
}

struct StoredNativeError {
    status: StatusCode,
    content_type: Option<axum::http::HeaderValue>,
    headers: Vec<(axum::http::HeaderName, axum::http::HeaderValue)>,
    body: bytes::Bytes,
}

async fn store_native_error(response: reqwest::Response) -> StoredNativeError {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let headers = valuable_anthropic_response_headers(response.headers());
    let body = response.bytes().await.unwrap_or_default();
    StoredNativeError {
        status,
        content_type,
        headers,
        body,
    }
}

fn stored_native_response(error: StoredNativeError) -> Response {
    let mut builder = Response::builder().status(error.status);
    if let Some(content_type) = error.content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    for (name, value) in error.headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::from(error.body)).unwrap_or_else(|_| {
        anthropic_error(
            StatusCode::BAD_GATEWAY,
            "api_error",
            "Unable to proxy native Anthropic response",
        )
    })
}

fn openai_error_response(
    status: StatusCode,
    message: &str,
    headers: &reqwest::header::HeaderMap,
) -> Response {
    // Upstream credentials belong to the gateway. Do not report an upstream
    // 401/403 as though the Claude Code caller supplied a bad local key.
    let (downstream_status, kind) = match status.as_u16() {
        429 => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        400 | 404 | 408 | 409 | 422 => (status, "invalid_request_error"),
        _ => (StatusCode::BAD_GATEWAY, "api_error"),
    };
    let mut response = anthropic_error(downstream_status, kind, message);
    if let Some(retry_after) = headers.get("retry-after") {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, retry_after.clone());
    }
    response
}

fn sanitized_anthropic_log_body(request: &serde_json::Value) -> (Option<String>, bool) {
    let redacted = security::redact::redact_json_for_logging(request);
    let sanitized = redacted != *request;
    (serde_json::to_string(&redacted).ok(), sanitized)
}

/// 11 distinct immutable inputs that differ per call site (success vs failure,
/// channel present/absent, upstream model present/absent, varied status/error/
/// usage).  16 call sites across the Messages/native/stream paths pass wildly
/// different combinations; folding them into a struct would ripple through every
/// caller on a frozen handler surface for no functional gain, so the lint is
/// scoped here.
#[allow(clippy::too_many_arguments)]
async fn record_anthropic_outcome(
    repo: std::sync::Arc<Repository>,
    key: &crate::db::models::ApiKey,
    channel: Option<&crate::db::models::Channel>,
    model: &str,
    upstream_model: Option<String>,
    request: &serde_json::Value,
    security_result: &security::SecurityScanResult,
    is_stream: bool,
    status_code: i64,
    error_message: Option<String>,
    usage: Option<(i64, i64, i64)>,
) {
    let (prompt_tokens, completion_tokens, cached_tokens) = usage.unwrap_or((0, 0, 0));
    let mut total_tokens = prompt_tokens + completion_tokens;
    let mut prompt_tokens = prompt_tokens;
    let mut completion_tokens = completion_tokens;

    // Fallback: estimate tokens when upstream didn't return usage.
    if total_tokens == 0
        && prompt_tokens == 0
        && completion_tokens == 0
        && status_code >= 200
        && status_code < 300
    {
        let req_body = serde_json::to_value(request).unwrap_or(serde_json::Value::Null);
        let (p, c, t) =
            crate::endpoint_executor::estimate_usage::estimate_usage(&req_body, None, model);
        prompt_tokens = p;
        completion_tokens = c;
        total_tokens = t;
        if total_tokens > 0 {
            eprintln!("[INFO] anthropic token usage estimated (handlers.rs): prompt={}, completion={}, total={}", prompt_tokens, completion_tokens, total_tokens);
        }
    }

    let (request_body, log_sanitized) = sanitized_anthropic_log_body(request);
    let log = crate::db::models::RequestLog {
        id: crate::utils::id::new_id(),
        seq: None,
        api_key_id: Some(key.id.clone()),
        api_key_name: Some(key.name.clone()),
        channel_id: channel.map(|channel| channel.id.clone()),
        channel_name: channel.map(|channel| channel.name.clone()),
        model: model.to_string(),
        upstream_model,
        mode: "anthropic".to_string(),
        status_code,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cached_tokens,
        duration_ms: 0,
        error_message,
        is_stream: i64::from(is_stream),
        is_retry: 0,
        created_at: crate::utils::time::now_iso(),
        request_body,
        response_choices: None,
        risk_level: security_result.risk_level.as_str().to_string(),
        risk_score: security_result.risk_score as i64,
        risk_summary: Some(security_result.summary.clone()),
        security_action: security_result.action.as_str().to_string(),
        sanitized: i64::from(log_sanitized || security_result.sanitized),
        blocked_reason: security_result.blocked_reason.clone(),
        trace_id: None,
        // T09: native/OpenAI-compat Messages path.  downstream is the Messages
        // endpoint; the other observability fields (route_group / upstream
        // protocol/endpoint / codec / failure class) are populated by the
        // facade path (T06) which carries PreparedAttempt.  upstream_model is
        // the mapping-consistency fix: see the facade seam note in the T09
        // report (the legacy record_anthropic_outcome cannot see the mapped
        // model without a signature change that T06's restructure supersedes).
        downstream_protocol: Some("messages".to_string()),
        downstream_endpoint: Some("messages".to_string()),
        ..Default::default()
    };
    let log_id = log.id.clone();
    let _ = repo.create_log(&log).await;
    let _ = repo
        .create_security_findings(
            &log_id,
            &security_result.findings,
            security_result.action.as_str(),
        )
        .await;
    if total_tokens > 0 {
        let _ = repo.increment_quota(&key.id, total_tokens).await;
    }
}

/// Thin success wrapper over `record_anthropic_outcome` (which carries the
/// scoped too-many-arguments allow); kept as a convenience for the success-only
/// call sites.
#[allow(clippy::too_many_arguments)]
async fn record_anthropic_success(
    repo: std::sync::Arc<Repository>,
    key: &crate::db::models::ApiKey,
    channel: &crate::db::models::Channel,
    model: &str,
    upstream_model: Option<String>,
    request: &serde_json::Value,
    security_result: &security::SecurityScanResult,
    is_stream: bool,
    usage: Option<(i64, i64, i64)>,
) {
    record_anthropic_outcome(
        repo,
        key,
        Some(channel),
        model,
        upstream_model,
        request,
        security_result,
        is_stream,
        200,
        None,
        usage,
    )
    .await;
}

/// Anthropic Messages compatibility endpoint.
///
/// Channel selection is performed before any format conversion. Claude channels
/// receive the original request and native response bytes; every other channel
/// is the explicit OpenAI Chat Completions compatibility path.
pub async fn handle_messages(
    State(shared): State<SharedState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Invalid JSON: {error}"),
            )
        }
    };
    let api_key = match protocol::extract_api_key(&headers) {
        Some(key) => key,
        None => {
            return anthropic_error(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Missing API key",
            )
        }
    };
    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));
    let key = match repo.get_api_key_by_key(&api_key).await {
        Ok(key) => key,
        Err(e) if is_key_lookup_storage_error(&e) => {
            return anthropic_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "Key lookup failed",
            )
        }
        Err(_) => {
            return anthropic_error(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Invalid API key",
            )
        }
    };
    if key.quota_limit > 0 && key.quota_used >= key.quota_limit {
        return anthropic_error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "Quota exceeded",
        );
    }
    let model = match json
        .get("model")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
    {
        Some(model) => model.to_string(),
        None => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "model is required",
            )
        }
    };
    let stream = json
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    // Unified security audit gate — audits the ORIGINAL Messages protocol JSON
    // full tree before any routing/codec.  The raw query string is audited as
    // part of the envelope so the native executor can forward it safely.
    let query = uri.query().map(|s| s.to_string());
    let audited = match audit_original(
        security::gate::DownstreamProtocol::Messages,
        "/v1/messages",
        json.clone(),
        query.clone(),
        None,
        &shared,
    )
    .await
    {
        Ok(audited) => audited,
        Err(response) => return response,
    };
    let security_result = audited.audit_result.clone();
    let forward_json = audited.forward_json.clone();
    let sanitized_log_json = audited.sanitized_log_json.clone();
    if matches!(security_result.action, security::SecurityAction::Block) {
        record_anthropic_outcome(
            repo.clone(),
            &key,
            None,
            &model,
            None,
            &sanitized_log_json,
            &security_result,
            stream,
            451,
            security_result.blocked_reason.clone(),
            None,
        )
        .await;
        return anthropic_error(
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            "api_error",
            security_result.summary,
        );
    }
    // T06: when `new_routeplan` is ON, route Messages through the facade
    // (native Anthropic G1 first, then OpenAI Chat G2 via the codec).
    let safe_headers = crate::endpoint_executor::safe_request_headers(&headers);
    let sanitized_log_body = serde_json::to_string(&sanitized_log_json).unwrap_or_default();
    match maybe_route_plan(
        &shared,
        &repo,
        &key,
        &audited,
        EndpointKind::Messages,
        stream,
        "anthropic",
        &safe_headers,
        &sanitized_log_body,
        None,
    )
    .await
    {
        Ok(Some(resp)) => return resp,
        Ok(None) => {}
        Err(resp) => return resp,
    }
    let channels = match repo.get_enabled_channels().await {
        Ok(channels) => channels,
        Err(_) => {
            return anthropic_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "api_error",
                "No channels available",
            )
        }
    };
    let mut selected = Dispatcher::select_channels(&channels, &model);
    // A native Anthropic channel preserves all current and future Messages
    // features, so prefer it before entering the intentionally smaller Chat
    // codec.  Selection is identity-based (protocol == "anthropic" AND the
    // `messages` capability), NOT `type == "claude"` (design 6.2).
    selected.sort_by_key(|channel| !crate::endpoint_executor::driver::is_native_anthropic(channel));
    if selected.is_empty() {
        return anthropic_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            format!("No channel for model: {model}"),
        );
    }
    let (retry_enabled, retry_times) = proxy::get_retry_settings(&shared.state.settings);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(selected.len())
    } else {
        1
    };
    let mut last_error = "unknown upstream error".to_string();
    let mut last_native_error = None;
    let mut last_openai_error: Option<(StatusCode, String, reqwest::header::HeaderMap)> = None;
    let mut upstream_attempts = 0usize;

    for channel in selected {
        let config = Dispatcher::channel_to_config(&channel);
        if crate::endpoint_executor::driver::is_native_anthropic(&channel) {
            if upstream_attempts >= max_attempts {
                break;
            }
            upstream_attempts += 1;
            match native_anthropic_request(&config, &headers, &forward_json, false, uri.query())
                .await
            {
                Ok((response, upstream_model)) if response.status().is_success() => {
                    return native_response(
                        response,
                        Some(StreamLogContext {
                            repo: repo.clone(),
                            key: key.clone(),
                            channel: channel.clone(),
                            model: model.clone(),
                            upstream_model,
                            request: sanitized_log_json.clone(),
                            security: security_result.clone(),
                            is_stream: stream,
                        }),
                    )
                }
                Ok((response, upstream_model)) => {
                    let status = StatusCode::from_u16(response.status().as_u16())
                        .unwrap_or(StatusCode::BAD_GATEWAY);
                    match upstream_failover_decision(status.as_u16()) {
                        FailoverDecision::Failover => {
                            last_error = format!("{}: HTTP {}", channel.name, status);
                            last_native_error = Some(store_native_error(response).await);
                        }
                        FailoverDecision::Stop { downstream_status } => {
                            record_anthropic_outcome(
                                repo.clone(),
                                &key,
                                Some(&channel),
                                &model,
                                upstream_model,
                                &sanitized_log_json,
                                &security_result,
                                stream,
                                status.as_u16() as i64,
                                Some(format!("Native upstream returned HTTP {status}")),
                                None,
                            )
                            .await;
                            // An upstream 401/403 is a channel-credential
                            // failure — answer 502 instead of passing it
                            // through (the outcome log keeps the real status).
                            if downstream_status != status.as_u16() {
                                return anthropic_error(
                                    StatusCode::BAD_GATEWAY,
                                    "api_error",
                                    "Upstream channel authentication failed",
                                );
                            }
                            return native_response(response, None);
                        }
                    }
                }
                Err(error) => {
                    last_error = format!("{}: {error}", channel.name);
                    record_anthropic_outcome(
                        repo.clone(),
                        &key,
                        Some(&channel),
                        &model,
                        None,
                        &sanitized_log_json,
                        &security_result,
                        stream,
                        502,
                        Some(last_error.clone()),
                        None,
                    )
                    .await;
                }
            }
            continue;
        }

        let openai_body = match protocol::anthropic_to_openai(&forward_json) {
            Ok(value) => value,
            Err(message) => {
                last_error = format!(
                    "{}: incompatible with OpenAI Chat Completions: {message}",
                    channel.name
                );
                continue;
            }
        };
        if upstream_attempts >= max_attempts {
            break;
        }
        upstream_attempts += 1;
        match openai_messages_request(&config, &openai_body, stream).await {
            Ok((response, upstream_model)) if response.status().is_success() && stream => {
                return openai_sse_response(
                    response,
                    &model,
                    StreamLogContext {
                        repo: repo.clone(),
                        key: key.clone(),
                        channel: channel.clone(),
                        model: model.clone(),
                        upstream_model,
                        request: sanitized_log_json.clone(),
                        security: security_result.clone(),
                        is_stream: true,
                    },
                )
            }
            Ok((response, upstream_model)) if response.status().is_success() => {
                let body: serde_json::Value = match response.json().await {
                    Ok(value) => value,
                    Err(error) => {
                        last_error = format!("{}: {error}", channel.name);
                        // A successful HTTP response that cannot be decoded is
                        // not an upstream attempt for failover purposes.
                        upstream_attempts = upstream_attempts.saturating_sub(1);
                        continue;
                    }
                };
                return match protocol::openai_to_anthropic(&body, &model) {
                    Ok(value) => {
                        let usage = Some((
                            body.pointer("/usage/prompt_tokens")
                                .and_then(|value| value.as_i64())
                                .unwrap_or(0),
                            body.pointer("/usage/completion_tokens")
                                .and_then(|value| value.as_i64())
                                .unwrap_or(0),
                            body.pointer("/usage/prompt_tokens_details/cached_tokens")
                                .and_then(|value| value.as_i64())
                                .unwrap_or(0),
                        ));
                        record_anthropic_success(
                            repo.clone(),
                            &key,
                            &channel,
                            &model,
                            upstream_model,
                            &sanitized_log_json,
                            &security_result,
                            false,
                            usage,
                        )
                        .await;
                        (StatusCode::OK, Json(value)).into_response()
                    }
                    Err(message) => {
                        // A 200 transport response is not a usable channel if
                        // its tool arguments/content cannot satisfy Messages.
                        last_error = format!("{}: conversion failed: {message}", channel.name);
                        record_anthropic_outcome(
                            repo.clone(),
                            &key,
                            Some(&channel),
                            &model,
                            upstream_model,
                            &sanitized_log_json,
                            &security_result,
                            false,
                            502,
                            Some(message),
                            None,
                        )
                        .await;
                        upstream_attempts = upstream_attempts.saturating_sub(1);
                        continue;
                    }
                };
            }
            Ok((response, upstream_model)) => {
                let status = StatusCode::from_u16(response.status().as_u16())
                    .unwrap_or(StatusCode::BAD_GATEWAY);
                let response_headers = response.headers().clone();
                let upstream: serde_json::Value =
                    response.json().await.unwrap_or(serde_json::Value::Null);
                let message = upstream
                    .pointer("/error/message")
                    .and_then(|value| value.as_str())
                    .unwrap_or("OpenAI Chat Completions upstream rejected the request");
                last_error = format!("{}: {message}", channel.name);
                match upstream_failover_decision(status.as_u16()) {
                    FailoverDecision::Failover => {
                        last_openai_error = Some((status, message.to_string(), response_headers));
                    }
                    FailoverDecision::Stop { .. } => {
                        record_anthropic_outcome(
                            repo.clone(),
                            &key,
                            Some(&channel),
                            &model,
                            upstream_model,
                            &sanitized_log_json,
                            &security_result,
                            stream,
                            status.as_u16() as i64,
                            Some(last_error.clone()),
                            None,
                        )
                        .await;
                        // openai_error_response already maps an upstream
                        // 401/403 to 502 api_error (429 keeps Retry-After).
                        return openai_error_response(status, message, &response_headers);
                    }
                }
            }
            Err(error) => {
                last_error = format!("{}: {error}", channel.name);
                record_anthropic_outcome(
                    repo.clone(),
                    &key,
                    Some(&channel),
                    &model,
                    None,
                    &sanitized_log_json,
                    &security_result,
                    stream,
                    502,
                    Some(last_error.clone()),
                    None,
                )
                .await;
            }
        }
    }
    if let Some((status, message, headers)) = last_openai_error {
        return openai_error_response(status, &message, &headers);
    }
    if let Some(response) = last_native_error {
        return stored_native_response(response);
    }
    if last_error.contains("incompatible with OpenAI Chat Completions") {
        record_anthropic_outcome(
            repo.clone(),
            &key,
            None,
            &model,
            None,
            &sanitized_log_json,
            &security_result,
            stream,
            400,
            Some(last_error.clone()),
            None,
        )
        .await;
        return anthropic_error(StatusCode::BAD_REQUEST, "invalid_request_error", last_error);
    }
    record_anthropic_outcome(
        repo.clone(),
        &key,
        None,
        &model,
        None,
        &sanitized_log_json,
        &security_result,
        stream,
        502,
        Some(last_error.clone()),
        None,
    )
    .await;
    anthropic_error(
        StatusCode::BAD_GATEWAY,
        "api_error",
        format!("All channels failed for model {model}: {last_error}"),
    )
}

fn openai_sse_response(
    response: reqwest::Response,
    model: &str,
    accounting: StreamLogContext,
) -> Response {
    let model = model.to_string();
    let message_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let upstream = response.bytes_stream();
    let stream = async_stream::stream! {
        tokio::pin!(upstream);
        let mut state = crate::protocol::anthropic::AnthropicStreamState::default();
        let mut failed = false;
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => match state.feed(&bytes, &model, &message_id) {
                    Ok(events) => for event in events { yield Ok::<_, std::io::Error>(bytes::Bytes::from(event.into_bytes())); },
                    Err(message) => {
                        failed = true;
                        record_anthropic_outcome(accounting.repo.clone(), &accounting.key, Some(&accounting.channel), &accounting.model, None, &accounting.request, &accounting.security, true, 502, Some(format!("OpenAI stream conversion failed: {message}")), None).await;
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from(format!("event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"api_error\",\"message\":{}}}}}\n\n", serde_json::to_string(&message).unwrap()).into_bytes()));
                        break;
                    }
                },
                Err(error) => {
                    failed = true;
                    let message = format!("OpenAI stream interrupted: {error}");
                    record_anthropic_outcome(accounting.repo.clone(), &accounting.key, Some(&accounting.channel), &accounting.model, None, &accounting.request, &accounting.security, true, 502, Some(message.clone()), None).await;
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(format!("event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"api_error\",\"message\":{}}}}}\n\n", serde_json::to_string(&message).unwrap()).into_bytes()));
                    break;
                }
            }
        }
        if !failed {
            match state.finish(&model, &message_id) {
                Ok(events) => {
                    for event in events { yield Ok::<_, std::io::Error>(bytes::Bytes::from(event.into_bytes())); }
                    let usage = state.usage();
                    record_anthropic_success(accounting.repo, &accounting.key, &accounting.channel, &accounting.model, None, &accounting.request, &accounting.security, true, Some(usage)).await;
                },
                Err(message) => {
                    record_anthropic_outcome(accounting.repo.clone(), &accounting.key, Some(&accounting.channel), &accounting.model, None, &accounting.request, &accounting.security, true, 502, Some(format!("OpenAI stream conversion failed: {message}")), None).await;
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(format!("event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"api_error\",\"message\":{}}}}}\n\n", serde_json::to_string(&message).unwrap()).into_bytes()))
                },
            }
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Claude Code calls this endpoint while constructing context.  Exact counts
/// are only available from a native Anthropic channel; returning characters/4
/// would falsely advertise precision.
pub async fn handle_messages_count_tokens(
    State(shared): State<SharedState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("Invalid JSON: {error}"),
            )
        }
    };
    let api_key = match protocol::extract_api_key(&headers) {
        Some(key) => key,
        None => {
            return anthropic_error(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Missing API key",
            )
        }
    };
    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));
    let key = match repo.get_api_key_by_key(&api_key).await {
        Ok(key) => key,
        Err(e) if is_key_lookup_storage_error(&e) => {
            return anthropic_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "Key lookup failed",
            )
        }
        Err(_) => {
            return anthropic_error(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Invalid API key",
            )
        }
    };
    let model = match json.get("model").and_then(|value| value.as_str()) {
        Some(model) => model,
        None => {
            return anthropic_error(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "model is required",
            )
        }
    };
    // Unified security audit gate — audits the ORIGINAL Count Tokens JSON.
    let query = uri.query().map(|s| s.to_string());
    let audited = match audit_original(
        security::gate::DownstreamProtocol::CountTokens,
        "/v1/messages/count_tokens",
        json.clone(),
        query.clone(),
        None,
        &shared,
    )
    .await
    {
        Ok(audited) => audited,
        Err(response) => return response,
    };
    let forward_json = audited.forward_json.clone();
    let sanitized_log_json = audited.sanitized_log_json.clone();
    if matches!(audited.audit_result.action, security::SecurityAction::Block) {
        return anthropic_error(
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            "api_error",
            audited.audit_result.summary.clone(),
        );
    }
    // T06: when `new_routeplan` is ON, route CountTokens through the facade
    // (Anthropic `count_tokens` capability only; 501 when no such channel).
    let safe_headers = crate::endpoint_executor::safe_request_headers(&headers);
    let sanitized_log_body = serde_json::to_string(&sanitized_log_json).unwrap_or_default();
    match maybe_route_plan(
        &shared,
        &repo,
        &key,
        &audited,
        EndpointKind::CountTokens,
        false,
        "anthropic_count_tokens",
        &safe_headers,
        &sanitized_log_body,
        None,
    )
    .await
    {
        Ok(Some(resp)) => return resp,
        Ok(None) => {}
        Err(resp) => return resp,
    }
    let channels = match repo.get_enabled_channels().await {
        Ok(channels) => channels,
        Err(_) => {
            return anthropic_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "api_error",
                "No channels available",
            )
        }
    };
    // T06: count_tokens routes only to channels that DECLARE the capability
    // (identity-based), not merely `type == "claude"` (design 6.2).
    let native_channels: Vec<_> = Dispatcher::select_channels(&channels, model)
        .into_iter()
        .filter(crate::endpoint_executor::driver::supports_count_tokens)
        .collect();
    let (retry_enabled, retry_times) = proxy::get_retry_settings(&shared.state.settings);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(native_channels.len())
    } else {
        native_channels.len().min(1)
    };
    let mut last_error = None;
    for channel in native_channels.into_iter().take(max_attempts) {
        let config = Dispatcher::channel_to_config(&channel);
        match native_anthropic_request(&config, &headers, &forward_json, true, uri.query()).await {
            Ok((response, _upstream_model)) if response.status().is_success() => {
                return native_response(response, None)
            }
            Ok((response, _upstream_model)) => {
                let status = StatusCode::from_u16(response.status().as_u16())
                    .unwrap_or(StatusCode::BAD_GATEWAY);
                match upstream_failover_decision(status.as_u16()) {
                    FailoverDecision::Failover => {
                        last_error = Some(store_native_error(response).await);
                    }
                    FailoverDecision::Stop { downstream_status } => {
                        // Mask a channel-credential failure (401/403) to 502;
                        // every other terminal status passes through verbatim.
                        if downstream_status != status.as_u16() {
                            return anthropic_error(
                                StatusCode::BAD_GATEWAY,
                                "api_error",
                                "Upstream channel authentication failed",
                            );
                        }
                        return native_response(response, None);
                    }
                }
            }
            Err(_) => continue,
        }
    }
    if let Some(response) = last_error {
        return stored_native_response(response);
    }
    anthropic_error(
        StatusCode::NOT_IMPLEMENTED,
        "api_error",
        "Exact Anthropic count_tokens requires a native Anthropic Messages channel",
    )
}

// ─── OpenAI Responses API: POST /v1/responses ────────────────────────────────
// Accepts Responses API format and proxies to upstream channels via Chat Completions.
// Converts: Responses input → OpenAI messages → upstream → OpenAI response → Responses output.

pub async fn handle_responses(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let body_str = String::from_utf8_lossy(&body);
    let json: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(j) => j,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)).into_response(),
    };

    let is_stream = json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let api_key = match protocol::extract_api_key(&headers) {
        Some(k) => k,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": {"message": "Missing API key", "type": "authentication_error"}
                })),
            )
                .into_response()
        }
    };

    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));
    let key_record = match repo.get_api_key_by_key(&api_key).await {
        Ok(k) => k,
        Err(e) if is_key_lookup_storage_error(&e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {"message": "Key lookup failed", "type": "server_error"}
                })),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": {"message": "Invalid API key", "type": "authentication_error"}
                })),
            )
                .into_response()
        }
    };

    if key_record.quota_limit > 0 && key_record.quota_used >= key_record.quota_limit {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {"message": "Quota exceeded", "type": "rate_limit_error"}
            })),
        )
            .into_response();
    }

    let trace_id = headers
        .get("Wali-Trace-Id")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    // Unified security audit gate — audits the ORIGINAL Responses protocol
    // JSON full tree (built-in tools, image URLs, files, unknown blocks)
    // before any Responses→Chat conversion.
    let audited = match audit_original(
        security::gate::DownstreamProtocol::Responses,
        "/v1/responses",
        json.clone(),
        None,
        trace_id.clone(),
        &shared,
    )
    .await
    {
        Ok(audited) => audited,
        Err(response) => return response,
    };
    let audit_result = audited.audit_result.clone();
    let forward_json = audited.forward_json.clone();
    let request_body_str = serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default();

    if matches!(audit_result.action, security::SecurityAction::Block) {
        log_security_block(
            &repo,
            &key_record.id,
            &key_record.name,
            audited.envelope.model.clone(),
            "responses",
            is_stream,
            &audited.sanitized_log_json,
            &audit_result,
            trace_id.clone(),
        )
        .await;
        let err_body = serde_json::json!({"error": {"message": audit_result.summary, "type": "security_blocked"}});
        return (StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS, Json(err_body)).into_response();
    }

    let model = forward_json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    // T06: when `new_routeplan` is ON, route Responses through the facade —
    // G1 native `/responses` passthrough (when `native_responses` is ON) first,
    // G2 explicit `responses_via_chat_v1` debt only.  The old unconditional
    // Responses→Chat conversion is removed from this path.
    let safe_headers = crate::endpoint_executor::safe_request_headers(&headers);
    match maybe_route_plan(
        &shared,
        &repo,
        &key_record,
        &audited,
        EndpointKind::Responses,
        is_stream,
        "responses",
        &safe_headers,
        &request_body_str,
        trace_id.clone(),
    )
    .await
    {
        Ok(Some(resp)) => return resp,
        Ok(None) => {}
        Err(resp) => return resp,
    }

    // Convert (already-gated) Responses request to OpenAI Chat Completions format
    let openai_body = match protocol::responses_to_openai(&forward_json) {
        Ok(body) => body,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("request cannot be converted to chat_completions: {}", error.message),
                        "type": "invalid_request_error",
                        "code": "unsupported_features"
                    }
                })),
            )
                .into_response();
        }
    };

    if is_stream {
        handle_responses_stream(
            shared,
            openai_body,
            model,
            key_record.id,
            key_record.name,
            request_body_str,
            audit_result,
            trace_id,
        )
        .await
    } else {
        match proxy::handle_request(
            &repo,
            &shared.state.settings,
            &key_record.id,
            &key_record.name,
            openai_body,
            false,
            Some(request_body_str),
            trace_id,
            Some(&audit_result),
        )
        .await
        {
            Ok(result) => {
                // Convert OpenAI response to Responses API format.  A
                // caller-terminal upstream status keeps its status code; only
                // the body is re-framed.
                let responses_resp = protocol::openai_to_responses(&result.body, &model);
                (
                    StatusCode::from_u16(result.status).unwrap_or(StatusCode::OK),
                    Json(responses_resp),
                )
                    .into_response()
            }
            Err((code, msg)) => {
                let err_body = serde_json::json!({
                    "error": {"message": msg, "type": "upstream_error", "code": code}
                });
                (
                    StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                    Json(err_body),
                )
                    .into_response()
            }
        }
    }
}

/// Process one complete OpenAI SSE record through the Responses streaming
/// pipeline: record token usage, accumulate content/reasoning for logging, and
/// convert the record into Responses SSE events.
///
/// Both the Anthropic bridge (converted frames) and OpenAI-compatible
/// upstreams feed records here, so a single pipeline serves every protocol.
#[allow(clippy::too_many_arguments)]
fn process_openai_record_for_responses(
    record: &str,
    model: &str,
    response_id: &str,
    stream_state: &mut crate::protocol::responses::StreamState,
    accumulated_content: &mut String,
    accumulated_reasoning: &mut String,
    usage_prompt: &mut i64,
    usage_completion: &mut i64,
    usage_total: &mut i64,
) -> Vec<String> {
    if let Some((p, c, t)) = crate::protocol::responses::parse_usage_from_sse_chunk(record) {
        *usage_prompt = p;
        *usage_completion = c;
        *usage_total = t;
    }
    // Accumulate content for logging
    for line in record.lines() {
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
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            accumulated_content.push_str(content);
                        }
                        if let Some(reasoning) =
                            delta.get("reasoning_content").and_then(|c| c.as_str())
                        {
                            accumulated_reasoning.push_str(reasoning);
                        }
                    }
                }
            }
        }
    }
    crate::protocol::responses::convert_openai_sse_to_responses(
        record,
        model,
        response_id,
        accumulated_content,
        stream_state,
    )
}

/// Stream handler for Responses API.
/// Converts OpenAI SSE stream to Responses API SSE events.
///
/// Single caller (`handle_responses`); all 8 inputs are distinct immutable
/// request-scoped values.  Folding them into a struct would add boilerplate at
/// one call site for no functional gain, so the lint is scoped here.
#[allow(clippy::too_many_arguments)]
async fn handle_responses_stream(
    shared: SharedState,
    openai_body: serde_json::Value,
    model: String,
    api_key_id: String,
    api_key_name: String,
    request_body: String,
    security_result: security::SecurityScanResult,
    trace_id: Option<String>,
) -> Response {
    // Security gate already ran on the ORIGINAL Responses JSON at the entry
    // handler; `request_body` is the gate's sanitized log body.
    let forward_json = openai_body.clone();

    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));

    if matches!(security_result.action, security::SecurityAction::Block) {
        let log = crate::db::models::RequestLog {
            response_choices: None,
            id: crate::utils::id::new_id(),
            seq: None,
            api_key_id: Some(api_key_id.clone()),
            api_key_name: Some(api_key_name.clone()),
            channel_id: None,
            channel_name: None,
            model: model.clone(),
            upstream_model: None,
            mode: "responses".to_string(),
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
        let err_body = serde_json::json!({"error": {"message": security_result.summary, "type": "security_blocked"}});
        return (StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS, Json(err_body)).into_response();
    }

    let channels = match repo.get_enabled_channels().await {
        Ok(c) => c,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "No channels available").into_response()
        }
    };

    let selected_channels = Dispatcher::select_channels(&channels, &model);
    if selected_channels.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "No channel for model").into_response();
    }

    let (retry_enabled, retry_times) = proxy::get_retry_settings(&shared.state.settings);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(selected_channels.len())
    } else {
        1
    };

    let mut last_error = None;
    // Set when an upstream returned a terminal (non-retryable) status before
    // any bytes were streamed, so the loop stops and the final response keeps
    // that status instead of a generic 502.
    let mut last_error_status: Option<StatusCode> = None;

    for (attempt, channel) in selected_channels.into_iter().take(max_attempts).enumerate() {
        let config = Dispatcher::channel_to_config(&channel);
        let adaptor = get_adaptor(&channel.channel_type);
        // Bake the sampled upstream model into the body ONCE per attempt so the
        // actual request and the log share the same model (design 11.4);
        // apply_model_mapping no longer re-samples arrays.
        let upstream_model = resolve_mapped_model(&config.model_mapping, &model);
        let mut attempt_body = forward_json.clone();
        if let Some(obj) = attempt_body.as_object_mut() {
            obj.insert(
                "model".into(),
                serde_json::Value::String(upstream_model.clone()),
            );
        }
        let request = ProxyRequest {
            model: model.clone(),
            body: attempt_body,
            stream: true,
        };

        match adaptor.forward_stream(&request, &config).await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body_str = resp.text().await.unwrap_or_default();
                    last_error = Some(format!("{}: {}", channel.name, body_str));
                    match upstream_failover_decision(status.as_u16()) {
                        FailoverDecision::Failover => continue,
                        FailoverDecision::Stop { downstream_status } => {
                            // Nothing has been streamed yet — stop cycling
                            // channels; the tail returns this status (an
                            // upstream 401/403 is masked to 502; last_error
                            // above keeps the real response text).
                            last_error_status =
                                Some(StatusCode::from_u16(downstream_status).unwrap_or(status));
                            break;
                        }
                    }
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

                // Claude/Anthropic channels return Anthropic SSE from
                // forward_stream; bridge it to OpenAI SSE before conversion.
                let upstream_is_anthropic = crate::protocol::sse_bridge::is_anthropic_upstream(
                    &channel.channel_type,
                    channel.protocol.as_deref(),
                );

                let response_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
                let upstream_stream = resp.bytes_stream();

                let passthrough_stream = async_stream::stream! {
                    tokio::pin!(upstream_stream);

                    // Emit response.created event
                    let created = crate::protocol::responses::create_response_created_event(&model_clone, &response_id);
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(created.into_bytes()));

                    let mut usage_prompt: i64 = 0;
                    let mut usage_completion: i64 = 0;
                    let mut usage_total: i64 = 0;
                    let mut had_error = false;
                    let mut stream_state = crate::protocol::responses::StreamState::default();
                    let mut accumulated_content = String::new();
                    let mut accumulated_reasoning = String::new();
                    let mut sse_bridge = crate::protocol::sse_bridge::UpstreamSseBridge::for_upstream(
                        upstream_is_anthropic,
                        &upstream_model_clone,
                    );

                    while let Some(chunk_result) = upstream_stream.next().await {
                        match chunk_result {
                            Ok(bytes) => {
                                // The bridge reassembles fragmented records AND, on
                                // Anthropic channels, converts Anthropic SSE → OpenAI
                                // SSE. The downstream pipeline below only ever sees
                                // complete OpenAI `data:` records.
                                //
                                // Feed RAW bytes, never `str::from_utf8`-gated: a
                                // chunk boundary can split a multibyte UTF-8 codepoint,
                                // and the old else-branch leaked the raw Anthropic
                                // frame to the OpenAI client.  The bridge buffers bytes
                                // and decodes only complete records, so a mid-codepoint
                                // split is held and reassembled across calls.
                                match sse_bridge.push(&bytes) {
                                        Ok(records) => {
                                            for record in records {
                                                let events = process_openai_record_for_responses(
                                                    &record,
                                                    &model_clone,
                                                    &response_id,
                                                    &mut stream_state,
                                                    &mut accumulated_content,
                                                    &mut accumulated_reasoning,
                                                    &mut usage_prompt,
                                                    &mut usage_completion,
                                                    &mut usage_total,
                                                );
                                                for event in events {
                                                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(event.into_bytes()));
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            had_error = true;
                                            let err_event = format!(
                                                "event: response.failed\ndata: {{\"type\":\"response.failed\",\"response_id\":\"{}\",\"error\":{{\"message\":\"Upstream conversion failed: {}\"}}}}\n\n",
                                                response_id, e
                                            );
                                            yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_event.into_bytes()));
                                            break;
                                        }
                                }
                            }
                            Err(e) => {
                                had_error = true;
                                let err_event = format!(
                                    "event: response.failed\ndata: {{\"type\":\"response.failed\",\"response_id\":\"{}\",\"error\":{{\"message\":\"Stream interrupted: {}\"}}}}\n\n",
                                    response_id, e
                                );
                                yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_event.into_bytes()));
                                break;
                            }
                        }
                    }

                    // Stream ended. Flush any record whose terminator arrived exactly
                    // at EOF so its deltas are not lost, then emit final response.completed
                    // with usage.
                    // (convert_openai_sse_to_responses sends everything up to output_item.done,
                    // but NOT response.completed — that's sent here with usage from the final chunk)
                    // The Anthropic bridge emits its exactly-once final sequence here too
                    // (finish_reason + usage frame, then [DONE]).
                    match sse_bridge.finish() {
                        Ok(records) => {
                            for record in records {
                                let events = process_openai_record_for_responses(
                                    &record,
                                    &model_clone,
                                    &response_id,
                                    &mut stream_state,
                                    &mut accumulated_content,
                                    &mut accumulated_reasoning,
                                    &mut usage_prompt,
                                    &mut usage_completion,
                                    &mut usage_total,
                                );
                                for event in events {
                                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(event.into_bytes()));
                                }
                            }
                        }
                        Err(e) => {
                            had_error = true;
                            let err_event = format!(
                                "event: response.failed\ndata: {{\"type\":\"response.failed\",\"response_id\":\"{}\",\"error\":{{\"message\":\"Upstream conversion failed: {}\"}}}}\n\n",
                                response_id, e
                            );
                            yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_event.into_bytes()));
                        }
                    }
                    if !had_error {
                        let synth_events = crate::protocol::responses::create_synthetic_completed_events(
                            &model_clone,
                            &response_id,
                            &accumulated_content,
                            &stream_state,
                            usage_prompt,
                            usage_completion,
                        );
                        for ev in synth_events {
                            yield Ok::<_, std::io::Error>(bytes::Bytes::from(ev.into_bytes()));
                        }
                        // Emit [DONE] after response.completed
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: [DONE]\n\n"));
                    }

                    // Build response_choices for logging
                    // Fallback: estimate tokens when upstream didn't return usage.
                    if usage_total == 0 && usage_prompt == 0 && usage_completion == 0 && !had_error {
                        let req_body: serde_json::Value = serde_json::from_str(&request_body_clone).unwrap_or(serde_json::Value::Null);
                        let (p, c, t) = crate::endpoint_executor::estimate_usage::estimate_usage(&req_body, Some(&accumulated_content), &model_clone);
                        usage_prompt = p;
                        usage_completion = c;
                        usage_total = t;
                        if usage_total > 0 {
                            eprintln!("[INFO] stream token usage estimated (handlers.rs responses): prompt={}, completion={}, total={}", usage_prompt, usage_completion, usage_total);
                        }
                    }
                    let response_choices = if !accumulated_content.is_empty() || !accumulated_reasoning.is_empty() {
                        let mut msg = serde_json::json!({"role": "assistant"});
                        if !accumulated_content.is_empty() {
                            msg["content"] = serde_json::json!(accumulated_content);
                        }
                        if !accumulated_reasoning.is_empty() {
                            msg["reasoning_content"] = serde_json::json!(accumulated_reasoning);
                        }
                        Some(serde_json::to_string(&vec![serde_json::json!({
                            "index": 0,
                            "message": msg,
                            "finish_reason": "stop",
                        })]).unwrap_or_default())
                    } else { None };

                    let log = crate::db::models::RequestLog {
                        id: crate::utils::id::new_id(),
                        seq: None,
                        api_key_id: Some(api_key_id_clone.clone()),
                        api_key_name: Some(api_key_name_clone.clone()),
                        channel_id: Some(channel_id),
                        channel_name: Some(channel_name),
                        model: model_clone.clone(),
                        upstream_model: Some(upstream_model_clone),
                        mode: "responses".to_string(),
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
                    ..Default::default()
                    };
                    let log_id = log.id.clone();
                    if let Err(e) = repo_clone.create_log(&log).await { eprintln!("[WARN] create_log failed: {}", e); }
                    if let Err(e) = repo_clone.create_security_findings(&log_id, &security_result_clone.findings, security_result_clone.action.as_str()).await { eprintln!("[WARN] create_security_findings failed: {}", e); }
                    if usage_total > 0 {
                        if let Err(e) = repo_clone.increment_quota(&api_key_id_clone, usage_total).await { eprintln!("[WARN] increment_quota failed: {}", e); }
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
                    mode: "responses".to_string(),
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
                last_error = Some(format!("{}: {}", channel.name, error_message));
            }
        }
    }

    let err_body = serde_json::json!({
        "error": {
            "message": format!("All channels failed for model {} after {} attempt(s): {}", model, max_attempts, last_error.unwrap_or_else(|| "unknown".to_string())),
            "type": "upstream_error"
        }
    });
    let status = last_error_status.unwrap_or(StatusCode::BAD_GATEWAY);
    (status, Json(err_body)).into_response()
}

pub async fn handle_completions(State(_shared): State<SharedState>) -> Response {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented yet").into_response()
}

pub async fn handle_embeddings(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let body_str = String::from_utf8_lossy(&body);
    let json: serde_json::Value = match serde_json::from_str(&body_str) {
        Ok(j) => j,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid JSON: {}", e)).into_response(),
    };

    let api_key = match protocol::extract_api_key(&headers) {
        Some(k) => k,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": {"message": "Missing API key", "type": "authentication_error"}
                })),
            )
                .into_response()
        }
    };

    let repo = std::sync::Arc::new(Repository::new(shared.state.db.pool.clone()));
    let key_record = match repo.get_api_key_by_key(&api_key).await {
        Ok(k) => k,
        Err(e) if is_key_lookup_storage_error(&e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {"message": "Key lookup failed", "type": "server_error"}
                })),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": {"message": "Invalid API key", "type": "authentication_error"}
                })),
            )
                .into_response()
        }
    };

    if key_record.quota_limit > 0 && key_record.quota_used >= key_record.quota_limit {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {"message": "Quota exceeded", "type": "rate_limit_error"}
            })),
        )
            .into_response();
    }

    let model = json
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let trace_id = headers
        .get("Wali-Trace-Id")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    // Unified security audit gate — audits the ORIGINAL Embeddings JSON.
    let audited = match audit_original(
        security::gate::DownstreamProtocol::Embeddings,
        "/v1/embeddings",
        json.clone(),
        None,
        trace_id.clone(),
        &shared,
    )
    .await
    {
        Ok(audited) => audited,
        Err(response) => return response,
    };
    let security_result = audited.audit_result.clone();
    let forward_json = audited.forward_json.clone();
    let request_body_str = serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default();

    if matches!(security_result.action, security::SecurityAction::Block) {
        log_security_block(
            &repo,
            &key_record.id,
            &key_record.name,
            model.clone(),
            "embedding",
            false,
            &audited.sanitized_log_json,
            &security_result,
            trace_id.clone(),
        )
        .await;
        return (
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            Json(serde_json::json!({
                "error": {"message": security_result.summary, "type": "security_blocked"}
            })),
        )
            .into_response();
    }

    // T06: when `new_routeplan` is ON, route Embeddings through the facade
    // (OpenAI `embeddings` capability only; 501 when no such channel).
    let safe_headers = crate::endpoint_executor::safe_request_headers(&headers);
    match maybe_route_plan(
        &shared,
        &repo,
        &key_record,
        &audited,
        EndpointKind::Embeddings,
        false,
        "embedding",
        &safe_headers,
        &request_body_str,
        trace_id.clone(),
    )
    .await
    {
        Ok(Some(resp)) => return resp,
        Ok(None) => {}
        Err(resp) => return resp,
    }

    // Select channels
    let channels = match repo.get_enabled_channels().await {
        Ok(c) => c,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "No channels available").into_response()
        }
    };

    let selected_channels = Dispatcher::select_channels(&channels, &model);
    if selected_channels.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "No channel for model").into_response();
    }

    let (retry_enabled, retry_times) = proxy::get_retry_settings(&shared.state.settings);
    let max_attempts = if retry_enabled {
        (retry_times.max(0) as usize + 1).min(selected_channels.len())
    } else {
        1
    };

    let mut last_error = None;
    let start = std::time::Instant::now();
    let client = crate::adaptor::blocking_client(
        selected_channels
            .first()
            .map(|ch| ch.timeout_secs.max(1) as u64)
            .unwrap_or(60),
    );

    for (attempt, channel) in selected_channels.into_iter().take(max_attempts).enumerate() {
        let config = Dispatcher::channel_to_config(&channel);
        let upstream_model = resolve_mapped_model(&config.model_mapping, &model);

        // Build upstream embedding request — send directly to /embeddings
        // (adaptor.forward() hard-codes /chat/completions which doesn't work for embeddings)
        let base_url = config.base_url.trim_end_matches('/');
        let embed_url = format!("{}/embeddings", base_url);
        let embed_body = serde_json::json!({
            "model": upstream_model,
            "input": forward_json.get("input").cloned().unwrap_or(serde_json::Value::Null),
            "encoding_format": "float"
        });

        let result = client
            .post(&embed_url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&embed_body)
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .send()
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status();
                let resp_body: serde_json::Value =
                    resp.json().await.unwrap_or(serde_json::Value::Null);

                if !status.is_success() {
                    let error_message = format!(
                        "HTTP {}: {}",
                        status,
                        serde_json::to_string(&resp_body)
                            .unwrap_or_default()
                            .chars()
                            .take(300)
                            .collect::<String>()
                    );
                    let log = crate::db::models::RequestLog {
                        id: crate::utils::id::new_id(),
                        seq: None,
                        api_key_id: Some(key_record.id.clone()),
                        api_key_name: Some(key_record.name.clone()),
                        channel_id: Some(channel.id.clone()),
                        channel_name: Some(channel.name.clone()),
                        model: model.clone(),
                        upstream_model: Some(upstream_model.clone()),
                        mode: "embedding".to_string(),
                        status_code: status.as_u16() as i64,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                        duration_ms: start.elapsed().as_millis() as i64,
                        error_message: Some(error_message.clone()),
                        is_stream: 0,
                        is_retry: if attempt > 0 { 1 } else { 0 },
                        created_at: crate::utils::time::now_iso(),
                        request_body: Some(request_body_str.clone()),
                        response_choices: None,
                        risk_level: security_result.risk_level.as_str().to_string(),
                        risk_score: security_result.risk_score as i64,
                        risk_summary: Some(security_result.summary.clone()),
                        security_action: security_result.action.as_str().to_string(),
                        sanitized: if security_result.sanitized { 1 } else { 0 },
                        blocked_reason: security_result.blocked_reason.clone(),
                        trace_id: trace_id.clone(),
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
                    match upstream_failover_decision(status.as_u16()) {
                        FailoverDecision::Failover => continue,
                        FailoverDecision::Stop { downstream_status } => {
                            // Terminal status: the same request would fail
                            // identically on every channel — surface this
                            // response (an upstream 401/403 is masked to
                            // 502; the log above keeps the real status).
                            return (
                                StatusCode::from_u16(downstream_status)
                                    .unwrap_or(StatusCode::BAD_GATEWAY),
                                Json(resp_body),
                            )
                                .into_response();
                        }
                    }
                }

                // Extract usage from response
                let mut usage_total = resp_body
                    .get("usage")
                    .and_then(|u| u.get("total_tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as i64;
                let mut usage_prompt = resp_body
                    .get("usage")
                    .and_then(|u| u.get("prompt_tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as i64;

                // Fallback: estimate tokens when upstream didn't return usage.
                if usage_total == 0 && usage_prompt == 0 && status.is_success() {
                    let req_body: serde_json::Value =
                        serde_json::from_str(&request_body_str).unwrap_or(serde_json::Value::Null);
                    let (p, _, t) = crate::endpoint_executor::estimate_usage::estimate_usage(
                        &req_body, None, &model,
                    );
                    usage_prompt = p;
                    usage_total = t;
                    if usage_total > 0 {
                        eprintln!("[INFO] embedding token usage estimated (handlers.rs): prompt={}, total={}", usage_prompt, usage_total);
                    }
                }

                let log = crate::db::models::RequestLog {
                    id: crate::utils::id::new_id(),
                    seq: None,
                    api_key_id: Some(key_record.id.clone()),
                    api_key_name: Some(key_record.name.clone()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(upstream_model.clone()),
                    mode: "embedding".to_string(),
                    status_code: status.as_u16() as i64,
                    prompt_tokens: usage_prompt,
                    completion_tokens: 0,
                    total_tokens: usage_total,
                    duration_ms: start.elapsed().as_millis() as i64,
                    error_message: None,
                    is_stream: 0,
                    is_retry: if attempt > 0 { 1 } else { 0 },
                    created_at: crate::utils::time::now_iso(),
                    request_body: Some(request_body_str.clone()),
                    response_choices: None,
                    risk_level: security_result.risk_level.as_str().to_string(),
                    risk_score: security_result.risk_score as i64,
                    risk_summary: Some(security_result.summary.clone()),
                    security_action: security_result.action.as_str().to_string(),
                    sanitized: if security_result.sanitized { 1 } else { 0 },
                    blocked_reason: security_result.blocked_reason.clone(),
                    trace_id: trace_id.clone(),
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
                if usage_total > 0 {
                    if let Err(e) = repo.increment_quota(&key_record.id, usage_total).await {
                        eprintln!("[WARN] increment_quota failed: {}", e);
                    }
                }

                return (StatusCode::OK, Json(resp_body)).into_response();
            }
            Err(e) => {
                let error_message = e.to_string();
                let log = crate::db::models::RequestLog {
                    id: crate::utils::id::new_id(),
                    seq: None,
                    api_key_id: Some(key_record.id.clone()),
                    api_key_name: Some(key_record.name.clone()),
                    channel_id: Some(channel.id.clone()),
                    channel_name: Some(channel.name.clone()),
                    model: model.clone(),
                    upstream_model: Some(upstream_model.clone()),
                    mode: "embedding".to_string(),
                    status_code: 502,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    duration_ms: start.elapsed().as_millis() as i64,
                    error_message: Some(error_message.clone()),
                    is_stream: 0,
                    is_retry: if attempt > 0 { 1 } else { 0 },
                    created_at: crate::utils::time::now_iso(),
                    request_body: Some(request_body_str.clone()),
                    response_choices: None,
                    risk_level: security_result.risk_level.as_str().to_string(),
                    risk_score: security_result.risk_score as i64,
                    risk_summary: Some(security_result.summary.clone()),
                    security_action: security_result.action.as_str().to_string(),
                    sanitized: if security_result.sanitized { 1 } else { 0 },
                    blocked_reason: security_result.blocked_reason.clone(),
                    trace_id: trace_id.clone(),
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

    let err_body = serde_json::json!({
        "error": {
            "message": format!("All channels failed for embedding model {} after {} attempt(s): {}", model, max_attempts, last_error.unwrap_or_else(|| "unknown".to_string())),
            "type": "upstream_error"
        }
    });
    (StatusCode::BAD_GATEWAY, Json(err_body)).into_response()
}

/// True when the request came from an Anthropic client: it authenticates with
/// `x-api-key` and sends no `Authorization: Bearer`. Matches the downstream
/// protocol selection rule (both present → OpenAI, keeping existing behavior).
fn request_is_anthropic(headers: &HeaderMap) -> bool {
    headers.contains_key("x-api-key") && !headers.contains_key("authorization")
}

/// One model exposed to downstream clients: its public `id` plus the channel
/// type of the first channel that listed it (kept for the OpenAI `owned_by`).
#[derive(Debug, Clone, PartialEq)]
struct ConfigModel {
    id: String,
    owned_by: String,
}

/// Aggregate configured models across enabled channels, deduped:
/// each channel's `models` list, then the keys of its `model_mapping`
/// (mapping values are upstream model names and are NOT exposed).
fn collect_config_models(channels: &[crate::db::models::Channel]) -> Vec<ConfigModel> {
    let mut out: Vec<ConfigModel> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for ch in channels {
        let ch_models: Vec<String> = serde_json::from_str(&ch.models).unwrap_or_default();
        for m in ch_models {
            if seen.insert(m.clone()) {
                out.push(ConfigModel {
                    id: m,
                    owned_by: ch.channel_type.clone(),
                });
            }
        }
        let mapping: serde_json::Value = serde_json::from_str(&ch.model_mapping)
            .unwrap_or(serde_json::Value::Object(Default::default()));
        if let Some(obj) = mapping.as_object() {
            for key in obj.keys() {
                if seen.insert(key.clone()) {
                    out.push(ConfigModel {
                        id: key.clone(),
                        owned_by: ch.channel_type.clone(),
                    });
                }
            }
        }
    }
    out
}

/// Aggregate models from auth accounts: each account's synced model snapshot
/// (only entries with `status == "available"` and `!unavailable`) and the
/// source keys of its `model_mapping` (mapping values are upstream names and
/// are NOT exposed).  `owned_by` is the account provider.  Dedup is shared with
/// the channel aggregator via `seen` so a model advertised by both a channel
/// and an account is listed once (channel wins, preserving `owned_by`).
fn collect_auth_account_models(
    accounts: &[crate::db::models::AuthAccount],
    seen: &mut std::collections::HashSet<String>,
) -> Vec<ConfigModel> {
    let mut out: Vec<ConfigModel> = Vec::new();
    for account in accounts {
        if let Ok(states) = account.model_states() {
            for state in &states.models {
                if state.status == "available"
                    && !state.unavailable
                    && seen.insert(state.id.clone())
                {
                    out.push(ConfigModel {
                        id: state.id.clone(),
                        owned_by: account.provider.clone(),
                    });
                }
            }
        }
        if let Ok(mapping) = account.model_mapping() {
            if let Some(obj) = mapping.as_object() {
                for key in obj.keys() {
                    if seen.insert(key.clone()) {
                        out.push(ConfigModel {
                            id: key.clone(),
                            owned_by: account.provider.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// OpenAI `/v1/models` response body: `{"object":"list","data":[...]}`.
fn openai_models_response(models: &[ConfigModel]) -> serde_json::Value {
    let data: Vec<serde_json::Value> = models
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "object": "model",
                "created": chrono::Utc::now().timestamp(),
                "owned_by": m.owned_by,
            })
        })
        .collect();
    serde_json::json!({ "object": "list", "data": data })
}

/// Anthropic `/v1/models` response body: `{"data":[{"type":"model","id",...}]}`.
fn anthropic_models_response(models: &[ConfigModel]) -> serde_json::Value {
    let data: Vec<serde_json::Value> = models
        .iter()
        .map(|m| {
            serde_json::json!({
                "type": "model",
                "id": m.id,
                "display_name": m.id,
                "created_at": crate::utils::time::now_iso(),
            })
        })
        .collect();
    serde_json::json!({ "data": data })
}

/// OpenAI-style error body: `{"error":{"message","type","code"}}`.
fn openai_error(status: StatusCode, message: impl Into<String>, kind: &str) -> Response {
    let message = message.into();
    let code = status.as_u16().to_string();
    (
        status,
        Json(serde_json::json!({
            "error": { "message": message, "type": kind, "code": code }
        })),
    )
        .into_response()
}

/// Build an error in the caller's protocol format (Anthropic vs OpenAI).
fn models_error(
    anthropic: bool,
    status: StatusCode,
    kind: &str,
    message: impl Into<String>,
) -> Response {
    if anthropic {
        anthropic_error(status, kind, message)
    } else {
        openai_error(status, message, kind)
    }
}

/// Core of `/v1/models`: auth, quota, configured-model aggregation, then a
/// response in the caller's protocol format. Extractable because it never
/// needs the Tauri `AppHandle` — only a SQLite pool.
async fn list_models_impl(pool: SqlitePool, headers: &HeaderMap) -> Response {
    let anthropic = request_is_anthropic(headers);
    let api_key = match protocol::extract_api_key(headers) {
        Some(key) => key,
        None => {
            return models_error(
                anthropic,
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Missing API key",
            )
        }
    };
    let repo = Repository::new(pool);
    let key = match repo.get_api_key_by_key(&api_key).await {
        Ok(key) => key,
        Err(e) if is_key_lookup_storage_error(&e) => {
            return models_error(
                anthropic,
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "Key lookup failed",
            )
        }
        Err(_) => {
            return models_error(
                anthropic,
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Invalid API key",
            )
        }
    };
    if key.quota_limit > 0 && key.quota_used >= key.quota_limit {
        return models_error(
            anthropic,
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "Quota exceeded",
        );
    }
    let channels = match repo.get_enabled_channels().await {
        Ok(channels) => channels,
        Err(_) => {
            return models_error(
                anthropic,
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "Failed to load channels",
            )
        }
    };
    let accounts = match repo.list_active_auth_accounts().await {
        Ok(accounts) => accounts,
        Err(_) => {
            return models_error(
                anthropic,
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "Failed to load auth accounts",
            )
        }
    };
    let mut seen = std::collections::HashSet::new();
    let mut models = collect_config_models(&channels);
    // Track channel-advertised IDs so auth-account duplicates are skipped.
    for m in &models {
        seen.insert(m.id.clone());
    }
    models.extend(collect_auth_account_models(&accounts, &mut seen));
    let body = if anthropic {
        anthropic_models_response(&models)
    } else {
        openai_models_response(&models)
    };
    (StatusCode::OK, Json(body)).into_response()
}

pub async fn handle_list_models(State(shared): State<SharedState>, headers: HeaderMap) -> Response {
    list_models_impl(shared.state.db.pool.clone(), &headers).await
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
    let port = *shared.state.server_port.read().await;
    let running = shared
        .state
        .server_running
        .load(std::sync::atomic::Ordering::SeqCst);
    Json(serde_json::json!({
        "status": "ok",
        "running": running,
        "port": port,
        "url": format!("http://127.0.0.1:{}", port),
    }))
    .into_response()
}

#[cfg(test)]
mod anthropic_handler_tests {
    use super::*;

    #[test]
    fn native_forwarding_keeps_anthropic_headers_and_only_maps_model() {
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        headers.insert("anthropic-beta", "prompt-caching".parse().unwrap());
        headers.insert("x-api-key", "local-only-key".parse().unwrap());
        headers.insert("authorization", "Bearer caller-secret".parse().unwrap());
        headers.insert("cookie", "session=caller-secret".parse().unwrap());
        headers.insert("x-anthropic-future-feature", "on".parse().unwrap());
        let kept = forwarded_anthropic_headers(&headers);
        assert!(kept.iter().any(|(name, _)| name == "anthropic-version"));
        assert!(kept.iter().any(|(name, _)| name == "anthropic-beta"));
        assert!(kept
            .iter()
            .any(|(name, _)| name == "x-anthropic-future-feature"));
        assert!(!kept
            .iter()
            .any(|(name, _)| name == "x-api-key" || name == "authorization" || name == "cookie"));
        let body = serde_json::json!({"model":"public-model", "system":[{"type":"thinking"}], "messages":[]});
        let (mapped, upstream_model) =
            mapped_anthropic_body(&body, &serde_json::json!({"public-model":"upstream-model"}));
        assert_eq!(mapped["model"], "upstream-model");
        assert_eq!(mapped["system"], body["system"]);
        // T09: the SAME sampled model is returned for the log (design 11.4).
        assert_eq!(upstream_model, "upstream-model");
    }

    #[test]
    fn mapped_anthropic_body_supports_array_mapping() {
        let body = serde_json::json!({"model":"auto", "messages":[]});
        let mapping = serde_json::json!({"auto":["model-a", "model-b"]});
        let (mapped, upstream_model) = mapped_anthropic_body(&body, &mapping);
        let result = mapped["model"].as_str().unwrap();
        assert!(result == "model-a" || result == "model-b");
        // T09: request body model == logged upstream_model (single sample).
        assert_eq!(upstream_model, result);
    }

    #[tokio::test]
    async fn maps_openai_rate_limits_without_exposing_channel_auth_as_client_auth() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "17".parse().unwrap());
        let rate_limited =
            openai_error_response(StatusCode::TOO_MANY_REQUESTS, "slow down", &headers);
        assert_eq!(rate_limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rate_limited.headers()[header::RETRY_AFTER], "17");
        let rate_body = axum::body::to_bytes(rate_limited.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(std::str::from_utf8(&rate_body)
            .unwrap()
            .contains("rate_limit_error"));

        let auth_failed = openai_error_response(
            StatusCode::UNAUTHORIZED,
            "upstream key rejected",
            &reqwest::header::HeaderMap::new(),
        );
        assert_eq!(auth_failed.status(), StatusCode::BAD_GATEWAY);
        let auth_body = axum::body::to_bytes(auth_failed.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(std::str::from_utf8(&auth_body)
            .unwrap()
            .contains("api_error"));
    }

    #[test]
    fn reads_native_message_and_sse_usage_without_changing_payload() {
        assert_eq!(
            native_usage(br#"{"usage":{"input_tokens":12,"output_tokens":4}}"#, false),
            Some((12, 4, 0))
        );
        let sse = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_creation_input_tokens\":2,\"cache_read_input_tokens\":3}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        assert_eq!(native_usage(sse, true), Some((17, 4, 3)));
        assert_eq!(native_usage(&sse[..sse.len() - 48], true), None);

        let mut incremental = NativeSseUsageParser::default();
        for piece in sse.chunks(7) {
            incremental.feed(piece);
        }
        assert_eq!(incremental.finish(), Some((17, 4, 3)));
        let mut oversized = NativeSseUsageParser::default();
        oversized.feed(&vec![b'x'; MAX_NATIVE_SSE_RECORD_BYTES + 1]);
        assert!(oversized.finish().is_none());
    }

    #[test]
    fn preserves_query_beta_for_both_native_message_paths() {
        let config = crate::adaptor::ChannelConfig {
            base_url: "https://upstream.example/v1/".to_string(),
            api_key: "key".to_string(),
            models: vec![],
            model_mapping: serde_json::json!({}),
            extra: serde_json::json!({}),
            timeout_secs: 60,
        };
        assert_eq!(
            native_anthropic_url(&config, "messages", Some("beta=true")),
            "https://upstream.example/v1/messages?beta=true"
        );
        assert_eq!(
            native_anthropic_url(&config, "messages/count_tokens", Some("beta=true")),
            "https://upstream.example/v1/messages/count_tokens?beta=true"
        );
    }

    #[test]
    fn always_redacts_log_body_even_when_forwarding_redaction_is_off() {
        let request = serde_json::json!({"messages":[{"role":"user","content":"sk-abcdefghijklmnopqrstuvwx123456"}]});
        let (body, sanitized) = sanitized_anthropic_log_body(&request);
        assert!(sanitized);
        assert!(!body.unwrap().contains("abcdefghijklmnopqrstuvwx"));
    }

    #[test]
    fn images_audio_placeholders_stay_early_rejected_and_must_use_gate_once_enabled() {
        // T03 guard (STRUCTURAL, compiler-enforced): every content-bearing
        // entry — including the not-yet-enabled Images/Audio 501 placeholders —
        // must route through `security::gate::gate_dispatch`.  That function
        // holds an exhaustive `match` over ALL `DownstreamProtocol` variants
        // with no wildcard arm: adding a new variant (or enabling Images/Audio
        // without wiring the gate) is a COMPILE ERROR there, so a newly-enabled
        // handler cannot forward model content without the audit by accident.
        //
        // This test exercises the dispatch for every variant so the checklist
        // stays live, and proves the Images/Audio variants audit a clean body
        // through the gate today (while their HTTP handlers are still 501).
        use crate::security::gate::{gate_dispatch, DownstreamProtocol};
        let settings = crate::security::SecuritySettings::default();
        for protocol in [
            DownstreamProtocol::Images,
            DownstreamProtocol::Audio,
            DownstreamProtocol::ChatCompletions,
            DownstreamProtocol::Completions,
            DownstreamProtocol::Responses,
            DownstreamProtocol::Messages,
            DownstreamProtocol::CountTokens,
            DownstreamProtocol::Embeddings,
        ] {
            let audited = gate_dispatch(
                protocol,
                "/v1/gate-dispatch-test",
                serde_json::json!({"model": "m"}),
                None,
                "m".to_string(),
                false,
                None,
                &settings,
                None,
                vec![],
            )
            .unwrap();
            assert_eq!(audited.envelope.downstream_protocol, protocol);
            assert!(audited.body_hash.len() >= 64);
            // Sanitized log body is a JSON object for a clean body.
            assert!(audited.sanitized_log_json.is_object());
        }
    }

    #[test]
    fn raw_request_body_is_never_the_persisted_log_value() {
        // The gate's sanitized_log_json must differ from the raw body when a
        // secret is present — persistence only ever receives the sanitized
        // log body.
        let raw = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "Bearer sk-abcdefghijklmnopqrstuvwx123456"}]});
        let audited = crate::security::gate::gate_original(
            crate::security::gate::DownstreamProtocol::ChatCompletions,
            "/v1/chat/completions",
            raw.clone(),
            None,
            "m".to_string(),
            false,
            None,
            &crate::security::SecuritySettings::default(),
            None,
            vec![],
        )
        .unwrap();
        let raw_str = serde_json::to_string(&raw).unwrap();
        let log_str = serde_json::to_string(&audited.sanitized_log_json).unwrap();
        assert!(raw_str.contains("sk-abcdefghijklmnopqrstuvwx123456"));
        assert!(!log_str.contains("sk-abcdefghijklmnopqrstuvwx123456"));
    }
}

#[cfg(test)]
mod list_models_tests {
    use super::*;
    use crate::db::models::{ApiKey, CreateApiKeyInput, CreateChannelInput};
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn detects_anthropic_only_when_x_api_key_without_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "sk-test".parse().unwrap());
        assert!(request_is_anthropic(&headers));

        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer sk-test".parse().unwrap());
        assert!(!request_is_anthropic(&headers));

        // 极端情况：两法都带 key → 保持现有行为，按 OpenAI
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "sk-test".parse().unwrap());
        headers.insert("authorization", "Bearer sk-test".parse().unwrap());
        assert!(!request_is_anthropic(&headers));

        let headers = HeaderMap::new();
        assert!(!request_is_anthropic(&headers));
    }

    fn channel(
        name: &str,
        ch_type: &str,
        models: &[&str],
        mapping: serde_json::Value,
    ) -> crate::db::models::Channel {
        crate::db::models::Channel {
            id: name.to_string(),
            name: name.to_string(),
            channel_type: ch_type.to_string(),
            base_url: "http://example.com".to_string(),
            api_key: "k".to_string(),
            models: serde_json::to_string(models).unwrap(),
            status: 1,
            priority: 0,
            weight: 1,
            config: "{}".to_string(),
            model_mapping: mapping.to_string(),
            timeout_secs: 60,
            protocol: None,
            provider: None,
            native_base_url: None,
            native_endpoints: None,
            preset_revision: None,
            identity_revision: 0,
            legacy_executor_override: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
            last_test_at: None,
            last_test_ok: None,
        }
    }

    #[test]
    fn aggregates_models_and_mapping_keys_with_dedup() {
        let channels = vec![
            channel(
                "a",
                "openai",
                &["gpt-4o", "gpt-4o-mini"],
                serde_json::json!({"gpt-4o": "upstream-x"}),
            ),
            channel(
                "b",
                "claude",
                &["claude-sonnet-4"],
                serde_json::json!({"gpt-4o": "claude-sonnet-5", "claude-35": "claude-3-5-sonnet"}),
            ),
        ];
        let models = collect_config_models(&channels);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        // a.models → gpt-4o, gpt-4o-mini；a.mapping keys → gpt-4o(重复跳过)；
        // b.models → claude-sonnet-4；b.mapping keys → gpt-4o(重复跳过), claude-35
        assert_eq!(
            ids,
            vec!["gpt-4o", "gpt-4o-mini", "claude-sonnet-4", "claude-35"]
        );
        // 首个列出该模型的渠道胜出（owned_by 为 openai，而非 claude）
        assert_eq!(models[0].owned_by, "openai");
    }

    #[test]
    fn mapping_values_are_never_listed_as_models() {
        // value（"upstream-y"）是上游实际模型，不参与列出
        let channels = vec![channel(
            "a",
            "openai",
            &["real-a"],
            serde_json::json!({"alias": "upstream-y"}),
        )];
        let models = collect_config_models(&channels);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["real-a", "alias"]);
    }

    #[test]
    fn builds_openai_models_response() {
        let models = vec![
            ConfigModel {
                id: "gpt-4o".to_string(),
                owned_by: "openai".to_string(),
            },
            ConfigModel {
                id: "claude-35".to_string(),
                owned_by: "claude".to_string(),
            },
        ];
        let value = openai_models_response(&models);
        assert_eq!(value["object"], "list");
        let data = value["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["id"], "gpt-4o");
        assert_eq!(data[0]["object"], "model");
        assert_eq!(data[0]["owned_by"], "openai");
        assert!(data[0]["created"].is_number());
    }

    #[test]
    fn builds_anthropic_models_response() {
        let models = vec![ConfigModel {
            id: "claude-sonnet-4".to_string(),
            owned_by: "claude".to_string(),
        }];
        let value = anthropic_models_response(&models);
        let data = value["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["type"], "model");
        assert_eq!(data[0]["id"], "claude-sonnet-4");
        assert_eq!(data[0]["display_name"], "claude-sonnet-4");
        assert!(data[0]["created_at"].as_str().is_some());
    }

    #[tokio::test]
    async fn builds_openai_style_errors() {
        let resp = openai_error(
            StatusCode::UNAUTHORIZED,
            "Missing API key",
            "authentication_error",
        );
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["type"], "authentication_error");
        assert_eq!(json["error"]["message"], "Missing API key");
        assert_eq!(json["error"]["code"], "401");
    }

    #[tokio::test]
    async fn models_error_dispatches_by_protocol() {
        let resp = models_error(
            true,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "nope",
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["type"], "error"); // Anthropic wrapper

        let resp = models_error(
            false,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "nope",
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["type"], "authentication_error");
        assert_eq!(json["error"]["code"], "401");
    }

    async fn seed_test_db() -> (SqlitePool, ApiKey) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let repo = Repository::new(pool.clone());
        repo.create_channel(&CreateChannelInput {
            name: "ch-a".to_string(),
            channel_type: "openai".to_string(),
            base_url: "http://example.com".to_string(),
            api_key: "upstream".to_string(),
            models: vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
            priority: Some(0),
            weight: Some(1),
            config: None,
            model_mapping: Some(serde_json::json!({
                "alias-1": "gpt-4o",
                "alias-2": ["gpt-4o", "gpt-4o-mini"],
            })),
            timeout_secs: Some(60),
            protocol: None,
            provider: None,
            native_base_url: None,
            native_endpoints: None,
            preset_revision: None,
            legacy_executor_override: None,
            test_run_id: None,
            draft_fingerprint: None,
            force_save: None,
            extra_keys: None,
        })
        .await
        .unwrap();
        let api_key = repo
            .create_api_key(&CreateApiKeyInput {
                name: "test-key".to_string(),
                allowed_models: None,
                allowed_channels: None,
                denied_models: None,
                denied_channels: None,
                quota_limit: Some(1000),
                expires_at: None,
            })
            .await
            .unwrap();
        (pool, api_key)
    }

    #[tokio::test]
    async fn openai_bearer_returns_openai_format() {
        let (pool, api_key) = seed_test_db().await;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", api_key.key).parse().unwrap(),
        );
        let resp = list_models_impl(pool, &headers).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["object"], "list");
        let ids: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        // models: gpt-4o, gpt-4o-mini；mapping keys: alias-1, alias-2 → 4 个，去重后无重复
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"gpt-4o"));
        assert!(ids.contains(&"gpt-4o-mini"));
        assert!(ids.contains(&"alias-1"));
        assert!(ids.contains(&"alias-2"));
        assert!(json["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["object"] == "model"));
    }

    #[tokio::test]
    async fn anthropic_x_api_key_returns_anthropic_format() {
        let (pool, api_key) = seed_test_db().await;
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", api_key.key.parse().unwrap());
        let resp = list_models_impl(pool, &headers).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let data = json["data"].as_array().unwrap();
        assert!(!data.is_empty());
        assert!(data.iter().all(|m| m["type"] == "model"));
        assert!(data.iter().all(|m| m["id"].as_str().is_some()));
    }

    #[tokio::test]
    async fn missing_key_returns_401_openai_format() {
        let (pool, _) = seed_test_db().await;
        let resp = list_models_impl(pool, &HeaderMap::new()).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["type"], "authentication_error");
        assert_eq!(json["error"]["code"], "401");
    }

    #[tokio::test]
    async fn invalid_key_returns_401() {
        let (pool, _) = seed_test_db().await;
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer sk-wrong".parse().unwrap());
        let resp = list_models_impl(pool, &headers).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invalid_key_anthropic_returns_anthropic_error_format() {
        let (pool, _) = seed_test_db().await;
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "sk-wrong".parse().unwrap());
        let resp = list_models_impl(pool, &headers).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["error"]["type"], "authentication_error");
    }

    #[tokio::test]
    async fn quota_exceeded_returns_429() {
        let (pool, api_key) = seed_test_db().await;
        Repository::new(pool.clone())
            .increment_quota(&api_key.id, 2000)
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", api_key.key).parse().unwrap(),
        );
        let resp = list_models_impl(pool, &headers).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"]["type"], "rate_limit_error");
        assert_eq!(json["error"]["code"], "429");
    }

    /// Insert an auth account directly via SQL (mirrors migration 019/021
    /// schema) so `list_models_impl` can surface its models.
    async fn seed_auth_account(
        pool: &SqlitePool,
        id: &str,
        provider: &str,
        model_states: serde_json::Value,
        mapping: serde_json::Value,
    ) {
        let now = "2026-01-01T00:00:00.000Z";
        sqlx::query(
            "INSERT INTO auth_accounts
             (id, provider, label, account_id, status, disabled, priority, weight,
              quota_json, model_states_json, model_mapping_json, attributes_json,
              payload_json, last_refreshed_at, last_models_sync_at,
              next_refresh_after, next_retry_after, created_at, updated_at)
             VALUES (?, ?, ?, ?, 'active', 0, 1, 1, NULL, ?, ?, '{}',
                     '{}', NULL, NULL, NULL, NULL, ?, ?)",
        )
        .bind(id)
        .bind(provider)
        .bind(format!("label-{id}"))
        .bind(format!("remote-{id}"))
        .bind(model_states.to_string())
        .bind(mapping.to_string())
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn auth_account_available_models_are_listed() {
        let (pool, api_key) = seed_test_db().await;
        seed_auth_account(
            &pool,
            "acc-1",
            "codex",
            serde_json::json!({
                "version": 1,
                "models": [
                    {"id": "gpt-5-codex", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null},
                    {"id": "o4-mini", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null}
                ]
            }),
            serde_json::json!({}),
        )
        .await;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", api_key.key).parse().unwrap(),
        );
        let resp = list_models_impl(pool, &headers).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let ids: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        // Channel models (gpt-4o, gpt-4o-mini, alias-1, alias-2) + auth-account
        // models (gpt-5-codex, o4-mini).
        assert!(ids.contains(&"gpt-5-codex"));
        assert!(ids.contains(&"o4-mini"));
        assert!(ids.contains(&"gpt-4o"));
        // Auth-account model keeps channel models intact.
        assert!(ids.contains(&"alias-1"));
        assert_eq!(ids.len(), 6);
        // owned_by reflects the auth-account provider.
        let codex = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "gpt-5-codex")
            .unwrap();
        assert_eq!(codex["owned_by"], "codex");
    }

    #[tokio::test]
    async fn auth_account_unavailable_models_are_skipped() {
        let (pool, api_key) = seed_test_db().await;
        seed_auth_account(
            &pool,
            "acc-2",
            "codex",
            serde_json::json!({
                "version": 1,
                "models": [
                    {"id": "good", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null},
                    {"id": "bad-status", "status": "disabled", "unavailable": false,
                     "next_retry_after": null, "last_error": null},
                    {"id": "bad-unavail", "status": "available", "unavailable": true,
                     "next_retry_after": null, "last_error": null}
                ]
            }),
            serde_json::json!({}),
        )
        .await;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", api_key.key).parse().unwrap(),
        );
        let resp = list_models_impl(pool, &headers).await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let ids: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"good"));
        assert!(!ids.contains(&"bad-status"));
        assert!(!ids.contains(&"bad-unavail"));
    }

    #[tokio::test]
    async fn auth_account_mapping_keys_are_listed_and_deduped() {
        let (pool, api_key) = seed_test_db().await;
        seed_auth_account(
            &pool,
            "acc-3",
            "kimi",
            serde_json::json!({
                "version": 1,
                "models": [
                    {"id": "kimi-k2", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null}
                ]
            }),
            // "gpt-4o" duplicates a channel model → skipped; "kimi-alias" is new.
            serde_json::json!({"gpt-4o": "kimi-k2", "kimi-alias": "kimi-k2"}),
        )
        .await;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", api_key.key).parse().unwrap(),
        );
        let resp = list_models_impl(pool, &headers).await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let ids: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"kimi-k2"));
        assert!(ids.contains(&"kimi-alias"));
        // No duplicate gpt-4o.
        assert_eq!(ids.iter().filter(|&&m| m == "gpt-4o").count(), 1);
        // owned_by of the deduped gpt-4o stays with the channel (openai).
        let gpt4o = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "gpt-4o")
            .unwrap();
        assert_eq!(gpt4o["owned_by"], "openai");
    }

    #[tokio::test]
    async fn disabled_auth_account_models_are_not_listed() {
        let (pool, api_key) = seed_test_db().await;
        // Insert a disabled account directly.
        let now = "2026-01-01T00:00:00.000Z";
        sqlx::query(
            "INSERT INTO auth_accounts
             (id, provider, label, account_id, status, disabled, priority, weight,
              quota_json, model_states_json, model_mapping_json, attributes_json,
              payload_json, last_refreshed_at, last_models_sync_at,
              next_refresh_after, next_retry_after, created_at, updated_at)
             VALUES ('acc-off', 'codex', 'off', 'remote-off', 'active', 1, 1, 1,
                     NULL, ?, '{}', '{}', '{}', NULL, NULL, NULL, NULL, ?, ?)",
        )
        .bind(
            serde_json::json!({
                "version": 1,
                "models": [{"id": "should-not-show", "status": "available",
                            "unavailable": false, "next_retry_after": null,
                            "last_error": null}]
            })
            .to_string(),
        )
        .bind(now)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", api_key.key).parse().unwrap(),
        );
        let resp = list_models_impl(pool, &headers).await;
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let ids: Vec<&str> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(!ids.contains(&"should-not-show"));
    }
}
