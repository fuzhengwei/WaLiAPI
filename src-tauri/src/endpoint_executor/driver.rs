//! Streaming plan driver (T06).
//!
//! Ties the T05 [`RoutePlan`] / [`AttemptFlow`] to the transport executors and
//! the pure [`StreamPumpCore`] commit barrier, and writes the RequestLog +
//! quota accounting on the facade path (T05 handoff #3).
//!
//! Flow per attempt:
//! ```text
//! connect → dispatch_stream_executor → (2xx) → read+buffer first SSE record →
//! validate → FirstFrameBufferedAndValidated → commit_downstream →
//! begin_streaming → pump bytes → complete | abort
//! ```
//! Pre-commit failures (connect / first-frame invalid / 4xx-5xx) run back through
//! [`AttemptFlow`] so the next candidate may be tried; post-commit errors only
//! emit a protocol-representable error, never a retry.

use crate::core::attempt::{
    build_prepared_attempt, AttemptFailure, AttemptFlow, FailureClass, FlowStep,
};
use crate::core::channel_identity::{resolve_channel_identity, ChannelIdentityRow};
use crate::core::route_plan::{EndpointKind, RouteCandidate, RoutePlan};
use crate::db::models::{ApiKey, Channel, RequestLog};
use crate::db::repository::Repository;
use crate::endpoint_executor::sse::StreamPumpCore;
use crate::endpoint_executor::{
    dispatch_auth_account_executor, dispatch_auth_account_stream_executor, dispatch_executor,
    dispatch_stream_executor, next_upstream_item, StreamAttemptResult, UpstreamItem,
    UpstreamStream,
};
use crate::security;
use crate::security::gate::AuditedRequest;
use crate::utils;
use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{self, DateTime, SecondsFormat, Utc};
use futures_util::StreamExt;
use rand::Rng;
use rand::SeedableRng;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Extract the client-requested reasoning effort from the ORIGINAL downstream
/// request body, per downstream protocol:
/// - Chat Completions: top-level `reasoning_effort` string.
/// - Responses: `reasoning.effort` (fallback: top-level `reasoning_effort`).
/// - Anthropic Messages: `thinking` config mapped to an effort level.
/// Returns `None` when the client did not specify any reasoning preference.
fn extract_reasoning_effort(audited: &AuditedRequest) -> Option<String> {
    use crate::security::gate::DownstreamProtocol;
    let body = &audited.envelope.original_json;
    match audited.envelope.downstream_protocol {
        DownstreamProtocol::ChatCompletions | DownstreamProtocol::Completions => body
            .get("reasoning_effort")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        DownstreamProtocol::Responses => body
            .get("reasoning")
            .and_then(|r| r.get("effort"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                body.get("reasoning_effort")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            }),
        DownstreamProtocol::Messages => {
            crate::protocol::codec::messages::anthropic_thinking_to_reasoning_effort(body)
        }
        _ => None,
    }
}

/// Freeze the enabled credential slots for one request and order them by
/// weighted sampling without replacement.  The returned `Channel` copies are
/// intentionally request-local: credential values never enter PreparedAttempt,
/// request logs, or tracing fields.
async fn channel_key_slots(channel: &Channel, repo: &Arc<Repository>) -> Vec<Channel> {
    let extra_keys = match repo.get_channel_api_keys(&channel.id).await {
        Ok(keys) => keys
            .into_iter()
            .filter(|k| k.status == 1)
            .collect::<Vec<_>>(),
        Err(_) => return vec![channel.clone()],
    };
    if extra_keys.is_empty() {
        return vec![channel.clone()];
    }
    // Build weighted pool: primary key (weight = channel.weight) + extras.
    let mut pool: Vec<(String, i64)> = Vec::new();
    if !channel.api_key.is_empty() {
        pool.push((channel.api_key.clone(), channel.weight.max(1)));
    }
    for k in &extra_keys {
        pool.push((k.api_key.clone(), k.weight.max(1)));
    }
    if pool.is_empty() {
        return vec![channel.clone()];
    }
    // Duplicate values are one credential slot, not two chances to send the
    // same secret. Preserve the first configured weight for deterministic
    // de-duplication without ever exposing the value outside this function.
    let mut unique = Vec::new();
    for item in pool {
        if !unique.iter().any(|(key, _): &(String, i64)| key == &item.0) {
            unique.push(item);
        }
    }
    let mut slots = Vec::with_capacity(unique.len());
    while !unique.is_empty() {
        let chosen = crate::core::weighted_key::pick_weighted_key(&unique)
            .unwrap_or_else(|| unique[0].0.clone());
        if let Some(index) = unique.iter().position(|(key, _)| key == &chosen) {
            unique.remove(index);
        } else {
            break;
        }
        let mut slot = channel.clone();
        slot.api_key = chosen;
        slots.push(slot);
    }
    slots
}

/// Try every frozen credential slot only while the failure is retryable.
/// This makes capacity envelopes useful for a channel with multiple keys while
/// retaining the route planner's terminal/auth boundaries.
async fn dispatch_channel_with_key_failover(
    endpoint: EndpointKind,
    attempt: &crate::core::attempt::PreparedAttempt,
    channel: &Channel,
    identity: &crate::core::channel_identity::ChannelIdentity,
    safe_headers: &[(String, String)],
    query: Option<&str>,
    repo: &Arc<Repository>,
) -> crate::core::attempt::AttemptResult {
    let slots = channel_key_slots(channel, repo).await;
    let mut last = None;
    // Keep credential expansion bounded by the same conservative default as
    // RoutePlan's per-group budget. The planner still owns cross-candidate and
    // total-attempt limits; this cap prevents a channel with dozens of keys
    // from bypassing those limits in one closure invocation.
    for slot in slots.into_iter().take(3) {
        let result =
            dispatch_executor(endpoint, attempt, &slot, identity, safe_headers, query).await;
        match result {
            crate::core::attempt::AttemptResult::Failure(ref failure)
                if failure.failure_class == FailureClass::Retryable =>
            {
                last = Some(result)
            }
            _ => return result,
        }
    }
    last.unwrap_or_else(|| {
        crate::core::attempt::AttemptResult::Failure(AttemptFailure {
            failure_class: FailureClass::Retryable,
            message: "no enabled channel credential slot".to_string(),
            status_code: Some(502),
            retry_after: None,
        })
    })
}

fn candidate_lookup(plan: &RoutePlan) -> HashMap<String, RouteCandidate> {
    plan.groups
        .iter()
        .flat_map(|group| group.candidates.iter())
        .map(|candidate| {
            (
                candidate.candidate.id().to_owned(),
                candidate.candidate.clone(),
            )
        })
        .collect()
}

fn missing_candidate_failure(candidate_id: &str) -> AttemptFailure {
    AttemptFailure {
        failure_class: FailureClass::CallerTerminal,
        message: format!("route plan candidate lookup failed for {candidate_id}"),
        status_code: Some(500),
        retry_after: None,
    }
}

const MODE_FAILURE_COOLDOWN_MINUTES: i64 = 5;

/// Only transport/protocol failures demonstrate that a particular endpoint and
/// transport mode is unhealthy. Caller errors, auth errors, and rate limits
/// must never poison a channel's capability state.
fn affects_mode_health(failure: &AttemptFailure) -> bool {
    failure.failure_class == FailureClass::UpstreamProtocolError
        && (failure
            .message
            .starts_with("upstream returned an undecodable body")
            || failure
                .message
                .starts_with("upstream response could not be decoded:")
            || failure
                .message
                .starts_with("upstream stream ended before a valid first SSE record")
            || failure
                .message
                .starts_with("upstream first frame could not be converted"))
}

async fn record_channel_mode_outcome(
    repo: &Arc<Repository>,
    channel_id: &str,
    endpoint: &str,
    is_stream: bool,
    result: &crate::core::attempt::AttemptResult,
) {
    let outcome = match result {
        crate::core::attempt::AttemptResult::Success(_) => {
            // 被动反哺（C-04）：真实请求传输成功 → 立即恢复渠道的主动探测
            // 健康标记（与 mode_health 的成功清除正交，各管各的表）。
            // 必须 .await：mark_probe_ok 是 async fn，不 await 的 future 会被直接丢弃，
            // 函数体一次都不执行（编译器已报 unused_must_use），等于这个反哺机制失效。
            repo.mark_probe_ok(channel_id).await;
            repo.record_channel_mode_success(channel_id, endpoint, is_stream)
                .await
        }
        crate::core::attempt::AttemptResult::Failure(failure) if affects_mode_health(failure) => {
            let now = crate::utils::time::now_iso();
            let cooldown_until = (Utc::now()
                + chrono::Duration::minutes(MODE_FAILURE_COOLDOWN_MINUTES))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
            repo.record_channel_mode_failure(
                channel_id,
                endpoint,
                is_stream,
                &now,
                &cooldown_until,
                &failure.message,
            )
            .await
        }
        _ => return,
    };
    if let Err(error) = outcome {
        eprintln!("[WARN] channel mode health update failed: {error}");
    }
}

fn plan_error_response(
    status: u16,
    message: impl Into<String>,
    failure_class: Option<&'static str>,
) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut error = json!({
        "message": message.into(), "type": "route_plan_error", "code": code.as_u16()
    });
    if let Some(class) = failure_class {
        error["failure_class"] = json!(class);
    }
    (code, axum::Json(json!({ "error": error }))).into_response()
}

/// Count Tokens is a planning probe rather than a billable/request-history
/// event. All other routable endpoints retain normal observability logs.
pub(crate) fn should_write_request_log(endpoint: EndpointKind) -> bool {
    endpoint != EndpointKind::CountTokens
}

/// Candidate context retained for failures that exhaust a streaming plan before
/// the downstream stream commits. `FlowStep::Halt` only carries the terminal
/// error, so the driver must retain this separately for observability.
#[derive(Clone)]
struct StreamFailureMeta {
    channel_id: String,
    channel_name: String,
    upstream_type: String,
    route_group: String,
    upstream_protocol: String,
    upstream_endpoint: String,
    upstream_model: String,
    provider: String,
    identity_revision: i64,
    codec_version: Option<String>,
}

