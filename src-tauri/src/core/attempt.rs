//! Prepared attempt + retry/budget state machine (T05).
//!
//! * [`PreparedAttempt`] — T00 decision 1 shape: one fully-encoded, model-mapped
//!   attempt ready for an executor.  The array model mapping is sampled EXACTLY
//!   ONCE here; body / logs / stats all share the same `upstream_model`.
//! * [`AttemptFlow`] — per-group + total retry budget, candidate failover, and
//!   the T00 decision 5 error classification.
//! * [`FailureClass`] — the six error classes from T00 decision 5.
//!
//! Group-transition rule (leader adjudication 2026-08-05): only
//! `EndpointUnsupported` and `Retryable` may cross to the next protocol group
//! after this group is exhausted.  `CallerTerminal` / `CommittedStreamError`
//! halt immediately; `ChannelAuthTerminal` / `UpstreamProtocolError` continue
//! to the next candidate WITHIN the group but never cross groups.

use crate::core::protocol_boundary::downstream_protocol;
use crate::core::route_plan::{
    resolve_upstream_model, AuthNonStreamFraming, GroupTier, RouteGroup, RouteGroupCandidate,
    RoutePlan,
};
use crate::protocol::codec::{CodecRegistry, PreparedCodec};
use crate::security::gate::AuditedRequest;
use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Error classification (T00 decision 5)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum FailureClass {
    /// Local schema / codec unsupported / upstream 400/422 → end immediately.
    CallerTerminal,
    /// Upstream 401/403 → may continue within the same group, never cross groups.
    ChannelAuthTerminal,
    /// 405/501, or 404 proven to be path-not-found → degradable.
    EndpointUnsupported,
    /// Connect failure, timeouts, 408/409/429/5xx/529 → degradable within budget.
    Retryable,
    /// Pre-commit undecodable upstream response → try the next candidate.
    UpstreamProtocolError,
    /// Downstream already committed → NO retry; only a protocol-representable
    /// error may be sent downstream.
    CommittedStreamError,
}

impl FailureClass {
    /// Whether the flow may leave this group for the next group after this
    /// failure.
    ///
    /// NOTE (leader adjudication 2026-08-05): `UpstreamProtocolError` is NOT
    /// cross-group.  T00 decision 5 says it "可继续下一候选" (next candidate)
    /// WITHOUT the cross-group clause that `retryable` carries; design 6.0.1
    /// does not list "上游响应无法解码" among group-switch conditions, and the
    /// "语义/权限错误不得跨协议掩盖" rule governs — an undecodable upstream
    /// response must not be papered over by silently routing to another
    /// protocol's group.  It behaves like `ChannelAuthTerminal` for the
    /// cross-group decision, but (like every non-terminal class) still advances
    /// to the next candidate within the same group.
    pub fn is_degradable(&self) -> bool {
        matches!(
            self,
            FailureClass::EndpointUnsupported | FailureClass::Retryable
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FailureClass::CallerTerminal => "caller_terminal",
            FailureClass::ChannelAuthTerminal => "channel_auth_terminal",
            FailureClass::EndpointUnsupported => "endpoint_unsupported",
            FailureClass::Retryable => "retryable",
            FailureClass::UpstreamProtocolError => "upstream_protocol_error",
            FailureClass::CommittedStreamError => "committed_stream_error",
        }
    }
}

/// Classify an upstream HTTP status.  Returns `None` for 404 because a bare 404
/// is ambiguous: it is only `EndpointUnsupported` when the executor has proven
/// the *path* is missing (T00 decision 5), never for a missing model.
pub fn classify_http_status(status: u16) -> Option<FailureClass> {
    match status {
        400 | 422 => Some(FailureClass::CallerTerminal),
        401 | 403 => Some(FailureClass::ChannelAuthTerminal),
        404 => None,
        405 | 501 => Some(FailureClass::EndpointUnsupported),
        408 | 409 | 429 | 529 => Some(FailureClass::Retryable),
        500..=599 => Some(FailureClass::Retryable),
        // 2xx handled by the executor as success; anything else treat as
        // retryable (conservative — a 3xx/418 etc. should not hard-stop).
        _ => Some(FailureClass::Retryable),
    }
}

/// 404 响应体嗅探（T00 decision 5 的单一事实源）：只有响应体能证明「路径
/// 不存在」才归类 EndpointUnsupported，缺失模型等其余 404 一律 Retryable
/// （缺模型绝不能把调用方终态 4xx 透传下去）。FIX-25：此逻辑原在
/// endpoint_executor，legacy 轨无法访问导致同一 404 两轨语义相反——现
/// 两轨共用本函数。
fn classify_404_with_body(body: Option<&str>) -> FailureClass {
    let Some(body) = body else {
        return FailureClass::Retryable;
    };
    let lower = body.to_lowercase();
    let path_missing = lower.contains("no such endpoint")
        || lower.contains("unknown endpoint")
        || lower.contains("endpoint does not exist")
        || lower.contains("path not found")
        || lower.contains("no such path")
        || (lower.contains("not found")
            && !lower.contains("model")
            && !lower.contains("model_not_found"));
    if path_missing {
        FailureClass::EndpointUnsupported
    } else {
        FailureClass::Retryable
    }
}

/// [`classify_http_status`] 的响应体感知版本：404 结合响应体判定（单一
/// 事实源，新旧两轨共用），其余状态码与无 body 版本逐字一致。
pub fn classify_http_status_with_body(status: u16, body: Option<&str>) -> Option<FailureClass> {
    if status == 404 {
        return Some(classify_404_with_body(body));
    }
    classify_http_status(status)
}

/// Downstream HTTP status reported when the flow halts after the given class.
pub fn terminal_status(class: FailureClass) -> u16 {
    match class {
        FailureClass::CallerTerminal => 400,
        // Do NOT leak an upstream channel's 401/403 as the client's own auth
        // failure.  The gateway holds the channel credentials.
        FailureClass::ChannelAuthTerminal => 502,
        FailureClass::EndpointUnsupported => 501,
        FailureClass::Retryable => 502,
        FailureClass::UpstreamProtocolError => 502,
        FailureClass::CommittedStreamError => 502,
    }
}