/// Run a NON-STREAM plan to a complete Response, writing RequestLog + quota.
///
/// `safe_headers` are the already-filtered request headers to forward.
///
/// All 8 parameters are distinct immutable inputs threaded from the T06 handler
/// seam; factoring them into a struct would ripple through `handlers.rs` (a
/// frozen interface) for no functional gain, so the lint is scoped here.
#[allow(clippy::too_many_arguments)]
pub async fn route_plan_response(
    plan: RoutePlan,
    audited: &AuditedRequest,
    key: &ApiKey,
    safe_headers: &[(String, String)],
    mode: &str,
    repo: &Arc<Repository>,
    sanitized_log_body: &str,
    trace_id: Option<String>,
) -> Response {
    let auth_service = Arc::new(crate::auth_provider::service::AuthService::new(
        repo.clone(),
        crate::auth_provider::ProviderRegistry::new(),
    ));
    route_plan_response_with_auth_service(
        plan,
        audited,
        key,
        safe_headers,
        mode,
        repo,
        sanitized_log_body,
        trace_id,
        auth_service,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn route_plan_response_with_auth_service(
    plan: RoutePlan,
    audited: &AuditedRequest,
    key: &ApiKey,
    safe_headers: &[(String, String)],
    mode: &str,
    repo: &Arc<Repository>,
    sanitized_log_body: &str,
    trace_id: Option<String>,
    auth_service: Arc<crate::auth_provider::service::AuthService>,
) -> Response {
    let lookup = candidate_lookup(&plan);
    let endpoint = plan.endpoint;
    let query = audited.envelope.query.clone();
    let mode_health_repo = repo.clone();
    let started = Instant::now();
    let execution = crate::core::plan_executor::execute_plan(
        plan,
        audited,
        rand::rngs::StdRng::from_os_rng(),
        |attempt| {
            let safe = safe_headers.to_vec();
            let query = query.clone();
            let candidate = lookup.get(&attempt.channel_id).cloned();
            let auth_service = auth_service.clone();
            let mode_health_repo = mode_health_repo.clone();
            // Clone the attempt so the returned future does not borrow it
            // (execute_plan requires a `'static`-capable executor future).
            let attempt = attempt.clone();
            async move {
                match candidate {
                    Some(RouteCandidate::Channel { channel, identity }) => {
                        let result = dispatch_channel_with_key_failover(
                            endpoint,
                            &attempt,
                            &channel,
                            &identity,
                            &safe,
                            query.as_deref(),
                            &mode_health_repo,
                        )
                        .await;
                        record_channel_mode_outcome(
                            &mode_health_repo,
                            &channel.id,
                            endpoint.as_str(),
                            false,
                            &result,
                        )
                        .await;
                        result
                    }
                    Some(RouteCandidate::AuthAccount(_)) => {
                        dispatch_auth_account_executor(endpoint, &attempt, &auth_service, &safe)
                            .await
                    }
                    None => crate::core::attempt::AttemptResult::Failure(
                        missing_candidate_failure(&attempt.channel_id),
                    ),
                }
            }
        },
    )
    .await;
    let duration_ms = started.elapsed().as_millis() as u64;

    // Count Tokens is a context-planning probe, not model usage. Keep it out
    // of request history (including route-plan failures) while preserving the
    // actual routing and response behavior.
    if should_write_request_log(endpoint) {
        write_non_stream_log(
            repo,
            key,
            audited,
            mode,
            &execution,
            duration_ms,
            sanitized_log_body,
            trace_id,
        )
        .await;
    }

    let code = StatusCode::from_u16(execution.status).unwrap_or(StatusCode::BAD_GATEWAY);
    // T06 M-2: forward safely-passthrough upstream response headers (e.g.
    // anthropic-ratelimit-*) on the non-stream facade path.
    let mut builder = axum::response::Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in &execution.response_headers {
        if name.eq_ignore_ascii_case("content-type")
            || name.eq_ignore_ascii_case("content-length")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(Body::from(
            serde_json::to_string(&execution.body).unwrap_or_default(),
        ))
        .unwrap_or_else(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error": {"message": "response build failed"}})),
            )
                .into_response()
        })
}

/// Write the RequestLog + quota for a non-stream facade execution.
#[allow(clippy::too_many_arguments)]
async fn write_non_stream_log(
    repo: &Arc<Repository>,
    key: &ApiKey,
    audited: &AuditedRequest,
    mode: &str,
    execution: &crate::core::plan_executor::PlanExecution,
    duration_ms: u64,
    sanitized_log_body: &str,
    trace_id: Option<String>,
) {
    let usage = execution.usage.as_ref().map(|u| {
        (
            u.prompt_tokens as i64,
            u.completion_tokens as i64,
            u.total_tokens as i64,
            u.cached_tokens as i64,
        )
    });
    let (mut prompt, mut completion, mut total, cached_tokens) = usage.unwrap_or((0, 0, 0, 0));

    // Fallback: estimate tokens locally when upstream didn't return usage.
    // Only estimate for successful (2xx) responses — errors have no real content.
    if total == 0
        && prompt == 0
        && completion == 0
        && execution.status >= 200
        && execution.status < 300
    {
        let req_body: serde_json::Value =
            serde_json::from_str(sanitized_log_body).unwrap_or(serde_json::Value::Null);
        let resp_text = super::estimate_usage::extract_response_text(&execution.body);
        let (p, c, t) = super::estimate_usage::estimate_usage(
            &req_body,
            Some(&resp_text),
            &audited.envelope.model,
        );
        prompt = p;
        completion = c;
        total = t;
        if total > 0 {
            eprintln!("[INFO] token usage estimated (upstream didn't return usage): prompt={}, completion={}, total={}", prompt, completion, total);
        }
    }

    let is_retry = execution.attempts > 1;
    let last_failure = execution.last_failure.as_ref();

    // Extract response_choices from the upstream response body for audit log display.
    let response_choices = if execution.status >= 200 && execution.status < 300 {
        // OpenAI Chat Completions: extract `choices` array
        if let Some(choices) = execution.body.get("choices") {
            serde_json::to_string(choices).ok()
        }
        // Anthropic Messages: synthesize a choices-like structure
        else if execution.body.get("content").is_some() {
            let msg = serde_json::json!({
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": execution.body.get("content"),
                },
                "finish_reason": execution.body.get("stop_reason").unwrap_or(&serde_json::Value::Null),
            });
            serde_json::to_string(&vec![msg]).ok()
        }
        // OpenAI Responses API: synthesize from `output`
        else if let Some(output) = execution.body.get("output").and_then(|o| o.as_array()) {
            let choices: Vec<serde_json::Value> = output
                .iter()
                .map(|item| {
                    serde_json::json!({
                        "index": item.get("index").unwrap_or(&serde_json::json!(0)),
                        "message": {
                            "role": "assistant",
                            "content": item.get("content"),
                        },
                        "finish_reason": "stop",
                    })
                })
                .collect();
            serde_json::to_string(&choices).ok()
        } else {
            None
        }
    } else {
        None
    };

    // FIX-16：响应侧扫描（尽力而为）——2xx 响应体过扫描，发现并入审计
    // 结果后落账；扫描异常不阻断响应（scan_response 内部自降级）。
    let mut audit_for_log = audited.audit_result.clone();
    if execution.status >= 200 && execution.status < 300 {
        security::scan_response_into(
            &mut audit_for_log,
            &execution.body,
            &audited.security_settings,
            &[],
        );
    }

    let log = RequestLog {
        id: utils::id::new_id(),
        seq: None,
        api_key_id: Some(key.id.clone()),
        api_key_name: Some(key.name.clone()),
        channel_id: execution.channel_id.clone(),
        channel_name: execution.channel_name.clone(),
        model: audited.envelope.model.clone(),
        upstream_model: execution.upstream_model.clone(),
        mode: mode.to_string(),
        status_code: execution.status as i64,
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_tokens: cached_tokens,
        duration_ms: duration_ms as i64,
        error_message: last_failure.map(|f| f.message.clone()),
        is_stream: 0,
        is_retry: i64::from(is_retry),
        created_at: utils::time::now_iso(),
        request_body: Some(sanitized_log_body.to_string()),
        response_choices,
        risk_level: audit_for_log.risk_level.as_str().to_string(),
        risk_score: audit_for_log.risk_score as i64,
        risk_summary: Some(audit_for_log.summary.clone()),
        security_action: audit_for_log.action.as_str().to_string(),
        sanitized: i64::from(audit_for_log.sanitized),
        blocked_reason: audit_for_log.blocked_reason.clone(),
        trace_id: trace_id.clone(),
        reasoning_effort: extract_reasoning_effort(&audited),
        // T09 observability fields we have on the facade path.  provider /
        // identity_revision / codec_version come from `PlanExecution`, which
        // the executor captures from the SAME PreparedAttempt + ChannelIdentity
        // that produced the request body (design 11.4).
        downstream_protocol: Some(audited.envelope.downstream_protocol.as_str().to_string()),
        downstream_endpoint: Some(audited.envelope.endpoint.clone()),
        route_group: execution.route_group.clone(),
        upstream_protocol: execution.upstream_protocol.clone(),
        upstream_endpoint: execution.upstream_endpoint.clone(),
        provider: execution.provider.clone(),
        codec_version: execution.codec_version.clone(),
        failure_class: last_failure.map(|f| f.failure_class.as_str().to_string()),
        identity_revision: execution.identity_revision,
        client_cancelled: Some(0),
        stream_committed: Some(0),
        upstream_type: execution
            .upstream_type
            .clone()
            .unwrap_or_else(|| "channel".to_string()),
    };
    let log_id = log.id.clone();
    if let Err(e) = repo.create_log(&log).await {
        eprintln!("[WARN] create_log failed: {}", e);
    }
    if let Err(e) = repo
        .create_security_findings(
            &log_id,
            &audit_for_log.findings,
            audit_for_log.action.as_str(),
        )
        .await
    {
        eprintln!("[WARN] create_security_findings failed: {}", e);
    }
    if total > 0 {
        if let Err(e) = repo.increment_quota(&key.id, total).await {
            eprintln!("[WARN] increment_quota failed: {}", e);
        }
    }
}