/// What a legacy flat retry loop (proxy path / server handlers) should do
/// after an upstream answered with a non-2xx status: cycle to the next
/// channel, or stop and answer downstream with `downstream_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverDecision {
    /// The same request may still succeed on another channel — keep cycling.
    Failover,
    /// The same request would fail on every other channel too — stop and
    /// surface this status downstream.  Never the raw upstream credential
    /// failure: 401/403 are masked to 502 (see `terminal_status`); the
    /// request log keeps the real upstream status.
    Stop { downstream_status: u16 },
}

/// Single source of truth for retry-vs-stop semantics in the legacy flat
/// loops, built on `classify_http_status` (T00 decision 5) so the legacy path
/// and the new track can never drift apart.
///
/// Callers must gate on a non-2xx status before consulting this — success
/// responses never reach a retry decision.  Exhausting every channel yields
/// 502 at the call site, not here.
pub fn upstream_failover_decision(status: u16) -> FailoverDecision {
    debug_assert!(
        !(200..300).contains(&status),
        "callers must gate on non-2xx before consulting the decision"
    );
    match classify_http_status(status) {
        Some(FailureClass::Retryable) => FailoverDecision::Failover,
        Some(FailureClass::ChannelAuthTerminal) => FailoverDecision::Stop {
            downstream_status: terminal_status(FailureClass::ChannelAuthTerminal),
        },
        // CallerTerminal / EndpointUnsupported stop with the status verbatim;
        // the ambiguous 404 (`None`) also stops because the legacy loops can
        // never prove path-not-found.  The remaining classes are never
        // produced by `classify_http_status`; stopping verbatim is the safe
        // default if that ever changes.
        Some(_) | None => FailoverDecision::Stop {
            downstream_status: status,
        },
    }
}

/// [`upstream_failover_decision`] 的响应体感知版本（FIX-25）：调用点持有
/// 上游错误体时必须用本函数——404 不再一律停止透传：无论响应体证明的是
/// 路径不存在（EndpointUnsupported）还是缺失模型（Retryable），新轨都视
/// 为 degradable 换候选渠道，legacy 对齐为 Failover（渠道级 404 往往只是
/// 该渠道没有此端点/模型，换渠道可能成功；全部候选失败由调用点兜底 502）。
/// 其余状态码语义与无 body 版本逐字一致（含 405/501 原样停止，该差异
/// 不在 FIX-25 范围）。
pub fn upstream_failover_decision_with_body(status: u16, _body: Option<&str>) -> FailoverDecision {
    if status == 404 {
        FailoverDecision::Failover
    } else {
        upstream_failover_decision(status)
    }
}

// ---------------------------------------------------------------------------
// Attempt + outcome
// ---------------------------------------------------------------------------