/// Write a RequestLog for a streaming request that failed BEFORE any downstream
/// byte was committed (I-3: all-candidates-exhausted / CallerTerminal /
/// codec rejection / authorize_and_plan rejection).  This keeps failed
/// streaming requests visible in the observability layer, matching the
/// non-stream path's coverage.
#[allow(clippy::too_many_arguments)]
pub async fn write_stream_precommit_failure_log(
    repo: &Arc<Repository>,
    key: &ApiKey,
    audited: &AuditedRequest,
    mode: &str,
    is_stream: bool,
    status: u16,
    message: &str,
    sanitized_log_body: &str,
    trace_id: Option<&str>,
) {
    write_stream_precommit_failure_log_with_meta(
        repo,
        key,
        audited,
        mode,
        is_stream,
        status,
        message,
        sanitized_log_body,
        trace_id,
        None,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn write_stream_precommit_failure_log_with_meta(
    repo: &Arc<Repository>,
    key: &ApiKey,
    audited: &AuditedRequest,
    mode: &str,
    is_stream: bool,
    status: u16,
    message: &str,
    sanitized_log_body: &str,
    trace_id: Option<&str>,
    last_attempt: Option<&StreamFailureMeta>,
) {
    let log = RequestLog {
        id: utils::id::new_id(),
        seq: None,
        api_key_id: Some(key.id.clone()),
        api_key_name: Some(key.name.clone()),
        channel_id: last_attempt.map(|meta| meta.channel_id.clone()),
        channel_name: last_attempt.map(|meta| meta.channel_name.clone()),
        model: audited.envelope.model.clone(),
        upstream_model: last_attempt.map(|meta| meta.upstream_model.clone()),
        mode: mode.to_string(),
        status_code: status as i64,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        cached_tokens: 0,
        duration_ms: 0,
        error_message: Some(message.to_string()),
        is_stream: i64::from(is_stream),
        is_retry: 0,
        created_at: utils::time::now_iso(),
        request_body: Some(sanitized_log_body.to_string()),
        response_choices: None,
        risk_level: audited.audit_result.risk_level.as_str().to_string(),
        risk_score: audited.audit_result.risk_score as i64,
        risk_summary: Some(audited.audit_result.summary.clone()),
        security_action: audited.audit_result.action.as_str().to_string(),
        sanitized: i64::from(audited.audit_result.sanitized),
        blocked_reason: audited.audit_result.blocked_reason.clone(),
        trace_id: trace_id.map(|s| s.to_string()),
        reasoning_effort: extract_reasoning_effort(&audited),
        // A planning rejection has no candidate context. Once a candidate was
        // selected, retain it so exhausted Auth Accounts are not logged as
        // legacy channels.
        downstream_protocol: Some(audited.envelope.downstream_protocol.as_str().to_string()),
        downstream_endpoint: Some(audited.envelope.endpoint.clone()),
        route_group: last_attempt.map(|meta| meta.route_group.clone()),
        upstream_protocol: last_attempt.map(|meta| meta.upstream_protocol.clone()),
        upstream_endpoint: last_attempt.map(|meta| meta.upstream_endpoint.clone()),
        provider: last_attempt.map(|meta| meta.provider.clone()),
        codec_version: last_attempt.and_then(|meta| meta.codec_version.clone()),
        failure_class: None,
        identity_revision: last_attempt.map(|meta| meta.identity_revision),
        client_cancelled: Some(0),
        stream_committed: Some(0),
        upstream_type: last_attempt
            .map(|meta| meta.upstream_type.clone())
            .unwrap_or_else(|| "channel".to_string()),
    };
    let log_id = log.id.clone();
    if let Err(e) = repo.create_log(&log).await {
        eprintln!("[WARN] create_log failed: {}", e);
    }
    if let Err(e) = repo
        .create_security_findings(
            &log_id,
            &audited.audit_result.findings,
            audited.audit_result.action.as_str(),
        )
        .await
    {
        eprintln!("[WARN] create_security_findings failed: {}", e);
    }
}

/// Run a STREAMING plan and return a committed Response.
///
/// The returned body stream drives the commit barrier, forwards raw / converted
/// SSE bytes, and writes the RequestLog + quota when the stream completes; a
/// client disconnect is recorded exactly once via `client_cancelled`.
///
/// All 8 parameters are distinct immutable inputs threaded from the T06 handler
/// seam; factoring them into a struct would ripple through `handlers.rs` (a
/// frozen interface), so the lint is scoped here.
#[allow(clippy::too_many_arguments)]
pub async fn route_stream_plan(
    plan: RoutePlan,
    audited: &AuditedRequest,
    key: &ApiKey,
    safe_headers: &[(String, String)],
    mode: &str,
    repo: &Arc<Repository>,
    sanitized_log_body: &str,
    trace_id: Option<String>,
) -> Response {
    let auth_service = Arc::new(crate::auth_provider::service::AuthService::new(
        repo.clone(),
        crate::auth_provider::ProviderRegistry::new(),
    ));
    route_stream_plan_with_auth_service(
        plan,
        audited,
        key,
        safe_headers,
        mode,
        repo,
        sanitized_log_body,
        trace_id,
        auth_service,
        // 无设置上下文的入口（测试/嵌入式调用）用缺省超时。
        StreamTimeouts::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn route_stream_plan_with_auth_service(
    plan: RoutePlan,
    audited: &AuditedRequest,
    key: &ApiKey,
    safe_headers: &[(String, String)],
    mode: &str,
    repo: &Arc<Repository>,
    sanitized_log_body: &str,
    trace_id: Option<String>,
    auth_service: Arc<crate::auth_provider::service::AuthService>,
    timeouts: StreamTimeouts,
) -> Response {
    let lookup = candidate_lookup(&plan);
    let endpoint = plan.endpoint;
    let mut flow = AttemptFlow::new(plan);
    let mut last_attempt_meta: Option<StreamFailureMeta> = None;

    loop {
        match flow.next_step() {
            FlowStep::Execute {
                group_idx,
                candidate_idx,
                attempt_no,
            } => {
                let attempt = {
                    let plan = flow.plan();
                    let group = &plan.groups[group_idx];
                    let candidate = &group.candidates[candidate_idx];
                    last_attempt_meta = Some(StreamFailureMeta {
                        channel_id: candidate.candidate.id().to_string(),
                        channel_name: candidate.candidate.name().to_string(),
                        upstream_type: candidate.candidate.upstream_type().to_string(),
                        route_group: group.id.clone(),
                        upstream_protocol: candidate.upstream_protocol.as_str().to_string(),
                        upstream_endpoint: candidate.upstream_endpoint.clone(),
                        // A failed construction has no PreparedAttempt yet;
                        // use the requested model until a built attempt supplies
                        // its mapped upstream model below.
                        upstream_model: audited.envelope.model.clone(),
                        provider: candidate.candidate.provider(),
                        identity_revision: candidate.candidate.identity_revision(),
                        codec_version: None,
                    });
                    build_prepared_attempt(
                        audited,
                        group,
                        candidate,
                        &mut rand::rngs::StdRng::from_os_rng(),
                        attempt_no,
                    )
                };

                let attempt = match attempt {
                    Err(f) => {
                        flow.record_failure(&f);
                        if f.failure_class == FailureClass::CallerTerminal
                            || f.failure_class == FailureClass::CommittedStreamError
                        {
                            // I-3: terminal pre-commit outcome must be logged.
                            let status = f.status_code.unwrap_or(400);
                            let failure_class = f.failure_class;
                            write_stream_precommit_failure_log_with_meta(
                                repo,
                                key,
                                audited,
                                mode,
                                true,
                                status,
                                &f.message,
                                sanitized_log_body,
                                trace_id.as_deref(),
                                last_attempt_meta.as_ref(),
                            )
                            .await;
                            return plan_error_response(
                                status,
                                f.message,
                                Some(failure_class.as_str()),
                            );
                        }
                        continue;
                    }
                    Ok(a) => a,
                };
                if let Some(meta) = last_attempt_meta.as_mut() {
                    meta.upstream_model = attempt.upstream_model.clone();
                    meta.codec_version = attempt.codec_version.clone();
                }
                let candidate = lookup.get(&attempt.channel_id).cloned();
                let query = audited.envelope.query.clone();
                let health_channel_id = candidate.as_ref().and_then(|candidate| match candidate {
                    RouteCandidate::Channel { channel, .. } => Some(channel.id.clone()),
                    RouteCandidate::AuthAccount(_) => None,
                });

                let dispatched = match candidate {
                    Some(RouteCandidate::Channel { channel, identity }) => {
                        let channel = channel_key_slots(&channel, repo)
                            .await
                            .into_iter()
                            .next()
                            .unwrap_or(channel);
                        dispatch_stream_executor(
                            endpoint,
                            &attempt,
                            &channel,
                            &identity,
                            safe_headers,
                            query.as_deref(),
                        )
                        .await
                    }
                    Some(RouteCandidate::AuthAccount(_)) => {
                        dispatch_auth_account_stream_executor(&attempt, &auth_service, safe_headers)
                            .await
                    }
                    None => {
                        StreamAttemptResult::Failure(missing_candidate_failure(&attempt.channel_id))
                    }
                };

                match dispatched {
                    StreamAttemptResult::Failure(f) => {
                        if let Some(channel_id) = health_channel_id.as_deref() {
                            record_channel_mode_outcome(
                                repo,
                                channel_id,
                                endpoint.as_str(),
                                true,
                                &crate::core::attempt::AttemptResult::Failure(f.clone()),
                            )
                            .await;
                        }
                        flow.record_failure(&f);
                        if f.failure_class == FailureClass::CallerTerminal
                            || f.failure_class == FailureClass::CommittedStreamError
                        {
                            // I-3: terminal pre-commit outcome must be logged.
                            let status = f.status_code.unwrap_or(400);
                            let failure_class = f.failure_class;
                            write_stream_precommit_failure_log_with_meta(
                                repo,
                                key,
                                audited,
                                mode,
                                true,
                                status,
                                &f.message,
                                sanitized_log_body,
                                trace_id.as_deref(),
                                last_attempt_meta.as_ref(),
                            )
                            .await;
                            return plan_error_response(
                                status,
                                f.message,
                                Some(failure_class.as_str()),
                            );
                        }
                        // Honor upstream Retry-After before the next attempt.
                        if let Some(secs) = f.retry_after {
                            tokio::time::sleep(Duration::from_secs(secs)).await;
                        }
                        continue;
                    }
                    StreamAttemptResult::Connected(mut upstream) => {
                        // --- first-frame validation (commit barrier) ---
                        // 首帧超时（FIX-08，提交前阶段）：半死连接的首记录
                        // 等待限时，超时按可重试失败换候选渠道。0 = 禁用。
                        let first_frame_result = if timeouts.first_frame.is_zero() {
                            buffer_first_record(&mut upstream).await
                        } else {
                            match tokio::time::timeout(
                                timeouts.first_frame,
                                buffer_first_record(&mut upstream),
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(_) => Err(format!(
                                    "first frame timed out after {:?} waiting for a valid SSE record",
                                    timeouts.first_frame
                                )),
                            }
                        };
                        let (first_frame, carry) = match first_frame_result {
                            Ok(x) => x,
                            Err(diagnostic) => {
                                // Empty / timed-out / undecodable upstream: pre-commit failover.
                                // The diagnostic suffix (bytes received + sanitized
                                // preview, or the FIX-08 timeout note) keeps the stable
                                // message prefix intact for `affects_mode_health` while
                                // making the audit log reveal WHAT actually happened.
                                let failure = AttemptFailure {
                                    failure_class: FailureClass::UpstreamProtocolError,
                                    message: format!(
                                        "upstream stream ended before a valid first SSE record ({diagnostic})"
                                    ),
                                    status_code: Some(502),
                                    retry_after: None,
                                };
                                if let Some(channel_id) = health_channel_id.as_deref() {
                                    record_channel_mode_outcome(
                                        repo,
                                        channel_id,
                                        endpoint.as_str(),
                                        true,
                                        &crate::core::attempt::AttemptResult::Failure(
                                            failure.clone(),
                                        ),
                                    )
                                    .await;
                                }
                                flow.record_failure(&failure);
                                continue;
                            }
                        };
                        if let Some(failure) = crate::endpoint_executor::semantic_stream_failure(
                            &attempt.upstream_protocol,
                            &first_frame,
                        ) {
                            if let Some(channel_id) = health_channel_id.as_deref() {
                                record_channel_mode_outcome(
                                    repo,
                                    channel_id,
                                    endpoint.as_str(),
                                    true,
                                    &crate::core::attempt::AttemptResult::Failure(failure.clone()),
                                )
                                .await;
                            }
                            flow.record_failure(&failure);
                            continue;
                        }

                        let mut supervisor =
                            crate::core::stream_supervisor::StreamSupervisor::new();
                        if supervisor.begin_connect().is_err() {
                            unreachable!()
                        }
                        if supervisor.on_upstream_headers().is_err() {
                            unreachable!()
                        }
                        if supervisor.on_first_frame_validated().is_err() {
                            unreachable!()
                        }
                        let Some(codec) = attempt.prepared_codec.as_ref() else {
                            let failure = AttemptFailure {
                                failure_class: FailureClass::CallerTerminal,
                                message:
                                    "three-protocol streaming attempt is missing its prepared codec"
                                        .to_string(),
                                status_code: Some(500),
                                retry_after: None,
                            };
                            flow.record_failure(&failure);
                            continue;
                        };
                        let codec_label = codec.label();
                        let is_identity = codec.is_identity();
                        // A fresh decoder is created for this particular
                        // attempt/retry and receives the same context that
                        // encoded its request. No string label is reinterpreted.
                        let pump = match StreamPumpCore::new(
                            supervisor,
                            codec.new_stream_decoder(),
                            first_frame.clone(),
                            carry.clone(),
                        ) {
                            Ok(p) => p,
                            Err(e) => {
                                let failure = AttemptFailure {
                                    failure_class: FailureClass::UpstreamProtocolError,
                                    message: format!(
                                        "upstream first frame could not be converted ({}): {}",
                                        codec_label,
                                        e.message()
                                    ),
                                    status_code: Some(502),
                                    retry_after: None,
                                };
                                if let Some(channel_id) = health_channel_id.as_deref() {
                                    record_channel_mode_outcome(
                                        repo,
                                        channel_id,
                                        endpoint.as_str(),
                                        true,
                                        &crate::core::attempt::AttemptResult::Failure(
                                            failure.clone(),
                                        ),
                                    )
                                    .await;
                                }
                                flow.record_failure(&failure);
                                continue;
                            }
                        };
                        if let Some(channel_id) = health_channel_id.as_deref() {
                            // A first record which has passed both SSE and codec
                            // validation proves this channel's stream mode is
                            // healthy, even if the client later disconnects.
                            record_channel_mode_outcome(
                                repo,
                                channel_id,
                                endpoint.as_str(),
                                true,
                                &crate::core::attempt::AttemptResult::Success(
                                    crate::core::attempt::AttemptSuccess {
                                        status: 200,
                                        body: serde_json::Value::Null,
                                        usage: None,
                                        downstream_events: None,
                                        upstream_model: None,
                                        response_headers: vec![],
                                    },
                                ),
                            )
                            .await;
                        }

                        let channel_id = attempt.channel_id.clone();
                        let channel_name = attempt.channel_name.clone();
                        let key = key.clone();
                        let audited = audited.clone();
                        let repo = repo.clone();
                        let sanitized_log_body = sanitized_log_body.to_string();
                        let trace_id = trace_id.clone();
                        // I-2: the DOWNSTREAM mode drives error formatting and the
                        // T09 log `mode` field — never the SSE transform mode.
                        let downstream_mode = mode.to_string();
                        let model = audited.envelope.model.clone();
                        let upstream_model = attempt.upstream_model.clone();
                        let is_retry = attempt_no > 1;

                        // T09 (design 11.4): the observability context comes from
                        // the SAME PreparedAttempt + ChannelIdentity that produced
                        // the request body (single source of truth).
                        let (identity_provider, identity_revision) =
                            match lookup.get(&attempt.channel_id) {
                                Some(candidate) => {
                                    (candidate.provider(), candidate.identity_revision())
                                }
                                None => ("unknown".to_string(), 0),
                            };
                        let route_group = attempt.route_group.clone();
                        let codec_version = attempt.codec_version.clone();
                        let upstream_protocol = attempt.upstream_protocol.clone();
                        let upstream_endpoint = attempt.upstream_endpoint.clone();
                        let upstream_type = attempt.upstream_type.clone();

                        // Forward the upstream content-type + safe response
                        // headers (native passthrough fidelity; design 11.1).
                        let upstream_content_type = upstream.content_type.clone();
                        let upstream_safe_headers = upstream.headers.clone();

                        let body = stream_response_body(
                            pump,
                            upstream,
                            repo,
                            key,
                            audited,
                            model,
                            upstream_model,
                            downstream_mode,
                            is_retry,
                            sanitized_log_body,
                            trace_id,
                            channel_id,
                            channel_name,
                            identity_provider,
                            identity_revision,
                            route_group,
                            codec_version,
                            upstream_protocol,
                            upstream_endpoint,
                            upstream_type,
                            timeouts,
                        );
                        let mut builder = Response::builder()
                            .status(StatusCode::OK)
                            .header(
                                header::CONTENT_TYPE,
                                if is_identity {
                                    upstream_content_type
                                } else {
                                    "text/event-stream".to_string()
                                },
                            )
                            .header(header::CACHE_CONTROL, "no-cache")
                            .header(header::CONNECTION, "keep-alive");
                        for (name, value) in upstream_safe_headers {
                            if name.eq_ignore_ascii_case("content-type")
                                || name.eq_ignore_ascii_case("content-length")
                            {
                                continue;
                            }
                            builder = builder.header(name, value);
                        }
                        return builder
                            .body(Body::from_stream(body))
                            .expect("valid SSE response");
                    }
                }
            }
            FlowStep::Halt { status, message } => {
                // I-3: streaming pre-commit terminal outcome must be logged.
                let failure_class = flow.last_failure().map(|f| f.failure_class.as_str());
                write_stream_precommit_failure_log_with_meta(
                    repo,
                    key,
                    audited,
                    mode,
                    true,
                    status,
                    &message,
                    sanitized_log_body,
                    trace_id.as_deref(),
                    last_attempt_meta.as_ref(),
                )
                .await;
                return plan_error_response(status, message, failure_class);
            }
        }
    }
}

/// Bounded, single-line preview of raw upstream bytes for diagnostics.
const FIRST_RECORD_SNIPPET_BYTES: usize = 240;
const MAX_FIRST_RECORD_BYTES: usize = 4 * 1024 * 1024;

fn upstream_snippet(bytes: &[u8], max_bytes: usize) -> String {
    let truncated = if bytes.len() > max_bytes {
        &bytes[..max_bytes]
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(truncated);
    // Collapse to a single line and drop control characters for log readability.
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\r' | '\n' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {}
            c => out.push(c),
        }
    }
    if bytes.len() > max_bytes {
        out.push_str("…");
    }
    out
}

/// Read + validate the first complete SSE record.  Returns `(first_frame,
/// carry)` where `carry` are the bytes read beyond the first record.
///
/// On failure the `Err` carries a bounded diagnostic suffix — bytes received,
/// upstream content-type (when not SSE), and a sanitized preview of what was
/// actually read — that the driver appends to the stable
/// "upstream stream ended before a valid first SSE record" message prefix.
async fn buffer_first_record(upstream: &mut UpstreamStream) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut buffer = Vec::new();
    let ct_note = if upstream
        .content_type
        .to_ascii_lowercase()
        .contains("event-stream")
    {
        String::new()
    } else {
        format!("; upstream content-type: {}", upstream.content_type)
    };
    loop {
        // Bound the first-frame buffer (a malicious upstream must not OOM us).
        if buffer.len() > MAX_FIRST_RECORD_BYTES {
            return Err(format!(
                "received {} bytes with no SSE record terminator{ct_note}; preview: \"{}\"",
                buffer.len(),
                upstream_snippet(&buffer, FIRST_RECORD_SNIPPET_BYTES),
            ));
        }
        if let Some(end) = crate::endpoint_executor::sse::record_end(&buffer) {
            let record = buffer[..end].to_vec();
            if crate::endpoint_executor::sse::validate_native_first_record(&record).is_ok() {
                let carry = buffer[end..].to_vec();
                return Ok((record, carry));
            }
            // A full record failed validation → pre-commit failover.
            return Err(format!(
                "first SSE record failed validation{ct_note}; preview: \"{}\"",
                upstream_snippet(&record, FIRST_RECORD_SNIPPET_BYTES),
            ));
        }
        match upstream.body.next().await {
            Some(Ok(bytes)) => buffer.extend_from_slice(&bytes),
            Some(Err(error)) => {
                return Err(format!(
                    "stream error after {} bytes ({error}){ct_note}; preview: \"{}\"",
                    buffer.len(),
                    upstream_snippet(&buffer, FIRST_RECORD_SNIPPET_BYTES),
                ));
            }
            None => {
                if buffer.is_empty() {
                    return Err(format!(
                        "received 0 bytes; upstream closed the response without sending a body{ct_note}"
                    ));
                }
                return Err(format!(
                    "received {} bytes before the stream closed with no complete SSE record{ct_note}; preview: \"{}\"",
                    buffer.len(),
                    upstream_snippet(&buffer, FIRST_RECORD_SNIPPET_BYTES),
                ));
            }
        }
    }
}

/// Exactly-once streaming log finalizer (T00 decision 6).
///
/// The normal stream path sets `completed` and writes the log inline; if the
/// client disconnects mid-stream the async-stream is dropped, this guard's
/// `Drop` runs, and a spawned task records a `client_cancelled` log.  The
/// `client_cancelled` marker is therefore written exactly once per request.
/// 流式落账快照：生成器在每帧后更新，客户端中途取消（Drop 路径）时
/// finalizer 读取——取消行记录已产生的真实用量与响应内容，而不是硬编码
/// 零值（issue #57：取消/断开导致已产生的计费数据整体丢失）。
#[derive(Default, Clone)]
struct StreamSnapshot {
    /// (prompt, completion, total, cached)，每帧更新。
    usage: (i64, i64, i64, i64),
    /// 已累积的响应文本（终止时或内容显著增长时重建，控制 O(n) 复制成本）。
    content: String,
    /// 终止后构建的完整 response_choices JSON。
    response_choices: Option<String>,
    terminal: bool,
}

/// 每帧后同步落账快照：用量恒新；响应内容/choices 首个非空内容立即快照
/// （短流取消时才有正文可折算 completion），之后仅在协议终止或内容较上次
/// 快照增长 ≥16KB 时重建——每帧重建会把流式复制放大成 O(n²)。
fn update_stream_snapshot(
    snapshot: &std::sync::Arc<std::sync::Mutex<StreamSnapshot>>,
    pump: &StreamPumpCore,
) {
    let mut s = snapshot.lock().unwrap_or_else(|e| e.into_inner());
    s.usage = pump.usage();
    let content = pump.accumulated_content();
    let grew = content.len().saturating_sub(s.content.len());
    if pump.terminated() {
        s.terminal = true;
        s.content = content.to_string();
        s.response_choices = pump.build_response_choices();
    } else if s.content.is_empty() || grew >= 16 * 1024 {
        s.content = content.to_string();
        s.response_choices = pump.build_response_choices();
    }
}

#[derive(Clone)]
struct StreamLogFinalizer {
    repo: Arc<Repository>,
    /// 预生成的日志行 id：Responses 逐帧持久化在流开始前就用它写段表，
    /// 落账行必须与段表同一个 id（续传回放按它关联完成状态）。
    log_id: String,
    key: ApiKey,
    audited: AuditedRequest,
    model: String,
    upstream_model: String,
    mode: String,
    is_retry: bool,
    sanitized_log_body: String,
    trace_id: Option<String>,
    channel_id: String,
    channel_name: String,
    identity_provider: String,
    identity_revision: i64,
    route_group: String,
    codec_version: Option<String>,
    upstream_protocol: String,
    upstream_endpoint: String,
    upstream_type: String,
    started: Instant,
    completed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 与流泵共享的落账快照（issue #57）：取消路径读取，见 [`StreamSnapshot`]。
    snapshot: std::sync::Arc<std::sync::Mutex<StreamSnapshot>>,
}

impl StreamLogFinalizer {
    async fn write(
        &self,
        client_cancelled: bool,
        had_error: bool,
        error_message: Option<&str>,
        usage_prompt: i64,
        usage_completion: i64,
        usage_total: i64,
        usage_cached: i64,
        response_choices: Option<String>,
    ) {
        // FIX-16：响应侧扫描（尽力而为）——扫描流泵累积的响应文本，发现
        // 并入审计结果后落账。取消/中断行同样覆盖：半程内容同样有泄露面。
        // 快照内容在协议终止或每增长 ≥16KB 时刷新，扫描到最近一次快照为止。
        let snapshot_content = {
            let snap = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
            snap.content.clone()
        };
        let mut audit_for_log = self.audited.audit_result.clone();
        if !snapshot_content.is_empty() {
            security::scan_response_into(
                &mut audit_for_log,
                &json!({ "content": snapshot_content }),
                &self.audited.security_settings,
                &[],
            );
        }
        let duration_ms = self.started.elapsed().as_millis() as i64;
        let log = RequestLog {
            id: self.log_id.clone(),
            seq: None,
            api_key_id: Some(self.key.id.clone()),
            api_key_name: Some(self.key.name.clone()),
            channel_id: Some(self.channel_id.clone()),
            channel_name: Some(self.channel_name.clone()),
            model: self.model.clone(),
            upstream_model: Some(self.upstream_model.clone()),
            mode: self.mode.clone(),
            // M-3: a client-cancelled row is NOT a success — use 499 so the
            // observability layer distinguishes it from a completed 200.
            status_code: if client_cancelled {
                499
            } else if had_error {
                502
            } else {
                200
            },
            prompt_tokens: usage_prompt,
            completion_tokens: usage_completion,
            total_tokens: usage_total,
            cached_tokens: usage_cached,
            duration_ms,
            error_message: error_message.map(|s| s.to_string()),
            is_stream: 1,
            is_retry: i64::from(self.is_retry),
            created_at: utils::time::now_iso(),
            request_body: Some(self.sanitized_log_body.clone()),
            response_choices,
            risk_level: audit_for_log.risk_level.as_str().to_string(),
            risk_score: audit_for_log.risk_score as i64,
            risk_summary: Some(audit_for_log.summary.clone()),
            security_action: audit_for_log.action.as_str().to_string(),
            sanitized: i64::from(audit_for_log.sanitized),
            blocked_reason: audit_for_log.blocked_reason.clone(),
            trace_id: self.trace_id.clone(),
            reasoning_effort: extract_reasoning_effort(&self.audited),
            // T09 observability fields (single source: PreparedAttempt + identity).
            downstream_protocol: Some(
                self.audited
                    .envelope
                    .downstream_protocol
                    .as_str()
                    .to_string(),
            ),
            downstream_endpoint: Some(self.audited.envelope.endpoint.clone()),
            route_group: Some(self.route_group.clone()),
            upstream_protocol: Some(self.upstream_protocol.clone()),
            upstream_endpoint: Some(self.upstream_endpoint.clone()),
            provider: Some(self.identity_provider.clone()),
            codec_version: self.codec_version.clone(),
            failure_class: None,
            identity_revision: Some(self.identity_revision),
            client_cancelled: Some(i64::from(client_cancelled)),
            stream_committed: Some(1),
            upstream_type: self.upstream_type.clone(),
        };
        let log_id = log.id.clone();
        if let Err(e) = self.repo.create_log(&log).await {
            eprintln!("[WARN] create_log failed: {}", e);
        }
        if let Err(e) = self
            .repo
            .create_security_findings(
                &log_id,
                &audit_for_log.findings,
                audit_for_log.action.as_str(),
            )
            .await
        {
            eprintln!("[WARN] create_security_findings failed: {}", e);
        }
        if usage_total > 0 {
            if let Err(e) = self.repo.increment_quota(&self.key.id, usage_total).await {
                eprintln!("[WARN] increment_quota failed: {}", e);
            }
        }
    }
}

impl Drop for StreamLogFinalizer {
    fn drop(&mut self) {
        // The normal path sets `completed` before writing the log inline.  An
        // early drop (client disconnect) lands here: record client_cancelled
        // exactly once via a spawned task (we are in Drop, so no await).
        //
        // T10 integration fix: the `completed` flag MUST be set BEFORE spawning.
        // The spawned task writes the 499 row and then DROPS its cloned
        // finalizer at task end; without setting the flag here, that drop sees
        // `completed == false` and spawns ANOTHER task, recursively — an
        // unbounded chain of duplicate 499 rows (and eventual stack overflow /
        // process abort).  Setting the flag first makes the write exactly-once.
        //
        // #57: the cancelled row keeps what the stream already produced —
        // the pump snapshot's real usage (fallback: local estimate) and the
        // accumulated response so far — instead of hardcoded zeros.
        if !self.completed.load(std::sync::atomic::Ordering::SeqCst) {
            self.completed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let f = self.clone();
            // FIX-26：Drop 可能在 runtime 已关停（应用退出中）时执行，
            // 裸 tokio::spawn 会 panic；守卫后优雅跳过该条落账。
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move { write_cancelled_row(&f).await });
            }
        }
    }
}

/// 取消行的实际写入（从 Drop 中拆出便于复用与测试）。
async fn write_cancelled_row(f: &StreamLogFinalizer) {
    let snap = f.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (mut p, mut c, mut t, cached) = snap.usage;
    let choices = snap.response_choices;
    // 兜底：上游未回报 usage 时按请求体 + 已收到内容本地估算，
    // 取消行的计费统计不再恒为 0。
    if t == 0 && p == 0 && c == 0 {
        let req_body: serde_json::Value =
            serde_json::from_str(&f.sanitized_log_body).unwrap_or(serde_json::Value::Null);
        let (ep, ec, et) =
            super::estimate_usage::estimate_usage(&req_body, Some(snap.content.as_str()), &f.model);
        p = ep;
        c = ec;
        t = et;
    }
    f.write(
        true,
        false,
        Some("client_cancelled"),
        p,
        c,
        t,
        cached,
        choices,
    )
    .await;
}

/// 流式超时配置（FIX-08）：首帧等待与帧间空闲分别限时，0 表示禁用该项。
/// 缺省 60s/120s，可经设置 `stream.first_frame_timeout_secs` /
/// `stream.idle_timeout_secs` 覆盖（SettingsStore 任意键读取，无需 schema）。
#[derive(Debug, Clone, Copy)]
pub struct StreamTimeouts {
    pub first_frame: std::time::Duration,
    pub idle: std::time::Duration,
}

impl Default for StreamTimeouts {
    fn default() -> Self {
        Self {
            first_frame: std::time::Duration::from_secs(60),
            idle: std::time::Duration::from_secs(120),
        }
    }
}