/// A fully-prepared attempt (T00 decision 1 + the fields an executor/log need).
#[derive(Debug, Clone, Serialize)]
pub struct PreparedAttempt {
    pub channel_id: String,
    pub channel_name: String,
    /// `channel` or `auth_account`, used by the executor/log boundary without
    /// ever deriving that information from credentials.
    pub upstream_type: String,
    pub route_group: String,
    pub upstream_protocol: String,
    pub upstream_endpoint: String,
    /// Single source of truth for body / logs / stats (design 11.4).
    pub upstream_model: String,
    pub native_base_url: String,
    /// Registered provider string for auth attempts (`codex`, `kimi`, `grok`); `None`
    /// for regular channels.  The executor never re-derives provider/framing.
    pub auth_provider: Option<String>,
    /// Non-stream framing frozen by RoutePlan for auth attempts; `None` for
    /// channels.  The executor branches solely on this value.
    pub auth_non_stream_framing: Option<AuthNonStreamFraming>,
    /// Compatibility label for persisted observability.  Runtime consumers use
    /// `prepared_codec` exclusively; this label is derived once at prepare time
    /// and is never used to select a decoder.
    pub codec_version: Option<String>,
    /// Immutable request-scoped codec plan.  Every response consumer creates a
    /// fresh decoder through this factory, retaining the exact conversion
    /// context (notably request id and mapped model) that encoded the request.
    /// `None` is the explicit CountTokens/Embeddings bypass only.
    pub prepared_codec: Option<PreparedCodec>,
    pub encoded_body: Value,
    pub conversion_report: Option<Value>,
    pub is_retry: bool,
    pub attempt_no: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Prompt tokens served from upstream cache.
    pub cached_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptSuccess {
    pub status: u16,
    pub body: Value,
    pub usage: Option<TokenUsage>,
    /// For streaming: the downstream SSE event bytes to emit (T06 uses this).
    pub downstream_events: Option<Vec<String>>,
    /// The upstream model actually used; the planner backfills this from the
    /// prepared attempt so logs/stats always match the real request model.
    pub upstream_model: Option<String>,
    /// T06 M-2: safely-passthrough upstream response headers (credentials and
    /// hop-by-hop fields dropped) so the non-stream executor boundary preserves
    /// them and the handler can forward them downstream.
    pub response_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptFailure {
    pub failure_class: FailureClass,
    pub message: String,
    pub status_code: Option<u16>,
    pub retry_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub enum AttemptResult {
    Success(AttemptSuccess),
    Failure(AttemptFailure),
}

// ---------------------------------------------------------------------------
// Attempt construction
// ---------------------------------------------------------------------------

/// Build the encoded attempt for a candidate.  This is where the array model
/// mapping is sampled (exactly once) and where cross-protocol requests are
/// passed through the versioned codec.  A codec rejection here is a
/// `CallerTerminal` failure that happens BEFORE any upstream access; the
/// returned [`AttemptFailure`] carries the codec's redacted rejection reason so
/// clients see which feature failed (T00 decision 8 fail-closed).
pub fn build_prepared_attempt<R: Rng + ?Sized>(
    audit: &AuditedRequest,
    group: &RouteGroup,
    candidate: &RouteGroupCandidate,
    rng: &mut R,
    attempt_no: usize,
) -> Result<PreparedAttempt, AttemptFailure> {
    let mapping: Value = candidate.candidate.mapping_json();
    let upstream_model = resolve_upstream_model(&mapping, &audit.envelope.model, rng);
    let is_retry = attempt_no > 1;
    let channel_id = candidate.candidate.id().to_string();
    let channel_name = candidate.candidate.name().to_string();
    let upstream_type = candidate.candidate.upstream_type().to_string();
    let route_group = group.id.clone();
    // Use the CANDIDATE's own protocol/endpoint, not the group's: a native group
    // may (legitimately) hold more than one upstream protocol when ollama_native
    // is enabled (e.g. OpenAI chat_completions and Ollama api_chat), and the
    // group-level fields only mirror the first candidate.
    let upstream_protocol = candidate.upstream_protocol.as_str().to_string();
    let upstream_endpoint = candidate.upstream_endpoint.clone();
    let native_base_url = candidate.native_base_url.clone();
    let auth_provider = candidate.auth_provider.clone();
    let auth_non_stream_framing = candidate.auth_non_stream_framing;

    // The typed matrix owns every route-plan protocol pair it understands,
    // including identity pairs.  A route-plan-approved Native candidate such
    // as Ollama `api_chat` is deliberately outside that matrix and must retain
    // its wire body; forcing it through a synthetic protocol pair would turn a
    // valid native route into a local 400.
    match downstream_protocol(group.downstream) {
        Some(downstream) => {
            let upstream = crate::core::protocol_boundary::upstream_protocol(
                candidate.upstream_protocol,
                &upstream_endpoint,
            );
            if upstream.is_none() && group.tier == GroupTier::Native {
                return Ok(native_attempt(
                    audit,
                    channel_id,
                    channel_name,
                    upstream_type,
                    route_group,
                    upstream_protocol,
                    upstream_endpoint,
                    upstream_model,
                    native_base_url,
                    auth_provider,
                    auth_non_stream_framing,
                    is_retry,
                    attempt_no,
                ));
            }
            let upstream = upstream.ok_or_else(|| AttemptFailure {
                failure_class: FailureClass::CallerTerminal,
                message: format!(
                    "route candidate {} / {} is not a valid upstream protocol endpoint for {}",
                    candidate.upstream_protocol.as_str(),
                    upstream_endpoint,
                    group.downstream.as_str(),
                ),
                status_code: Some(400),
                retry_after: None,
            })?;
            let prepared = CodecRegistry::prepare_pair(
                downstream,
                upstream,
                &upstream_model,
                &audit.forward_json,
            )
            .map_err(|e| AttemptFailure {
                failure_class: FailureClass::CallerTerminal,
                message: format!(
                    "request cannot be converted to {}: {}",
                    candidate.upstream_protocol.as_str(),
                    e
                ),
                status_code: Some(400),
                retry_after: None,
            })?;
            let report = serde_json::to_value(&prepared.report).unwrap_or(json!({}));
            let codec_version = Some(prepared.codec.label().to_string());
            Ok(PreparedAttempt {
                channel_id,
                channel_name,
                upstream_type,
                route_group,
                upstream_protocol,
                upstream_endpoint,
                upstream_model,
                native_base_url,
                auth_provider,
                auth_non_stream_framing,
                codec_version,
                prepared_codec: Some(prepared.codec),
                encoded_body: prepared.encoded_request,
                conversion_report: Some(report),
                is_retry,
                attempt_no,
            })
        }
        None => Ok(native_attempt(
            audit,
            channel_id,
            channel_name,
            upstream_type,
            route_group,
            upstream_protocol,
            upstream_endpoint,
            upstream_model,
            native_base_url,
            auth_provider,
            auth_non_stream_framing,
            is_retry,
            attempt_no,
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn native_attempt(
    audit: &AuditedRequest,
    channel_id: String,
    channel_name: String,
    upstream_type: String,
    route_group: String,
    upstream_protocol: String,
    upstream_endpoint: String,
    upstream_model: String,
    native_base_url: String,
    auth_provider: Option<String>,
    auth_non_stream_framing: Option<AuthNonStreamFraming>,
    is_retry: bool,
    attempt_no: usize,
) -> PreparedAttempt {
    let mut body = audit.forward_json.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".into(), Value::String(upstream_model.clone()));
    }
    PreparedAttempt {
        channel_id,
        channel_name,
        upstream_type,
        route_group,
        upstream_protocol,
        upstream_endpoint,
        upstream_model,
        native_base_url,
        auth_provider,
        auth_non_stream_framing,
        codec_version: None,
        prepared_codec: None,
        encoded_body: body,
        conversion_report: None,
        is_retry,
        attempt_no,
    }
}

// ---------------------------------------------------------------------------
// Retry / budget state machine (T00 decision 4)
// ---------------------------------------------------------------------------

/// The next action the executor should take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowStep {
    /// Execute `plan.groups[group_idx].candidates[candidate_idx]`.
    Execute {
        group_idx: usize,
        candidate_idx: usize,
        attempt_no: usize,
    },
    /// No more attempts within budget.  `status`/`message` are the final error.
    Halt { status: u16, message: String },
}

/// Drives candidate selection under per-group and total budgets.  Group
/// transition is decided by the LAST failure class:
///   * degradable → may cross to the next group after this group is exhausted;
///   * `ChannelAuthTerminal` → same group only, never cross;
///   * `CallerTerminal` / `CommittedStreamError` → the executor must halt
///     immediately (never call `next()` again).
pub struct AttemptFlow {
    pub plan: RoutePlan,
    group_idx: usize,
    candidate_idx: usize,
    group_attempts: usize,
    total_attempts: usize,
    last_failure: Option<AttemptFailure>,
    last_crossable: bool,
}

impl AttemptFlow {
    pub fn new(plan: RoutePlan) -> Self {
        Self {
            plan,
            group_idx: 0,
            candidate_idx: 0,
            group_attempts: 0,
            total_attempts: 0,
            last_failure: None,
            last_crossable: false,
        }
    }

    pub fn plan(&self) -> &RoutePlan {
        &self.plan
    }

    pub fn last_failure(&self) -> Option<&AttemptFailure> {
        self.last_failure.as_ref()
    }

    pub fn attempts_used(&self) -> usize {
        self.total_attempts
    }

    pub fn record_failure(&mut self, failure: &AttemptFailure) {
        self.last_failure = Some(failure.clone());
        self.last_crossable = failure.failure_class.is_degradable();
    }

    pub fn next_step(&mut self) -> FlowStep {
        loop {
            if self.total_attempts >= self.plan.max_attempts_total {
                return self.halt();
            }
            let Some(group) = self.plan.groups.get(self.group_idx) else {
                return self.halt();
            };
            let group_exhausted = self.group_attempts >= group.max_attempts;
            let candidates_exhausted = self.candidate_idx >= group.candidates.len();
            if group_exhausted || candidates_exhausted {
                if self.last_crossable && self.group_idx + 1 < self.plan.groups.len() {
                    self.group_idx += 1;
                    self.group_attempts = 0;
                    self.candidate_idx = 0;
                    self.last_crossable = false;
                    continue;
                }
                return self.halt();
            }
            let attempt_no = self.total_attempts + 1;
            let group_idx = self.group_idx;
            let candidate_idx = self.candidate_idx;
            self.candidate_idx += 1;
            self.group_attempts += 1;
            self.total_attempts += 1;
            return FlowStep::Execute {
                group_idx,
                candidate_idx,
                attempt_no,
            };
        }
    }

    fn halt(&self) -> FlowStep {
        let (status, message) = match &self.last_failure {
            Some(f) => {
                // ChannelAuthTerminal is normally masked to a canonical 502
                // (a channel's key problem must never look like the caller's).
                // Auth accounts own their credentials, so an explicit
                // status_code (401/403) is honored to let the caller
                // re-authenticate instead of seeing a misleading 502.
                let status = match f.failure_class {
                    FailureClass::ChannelAuthTerminal => f.status_code.unwrap_or(502),
                    _ => terminal_status(f.failure_class),
                };
                (status, f.message.clone())
            }
            None => (503, "No available upstream candidate".to_string()),
        };
        FlowStep::Halt { status, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::feature_flags::FeatureFlags;
    use crate::core::route_plan::{
        authorize_and_plan, authorize_and_plan_with_accounts, EndpointKind,
    };
    use crate::db::models::{ApiKey, AuthAccount, Channel};
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn api_key() -> ApiKey {
        ApiKey {
            id: "key-1".into(),
            name: "t".into(),
            key: "sk".into(),
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

    #[allow(clippy::too_many_arguments)]
    fn channel(
        id: &str,
        channel_type: &str,
        base_url: &str,
        models: &[&str],
        priority: i64,
        weight: i64,
    ) -> Channel {
        Channel {
            id: id.into(),
            name: format!("ch-{}", id),
            channel_type: channel_type.into(),
            base_url: base_url.into(),
            api_key: "sk-test".into(),
            models: serde_json::to_string(
                &models.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
            .unwrap(),
            status: 1,
            priority,
            weight,
            config: "{}".into(),
            model_mapping: "{}".into(),
            timeout_secs: 30,
            protocol: None,
            provider: None,
            native_base_url: None,
            native_endpoints: None,
            preset_revision: None,
            identity_revision: 0,
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

    fn flags() -> FeatureFlags {
        FeatureFlags::all_on()
    }

    fn auth_account(id: &str, model: &str, priority: i64) -> AuthAccount {
        AuthAccount {
            id: id.into(),
            provider: "codex".into(),
            label: format!("account-{id}"),
            account_id: format!("remote-{id}"),
            status: "active".into(),
            disabled: 0,
            priority,
            weight: 1,
            sort_order: 0,
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

    fn chat_plan(channels: Vec<Channel>, rng: &mut StdRng) -> RoutePlan {
        let key = api_key();
        authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &channels,
            &flags(),
            &json!({}),
            rng,
        )
        .unwrap()
    }

    fn failure(class: FailureClass) -> AttemptFailure {
        AttemptFailure {
            failure_class: class,
            message: format!("{} error", class.as_str()),
            status_code: None,
            retry_after: None,
        }
    }

    fn audited() -> crate::security::gate::AuditedRequest {
        use crate::security::gate::{DownstreamProtocol, RequestEnvelope, RequestFeatures};
        use crate::security::SecurityScanResult;
        crate::security::gate::AuditedRequest {
            envelope: RequestEnvelope {
                downstream_protocol: DownstreamProtocol::ChatCompletions,
                endpoint: "chat_completions".into(),
                original_json: json!({ "model": "m", "messages": [] }),
                safe_forward_headers: vec![],
                query: None,
                model: "m".into(),
                stream: false,
                trace_id: None,
            },
            forward_json: json!({ "model": "m", "messages": [] }),
            sanitized_log_json: json!({ "model": "m", "messages": [] }),
            body_hash: "h".into(),
            body_len: 0,
            audit_result: SecurityScanResult::default(),
            request_features: RequestFeatures::default(),
            security_settings: crate::security::SecuritySettings::default(),
        }
    }

    fn audited_messages() -> crate::security::gate::AuditedRequest {
        use crate::security::gate::{DownstreamProtocol, RequestEnvelope, RequestFeatures};
        use crate::security::SecurityScanResult;
        let body = json!({
            "model": "m",
            "max_tokens": 32,
            "messages": [{ "role": "user", "content": "hello" }]
        });
        crate::security::gate::AuditedRequest {
            envelope: RequestEnvelope {
                downstream_protocol: DownstreamProtocol::Messages,
                endpoint: "messages".into(),
                original_json: body.clone(),
                safe_forward_headers: vec![],
                query: None,
                model: "m".into(),
                stream: false,
                trace_id: None,
            },
            forward_json: body.clone(),
            sanitized_log_json: body,
            body_hash: "h".into(),
            body_len: 0,
            audit_result: SecurityScanResult::default(),
            request_features: RequestFeatures::default(),
            security_settings: crate::security::SecuritySettings::default(),
        }
    }

    fn with_stream(
        mut audit: crate::security::gate::AuditedRequest,
        stream: bool,
    ) -> crate::security::gate::AuditedRequest {
        audit.envelope.stream = stream;
        for body in [
            &mut audit.envelope.original_json,
            &mut audit.forward_json,
            &mut audit.sanitized_log_json,
        ] {
            body.as_object_mut()
                .expect("request fixture is an object")
                .insert("stream".into(), Value::Bool(stream));
        }
        audit
    }

    fn audited_responses(stream: bool) -> crate::security::gate::AuditedRequest {
        use crate::security::gate::{DownstreamProtocol, RequestEnvelope, RequestFeatures};
        use crate::security::SecurityScanResult;
        let body = json!({
            "model": "m",
            "input": "hi",
            "instructions": "You are a helpful assistant.",
            "stream": stream,
        });
        crate::security::gate::AuditedRequest {
            envelope: RequestEnvelope {
                downstream_protocol: DownstreamProtocol::Responses,
                endpoint: "responses".into(),
                original_json: body.clone(),
                safe_forward_headers: vec![],
                query: None,
                model: "m".into(),
                stream,
                trace_id: None,
            },
            forward_json: body.clone(),
            sanitized_log_json: body,
            body_hash: "h".into(),
            body_len: 0,
            audit_result: SecurityScanResult::default(),
            request_features: RequestFeatures::default(),
            security_settings: crate::security::SecuritySettings::default(),
        }
    }

    #[test]
    fn chat_conversion_failover_uses_each_candidate_codec_direction() {
        for stream in [false, true] {
            let audit = with_stream(audited(), stream);
            let first = channel(
                "anthropic",
                "claude",
                "https://api.anthropic.com/v1",
                &["m"],
                10,
                1,
            );
            let account = auth_account("responses", "m", 1);
            let plan = authorize_and_plan_with_accounts(
                &api_key(),
                "m",
                EndpointKind::ChatCompletions,
                &[first],
                &[account],
                &flags(),
                &audit.forward_json,
                &mut StdRng::seed_from_u64(7),
            )
            .expect("mixed conversion plan");
            let group = &plan.groups[0];
            assert_eq!(group.candidates.len(), 2);

            let mut flow = AttemptFlow::new(plan);
            let FlowStep::Execute {
                group_idx,
                candidate_idx,
                ..
            } = flow.next_step()
            else {
                panic!("first conversion candidate");
            };
            let mut rng = StdRng::seed_from_u64(7);
            let first_attempt = build_prepared_attempt(
                &audit,
                &flow.plan.groups[group_idx],
                &flow.plan.groups[group_idx].candidates[candidate_idx],
                &mut rng,
                1,
            )
            .expect("Anthropic conversion encodes");
            assert_eq!(
                first_attempt.codec_version.as_deref(),
                Some("chat_to_messages_v1")
            );

            flow.record_failure(&failure(FailureClass::Retryable));
            let FlowStep::Execute {
                group_idx,
                candidate_idx,
                ..
            } = flow.next_step()
            else {
                panic!("second conversion candidate after first failure");
            };
            let second_attempt = build_prepared_attempt(
                &audit,
                &flow.plan.groups[group_idx],
                &flow.plan.groups[group_idx].candidates[candidate_idx],
                &mut rng,
                2,
            )
            .expect("Responses auth fallback encodes");
            assert_eq!(second_attempt.upstream_type, "auth_account");
            assert_eq!(
                second_attempt.codec_version.as_deref(),
                Some("chat_to_responses_v1")
            );
            assert!(second_attempt.encoded_body.get("input").is_some());
        }
    }

    #[test]
    fn messages_conversion_failover_uses_each_candidate_codec_direction() {
        for stream in [false, true] {
            let first = channel(
                "openai",
                "openai",
                "https://api.openai.com/v1",
                &["m"],
                10,
                1,
            );
            let account = auth_account("responses", "m", 1);
            let audit = with_stream(audited_messages(), stream);
            let plan = authorize_and_plan_with_accounts(
                &api_key(),
                "m",
                EndpointKind::Messages,
                &[first],
                &[account],
                &flags(),
                &audit.forward_json,
                &mut StdRng::seed_from_u64(7),
            )
            .expect("mixed conversion plan");

            let mut flow = AttemptFlow::new(plan);
            let FlowStep::Execute {
                group_idx,
                candidate_idx,
                ..
            } = flow.next_step()
            else {
                panic!("first conversion candidate");
            };
            let mut rng = StdRng::seed_from_u64(7);
            let first_attempt = build_prepared_attempt(
                &audit,
                &flow.plan.groups[group_idx],
                &flow.plan.groups[group_idx].candidates[candidate_idx],
                &mut rng,
                1,
            )
            .expect("OpenAI conversion encodes");
            assert_eq!(
                first_attempt.codec_version.as_deref(),
                Some("messages_to_chat_v1")
            );

            flow.record_failure(&failure(FailureClass::Retryable));
            let FlowStep::Execute {
                group_idx,
                candidate_idx,
                ..
            } = flow.next_step()
            else {
                panic!("second conversion candidate after first failure");
            };
            let second_attempt = build_prepared_attempt(
                &audit,
                &flow.plan.groups[group_idx],
                &flow.plan.groups[group_idx].candidates[candidate_idx],
                &mut rng,
                2,
            )
            .expect("Responses auth fallback encodes");
            assert_eq!(second_attempt.upstream_type, "auth_account");
            assert_eq!(
                second_attempt.codec_version.as_deref(),
                Some("messages_to_responses_v2")
            );
            assert_eq!(
                second_attempt
                    .prepared_codec
                    .as_ref()
                    .map(PreparedCodec::label),
                Some("messages_to_responses_v2")
            );
            assert!(second_attempt.encoded_body.get("input").is_some());
        }
    }

    /// Responses → Chat uses the typed matrix entry and retains the prepared
    /// codec that must later create the inverse response decoder.
    #[test]
    fn responses_to_chat_attempt_encodes_openai_body() {
        for stream in [false, true] {
            let audit = audited_responses(stream);
            let ch = channel(
                "deepseek",
                "openai",
                "https://api.deepseek.com",
                &["m"],
                1,
                1,
            );
            let plan = authorize_and_plan(
                &api_key(),
                "m",
                EndpointKind::Responses,
                std::slice::from_ref(&ch),
                &flags(),
                &audit.forward_json,
                &mut StdRng::seed_from_u64(7),
            )
            .expect("responses_to_chat plan");
            let group = &plan.groups[0];
            assert_eq!(group.tier.as_str(), "conversion");
            assert_eq!(group.upstream_endpoint, "chat_completions");

            let mut rng = StdRng::seed_from_u64(7);
            let attempt = build_prepared_attempt(&audit, group, &group.candidates[0], &mut rng, 1)
                .expect("responses_to_chat encodes");
            assert_eq!(
                attempt.codec_version.as_deref(),
                Some("responses_to_chat_v1")
            );
            assert_eq!(
                attempt.prepared_codec.as_ref().map(PreparedCodec::label),
                Some("responses_to_chat_v1")
            );
            // The encoded body is the OpenAI chat shape, not the Responses shape.
            assert!(attempt.encoded_body.get("messages").is_some());
            assert!(attempt.encoded_body.get("input").is_none());
            assert_eq!(
                attempt.encoded_body["stream"],
                Value::Bool(stream),
                "stream flag must be preserved into the chat body"
            );
            assert_eq!(
                attempt.encoded_body["model"].as_str().unwrap_or(""),
                attempt.upstream_model.as_str(),
                "sampled upstream model must be baked into the encoded body"
            );
            // The codec version label makes this a conversion attempt.
            assert_eq!(attempt.upstream_protocol, "openai");
            assert_eq!(attempt.upstream_endpoint, "chat_completions");
        }
    }

    #[test]
    fn array_mapping_sampled_once_shared_by_body_and_logs() {
        let mut ch = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        ch.model_mapping = json!({ "m": ["up-a", "up-b", "up-c"] }).to_string();
        let plan = chat_plan(vec![ch], &mut StdRng::seed_from_u64(1));
        let group = &plan.groups[0];
        let candidate = &group.candidates[0];
        let mut rng = StdRng::seed_from_u64(1);
        let attempt = build_prepared_attempt(&audited(), group, candidate, &mut rng, 1)
            .expect("native attempt");
        // The body model baked into the attempt is the SAME sampled model used
        // for logs/stats (design 11.4 — "实际上游模型等于日志/统计模型").
        let body_model = attempt.encoded_body["model"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert_eq!(body_model, attempt.upstream_model);
        assert!(["up-a", "up-b", "up-c"].contains(&attempt.upstream_model.as_str()));
        assert!(!attempt.is_retry, "attempt_no 1 is not a retry");
        // Building the same plan with the same seed re-samples deterministically.
        let mut rng2 = StdRng::seed_from_u64(1);
        let attempt2 = build_prepared_attempt(&audited(), group, candidate, &mut rng2, 1).unwrap();
        assert_eq!(attempt.upstream_model, attempt2.upstream_model);
    }

    #[test]
    fn approved_ollama_api_chat_native_route_bypasses_codec_matrix() {
        let mut ollama = channel("ollama", "ollama", "http://localhost:11434", &["m"], 1, 1);
        ollama.protocol = Some("ollama".into());
        ollama.provider = Some("ollama".into());
        ollama.native_base_url = Some("http://localhost:11434".into());
        ollama.native_endpoints = Some(serde_json::json!(["api_chat"]).to_string());
        let plan = authorize_and_plan(
            &api_key(),
            "m",
            EndpointKind::ChatCompletions,
            &[ollama],
            &flags(),
            &audited().forward_json,
            &mut StdRng::seed_from_u64(7),
        )
        .expect("route-plan-approved native Ollama chat");
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(plan.groups[0].upstream_endpoint, "api_chat");
        let mut rng = StdRng::seed_from_u64(7);
        let attempt = build_prepared_attempt(
            &audited(),
            &plan.groups[0],
            &plan.groups[0].candidates[0],
            &mut rng,
            1,
        )
        .expect("native Ollama attempt must not be matrix-rejected");
        assert!(attempt.prepared_codec.is_none());
        assert_eq!(attempt.codec_version, None);
        assert_eq!(attempt.encoded_body["model"], attempt.upstream_model);
        assert_eq!(
            attempt.encoded_body["messages"],
            audited().forward_json["messages"]
        );
    }

    #[test]
    fn retry_re_samples_array_model_mapping() {
        // F4: a retry draws a NEW sample from the same array mapping.  We drive
        // two attempts from one RNG; each build consumes RNG state, so the
        // retry is a fresh draw (design 11.4: "重试是否重新抽样要显式定义").
        let mut ch = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        ch.model_mapping = json!({ "m": ["up-a", "up-b"] }).to_string();
        let plan = chat_plan(vec![ch], &mut StdRng::seed_from_u64(1));
        let group = &plan.groups[0];
        let candidate = &group.candidates[0];
        let mut rng = StdRng::seed_from_u64(1);
        let attempt1 = build_prepared_attempt(&audited(), group, candidate, &mut rng, 1).unwrap();
        let attempt2 = build_prepared_attempt(&audited(), group, candidate, &mut rng, 2).unwrap();
        assert!(!attempt1.is_retry);
        assert!(attempt2.is_retry, "attempt_no 2 must be flagged as a retry");
        assert!(attempt1.attempt_no < attempt2.attempt_no);
        // Both draws are valid members of the mapping array.
        for a in [&attempt1, &attempt2] {
            assert!(["up-a", "up-b"].contains(&a.upstream_model.as_str()));
            assert_eq!(
                a.encoded_body["model"].as_str().unwrap(),
                a.upstream_model,
                "body model must match the sampled upstream model on every attempt"
            );
        }
    }

    #[test]
    fn flow_crosses_when_group_ends_in_retryable_after_auth_terminal() {
        // F2 (leader-ratified per-last-failure semantics): group transition is
        // decided by the LAST failure's class.  [ChannelAuthTerminal,
        // Retryable] crosses because the last failure is retryable.
        let c1 = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 10, 1);
        let c2 = channel("n2", "openai", "https://api.openai.com/v1", &["m"], 5, 1);
        let conv = channel("c1", "claude", "https://api.anthropic.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![c1, c2, conv], &mut rng));

        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = step
        else {
            panic!()
        };
        assert_eq!(group_idx, 0);
        assert_eq!(
            flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id(),
            "n1"
        );
        flow.record_failure(&failure(FailureClass::ChannelAuthTerminal));

        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = step
        else {
            panic!()
        };
        assert_eq!(group_idx, 0, "auth terminal continues within the group");
        assert_eq!(
            flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id(),
            "n2"
        );
        flow.record_failure(&failure(FailureClass::Retryable));

        // Group exhausted; last failure is retryable → cross to conversion.
        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = step
        else {
            panic!("group ending in Retryable must cross to the next group");
        };
        assert_eq!(group_idx, 1, "cross to conversion group");
        assert_eq!(
            flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id(),
            "c1"
        );
    }

    #[test]
    fn endpoint_unsupported_exhausts_group_then_crosses() {
        // F9: EndpointUnsupported is degradable — it exhausts the current group
        // and crosses to the next group.
        let c1 = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 10, 1);
        let conv = channel("c1", "claude", "https://api.anthropic.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![c1, conv], &mut rng));

        let step = flow.next_step();
        let FlowStep::Execute { group_idx, .. } = step else {
            panic!()
        };
        assert_eq!(group_idx, 0);
        flow.record_failure(&failure(FailureClass::EndpointUnsupported));
        let step = flow.next_step();
        let FlowStep::Execute { group_idx, .. } = step else {
            panic!("EndpointUnsupported must cross to the next group after exhaustion");
        };
        assert_eq!(group_idx, 1);
    }

    #[test]
    fn flow_stays_in_native_group_then_enters_conversion_on_retryable() {
        let native = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        let conv = channel("c1", "claude", "https://api.anthropic.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![native, conv], &mut rng));
        assert_eq!(flow.plan.groups.len(), 2);

        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            attempt_no,
        } = step
        else {
            panic!("expected Execute");
        };
        assert_eq!(group_idx, 0, "native group first");
        assert_eq!(
            flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id(),
            "n1"
        );
        assert_eq!(attempt_no, 1);

        flow.record_failure(&failure(FailureClass::Retryable));
        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            attempt_no,
        } = step
        else {
            panic!("expected Execute after retryable");
        };
        assert_eq!(group_idx, 1, "conversion group after native exhausted");
        assert_eq!(
            flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id(),
            "c1"
        );
        assert_eq!(attempt_no, 2);
    }

    #[test]
    fn auth_terminal_blocks_cross_group() {
        let native = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        let conv = channel("c1", "claude", "https://api.anthropic.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![native, conv], &mut rng));

        let step = flow.next_step();
        assert!(matches!(step, FlowStep::Execute { .. }));
        flow.record_failure(&failure(FailureClass::ChannelAuthTerminal));
        let step = flow.next_step();
        assert!(
            matches!(step, FlowStep::Halt { .. }),
            "401/403 must not cross protocol groups"
        );
    }

    #[test]
    fn upstream_protocol_error_continues_within_group() {
        // Two same-group native candidates; the first returns an undecodable
        // upstream response.  The flow must continue to the SECOND candidate in
        // the SAME group (never cross to conversion yet).
        let c1 = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 10, 1);
        let c2 = channel("n2", "openai", "https://api.openai.com/v1", &["m"], 5, 1);
        let conv = channel("c1", "claude", "https://api.anthropic.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![c1, c2, conv], &mut rng));

        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = step
        else {
            panic!()
        };
        assert_eq!(group_idx, 0);
        assert_eq!(
            flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id(),
            "n1"
        );

        flow.record_failure(&failure(FailureClass::UpstreamProtocolError));
        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = step
        else {
            panic!("upstream_protocol_error must continue to the next candidate in the same group");
        };
        assert_eq!(group_idx, 0, "must stay in the native group");
        assert_eq!(
            flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id(),
            "n2"
        );
    }

    #[test]
    fn upstream_protocol_error_does_not_cross_group_when_group_exhausted() {
        // Native group has ONE candidate which fails with an undecodable
        // upstream response.  The group is now exhausted and the conversion
        // group MUST NOT be entered.
        let native = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        let conv = channel("c1", "claude", "https://api.anthropic.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![native, conv], &mut rng));
        assert_eq!(flow.plan.groups.len(), 2);

        let step = flow.next_step();
        let FlowStep::Execute { group_idx, .. } = step else {
            panic!()
        };
        assert_eq!(group_idx, 0);
        flow.record_failure(&failure(FailureClass::UpstreamProtocolError));
        let step = flow.next_step();
        let FlowStep::Halt { status, .. } = step else {
            panic!("upstream_protocol_error must NOT cross to the conversion group");
        };
        assert_eq!(status, 502);
        assert_eq!(
            flow.last_failure().unwrap().failure_class,
            FailureClass::UpstreamProtocolError
        );
    }

    #[test]
    fn auth_channel_auth_terminal_halt_honors_real_401() {
        // Auth accounts own their OAuth credentials: halt must surface the real
        // upstream 401 (carried in status_code) so the caller can re-login.
        // Channel failures carry no explicit status and stay masked at 502.
        let native = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![native], &mut rng));
        let _ = flow.next_step();
        let mut f = failure(FailureClass::ChannelAuthTerminal);
        f.status_code = Some(401);
        flow.record_failure(&f);
        let step = flow.next_step();
        let FlowStep::Halt { status, .. } = step else {
            panic!("single exhausted candidate must halt")
        };
        assert_eq!(status, 401);

        let native = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![native], &mut rng));
        let _ = flow.next_step();
        flow.record_failure(&failure(FailureClass::ChannelAuthTerminal));
        let step = flow.next_step();
        let FlowStep::Halt { status, .. } = step else {
            panic!("single exhausted candidate must halt")
        };
        assert_eq!(status, 502, "channels keep the canonical 502 mask");
    }

    #[test]
    fn caller_terminal_halts_immediately() {
        let native = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 1, 1);
        let conv = channel("c1", "claude", "https://api.anthropic.com/v1", &["m"], 1, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![native, conv], &mut rng));
        let _ = flow.next_step();
        flow.record_failure(&failure(FailureClass::CallerTerminal));
        let step = flow.next_step();
        let FlowStep::Halt { status, .. } = step else {
            panic!("expected Halt")
        };
        assert_eq!(status, 400);
    }

    #[test]
    fn within_group_failover_to_next_candidate() {
        let c1 = channel("n1", "openai", "https://api.openai.com/v1", &["m"], 10, 1);
        let c2 = channel("n2", "openai", "https://api.openai.com/v1", &["m"], 5, 1);
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(vec![c1, c2], &mut rng));

        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = step
        else {
            panic!()
        };
        assert_eq!(group_idx, 0);
        assert_eq!(
            flow.plan.groups[0].candidates[candidate_idx].candidate.id(),
            "n1"
        );
        flow.record_failure(&failure(FailureClass::ChannelAuthTerminal));
        let step = flow.next_step();
        let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = step
        else {
            panic!("same group should continue")
        };
        assert_eq!(group_idx, 0);
        assert_eq!(
            flow.plan.groups[0].candidates[candidate_idx].candidate.id(),
            "n2"
        );
    }

    #[test]
    fn each_candidate_tried_once_within_group_budget() {
        // 5 same-tier channels but the per-group budget is 3 → exactly 3 tried,
        // each at most once; then the group exhausts and the flow halts (there
        // is no conversion group to cross to).
        let channels: Vec<Channel> = (0..5)
            .map(|i| {
                channel(
                    &format!("n{}", i),
                    "openai",
                    "https://api.openai.com/v1",
                    &["m"],
                    1,
                    1,
                )
            })
            .collect();
        let mut rng = StdRng::seed_from_u64(7);
        let mut flow = AttemptFlow::new(chat_plan(channels, &mut rng));
        assert_eq!(flow.plan.max_attempts_total, 3, "total = min(6, group=3)");
        let mut seen = Vec::new();
        while let FlowStep::Execute {
            group_idx,
            candidate_idx,
            ..
        } = flow.next_step()
        {
            let id = flow.plan.groups[group_idx].candidates[candidate_idx]
                .candidate
                .id()
                .to_string();
            assert!(!seen.contains(&id), "candidate must be tried at most once");
            seen.push(id);
            flow.record_failure(&failure(FailureClass::Retryable));
        }
        assert_eq!(seen.len(), 3, "per-group budget caps attempts at 3");
        assert_eq!(flow.attempts_used(), 3);
    }

    #[test]
    fn classify_http_status_matrix() {
        assert_eq!(
            classify_http_status(400),
            Some(FailureClass::CallerTerminal)
        );
        assert_eq!(
            classify_http_status(422),
            Some(FailureClass::CallerTerminal)
        );
        assert_eq!(
            classify_http_status(401),
            Some(FailureClass::ChannelAuthTerminal)
        );
        assert_eq!(
            classify_http_status(403),
            Some(FailureClass::ChannelAuthTerminal)
        );
        assert_eq!(classify_http_status(404), None);
        assert_eq!(
            classify_http_status(405),
            Some(FailureClass::EndpointUnsupported)
        );
        assert_eq!(
            classify_http_status(501),
            Some(FailureClass::EndpointUnsupported)
        );
        assert_eq!(classify_http_status(408), Some(FailureClass::Retryable));
        assert_eq!(classify_http_status(409), Some(FailureClass::Retryable));
        assert_eq!(classify_http_status(429), Some(FailureClass::Retryable));
        assert_eq!(classify_http_status(529), Some(FailureClass::Retryable));
        assert_eq!(classify_http_status(500), Some(FailureClass::Retryable));
        assert_eq!(classify_http_status(503), Some(FailureClass::Retryable));
    }

    #[test]
    fn terminal_status_mapping() {
        assert_eq!(terminal_status(FailureClass::CallerTerminal), 400);
        assert_eq!(terminal_status(FailureClass::ChannelAuthTerminal), 502);
        assert_eq!(terminal_status(FailureClass::EndpointUnsupported), 501);
        assert_eq!(terminal_status(FailureClass::Retryable), 502);
        assert_eq!(terminal_status(FailureClass::UpstreamProtocolError), 502);
        assert_eq!(terminal_status(FailureClass::CommittedStreamError), 502);
    }

    #[test]
    fn upstream_failover_decision_matrix() {
        let stop = |downstream_status: u16| FailoverDecision::Stop { downstream_status };
        // Caller-terminal: the request itself is invalid — surface verbatim.
        assert_eq!(upstream_failover_decision(400), stop(400));
        assert_eq!(upstream_failover_decision(422), stop(422));
        // Channel credentials failed: masked to 502 so the caller never
        // mistakes an upstream auth failure for their own key (the request
        // log keeps the real upstream status).
        assert_eq!(upstream_failover_decision(401), stop(502));
        assert_eq!(upstream_failover_decision(403), stop(502));
        // Ambiguous 404: path-missing is never proven in the legacy loops.
        assert_eq!(upstream_failover_decision(404), stop(404));
        // Endpoint unsupported: verbatim, never retried as a generic 5xx.
        assert_eq!(upstream_failover_decision(405), stop(405));
        assert_eq!(upstream_failover_decision(501), stop(501));
        // Transient statuses and the conservative default for everything
        // unlisted (3xx / 402 / 418 / 425 / 451 …) keep cycling channels.
        for status in [
            408, 409, 429, 529, 500, 502, 503, 504, 301, 307, 402, 418, 425, 451,
        ] {
            assert_eq!(
                upstream_failover_decision(status),
                FailoverDecision::Failover,
                "status {status} should fail over"
            );
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "callers must gate on non-2xx")]
    fn upstream_failover_decision_rejects_2xx() {
        let _ = upstream_failover_decision(200);
    }

    /// FIX-25：404 结合响应体判定，与新轨 classify_upstream_status 的 404
    /// 语义一致（两种形态都 degradable → Failover）；其余状态码与无 body
    /// 版本逐字一致。
    #[test]
    fn upstream_failover_decision_404_body_aware_matches_new_track() {
        let stop = |downstream_status: u16| FailoverDecision::Stop { downstream_status };
        // 404 一律换渠道：路径不存在（渠道缺此端点，换渠道可能成功）与
        // 缺失模型（绝不能把调用方终态 4xx 透传）都 degradable。
        for body in [
            Some(r#"{"error":{"message":"no such endpoint: /v1/messages"}}"#),
            Some(r#"{"error":{"message":"model not found: gpt-x"}}"#),
            Some("Not Found"),
            None,
        ] {
            assert_eq!(
                upstream_failover_decision_with_body(404, body),
                FailoverDecision::Failover,
                "404 (body={body:?}) must fail over like the new track"
            );
        }
        // 非 404 与无 body 版本逐字一致（对照矩阵抽样）。
        assert_eq!(
            upstream_failover_decision_with_body(400, Some("x")),
            stop(400)
        );
        assert_eq!(upstream_failover_decision_with_body(401, None), stop(502));
        assert_eq!(upstream_failover_decision_with_body(405, None), stop(405));
        assert_eq!(
            upstream_failover_decision_with_body(429, None),
            FailoverDecision::Failover
        );
    }
}