impl StreamTimeouts {
    pub fn from_settings(settings: &crate::settings_store::SettingsStore) -> Self {
        let secs = |key: &str, default: u64| {
            std::time::Duration::from_secs(settings.get_u64(key, default))
        };
        Self {
            first_frame: secs("stream.first_frame_timeout_secs", 60),
            idle: secs("stream.idle_timeout_secs", 120),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_response_body(
    mut pump: StreamPumpCore,
    upstream: UpstreamStream,
    repo: Arc<Repository>,
    key: ApiKey,
    audited: AuditedRequest,
    model: String,
    upstream_model: String,
    mode: String,
    is_retry: bool,
    sanitized_log_body: String,
    trace_id: Option<String>,
    channel_id: String,
    channel_name: String,
    // --- T09 observability context (single source: PreparedAttempt/identity) ---
    identity_provider: String,
    identity_revision: i64,
    route_group: String,
    codec_version: Option<String>,
    upstream_protocol: String,
    upstream_endpoint: String,
    upstream_type: String,
    timeouts: StreamTimeouts,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> {
    let mode_for_error = mode.clone();
    let snapshot = std::sync::Arc::new(std::sync::Mutex::new(StreamSnapshot::default()));
    // Responses 逐帧持久化（续传锚点）：detailed 策略下，下游每帧 SSE 字节
    // 顺序落段表（seq 递增，response.id 从 response.created 帧提取）。
    // 客户端中途断开时已生成帧自然保留——「丢了」变成「可回放」。
    let resume_frames_enabled = mode_for_error == "responses"
        && crate::audit_log::current_policy().detail_level
            == crate::audit_log::LogDetailLevel::Detailed;
    let resume_log_id = utils::id::new_id();
    let finalizer = StreamLogFinalizer {
        repo: repo.clone(),
        log_id: resume_log_id.clone(),
        key,
        audited,
        model,
        upstream_model,
        mode,
        is_retry,
        sanitized_log_body,
        trace_id,
        channel_id,
        channel_name,
        identity_provider,
        identity_revision,
        route_group,
        codec_version,
        upstream_protocol,
        upstream_endpoint,
        upstream_type,
        started: Instant::now(),
        completed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        snapshot: snapshot.clone(),
    };
    let completed = finalizer.completed.clone();

    async_stream::stream! {
        let mut had_error = false;
        let mut error_message: Option<String> = None;
        // 终止帧 / 错误帧交给下游之前就已经落库时为 true，函数末尾不再重复写日志。
        let mut finalized = false;
        // Responses 逐帧持久化状态：帧序号 + 续传锚点（response.created 提取）
        let mut frame_seq: i64 = 0;
        let mut anchor_response_id: Option<String> = None;

        let upstream_bytes = upstream.body;
        tokio::pin!(upstream_bytes);

        // Emit the first frame.  The pump already encoded the first record AND
        // any carry bytes (records 2..N of the same upstream chunk) through the
        // decoder for conversion modes — so this is ONLY downstream-protocol
        // bytes, never raw upstream bytes.  Native passthrough preserves raw.
        match pump.start() {
            Ok(first) => {
                // 极短响应：首帧/伴随帧（carry）就含全部内容甚至终止标记，
                // 此时 while 循环一次都不进——这里同步一次落账快照，
                // 否则取消行丢内容、FIX-16 响应扫描拿到空快照。
                update_stream_snapshot(&snapshot, &pump);
                if !first.is_empty() {
                    if resume_frames_enabled {
                        persist_resume_frame(
                            &repo,
                            &resume_log_id,
                            &mut frame_seq,
                            &mut anchor_response_id,
                            &first,
                        )
                        .await;
                    }
                    if !finalized && downstream_terminal_frame(&mode_for_error, &first) {
                        finalized = true;
                        write_stream_log(
                            &finalizer,
                            &completed,
                            false,
                            None,
                            StreamLogSnapshot::take(&pump),
                        )
                        .await;
                    }
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(first));
                }
            }
            Err(e) => {
                had_error = true;
                error_message = Some(e.message().to_string());
            }
        }

        // 协议终止事件已到即视为流完成（#57）：不等上游 TCP EOF——部分上游
        // （如 ModelScope）发完数据后延迟关连接，期间客户端断开会把本已
        // 成功的调用记成 499 零用量行。终止后跳出循环走正常落账路径。
        // 首帧/伴随帧就含终止标记（极短响应）的情形由 pump 内部登记覆盖。
        while !had_error && !pump.terminated() {
            // 帧间空闲超时（FIX-08，提交后阶段）：上游挂死时不再无限等待，
            // 超时按流错误收尾（下发结构化错误事件 + 502 落账）。0 = 禁用。
            let next = if timeouts.idle.is_zero() {
                upstream_bytes.next().await
            } else {
                match next_upstream_item(&mut upstream_bytes, timeouts.idle).await {
                    UpstreamItem::Chunk(item) => item,
                    UpstreamItem::IdleTimeout => {
                        pump.mark_idle_timeout();
                        had_error = true;
                        error_message = Some(format!(
                            "upstream idle timeout: no frame for {:?}",
                            timeouts.idle
                        ));
                        break;
                    }
                }
            };
            match next {
                Some(Ok(bytes)) => match pump.push(&bytes) {
                    Ok(out) => {
                        // 每帧后同步取消路径要用的落账快照（#57）。
                        update_stream_snapshot(&snapshot, &pump);
                        if !out.is_empty() {
                            if resume_frames_enabled {
                                persist_resume_frame(
                                    &repo,
                                    &resume_log_id,
                                    &mut frame_seq,
                                    &mut anchor_response_id,
                                    &out,
                                )
                                .await;
                            }
                            if !finalized && downstream_terminal_frame(&mode_for_error, &out) {
                                finalized = true;
                                write_stream_log(
                                    &finalizer,
                                    &completed,
                                    false,
                                    None,
                                    StreamLogSnapshot::take(&pump),
                                )
                                .await;
                            }
                            yield Ok::<_, std::io::Error>(bytes::Bytes::from(out));
                        }
                    }
                    Err(e) => {
                        had_error = true;
                        error_message = Some(e.message().to_string());
                        break;
                    }
                },
                Some(Err(e)) => {
                    had_error = true;
                    // The upstream body failed mid-stream.  `error decoding
                    // response body` (reqwest Kind::Decode) hides the real cause
                    // behind a generic message, so walk the source chain to
                    // surface whether it was a corrupt content encoding (gzip
                    // frame cut short) or a dropped connection (hyper reset).
                    error_message = Some(format!(
                        "stream interrupted: {} (root: {})",
                        e,
                        error_chain_root(&e)
                    ));
                    break;
                }
                None => break,
            }
        }

        // End-of-stream flush (exactly-once terminal markers).
        if !had_error {
            match pump.finish() {
                Ok(out) => {
                    // 终止收尾后再同步一次快照：finish 可能补齐终止标记内容。
                    update_stream_snapshot(&snapshot, &pump);
                    if !out.is_empty() {
                        if !finalized && downstream_terminal_frame(&mode_for_error, &out) {
                            finalized = true;
                            write_stream_log(
                                &finalizer,
                                &completed,
                                false,
                                None,
                                StreamLogSnapshot::take(&pump),
                            )
                            .await;
                        }
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from(out));
                    }
                }
                Err(e) => {
                    had_error = true;
                    error_message = Some(e.message().to_string());
                }
            }
        }

        // A downstream error before/after commit must produce a protocol
        // error event (never a retry, never a fake success).
        if had_error {
            // 与终止帧同理：错误帧发出去后客户端也可能立刻关闭连接，日志必须先落库，
            // 否则这条 502 会被 Drop 里的 client_cancelled 覆盖成 499。
            if !finalized {
                finalized = true;
                write_stream_log(
                    &finalizer,
                    &completed,
                    true,
                    error_message.as_deref(),
                    StreamLogSnapshot::take(&pump),
                )
                .await;
            }
            let msg = error_message.clone().unwrap_or_else(|| "stream error".to_string());
            let ev = format_stream_error(&mode_for_error, &msg);
            yield Ok::<_, std::io::Error>(bytes::Bytes::from(ev));
        }

        // 兜底：上游自然结束、终止帧没被单独识别出来（例如只发到 finish_reason 就 EOF）
        // 时仍在这里写日志。已经落过库的请求不再重复写。
        if !finalized {
            write_stream_log(
                &finalizer,
                &completed,
                had_error,
                error_message.as_deref(),
                StreamLogSnapshot::take(&pump),
            )
            .await;
        }
    }
}

/// 落库所需的 pump 侧快照。
///
/// 必须在 `await` 之前同步取好：`stream_response_body` 是 `async_stream` 生成器，
/// 把 `&mut pump` 带过 `await` 会让生成器自引用。调用方只交出这个 owned 结构体，
/// 跨 `await` 存活的只有 `&finalizer` / `&completed`（与改动前的收尾代码同形）。
struct StreamLogSnapshot {
    usage: (i64, i64, i64, i64),
    response_choices: Option<String>,
    accumulated_content: String,
}

impl StreamLogSnapshot {
    fn take(pump: &StreamPumpCore) -> Self {
        Self {
            usage: pump.usage(),
            response_choices: pump.build_response_choices(),
            accumulated_content: pump.accumulated_content().to_string(),
        }
    }
}

/// 把一次流式请求的结果写成一条审计日志（成功，或已提交之后的流错误）。
///
/// 关键约束：**必须在终止帧交给下游之前调用**。终止帧一旦送达，客户端（Codex /
/// Claude Code / Node undici 等 Agent）就会立刻关闭连接，hyper 不再轮询本流，
/// 生成器直接被 drop；此时若日志还没写，`StreamLogFinalizer::drop` 会把一条完整
/// 成功的流误记成 `499 / client_cancelled / 0 token / 空响应`。
async fn write_stream_log(
    finalizer: &StreamLogFinalizer,
    completed: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    had_error: bool,
    error_message: Option<&str>,
    snapshot: StreamLogSnapshot,
) {
    let (mut usage_prompt, mut usage_completion, mut usage_total, usage_cached) = snapshot.usage;

    // Fallback: estimate tokens locally when upstream didn't return usage.
    // Only estimate for successful streams (no error).
    if usage_total == 0 && usage_prompt == 0 && usage_completion == 0 && !had_error {
        let req_body: serde_json::Value =
            serde_json::from_str(&finalizer.sanitized_log_body).unwrap_or(serde_json::Value::Null);
        let (p, c, t) = super::estimate_usage::estimate_usage(
            &req_body,
            Some(snapshot.accumulated_content.as_str()),
            &finalizer.model,
        );
        usage_prompt = p;
        usage_completion = c;
        usage_total = t;
        if usage_total > 0 {
            eprintln!("[INFO] stream token usage estimated (upstream didn't return usage): prompt={}, completion={}, total={}", usage_prompt, usage_completion, usage_total);
        }
    }

    // Mark the request completed so the Drop finalizer does NOT write a
    // duplicate client_cancelled row, then write the log.
    completed.store(true, std::sync::atomic::Ordering::SeqCst);
    let response_choices = if had_error {
        None
    } else {
        snapshot.response_choices
    };
    finalizer
        .write(
            false,
            had_error,
            error_message,
            usage_prompt,
            usage_completion,
            usage_total,
            usage_cached,
            response_choices,
        )
        .await;
}

/// 判断这段下游字节里是否已经出现该协议的终止帧。
///
/// 出现即代表响应体已完整交给下游，之后下游怎么关连接都不影响本次请求的成功性。
/// 逐行精确匹配而不是子串搜索：SSE 的 JSON 载荷里换行一定是 `\n` 转义，正文内容
/// 不可能伪造出一个行首的终止帧。
fn downstream_terminal_frame(mode: &str, out: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(out) else {
        return false;
    };
    // 与 `format_stream_error` 使用同一套下游协议判定。
    let terminal_event = match mode {
        "anthropic" | "anthropic_count_tokens" => Some("message_stop"),
        "responses" => Some("response.completed"),
        _ => None,
    };
    text.lines().any(|line| {
        let line = line.trim_end_matches('\r');
        if let Some(name) = terminal_event {
            line.strip_prefix("event:")
                .is_some_and(|value| value.trim() == name)
        } else {
            line.strip_prefix("data:")
                .is_some_and(|payload| payload.trim() == "[DONE]")
        }
    })
}

/// 从下游 SSE 帧字节中提取续传锚点（response.created 事件里的 response.id）。
/// 纯函数：只认 `data:` 行的 JSON，`type == "response.created"` 时取 `response.id`。
fn extract_response_id(frame: &str) -> Option<String> {
    for line in frame.lines() {
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload.trim()) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) == Some("response.created") {
            if let Some(id) = value
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(|id| id.as_str())
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Responses 逐帧持久化：一帧下游 SSE 字节顺序写入段表（best-effort）。
/// 首次见到 response.created 帧时提取锚点；此前已写入的帧锚点为 NULL
/// （实际首帧即 response.created，NULL 帧仅出现在上游乱序的病态场景）。
async fn persist_resume_frame(
    repo: &Repository,
    log_id: &str,
    seq: &mut i64,
    anchor: &mut Option<String>,
    frame: &[u8],
) {
    let text = String::from_utf8_lossy(frame);
    if anchor.is_none() {
        *anchor = extract_response_id(&text);
    }
    *seq += 1;
    repo.append_stream_frame(
        log_id,
        *seq,
        anchor.as_deref(),
        &text,
        &crate::utils::time::now_iso(),
    )
    .await;
}

/// Walk an error's `source()` chain to its root and return it as a string.
/// Used when a generic transport message (e.g. reqwest `Kind::Decode`'s
/// "error decoding response body") hides the actual failure cause.
fn error_chain_root(err: &dyn std::error::Error) -> String {
    let mut deepest: &dyn std::error::Error = err;
    while let Some(source) = deepest.source() {
        deepest = source;
    }
    deepest.to_string()
}

/// Format a post-commit stream error event in the DOWNSTREAM protocol (I-2).
/// `mode` is the downstream mode ("chat" / "anthropic" / "responses" /
/// "embedding" / "anthropic_count_tokens"), NOT the SSE transform mode.
///
/// FIX-20：data 载荷一律经 serde_json 构造——错误消息含引号/换行/反斜杠时
/// 手写转义拼接会产生非法 JSON 帧导致 SDK 断流。
pub(crate) fn format_stream_error(mode: &str, message: &str) -> String {
    if mode == "responses" {
        format!(
            "event: response.failed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.failed",
                "error": {"message": message}
            })
        )
    } else if mode == "anthropic" || mode == "anthropic_count_tokens" {
        format!(
            "event: error\ndata: {}\n\n",
            serde_json::json!({
                "type": "error",
                "error": {"type": "api_error", "message": message}
            })
        )
    } else {
        format!(
            "data: {}\ndata: [DONE]\n\n",
            serde_json::json!({
                "error": {"message": message, "type": "server_error"}
            })
        )
    }
}

/// Resolve a channel's identity row (used by the legacy flag-off paths that
/// still key off channel_type).
fn identity_for(channel: &Channel) -> crate::core::channel_identity::ChannelIdentity {
    resolve_channel_identity(&ChannelIdentityRow::from(channel))
}

/// Whether a channel is a native Anthropic Messages channel (identity-based,
/// NOT `type == "claude"` — the removed production-selection duty).
pub fn is_native_anthropic(channel: &Channel) -> bool {
    let id = identity_for(channel);
    id.protocol == "anthropic" && id.native_endpoints.iter().any(|e| e == "messages")
}

/// Whether a channel supports the Anthropic count_tokens endpoint.
pub fn supports_count_tokens(channel: &Channel) -> bool {
    let id = identity_for(channel);
    id.protocol == "anthropic" && id.native_endpoints.iter().any(|e| e == "count_tokens")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::Channel;

    fn upstream_from_chunks(chunks: Vec<&[u8]>) -> UpstreamStream {
        let body = futures_util::stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<_, std::io::Error>(bytes::Bytes::from(c.to_vec())))
                .collect::<Vec<_>>(),
        )
        .boxed();
        UpstreamStream {
            content_type: "text/event-stream".to_string(),
            headers: vec![],
            body,
        }
    }

    /// First-frame barrier diagnostics: empty upstream body must surface
    /// "0 bytes" so audit logs distinguish silent drops from content errors.
    #[tokio::test]
    async fn buffer_first_record_empty_stream_reports_zero_bytes() {
        let mut upstream = upstream_from_chunks(vec![]);
        let diagnostic = buffer_first_record(&mut upstream).await.unwrap_err();
        assert!(diagnostic.contains("received 0 bytes"), "{diagnostic}");
    }

    /// Codex/OpenAI-style failure: HTTP 200 + JSON error body with no SSE
    /// framing at all. The diagnostic must carry the body preview so the
    /// audit log reveals the actual upstream error.
    #[tokio::test]
    async fn buffer_first_record_non_sse_json_body_is_surfaced() {
        let mut upstream = upstream_from_chunks(vec![
            b"{\"error\":{\"message\":\"quota exceeded for account\"}}".as_slice(),
        ]);
        let diagnostic = buffer_first_record(&mut upstream).await.unwrap_err();
        assert!(diagnostic.contains("quota exceeded"), "{diagnostic}");
    }

    /// A complete SSE-shaped record that fails first-record validation (no
    /// `event:` line, `data:` payload is not valid JSON) must surface its
    /// content.  Note: records with no `data:` line at all are valid SSE
    /// keep-alives and pass validation by design.
    #[tokio::test]
    async fn buffer_first_record_invalid_record_is_surfaced() {
        let mut upstream = upstream_from_chunks(vec![b"data: not valid json\n\n"]);
        let diagnostic = buffer_first_record(&mut upstream).await.unwrap_err();
        assert!(diagnostic.contains("failed validation"), "{diagnostic}");
        assert!(diagnostic.contains("not valid json"), "{diagnostic}");
    }

    /// ChatGPT Codex can emit a very large first `response.created` event
    /// because the response object echoes request metadata/tools.  The commit
    /// barrier must wait for the real SSE terminator instead of treating a
    /// >256 KiB but otherwise valid first record as a protocol failure.
    #[tokio::test]
    async fn buffer_first_record_accepts_large_codex_created_event() {
        let large_metadata = "x".repeat(300 * 1024);
        let record = format!(
            "event: response.created\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.created",
                "response": {
                    "id": "resp_test",
                    "object": "response",
                    "status": "in_progress",
                    "metadata": { "large": large_metadata }
                },
                "sequence_number": 0
            })
        );
        let mut upstream = upstream_from_chunks(vec![record.as_bytes()]);

        let (first_frame, carry) = buffer_first_record(&mut upstream).await.unwrap();

        assert_eq!(first_frame, record.as_bytes());
        assert!(carry.is_empty());
    }

    /// Non-SSE content-type must be called out in the diagnostic.
    #[tokio::test]
    async fn buffer_first_record_surfaces_content_type() {
        let mut upstream = upstream_from_chunks(vec![b"<html>blocked</html>"]);
        upstream.content_type = "text/html".to_string();
        let diagnostic = buffer_first_record(&mut upstream).await.unwrap_err();
        assert!(diagnostic.contains("text/html"), "{diagnostic}");
    }

    /// Mid-stream idle timeout: after the first frame is committed, if the
    /// upstream stalls (no more data), the pump must emit a protocol error
    /// event containing "idle timeout" instead of hanging forever.
    #[tokio::test]
    async fn stream_response_body_idle_timeout_emits_error_event() {
        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool));

        // First chunk: one valid Anthropic record (gets through the barrier).
        let first_chunk = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"model\":\"up-model\",\"content\":[]}}\n\n";
        // Second chunk: NEVER ARRIVES — the stream ends after the first record
        // but we simulate a stall by providing only the first chunk and then
        // ending the stream.  To test idle timeout specifically, we use a
        // stream that yields one chunk then hangs (pending forever).
        let first_chunk = first_chunk.to_vec();
        let body = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from(
            first_chunk,
        ))])
        .chain(futures_util::stream::pending())
        .boxed();

        let mut upstream = UpstreamStream {
            content_type: "text/event-stream".to_string(),
            headers: vec![],
            body,
        };
        let (first_frame, carry) = buffer_first_record(&mut upstream).await.unwrap();
        assert!(carry.is_empty(), "single record, no carry");

        let mut sup = crate::core::stream_supervisor::StreamSupervisor::new();
        sup.begin_connect().unwrap();
        sup.on_upstream_headers().unwrap();
        sup.on_first_frame_validated().unwrap();
        let prepared = crate::protocol::codec::CodecRegistry::prepare_pair(
            crate::protocol::codec::Protocol::Chat,
            crate::protocol::codec::Protocol::Messages,
            "up-model",
            &json!({"model":"up-model", "messages":[{"role":"user","content":"hi"}]}),
        )
        .unwrap();
        let pump =
            StreamPumpCore::new(sup, prepared.codec.new_stream_decoder(), first_frame, carry)
                .unwrap();

        let stream = stream_response_body(
            pump,
            upstream,
            repo,
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            "{}".to_string(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            // Very short idle timeout for testing.
            StreamTimeouts {
                first_frame: Duration::ZERO,
                idle: Duration::from_millis(50),
            },
        );

        let mut bytes = Vec::new();
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            bytes.extend_from_slice(&item.unwrap());
        }
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("idle timeout"),
            "downstream must receive idle timeout error event: {text}"
        );
    }

    fn channel(protocol: Option<&str>, endpoints: &[&str]) -> Channel {
        Channel {
            id: "ch-1".into(),
            name: "t".into(),
            channel_type: "claude".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            api_key: "k".into(),
            models: "[\"m\"]".into(),
            status: 1,
            priority: 1,
            weight: 1,
            config: "{}".into(),
            model_mapping: "{}".into(),
            timeout_secs: 30,
            protocol: protocol.map(|s| s.to_string()),
            provider: Some("anthropic".into()),
            native_base_url: Some("https://api.anthropic.com".into()),
            native_endpoints: Some(serde_json::to_string(endpoints).unwrap()),
            preset_revision: Some("test".into()),
            identity_revision: 1,
            legacy_executor_override: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            last_test_at: None,
            last_test_ok: None,
            last_probe_at: None,
            last_probe_ok: None,
            probe_latency_ms: None,
        }
    }

    #[test]
    fn native_anthropic_is_identity_based_not_type_based() {
        // protocol=anthropic + messages → native, even though type is "claude".
        let c = channel(Some("anthropic"), &["messages"]);
        assert!(is_native_anthropic(&c));
        // An OpenAI channel (even with type "claude" impossible) is not native.
        let o = channel(Some("openai"), &["chat_completions"]);
        assert!(!is_native_anthropic(&o));
        // A claude-typed legacy row WITHOUT a declared messages capability is
        // not native (the type==claude heuristic is removed).
        let mut legacy = channel(None, &[]);
        legacy.identity_revision = 0;
        legacy.native_base_url = None;
        legacy.native_endpoints = None;
        legacy.channel_type = "openai".into();
        assert!(!is_native_anthropic(&legacy));
    }

    #[test]
    fn count_tokens_requires_declared_capability() {
        let with = channel(Some("anthropic"), &["messages", "count_tokens"]);
        assert!(supports_count_tokens(&with));
        let without = channel(Some("anthropic"), &["messages"]);
        assert!(!supports_count_tokens(&without));
    }

    #[test]
    fn count_tokens_is_excluded_from_request_logs() {
        assert!(!should_write_request_log(EndpointKind::CountTokens));
        assert!(should_write_request_log(EndpointKind::Messages));
        assert!(should_write_request_log(EndpointKind::ChatCompletions));
        assert!(should_write_request_log(EndpointKind::Responses));
    }

    /// T06 I-4 (leader adjudication): a legacy revision-0 `type == "claude"`
    /// row infers count_tokens from the resolver, so the flag-OFF count_tokens
    /// fallback still serves it (no-regression contract).
    #[test]
    fn legacy_claude_row_serves_count_tokens() {
        let mut legacy = channel(None, &[]);
        legacy.identity_revision = 0;
        legacy.native_base_url = None;
        legacy.native_endpoints = None;
        legacy.protocol = None;
        legacy.provider = None;
        legacy.channel_type = "claude".into();
        legacy.base_url = "https://api.anthropic.com/v1".into();
        let id = identity_for(&legacy);
        assert_eq!(id.protocol, "anthropic");
        assert!(
            id.native_endpoints.iter().any(|e| e == "count_tokens"),
            "legacy claude must infer count_tokens"
        );
        assert!(
            supports_count_tokens(&legacy),
            "flag-OFF count_tokens fallback must serve legacy claude"
        );
    }

    /// C-1 DRIVER-level regression (carry seam): `buffer_first_record` →
    /// `StreamPumpCore::new` → `start()` where the FIRST upstream chunk spans
    /// MULTIPLE records (message_start + a content_block_delta carry) in a
    /// conversion mode.  The downstream must receive ONLY codec-encoded bytes
    /// for records 1 AND 2 — never raw upstream protocol bytes, and the carry
    /// record must actually be decoded (its text present in the output).
    #[tokio::test]
    async fn driver_conversion_first_frame_never_raw_downstream() {
        // One chunk containing TWO Anthropic records (downstream Chat client).
        let raw = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"model\":\"up-model\",\"content\":[]}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"carried\"}}\n\n";
        let mut raw = raw.to_vec();
        let body = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from(
            std::mem::take(&mut raw),
        ))])
        .boxed();
        let mut upstream = UpstreamStream {
            content_type: "text/event-stream".to_string(),
            headers: vec![],
            body,
        };
        let (first_frame, carry) = buffer_first_record(&mut upstream).await.unwrap();
        assert!(
            !first_frame.is_empty(),
            "driver must buffer a real first record"
        );
        assert!(
            !carry.is_empty(),
            "the first chunk must span a carry record (the real C-1 seam)"
        );

        let mut sup = crate::core::stream_supervisor::StreamSupervisor::new();
        sup.begin_connect().unwrap();
        sup.on_upstream_headers().unwrap();
        sup.on_first_frame_validated().unwrap();
        let prepared = crate::protocol::codec::CodecRegistry::prepare_pair(
            crate::protocol::codec::Protocol::Chat,
            crate::protocol::codec::Protocol::Messages,
            "up-model",
            &json!({"model":"up-model", "messages":[{"role":"user","content":"hi"}]}),
        )
        .unwrap();
        let mut pump = StreamPumpCore::new(
            sup,
            prepared.codec.new_stream_decoder(),
            first_frame.clone(),
            carry.clone(),
        )
        .unwrap();

        let first_out = pump.start().unwrap();
        let text = String::from_utf8_lossy(&first_out);
        assert!(
            !text.contains("event: message_start") && !text.contains("event: content_block_delta"),
            "downstream Chat client must NEVER see raw Anthropic bytes (carry included): {text}"
        );
        assert!(
            text.contains("\"content\":\"carried\""),
            "the carry record (record 2) must be decoded into downstream output: {text}"
        );
        assert!(pump.committed());

        // A subsequent chunk converts normally.
        let delta = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n";
        let out = pump.push(delta).unwrap();
        assert!(String::from_utf8_lossy(&out).contains("\"content\":\"hi\""));
    }

    #[test]
    fn forward_headers_drops_credentials_and_hop_by_hop() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("authorization", "Bearer sk".parse().unwrap());
        headers.insert("x-api-key", "sk".parse().unwrap());
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        headers.insert("anthropic-beta", "prompt-caching".parse().unwrap());
        headers.insert("cookie", "a=b".parse().unwrap());
        headers.insert("x-anthropic-future", "on".parse().unwrap());
        let safe = crate::endpoint_executor::safe_request_headers(&headers);
        assert!(safe.iter().any(|(k, _)| k == "anthropic-version"));
        assert!(safe.iter().any(|(k, _)| k == "anthropic-beta"));
        assert!(safe.iter().any(|(k, _)| k == "x-anthropic-future"));
        assert!(!safe
            .iter()
            .any(|(k, _)| k == "authorization" || k == "x-api-key" || k == "cookie"));
    }

    /// I-2: post-commit stream errors must be formatted in the DOWNSTREAM
    /// protocol (Anthropic Messages / Responses / OpenAI Chat), not in the
    /// SSE transform mode string.
    #[test]
    fn stream_error_format_uses_downstream_protocol() {
        // A Messages-downstream stream error → Anthropic `event: error`.
        let anthropic = format_stream_error("anthropic", "boom");
        assert!(anthropic.contains("event: error"));
        assert!(anthropic.contains("\"type\":\"error\""));
        assert!(!anthropic.contains("data: [DONE]"));

        // A Responses-downstream stream error → `event: response.failed`.
        let responses = format_stream_error("responses", "boom");
        assert!(responses.contains("event: response.failed"));
        assert!(responses.contains("\"type\":\"response.failed\""));

        // A Chat-downstream stream error → OpenAI `data:` error + [DONE].
        let chat = format_stream_error("chat", "boom");
        assert!(chat.contains("data: {\"error\""));
        assert!(chat.contains("data: [DONE]"));

        // The SSE transform mode string must NEVER leak into error formatting.
        assert!(!format_stream_error("chat", "x").contains("chat_to_messages_v1"));
    }

    /// FIX-20：错误消息含引号/换行/反斜杠时 data 帧必须是合法 JSON
    /// （手写转义拼接会产生断流帧）。三种下游协议逐一断言可解析。
    #[test]
    fn stream_error_events_are_always_valid_json() {
        for mode in ["chat", "anthropic", "responses"] {
            let event = format_stream_error(mode, "quote \" backslash \\ newline \n tab \t");
            for line in event.lines() {
                if let Some(payload) = line.strip_prefix("data: ") {
                    if payload == "[DONE]" {
                        continue;
                    }
                    let parsed: serde_json::Value = serde_json::from_str(payload)
                        .unwrap_or_else(|e| panic!("{mode}: invalid JSON frame {payload:?}: {e}"));
                    // 消息原样往返（合法转义，不丢失字符）。
                    let msg = if mode == "responses" {
                        parsed.pointer("/error/message")
                    } else {
                        parsed.pointer("/error/message")
                    };
                    assert_eq!(
                        msg.and_then(|m| m.as_str()),
                        Some("quote \" backslash \\ newline \n tab \t")
                    );
                }
            }
        }
    }

    /// The root-cause walk must surface the innermost error even when a generic
    /// transport message (reqwest `Kind::Decode`) wraps it.
    #[test]
    fn error_chain_root_walks_to_deepest_cause() {
        // io::Error with no source → itself.
        let plain = std::io::Error::other("boom");
        assert_eq!(error_chain_root(&plain), "boom");

        // io::Error wraps an inner error via .source(); chain to the root.
        let root = std::io::Error::other("unexpected end of gzip file");
        let nested = std::io::Error::new(std::io::ErrorKind::Other, root);
        let chained = std::io::Error::new(std::io::ErrorKind::Other, nested);
        assert_eq!(error_chain_root(&chained), "unexpected end of gzip file");
    }

    fn now() -> String {
        crate::utils::time::now_iso()
    }

    async fn fresh_db() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn channel_mode_health_cools_down_only_the_failing_transport_mode() {
        use crate::db::models::CreateChannelInput;

        let repo = Repository::new(fresh_db().await);
        let channel = repo
            .create_channel(&CreateChannelInput {
                name: "mode-health".into(),
                channel_type: "openai".into(),
                base_url: "https://example.test/v1".into(),
                api_key: "sk-test".into(),
                models: vec!["gpt-4o".into()],
                protocol: Some("openai".into()),
                provider: Some("custom".into()),
                native_base_url: Some("https://example.test/v1".into()),
                native_endpoints: Some(vec!["chat_completions".into()]),
                ..Default::default()
            })
            .await
            .unwrap();
        let failure_at = "2026-08-11T00:00:00.000Z";
        let cooldown_until = "2099-08-11T00:00:00.000Z";

        for _ in 0..2 {
            repo.record_channel_mode_failure(
                &channel.id,
                "chat_completions",
                false,
                failure_at,
                cooldown_until,
                "upstream returned an undecodable body (HTTP 200, 0 bytes)",
            )
            .await
            .unwrap();
        }

        let available_non_stream = repo
            .get_enabled_channels_for_mode("chat_completions", false, failure_at)
            .await
            .unwrap();
        let available_stream = repo
            .get_enabled_channels_for_mode("chat_completions", true, failure_at)
            .await
            .unwrap();
        assert!(available_non_stream.is_empty());
        assert_eq!(available_stream.len(), 1);

        repo.record_channel_mode_success(&channel.id, "chat_completions", false)
            .await
            .unwrap();
        let recovered = repo
            .get_enabled_channels_for_mode("chat_completions", false, failure_at)
            .await
            .unwrap();
        assert_eq!(recovered.len(), 1);
    }

    fn audited_request() -> AuditedRequest {
        use crate::security::gate::{DownstreamProtocol, RequestEnvelope, RequestFeatures};
        use crate::security::SecurityScanResult;
        AuditedRequest {
            envelope: RequestEnvelope {
                downstream_protocol: DownstreamProtocol::ChatCompletions,
                endpoint: "chat_completions".into(),
                original_json: json!({"model": "m", "messages": []}),
                safe_forward_headers: vec![],
                query: None,
                model: "m".into(),
                stream: true,
                trace_id: None,
            },
            forward_json: json!({"model": "m", "messages": []}),
            sanitized_log_json: json!({"model": "m", "messages": []}),
            body_hash: "h".into(),
            body_len: 0,
            audit_result: SecurityScanResult::default(),
            request_features: RequestFeatures::default(),
            security_settings: crate::security::SecuritySettings::default(),
        }
    }

    fn api_key() -> ApiKey {
        ApiKey {
            id: "key-1".into(),
            name: "t".into(),
            key: "sk-test".into(),
            status: 1,
            allowed_models: "[]".into(),
            allowed_channels: "[]".into(),
            denied_models: "[]".into(),
            denied_channels: "[]".into(),
            quota_limit: 0,
            quota_used: 0,
            expires_at: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    // ─── C-03 第二档：Responses 逐帧持久化（续传锚点）────────────────────

    fn responses_upstream(terminated: bool) -> UpstreamStream {
        let mut chunks: Vec<Result<bytes::Bytes, std::io::Error>> = vec![Ok(
            bytes::Bytes::from_static(
                b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_test_42\",\"status\":\"in_progress\"}}\n\n",
            ),
        )];
        let mut second = String::from(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\n\n",
        );
        if terminated {
            second.push_str("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_test_42\",\"status\":\"completed\"}}\n\n");
        }
        chunks.push(Ok(bytes::Bytes::from(second)));
        let body = futures_util::stream::iter(chunks)
            .chain(futures_util::stream::pending::<
                Result<bytes::Bytes, std::io::Error>,
            >())
            .boxed();
        UpstreamStream {
            content_type: "text/event-stream".to_string(),
            headers: vec![],
            body,
        }
    }

    async fn pump_for_responses(upstream: &mut UpstreamStream) -> StreamPumpCore {
        let (first_frame, carry) = buffer_first_record(upstream).await.unwrap();
        let mut sup = crate::core::stream_supervisor::StreamSupervisor::new();
        sup.begin_connect().unwrap();
        sup.on_upstream_headers().unwrap();
        sup.on_first_frame_validated().unwrap();
        let prepared = crate::protocol::codec::CodecRegistry::prepare_pair(
            crate::protocol::codec::Protocol::Responses,
            crate::protocol::codec::Protocol::Responses,
            "up-model",
            &json!({"model":"up-model", "input":"hi"}),
        )
        .unwrap();
        StreamPumpCore::new(sup, prepared.codec.new_stream_decoder(), first_frame, carry).unwrap()
    }

    /// 正常完成的 Responses 流：全部帧按序落段，锚点（response.id）可反查，
    /// 段表 log_id 与落账行 id 一致（续传回放据此判定完成状态）。
    #[tokio::test]
    async fn responses_stream_persists_frames_progressively() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        let mut upstream = responses_upstream(true);
        let pump = pump_for_responses(&mut upstream).await;

        let mut stream = Box::pin(stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "responses".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "openai".to_string(),
            1,
            "responses_g1_native".to_string(),
            None,
            "responses".to_string(),
            "responses".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        ));
        while let Some(item) = stream.next().await {
            item.unwrap();
        }

        let frames = repo
            .get_stream_frames_after("resp_test_42", 0)
            .await
            .unwrap();
        assert!(!frames.is_empty(), "完成流应落全部帧");
        let joined: String = frames.iter().map(|(_, f)| f.as_str()).collect();
        assert!(joined.contains("response.created"));
        assert!(joined.contains("response.output_text.delta"));
        assert!(joined.contains("response.completed"), "终止帧应已持久化");

        // 锚点反查 → 段表 log_id == 落账行 id
        let (anchor_log_id, _) = repo
            .find_stream_anchor("resp_test_42")
            .await
            .unwrap()
            .expect("锚点应可反查");
        let logs = repo.get_logs(10, 0).await.unwrap();
        let row = logs.first().expect("完成流应有落账行");
        assert_eq!(row.id, anchor_log_id, "段表与落账行必须同一 log_id");

        // offset 语义：跳过 seq<=1 的帧
        let tail = repo
            .get_stream_frames_after("resp_test_42", 1)
            .await
            .unwrap();
        assert!(!tail.iter().any(|(_, f)| f.contains("response.created")));
    }

    /// 客户端中断的 Responses 流：已生成帧保留（回放可用），无终止帧，
    /// 499 取消行照常落账（既有取消语义不变）。
    #[tokio::test]
    async fn responses_cancel_keeps_persisted_frames_for_replay() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        let mut upstream = responses_upstream(false);
        let pump = pump_for_responses(&mut upstream).await;

        let mut stream = Box::pin(stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "responses".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "openai".to_string(),
            1,
            "responses_g1_native".to_string(),
            None,
            "responses".to_string(),
            "responses".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        ));
        stream.next().await.unwrap().unwrap();
        stream.next().await.unwrap().unwrap();
        drop(stream);

        // 499 取消行（Drop spawn 异步写）
        let mut cancelled = false;
        for _ in 0..100 {
            let logs = repo.get_logs(10, 0).await.unwrap();
            if logs.first().is_some_and(|row| row.status_code == 499) {
                cancelled = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(cancelled, "取消行必须照常落账");

        let frames = repo
            .get_stream_frames_after("resp_test_42", 0)
            .await
            .unwrap();
        let joined: String = frames.iter().map(|(_, f)| f.as_str()).collect();
        assert!(
            joined.contains("response.output_text.delta"),
            "中断前帧应保留"
        );
        assert!(
            !joined.contains("response.completed"),
            "中断流不应有终止帧（回放端点据此合成 incomplete 收尾）"
        );
    }

    /// 第一档端到端（真实路径）：chat SSE 流（带终止帧、有累计内容）完整走完
    /// stream_response_body → 落账行 → create_log 漏斗 → 段表出现内容段。
    /// 此前段测试直接构造 log 调漏斗，未覆盖 driver 真实累计内容到段的链路。
    #[tokio::test]
    async fn chat_stream_accumulated_content_reaches_segments() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        let mut upstream = hanging_after_terminal_upstream(true, true);
        let pump = pump_for(&mut upstream).await;

        let mut stream = Box::pin(stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        ));
        while let Some(item) = stream.next().await {
            item.unwrap();
        }

        // 落账行存在且为流式
        let row = repo
            .get_logs(10, 0)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("完成流应落账");
        assert_eq!(row.is_stream, 1);

        // 段表出现该 log_id 的内容段（detailed 默认策略 + 流式 + 有累计内容）
        let segments = repo.get_stream_segments(&row.id).await.unwrap();
        assert!(
            !segments.is_empty(),
            "chat SSE 累计内容应经漏斗落段（log {}）",
            row.id
        );
        let joined: String = segments.into_iter().map(|(_, c)| c).collect();
        assert!(
            joined.contains("hi"),
            "段内容应来自真实累计（含上游 delta 文本），实际: {joined}"
        );
    }

    #[test]
    fn extract_response_id_parses_created_event_only() {
        let created =
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_x\",\"status\":\"in_progress\"}}\n\n";
        assert_eq!(extract_response_id(created), Some("resp_x".to_string()));
        // 非 created 事件不提取；data 无空格前缀可解析；坏 JSON 跳过
        let delta = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"x\",\"response\":{\"id\":\"resp_y\"}}\n\n";
        assert_eq!(extract_response_id(delta), None);
        assert_eq!(extract_response_id("data: not-json\n\n"), None);
        assert_eq!(
            extract_response_id(
                "event: response.created\ndata: {\"type\":\"response.created\"}\n\n"
            ),
            None
        );
    }

    /// C-1: the FULL `stream_response_body` emission seam.  The first upstream
    /// chunk contains MULTIPLE Anthropic records (message_start first record +
    /// content_block_delta carry record).  The downstream byte stream must
    /// contain ONLY codec-encoded Chat SSE (records 1 AND 2 decoded), never raw
    /// upstream Anthropic bytes.
    #[tokio::test]
    async fn stream_response_body_carry_is_decoded_not_raw() {
        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool));

        // One upstream chunk: two Anthropic records (downstream Chat client).
        let chunk = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"model\":\"up-model\",\"content\":[]}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"carried\"}}\n\n";
        let mut chunk = chunk.to_vec();
        let body = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from(
            std::mem::take(&mut chunk),
        ))])
        .boxed();
        let mut upstream = UpstreamStream {
            content_type: "text/event-stream".to_string(),
            headers: vec![],
            body,
        };
        let (first_frame, carry) = buffer_first_record(&mut upstream).await.unwrap();
        assert!(
            !carry.is_empty(),
            "carry must span a second record (the real seam)"
        );

        let mut sup = crate::core::stream_supervisor::StreamSupervisor::new();
        sup.begin_connect().unwrap();
        sup.on_upstream_headers().unwrap();
        sup.on_first_frame_validated().unwrap();
        let prepared = crate::protocol::codec::CodecRegistry::prepare_pair(
            crate::protocol::codec::Protocol::Chat,
            crate::protocol::codec::Protocol::Messages,
            "up-model",
            &json!({"model":"up-model", "messages":[{"role":"user","content":"hi"}]}),
        )
        .unwrap();
        let pump =
            StreamPumpCore::new(sup, prepared.codec.new_stream_decoder(), first_frame, carry)
                .unwrap();

        let stream = stream_response_body(
            pump,
            upstream,
            repo,
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            "{}".to_string(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        );

        let mut bytes = Vec::new();
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            bytes.extend_from_slice(&item.unwrap());
        }
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("event: message_start") && !text.contains("event: content_block_delta"),
            "downstream Chat client must NEVER see raw Anthropic bytes via stream_response_body: {text}"
        );
        assert!(
            text.contains("\"content\":\"carried\""),
            "the carry record must be decoded and emitted by stream_response_body: {text}"
        );
    }

    // ─── #57：流式落账语义（终止早退 / 取消保留用量） ─────────────────────

    /// 构造「首记录 + 内容/用量块 + 终止标记，随后挂死」的上游体：
    /// 协议终止事件之后上游流永远不结束（模拟 ModelScope 发完数据延迟关
    /// 连接甚至不关连接的行为），用于验证终止早退。
    fn hanging_after_terminal_upstream(with_stop: bool, with_usage: bool) -> UpstreamStream {
        let mut chunks: Vec<Result<bytes::Bytes, std::io::Error>> = vec![Ok(bytes::Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"model\":\"up-model\",\"content\":[]}}\n\n",
        ))];
        let mut second = String::new();
        second.push_str("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n");
        if with_usage {
            second.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n");
        }
        if with_stop {
            second.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        }
        chunks.push(Ok(bytes::Bytes::from(second)));
        let body = futures_util::stream::iter(chunks)
            .chain(futures_util::stream::pending::<
                Result<bytes::Bytes, std::io::Error>,
            >())
            .boxed();
        UpstreamStream {
            content_type: "text/event-stream".to_string(),
            headers: vec![],
            body,
        }
    }

    async fn pump_for(upstream: &mut UpstreamStream) -> StreamPumpCore {
        let (first_frame, carry) = buffer_first_record(upstream).await.unwrap();
        let mut sup = crate::core::stream_supervisor::StreamSupervisor::new();
        sup.begin_connect().unwrap();
        sup.on_upstream_headers().unwrap();
        sup.on_first_frame_validated().unwrap();
        let prepared = crate::protocol::codec::CodecRegistry::prepare_pair(
            crate::protocol::codec::Protocol::Chat,
            crate::protocol::codec::Protocol::Messages,
            "up-model",
            &json!({"model":"up-model", "messages":[{"role":"user","content":"hi"}]}),
        )
        .unwrap();
        StreamPumpCore::new(sup, prepared.codec.new_stream_decoder(), first_frame, carry).unwrap()
    }

    fn full_request_body() -> String {
        serde_json::json!({
            "model": "m",
            "messages": [{"role":"user","content":"请帮我总结这段足够长的提示词内容，确保本地 token 估算明显大于零，用于验证取消路径的兜底估算逻辑。"}]
        })
        .to_string()
    }

    // ─── FIX-16：响应侧扫描接入主路径（可观察测试） ────────────────────────

    fn audited_request_with_response_scan() -> AuditedRequest {
        let mut audited = audited_request();
        audited.security_settings = crate::security::SecuritySettings {
            enabled: true,
            scan_response: true,
            ..Default::default()
        };
        audited
    }

    /// 非流式 facade：响应体含凭证 → 落账行风险升级、摘要拼「响应侧」、
    /// response 阶段发现入库。
    #[tokio::test]
    async fn non_stream_response_scan_records_findings() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        let audited = audited_request_with_response_scan();
        let execution = crate::core::plan_executor::PlanExecution {
            status: 200,
            body: json!({
                "choices": [{"message": {"role": "assistant",
                    "content": "token sk-abcdefghijklmnopqrstuvwx123456 end"}}]
            }),
            usage: None,
            channel_id: Some("ch-1".into()),
            channel_name: Some("ch".into()),
            upstream_type: Some("channel".into()),
            route_group: Some("chat_g1_native".into()),
            upstream_protocol: Some("openai".into()),
            upstream_endpoint: Some("chat_completions".into()),
            upstream_model: Some("up-model".into()),
            provider: Some("openai".into()),
            identity_revision: Some(1),
            codec_version: None,
            response_headers: vec![],
            attempts: 1,
            duration_ms: 5,
            last_failure: None,
        };
        write_non_stream_log(
            &repo,
            &api_key(),
            &audited,
            "chat",
            &execution,
            5,
            "{}",
            None,
        )
        .await;

        let logs = repo.get_logs(10, 0).await.unwrap();
        assert_eq!(logs.len(), 1);
        let row = &logs[0];
        assert_ne!(
            row.risk_level, "clean",
            "响应凭证必须升级风险等级: {}",
            row.risk_level
        );
        assert!(
            row.risk_summary.as_deref().unwrap_or("").contains("响应侧"),
            "summary: {:?}",
            row.risk_summary
        );
        let findings = repo.get_security_findings(&row.id).await.unwrap();
        assert!(
            findings.iter().any(|f| f.phase == "response"),
            "response 发现必须入库: {findings:?}"
        );
    }

    /// 流式 facade：累积内容含凭证 → 完成行的审计同样并入响应侧发现。
    #[tokio::test]
    async fn stream_response_scan_records_findings() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        let secret_delta = "leak sk-abcdefghijklmnopqrstuvwx123456 now";
        let body = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from(
            format!(
                "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_1\",\"role\":\"assistant\",\"model\":\"up-model\",\"content\":[]}}}}\n\n\
                 event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{secret_delta}\"}}}}\n\n\
                 event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":5}}}}\n\n\
                 event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
            ),
        ))])
        .boxed();
        let mut upstream = UpstreamStream {
            content_type: "text/event-stream".to_string(),
            headers: vec![],
            body,
        };
        let pump = pump_for(&mut upstream).await;

        let stream = stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request_with_response_scan(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        );
        let mut bytes = Vec::new();
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            bytes.extend_from_slice(&item.unwrap());
        }

        let logs = repo.get_logs(10, 0).await.unwrap();
        assert_eq!(logs.len(), 1);
        let row = &logs[0];
        assert_eq!(row.status_code, 200);
        assert_ne!(
            row.risk_level, "clean",
            "流式响应凭证必须升级风险等级: {}",
            row.risk_level
        );
        let findings = repo.get_security_findings(&row.id).await.unwrap();
        assert!(
            findings.iter().any(|f| f.phase == "response"),
            "response 发现必须入库: {findings:?}"
        );
    }

    /// #57 主场景：协议终止事件已到、上游不关连接（EOF 永远不来）时，
    /// 流必须正常完成并落 200 行（带真实 usage 与响应内容），而不是挂到
    /// 客户端超时取消再落 499 零用量行。
    #[tokio::test]
    async fn stream_completes_on_protocol_terminal_without_upstream_eof() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        let mut upstream = hanging_after_terminal_upstream(true, true);
        let pump = pump_for(&mut upstream).await;

        let stream = stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        );

        // 上游挂死时若实现退化回「等 EOF」，这里 10s 超时直接失败。
        let mut bytes = Vec::new();
        tokio::pin!(stream);
        let consumed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(item) = stream.next().await {
                bytes.extend_from_slice(&item.unwrap());
            }
        })
        .await;
        assert!(
            consumed.is_ok(),
            "stream must complete on protocol terminal without upstream EOF"
        );

        let logs = repo.get_logs(10, 0).await.unwrap();
        assert_eq!(logs.len(), 1, "exactly one row after terminal completion");
        let row = &logs[0];
        assert_eq!(
            row.status_code, 200,
            "protocol-complete stream is a success, not 499: {row:?}"
        );
        assert_eq!(
            row.completion_tokens, 5,
            "upstream-reported usage must be recorded"
        );
        assert!(row.client_cancelled.unwrap_or(0) == 0);
        let choices = row.response_choices.as_deref().unwrap_or_default();
        assert!(
            choices.contains("hi"),
            "accumulated content must be recorded: {choices}"
        );
    }

    /// #57 取消路径：客户端中途断开（流被 drop）时，499 行记录流泵快照里
    /// 已解析的真实 usage，而不是硬编码 0。
    #[tokio::test]
    async fn client_cancel_records_pump_usage_snapshot() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        // 无 message_stop：流未终止，客户端在收到第二帧输出后断开。
        let mut upstream = hanging_after_terminal_upstream(false, true);
        let pump = pump_for(&mut upstream).await;

        let mut stream = Box::pin(stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        ));

        // 消费两帧输出（首帧 + 含 usage 的第二帧），确保快照已更新。
        stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        assert!(!second.is_empty());

        // 客户端断开：drop 生成器 → finalizer Drop → 后台写 499 行。
        drop(stream);

        // Drop 的 spawn 是异步任务，轮询等待 499 行出现（上限 5s）。
        let mut row = None;
        for _ in 0..100 {
            let logs = repo.get_logs(10, 0).await.unwrap();
            if let Some(first) = logs.first() {
                row = Some(first.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let row = row.expect("499 row must be written after client cancel");
        assert_eq!(row.status_code, 499);
        assert_eq!(row.client_cancelled.unwrap_or(0), 1);
        assert_eq!(
            row.completion_tokens, 5,
            "cancelled row must keep the pump snapshot usage, not zeros"
        );
    }

    /// #57 兜底：上游从未回报 usage 且客户端取消时，499 行走本地估算
    /// （请求体 + 已收内容），计费统计不恒为 0。
    #[tokio::test]
    async fn client_cancel_estimates_usage_when_upstream_silent() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        let mut upstream = hanging_after_terminal_upstream(false, false);
        let pump = pump_for(&mut upstream).await;

        let mut stream = Box::pin(stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            StreamTimeouts::default(),
        ));

        stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        assert!(!second.is_empty());
        drop(stream);

        let mut row = None;
        for _ in 0..100 {
            let logs = repo.get_logs(10, 0).await.unwrap();
            if let Some(first) = logs.first() {
                row = Some(first.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let row = row.expect("499 row must be written after client cancel");
        assert_eq!(row.status_code, 499);
        assert!(
            row.total_tokens > 0,
            "cancelled row must fall back to a local usage estimate: {row:?}"
        );
    }

    /// FIX-08：帧间空闲超时——上游挂死（发帧后既无终止标记也关连接）时，
    /// 流在 idle 超时后以结构化错误事件收尾并落 502 行，不再无限等待。
    #[tokio::test]
    async fn stream_idle_timeout_ends_hung_upstream_with_error_event() {
        let repo = Arc::new(Repository::new(fresh_db().await));
        // 无 message_stop、无 EOF：第二帧输出后上游永久挂起。
        let mut upstream = hanging_after_terminal_upstream(false, false);
        let pump = pump_for(&mut upstream).await;

        let timeouts = StreamTimeouts {
            first_frame: std::time::Duration::from_secs(30),
            idle: std::time::Duration::from_millis(300),
        };
        let stream = stream_response_body(
            pump,
            upstream,
            repo.clone(),
            api_key(),
            audited_request(),
            "m".to_string(),
            "up-model".to_string(),
            "chat".to_string(),
            false,
            full_request_body(),
            None,
            "ch-1".to_string(),
            "ch".to_string(),
            "anthropic".to_string(),
            1,
            "messages_g1_native".to_string(),
            None,
            "anthropic".to_string(),
            "messages".to_string(),
            "channel".to_string(),
            timeouts,
        );

        let mut bytes = Vec::new();
        tokio::pin!(stream);
        let finished = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(item) = stream.next().await {
                bytes.extend_from_slice(&item.unwrap());
            }
        })
        .await;
        assert!(finished.is_ok(), "idle timeout must end the hung stream");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("upstream idle timeout"),
            "structured error event must carry the timeout reason: {text}"
        );

        // 提交后超时 = 流错误：落 502 行（非 499、非成功 200）。
        let logs = repo.get_logs(10, 0).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].status_code, 502,
            "idle timeout is a stream error: {logs:?}"
        );
    }

    /// FIX-08：StreamTimeouts 缺省值——首帧 60s、空闲 120s。
    #[test]
    fn stream_timeouts_defaults_are_sane() {
        let t = StreamTimeouts::default();
        assert_eq!(t.first_frame, std::time::Duration::from_secs(60));
        assert_eq!(t.idle, std::time::Duration::from_secs(120));
    }
}
