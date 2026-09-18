//! Protocol endpoint executors (T06).
//!
//! Separates URL construction, auth, HTTP send and safely-passthrough response
//! headers from the business codec.  An executor takes a [`PreparedAttempt`]
//! plus safe forward headers and produces the raw upstream status/headers/body
//! or a byte stream.  Executors do NOT do model randomization, routing, or log
//! sanitization; protocol conversion happens ONLY in the legacy Gemini override
//! (which is selected exclusively via `identity.legacy_executor_override`).
//!
//! URL matrix (design 5.1 / 6.2, per-preset mock verified):
//! | upstream | endpoint            | final path                      |
//! |----------|---------------------|---------------------------------|
//! | openai   | chat_completions    | `<native_base>/chat/completions`|
//! | openai   | responses           | `<native_base>/responses`       |
//! | openai   | embeddings          | `<native_base>/embeddings`      |
//! | anthropic| messages            | `<native_base>/messages`（Base 自带 /v1） |
//! | anthropic| count_tokens        | `<native_base>/messages/count_tokens` |
//! | ollama   | api_chat            | `<native_base>/api/chat`        |
//! | gemini   | (legacy override)   | `<base>/v1beta/models/{m}:generateContent?key=` |
//!
//! Auth matrix:
//! | scheme | header / URL |
//! |--------|--------------|
//! | bearer | `Authorization: Bearer <key>` |
//! | x_api_key | `x-api-key: <key>` (+ `anthropic-version`) |
//! | query_key | `?key=<key>` (legacy Gemini) |
//! | optional_bearer | `Authorization: Bearer <key>` only when the key is non-empty (Ollama) |

pub mod driver;
pub mod estimate_usage;
pub mod sse;

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod mock_tests;

use crate::channel_presets::AuthScheme;
use crate::core::attempt::{
    classify_http_status, AttemptFailure, AttemptResult, AttemptSuccess, FailureClass,
    PreparedAttempt, TokenUsage,
};
use crate::core::channel_identity::ChannelIdentity;
use crate::core::route_plan::{AuthNonStreamFraming, EndpointKind};
use crate::db::models::Channel;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::header;
use serde_json::Value;
use std::time::Duration;

/// Default idle timeout for mid-stream SSE reads is configurable via
/// `StreamTimeouts`（FIX-08：`stream.idle_timeout_secs`，缺省 120s，0 = 禁用），
/// 不再使用固定常量。

/// Maximum backoff applied to a `Retry-After` value before jitter.
const RETRY_AFTER_CAP_SECS: u64 = 5;

/// Parse an HTTP `Retry-After` header value into seconds.
///
/// Supports both delta-seconds (`"120"`) and RFC 7231 IMF-fixdate
/// (`"Wed, 21 Oct 2026 07:28:00 GMT"`).  Returns `None` on parse failure.
fn parse_retry_after(value: Option<&str>) -> Option<u64> {
    let raw = value?.trim();
    // Try delta-seconds first.
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(secs);
    }
    // Try RFC 7231 IMF-fixdate.
    if let Ok(dt) = DateTime::parse_from_rfc2822(raw) {
        let now = Utc::now();
        if dt > now {
            let secs = (dt.with_timezone(&Utc) - now).num_seconds().max(0) as u64;
            return Some(secs);
        }
        return Some(0);
    }
    None
}

/// Extract a capped, jittered backoff from a response's `Retry-After` header.
///
/// Returns `Some(Duration)` when the header is present and parseable,
/// capped to [`RETRY_AFTER_CAP_SECS`] with ±20% jitter applied.
fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = parse_retry_after(headers.get("retry-after").and_then(|v| v.to_str().ok()))?;
    let capped = raw.min(RETRY_AFTER_CAP_SECS);
    // ±20% jitter: multiply by a factor in [0.8, 1.2].
    let jitter_factor = 0.8 + (rand::random::<f64>() * 0.4);
    let jittered = (capped as f64 * jitter_factor).round() as u64;
    Some(Duration::from_secs(jittered))
}

/// Wrapper for the next item from an upstream byte stream with an idle guard.
///
/// [`UpstreamItem::Chunk`] wraps the raw `Option<Result<Bytes, _>>` from the
/// stream; [`UpstreamItem::IdleTimeout`] signals that no data arrived within
/// the configured idle window.
pub enum UpstreamItem<E> {
    Chunk(Option<Result<bytes::Bytes, E>>),
    IdleTimeout,
}

/// Read the next item from a stream with an idle-timeout guard.
///
/// If the stream does not produce an item within `idle_timeout`, returns
/// [`UpstreamItem::IdleTimeout`] instead of waiting indefinitely.
pub async fn next_upstream_item<S, E>(upstream: &mut S, idle_timeout: Duration) -> UpstreamItem<E>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
{
    match tokio::time::timeout(idle_timeout, upstream.next()).await {
        Ok(item) => UpstreamItem::Chunk(item),
        Err(_) => UpstreamItem::IdleTimeout,
    }
}

/// A connected upstream stream response (2xx, content still flowing).
pub struct UpstreamStream {
    /// Upstream `Content-Type` (forwarded downstream for native passthrough).
    pub content_type: String,
    /// Safe response headers to forward downstream (credentials/hop-by-hop dropped).
    pub headers: Vec<(String, String)>,
    /// Raw upstream byte stream.
    pub body: futures_util::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
}

/// Per-attempt streaming outcome produced by an executor closure.
pub enum StreamAttemptResult {
    /// The upstream accepted the request; headers + a byte stream are available.
    Connected(UpstreamStream),
    /// Failed before any downstream bytes were committed (classified).
    Failure(AttemptFailure),
}

/// Dispatch an Auth Account attempt.  Account providers intentionally own their
/// request policy (credential headers, 401 refresh/retry, and quota persistence);
/// this module only converts the provider result into the executor's common
/// attempt shape.
///
/// Non-stream framing comes exclusively from the RoutePlan-frozen
/// [`PreparedAttempt::auth_non_stream_framing`]:
///
/// * `ForcedResponsesSse` — Codex: force `stream:true`, aggregate the upstream
///   Responses SSE into a complete document, then decode.
/// * `Json` — Kimi Chat/Anthropic: force `stream:false`, drop `stream_options`,
///   send the encoded Chat/Messages body, parse the provider-native JSON and
///   decode through the prepared codec.
///
/// The executor never guesses framing from the body, Content-Type, or a
/// database row — the attempt is the only source of truth.
pub async fn dispatch_auth_account_executor(
    downstream: EndpointKind,
    attempt: &PreparedAttempt,
    auth_service: &crate::auth_provider::service::AuthService,
    safe_headers: &[(String, String)],
) -> AttemptResult {
    let headers = &header_map(safe_headers);
    let framing = attempt
        .auth_non_stream_framing
        .unwrap_or(AuthNonStreamFraming::ForcedResponsesSse);

    // Non-stream: both profiles send JSON.  Only the Responses accumulator
    // path needs the forced `stream:true` body.
    if framing == AuthNonStreamFraming::Json {
        let body = non_stream_json_body(&attempt.encoded_body, attempt);
        let response = match auth_service
            .outbound(
                &attempt.channel_id,
                &body,
                headers,
                false,
                &attempt.upstream_protocol,
                &attempt.upstream_endpoint,
            )
            .await
        {
            Ok(response) => response,
            Err(error) => return AttemptResult::Failure(provider_failure(error)),
        };
        let status = response.status().as_u16();
        if status >= 400 {
            let retry_after = retry_after_from_headers(response.headers());
            let text = response.text().await.unwrap_or_default();
            return AttemptResult::Failure(failure_from_auth_upstream(
                status,
                &text,
                retry_after.map(|d| d.as_secs()),
            ));
        }
        let response_headers = safe_response_headers(response.headers());
        let bytes = response.bytes().await.unwrap_or_default();
        let body: Value = match serde_json::from_slice(&bytes) {
            Ok(body) => body,
            Err(error) => {
                return AttemptResult::Failure(AttemptFailure {
                    failure_class: FailureClass::UpstreamProtocolError,
                    message: format!("Auth account non-stream body failed JSON decode: {error}"),
                    status_code: Some(502),
                    retry_after: None,
                })
            }
        };
        if let Some(failure) = semantic_failure(&attempt.upstream_protocol, &body) {
            return AttemptResult::Failure(failure);
        }
        return match decode_non_stream(downstream, attempt, &body) {
            Ok((body, usage)) => AttemptResult::Success(AttemptSuccess {
                status,
                body,
                usage,
                downstream_events: None,
                upstream_model: Some(attempt.upstream_model.clone()),
                response_headers,
            }),
            Err(failure) => AttemptResult::Failure(failure),
        };
    }

    // Codex forced Responses SSE path (existing behavior).
    let response = match auth_service
        .outbound(
            &attempt.channel_id,
            &force_responses_stream(&attempt.encoded_body),
            headers,
            true,
            &attempt.upstream_protocol,
            &attempt.upstream_endpoint,
        )
        .await
    {
        Ok(response) => response,
        Err(error) => return AttemptResult::Failure(provider_failure(error)),
    };
    let status = response.status().as_u16();
    if status >= 400 {
        let retry_after = retry_after_from_headers(response.headers());
        let text = response.text().await.unwrap_or_default();
        return AttemptResult::Failure(failure_from_auth_upstream(
            status,
            &text,
            retry_after.map(|d| d.as_secs()),
        ));
    }
    let response_headers = safe_response_headers(response.headers());
    let bytes = response.bytes().await.unwrap_or_default();
    let mut accumulator = crate::protocol::codec::ResponsesEventAccumulator::default();
    if let Err(error) = accumulator.push(&bytes) {
        return AttemptResult::Failure(responses_protocol_failure(error.message));
    }
    let body = match accumulator.finish() {
        Ok(body) => body,
        Err(error) => return AttemptResult::Failure(responses_protocol_failure(error.message)),
    };
    if let Some(failure) = semantic_failure(&attempt.upstream_protocol, &body) {
        return AttemptResult::Failure(failure);
    }
    match decode_non_stream(downstream, attempt, &body) {
        Ok((body, usage)) => AttemptResult::Success(AttemptSuccess {
            status,
            body,
            usage,
            downstream_events: None,
            upstream_model: Some(attempt.upstream_model.clone()),
            response_headers,
        }),
        Err(failure) => AttemptResult::Failure(failure),
    }
}

/// Connect an Auth Account stream without committing downstream bytes.  The
/// driver retains the same first-frame commit barrier used for Channels, so a
/// malformed first event can still fail over.
///
/// Framing comes from the RoutePlan-frozen attempt: Codex forces Responses SSE;
/// Kimi Chat forces `stream:true` and injects `stream_options.include_usage`;
/// Kimi Messages beta forces `stream:true` with no Chat-only fields.
pub async fn dispatch_auth_account_stream_executor(
    attempt: &PreparedAttempt,
    auth_service: &crate::auth_provider::service::AuthService,
    safe_headers: &[(String, String)],
) -> StreamAttemptResult {
    let headers = &header_map(safe_headers);
    let framing = attempt
        .auth_non_stream_framing
        .unwrap_or(AuthNonStreamFraming::ForcedResponsesSse);

    let body = if framing == AuthNonStreamFraming::Json {
        streaming_json_body(&attempt.encoded_body, attempt)
    } else {
        force_responses_stream(&attempt.encoded_body)
    };

    let response = match auth_service
        .outbound(
            &attempt.channel_id,
            &body,
            headers,
            true,
            &attempt.upstream_protocol,
            &attempt.upstream_endpoint,
        )
        .await
    {
        Ok(response) => response,
        Err(error) => return StreamAttemptResult::Failure(provider_failure(error)),
    };
    let status = response.status().as_u16();
    if status >= 400 {
        let retry_after = retry_after_from_headers(response.headers());
        let text = response.text().await.unwrap_or_default();
        return StreamAttemptResult::Failure(failure_from_auth_upstream(
            status,
            &text,
            retry_after.map(|d| d.as_secs()),
        ));
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_owned();
    let headers = safe_response_headers(response.headers());
    StreamAttemptResult::Connected(UpstreamStream {
        content_type,
        headers,
        body: response
            .bytes_stream()
            .map(|result| result.map_err(std::io::Error::other))
            .boxed(),
    })
}

fn force_responses_stream(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(object) = body.as_object_mut() {
        object.insert("stream".to_owned(), Value::Bool(true));
    }
    body
}

/// Kimi Chat / Anthropic Messages non-stream body: explicitly `stream:false`,
/// `stream_options` removed (Chat clients may send `stream_options` even when
/// not streaming, and the fixed Kimi transport must not echo it), and the
/// fixed Messages beta `betas` token ensured on the Anthropic profile.
fn is_gemini_auth(attempt: &PreparedAttempt) -> bool {
    attempt.upstream_protocol == "gemini"
}

fn non_stream_json_body(body: &Value, attempt: &PreparedAttempt) -> Value {
    let mut body = body.clone();
    if is_gemini_auth(attempt) {
        if let Some(object) = body.as_object_mut() {
            object.remove("stream");
            object.remove("stream_options");
        }
        return body;
    }
    if let Some(object) = body.as_object_mut() {
        object.insert("stream".to_owned(), Value::Bool(false));
        object.remove("stream_options");
    }
    ensure_messages_betas(&mut body, attempt);
    body
}

/// Anthropic Messages beta: ensure the fixed `betas` list contains the
/// official transport's default token, without duplicates.  This is part of
/// the Kimi endpoint transport contract, not a codec conversion, and can
/// never be overridden by renderer/headers/attributes.
fn ensure_messages_betas(body: &mut Value, attempt: &PreparedAttempt) {
    if attempt.upstream_protocol != "anthropic" || attempt.upstream_endpoint != "messages_beta" {
        return;
    }
    if let Some(object) = body.as_object_mut() {
        let betas = object
            .entry("betas")
            .or_insert_with(|| Value::Array(Default::default()));
        if let Some(array) = betas.as_array_mut() {
            if !array.iter().any(|v| v.as_str() == Some(KIMI_BETAS_DEFAULT)) {
                array.push(Value::String(KIMI_BETAS_DEFAULT.to_owned()));
            }
        }
    }
}

/// Kimi streaming body: `stream:true`, and for the Chat profile only inject
/// `stream_options.include_usage` so every downstream Chat/Responses/Messages
/// entry (even those that never asked) still gets usage.  The Messages beta
/// profile gets no Chat-only `stream_options`.
fn streaming_json_body(body: &Value, attempt: &PreparedAttempt) -> Value {
    let mut body = body.clone();
    if is_gemini_auth(attempt) {
        if let Some(object) = body.as_object_mut() {
            object.remove("stream");
            object.remove("stream_options");
        }
        return body;
    }
    if let Some(object) = body.as_object_mut() {
        object.insert("stream".to_owned(), Value::Bool(true));
        if attempt.upstream_endpoint == "chat_completions" && attempt.upstream_protocol == "openai"
        {
            let options = object
                .entry("stream_options")
                .or_insert_with(|| Value::Object(Default::default()));
            if let Some(options) = options.as_object_mut() {
                options.insert("include_usage".to_owned(), Value::Bool(true));
            }
        }
    }
    ensure_messages_betas(&mut body, attempt);
    body
}

/// Official Kimi Anthropic beta transport default (kimi-code kosong/anthropic).
const KIMI_BETAS_DEFAULT: &str = "interleaved-thinking-2025-05-14";

fn header_map(headers: &[(String, String)]) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(value) = reqwest::header::HeaderValue::from_str(value) else {
            continue;
        };
        map.insert(name, value);
    }
    map
}

/// Map an Auth Account provider error into an [`AttemptFailure`].
///
/// Unlike channels, an auth account's credentials belong to the USER (their
/// OAuth login), so a terminal auth failure keeps the real 401 status instead
/// of the channel-style 502 mask: the caller needs to know re-login fixes it.
fn provider_failure(error: crate::auth_provider::ProviderError) -> AttemptFailure {
    let failure_class = error.failure_class();
    AttemptFailure {
        status_code: Some(match failure_class {
            FailureClass::CallerTerminal => 400,
            FailureClass::ChannelAuthTerminal => 401,
            _ => 502,
        }),
        failure_class,
        message: error.to_string(),
        retry_after: None,
    }
}

fn responses_protocol_failure(message: String) -> AttemptFailure {
    AttemptFailure {
        failure_class: FailureClass::UpstreamProtocolError,
        message: format!("Responses SSE aggregation failed: {message}"),
        status_code: Some(502),
        retry_after: None,
    }
}

/// RFC 9110 hop-by-hop fields and credentials belonging to the *client* must
/// never cross the gateway boundary; everything else may be forwarded so
/// future end-to-end headers (e.g. `anthropic-beta`, `anthropic-version`)
/// keep working.
pub fn is_unsafe_proxy_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
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

/// Filter an incoming `HeaderMap` down to the safe forward headers.
pub fn safe_request_headers(headers: &axum::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            if is_unsafe_proxy_header(name.as_str()) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Filter an upstream `HeaderMap` down to the safely-passthrough response headers.
pub fn safe_response_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            if is_unsafe_proxy_header(name.as_str()) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Join a native base URL + endpoint template into the final upstream URL.
/// A trailing `/` on the base and a leading `/` on the endpoint are both safe.
pub fn final_url(base: &str, endpoint: &str, query: Option<&str>) -> String {
    let joined = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        endpoint.trim_start_matches('/')
    );
    match query.map(|q| q.trim()).filter(|q| !q.is_empty()) {
        Some(q) => format!("{}?{}", joined, q.trim_start_matches('?')),
        None => joined,
    }
}

/// Auth header(s) for a scheme + api key (T00 decision 9 / design 4.2).
pub fn auth_headers(scheme: AuthScheme, api_key: &str) -> Vec<(String, String)> {
    match scheme {
        AuthScheme::Bearer => vec![("authorization".to_string(), format!("Bearer {api_key}"))],
        AuthScheme::XApiKey => vec![("x-api-key".to_string(), api_key.to_string())],
        AuthScheme::OptionalBearer => {
            if api_key.is_empty() {
                vec![]
            } else {
                vec![("authorization".to_string(), format!("Bearer {api_key}"))]
            }
        }
        // The legacy Gemini key travels as a URL query param (never a header).
        AuthScheme::QueryKey => vec![],
    }
}

/// The auth scheme derived from the channel identity (T02 executor_kind).
pub fn auth_scheme_for(identity: &ChannelIdentity) -> AuthScheme {
    if identity.legacy_executor_override.as_deref() == Some("gemini_native") {
        return AuthScheme::QueryKey;
    }
    match identity.protocol.as_str() {
        "anthropic" => AuthScheme::XApiKey,
        "ollama" => AuthScheme::OptionalBearer,
        _ => AuthScheme::Bearer,
    }
}

/// The full upstream path template for an (upstream_protocol, upstream_endpoint).
pub fn endpoint_path(protocol: &str, endpoint: &str) -> String {
    match (protocol, endpoint) {
        ("openai", "chat_completions") => "chat/completions".to_string(),
        ("openai", "responses") => "responses".to_string(),
        ("openai", "embeddings") => "embeddings".to_string(),
        // main 分支约定：Anthropic Base URL 自带 /v1（如 api.anthropic.com/v1），
        // 端点只补 /messages（T01 url_fixtures 硬性样例）。
        ("anthropic", "messages") => "/messages".to_string(),
        ("anthropic", "count_tokens") => "/messages/count_tokens".to_string(),
        ("ollama", "api_chat") => "api/chat".to_string(),
        _ => endpoint.to_string(),
    }
}

/// Classify an upstream HTTP status into a [`FailureClass`].
///
/// The T00 decision 5 rule for 404 is honored: a bare 404 is ambiguous and is
/// only `EndpointUnsupported` when the body proves the *path* is missing, never
/// for a missing model.  401/403 map to `ChannelAuthTerminal` (never a local
/// API-key error); not every `>= 400` is retried.
///
/// FIX-25：404 嗅探的单一事实源在 `core::attempt`（legacy 轨经
/// `classify_http_status_with_body` 共用同一实现），此处仅委托。
pub fn classify_upstream_status(status: u16, body: &str) -> Option<FailureClass> {
    crate::core::attempt::classify_http_status_with_body(status, Some(body))
}

/// 将 HTTP 2xx 中的协议失败统一收敛到这里。这个函数只接受已读完的
/// JSON/SSE data，绝不消费无界流；调用者仍负责 HTTP 状态分类。
///
/// 不以任意 `message` 中出现 error 为依据。必须是协议规定的 error shape，
/// 才会在下游提交前触发故障转移。
pub fn semantic_failure(protocol: &str, body: &Value) -> Option<AttemptFailure> {
    let response_status = body.get("status").and_then(Value::as_str);
    let responses_failed =
        protocol == "openai" && matches!(response_status, Some("failed") | Some("cancelled"));
    let anthropic_error = protocol == "anthropic"
        && (body.get("type").and_then(Value::as_str) == Some("error")
            || body.get("error").is_some_and(|v| !v.is_null()));
    // OpenAI-compatible providers conventionally use a top-level non-null
    // error object. A normal Chat/Responses success does not have this shape.
    let openai_error = protocol != "anthropic" && body.get("error").is_some_and(|v| !v.is_null());
    if protocol == "gemini" {
        if let Some(failure) = gemini_prompt_feedback_failure(body) {
            return Some(failure);
        }
    }
    if !(responses_failed || anthropic_error || openai_error) {
        return None;
    }
    let error = body.get("error").filter(|v| !v.is_null()).unwrap_or(body);
    let kind = error
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| error.get("code").and_then(Value::as_str))
        .or_else(|| body.get("type").and_then(Value::as_str))
        .unwrap_or("upstream_error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| body.get("message").and_then(Value::as_str))
        .or_else(|| error.as_str())
        .unwrap_or("upstream returned a semantic error")
        .chars()
        .take(300)
        .collect::<String>();
    let retry_after = error
        .get("retry_after")
        .and_then(|v| v.as_u64())
        .or_else(|| body.get("retry_after").and_then(|v| v.as_u64()))
        .map(|seconds| seconds.min(RETRY_AFTER_CAP_SECS));
    let haystack = format!(
        "{} {}",
        kind.to_ascii_lowercase(),
        message.to_ascii_lowercase()
    );
    let failure_class = if haystack.contains("invalid_api_key")
        || haystack.contains("authentication")
        || haystack.contains("unauthorized")
        || haystack.contains("permission_denied")
    {
        FailureClass::ChannelAuthTerminal
    } else if haystack.contains("invalid_request")
        || haystack.contains("invalid model")
        || haystack.contains("unsupported parameter")
    {
        FailureClass::CallerTerminal
    } else {
        // overload/rate-limit have the same routing result as provider-side
        // temporary failures: retry another credential/candidate.
        FailureClass::Retryable
    };
    Some(AttemptFailure {
        failure_class,
        message: format!("{kind}: {message}"),
        status_code: Some(if failure_class == FailureClass::CallerTerminal {
            400
        } else {
            502
        }),
        retry_after,
    })
}

fn gemini_prompt_feedback_failure(body: &Value) -> Option<AttemptFailure> {
    let vertex = body.get("response").unwrap_or(body);
    let feedback = vertex.get("promptFeedback")?;
    let reason = feedback
        .get("blockReason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && *s != "BLOCK_REASON_UNSPECIFIED")?;
    let message = feedback
        .get("blockReasonMessage")
        .and_then(Value::as_str)
        .unwrap_or("prompt blocked");
    Some(AttemptFailure {
        failure_class: FailureClass::CallerTerminal,
        message: format!("gemini promptFeedback {reason}: {message}")
            .chars()
            .take(300)
            .collect(),
        status_code: Some(400),
        retry_after: None,
    })
}

/// Backwards-compatible Anthropic helper retained for the existing callers and
/// tests. New code should use [`semantic_failure`].
pub fn anthropic_error_envelope_message(body: &Value) -> Option<String> {
    semantic_failure("anthropic", body).map(|failure| failure.message)
}

/// Parse a bounded response body and detect an Anthropic error envelope.
pub fn anthropic_error_envelope_from_bytes(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| anthropic_error_envelope_message(&value))
}

/// Inspect a complete upstream SSE record before any downstream byte is sent.
/// It supports multi-line `data:` fields and both event-name and JSON shapes.
pub fn semantic_stream_failure(protocol: &str, record: &[u8]) -> Option<AttemptFailure> {
    let text = std::str::from_utf8(record).ok()?;
    let event = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("event:").map(str::trim));
    let data = text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    if protocol == "anthropic" && event == Some("error") {
        return serde_json::from_str::<Value>(&data)
            .ok()
            .and_then(|value| semantic_failure(protocol, &value))
            .or_else(|| {
                Some(AttemptFailure {
                    failure_class: FailureClass::Retryable,
                    message: "Anthropic upstream stream error".to_string(),
                    status_code: Some(502),
                    retry_after: None,
                })
            });
    }
    if protocol == "openai" && matches!(event, Some("response.failed") | Some("response.cancelled"))
    {
        return serde_json::from_str::<Value>(&data)
            .ok()
            .and_then(|value| semantic_failure(protocol, value.get("response").unwrap_or(&value)))
            .or_else(|| {
                Some(AttemptFailure {
                    failure_class: FailureClass::Retryable,
                    message: "Responses upstream stream failed before output".to_string(),
                    status_code: Some(502),
                    retry_after: None,
                })
            });
    }
    serde_json::from_str::<Value>(&data)
        .ok()
        .and_then(|value| semantic_failure(protocol, &value))
}

/// Inspect one complete Anthropic SSE record for an error event/envelope.
pub fn anthropic_stream_error_message(record: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(record).ok()?;
    let event_error = text.lines().any(|line| line.trim() == "event: error");
    let mut data = String::new();
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
        }
    }
    let envelope = anthropic_error_envelope_from_bytes(data.as_bytes());
    if event_error {
        return envelope.or_else(|| Some("error: Anthropic upstream stream error".to_string()));
    }
    envelope
}

/// Build a classified [`AttemptFailure`] from an upstream non-2xx.
///
/// `retry_after_secs` — when the upstream returned a `Retry-After` header,
/// the parsed (capped) seconds value; `None` if absent or unparseable.
pub fn failure_from_upstream(
    status: u16,
    body: &str,
    retry_after_secs: Option<u64>,
) -> AttemptFailure {
    let class = classify_upstream_status(status, body).unwrap_or(FailureClass::Retryable);
    let message = error_message_from_body(body).unwrap_or_else(|| {
        let cls = class.as_str();
        format!("upstream HTTP {status} ({cls})")
    });
    // Upstream channel credentials belong to the gateway: never surface a
    // channel's 401/403 as the caller's own key problem.
    let status_code = if class == FailureClass::ChannelAuthTerminal {
        Some(502)
    } else {
        Some(status)
    };
    AttemptFailure {
        failure_class: class,
        message,
        status_code,
        retry_after: retry_after_secs,
    }
}

/// Auth Account variant of [`failure_from_upstream`]: the account belongs to
/// the user (OAuth login), so a terminal 401/403 keeps its real status instead
/// of the channel mask.  The failure CLASS is unchanged, so failover semantics
/// (in-group only, never cross-group) are identical to channels.
pub fn failure_from_auth_upstream(
    status: u16,
    body: &str,
    retry_after_secs: Option<u64>,
) -> AttemptFailure {
    let mut failure = failure_from_upstream(status, body, retry_after_secs);
    if failure.failure_class == FailureClass::ChannelAuthTerminal {
        failure.status_code = Some(status);
    }
    failure
}

fn error_message_from_body(body: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        for pointer in ["/error/message", "/error", "/message"] {
            if let Some(s) = value.pointer(pointer).and_then(Value::as_str) {
                return Some(s.chars().take(300).collect());
            }
        }
    }
    None
}

fn transport_failure(e: reqwest::Error) -> AttemptFailure {
    AttemptFailure {
        failure_class: FailureClass::Retryable,
        message: format!("upstream connection failed: {e}"),
        status_code: Some(502),
        retry_after: None,
    }
}

fn undecodable_body(
    status: u16,
    body_len: usize,
    content_type: Option<&str>,
    e: impl std::fmt::Display,
) -> AttemptFailure {
    let content_type = content_type.unwrap_or("unknown");
    AttemptFailure {
        failure_class: FailureClass::UpstreamProtocolError,
        message: format!(
            "upstream returned an undecodable body (HTTP {status}, {body_len} bytes, content-type {content_type}): {e}"
        ),
        status_code: Some(502),
        retry_after: None,
    }
}

/// Extract the first `data:` JSON payload from an SSE byte stream.  Returns
/// `None` when the stream has no complete record or no valid JSON frame.
/// Used ONLY for draft-test probes (see the SSE-tolerance branch above).
fn first_sse_data_json(bytes: &[u8]) -> Option<Value> {
    let mut offset = 0;
    while offset < bytes.len() {
        let len = crate::endpoint_executor::sse::record_end(&bytes[offset..])?;
        let record = &bytes[offset..offset + len];
        offset += len;
        let payload = crate::endpoint_executor::sse::parse_data_payload(record).ok()?;
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&payload) {
            return Some(v);
        }
    }
    None
}

fn client(timeout_secs: i64, is_stream: bool) -> reqwest::Client {
    if is_stream {
        crate::adaptor::streaming_client()
    } else {
        crate::adaptor::blocking_client(timeout_secs.max(1) as u64)
    }
}

fn is_stream_body(body: &Value) -> bool {
    body.get("stream").and_then(Value::as_bool).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Legacy Gemini override executor (selected ONLY via identity override)
// ---------------------------------------------------------------------------

fn gemini_url(base: &str, model: &str, api_key: &str, stream: bool) -> String {
    let path = if stream {
        format!("v1beta/models/{}:streamGenerateContent", model)
    } else {
        format!("v1beta/models/{}:generateContent", model)
    };
    let mut url = format!("{}/{}", base.trim_end_matches('/'), path);
    url.push('?');
    url.push_str(&format!("key={}", urlencoding(api_key)));
    if stream {
        url.push_str("&alt=sse");
    }
    url
}

fn urlencoding(s: &str) -> String {
    // Minimal form-url encoding for the query key (API keys are typically
    // alphanumeric, but this keeps arbitrary keys safe in a query string).
    s.replace('%', "%25")
        .replace('&', "%26")
        .replace('+', "%2B")
        .replace('=', "%3D")
        .replace('?', "%3F")
}

/// Convert an OpenAI Chat body to the legacy Gemini `generateContent` shape.
/// Kept inside the override executor (its legacy behavior is inherently a
/// conversion; it is the ONLY executor that converts).
fn convert_chat_to_gemini(body: &Value) -> Value {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut system_instruction = None;
    let mut contents = Vec::new();

    for msg in &messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = msg.get("content").and_then(Value::as_str).unwrap_or("");

        if role == "system" {
            system_instruction = Some(serde_json::json!({ "parts": [{ "text": content }] }));
        } else {
            if role == "assistant" && content.is_empty() {
                let has_tool_calls = msg
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !has_tool_calls {
                    continue;
                }
            }
            contents.push(serde_json::json!({
                "role": if role == "assistant" { "model" } else { "user" },
                "parts": [{ "text": content }],
            }));
        }
    }

    let mut gemini_body = serde_json::json!({ "contents": contents });
    if let Some(si) = system_instruction {
        gemini_body["systemInstruction"] = si;
    }
    if let Some(temp) = body.get("temperature") {
        gemini_body["generationConfig"]["temperature"] = temp.clone();
    }
    if let Some(max_tokens) = body.get("max_tokens") {
        gemini_body["generationConfig"]["maxOutputTokens"] = max_tokens.clone();
    }
    gemini_body
}

/// Convert a legacy Gemini `generateContent` response back to OpenAI Chat.
fn convert_gemini_to_chat(gemini_json: &Value, model: &str) -> Value {
    let content = gemini_json
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|cand| cand.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    let prompt_tokens = gemini_json
        .pointer("/usageMetadata/promptTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = gemini_json
        .pointer("/usageMetadata/candidatesTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    serde_json::json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    })
}

// ---------------------------------------------------------------------------
// Request send
// ---------------------------------------------------------------------------

/// Build and send the upstream request for an attempt, returning the raw
/// `reqwest::Response`.  The caller classifies the status.
async fn send_request(
    attempt: &PreparedAttempt,
    channel: &Channel,
    identity: &ChannelIdentity,
    safe_headers: &[(String, String)],
    query: Option<&str>,
) -> Result<reqwest::Response, AttemptFailure> {
    let is_gemini = identity.legacy_executor_override.as_deref() == Some("gemini_native");
    let stream = is_stream_body(&attempt.encoded_body);
    let c = client(channel.timeout_secs, stream);

    if is_gemini {
        let url = gemini_url(
            &attempt.native_base_url,
            &attempt.upstream_model,
            &channel.api_key,
            stream,
        );
        let body = convert_chat_to_gemini(&attempt.encoded_body);
        return c
            .post(url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(transport_failure);
    }

    let url = final_url(
        &attempt.native_base_url,
        &endpoint_path(&attempt.upstream_protocol, &attempt.upstream_endpoint),
        query,
    );
    let scheme = auth_scheme_for(identity);
    let mut req = c.post(url).header("content-type", "application/json");
    for (k, v) in auth_headers(scheme, &channel.api_key) {
        req = req.header(k, v);
    }
    // Anthropic Messages requires the version header; a client-provided value
    // (from safe_headers) overrides the default.
    if attempt.upstream_protocol == "anthropic" {
        req = req.header("anthropic-version", "2023-06-01");
    }
    for (k, v) in safe_headers {
        req = req.header(k, v);
    }
    // 渠道级自定义请求头：仅发送启用项，并禁止覆盖网关鉴权与传输层受保护头。
    if let Ok(config) = serde_json::from_str::<Value>(&channel.config) {
        if let Some(headers) = config.get("request_headers").and_then(Value::as_array) {
            for item in headers {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let value = item.get("value").and_then(Value::as_str).unwrap_or("");
                let status = item.get("status").and_then(Value::as_i64).unwrap_or(1);
                if !name.is_empty() && status != 0 && !is_unsafe_proxy_header(name) {
                    if let (Ok(header_name), Ok(header_value)) = (
                        reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                        reqwest::header::HeaderValue::from_str(value),
                    ) {
                        req = req.header(header_name, header_value);
                    }
                }
            }
        }
    }
    req.json(&attempt.encoded_body)
        .send()
        .await
        .map_err(transport_failure)
}

// ---------------------------------------------------------------------------
// Non-stream execution
// ---------------------------------------------------------------------------

/// Run one non-stream attempt to completion and classify into [`AttemptResult`].
pub async fn dispatch_executor(
    downstream: EndpointKind,
    attempt: &PreparedAttempt,
    channel: &Channel,
    identity: &ChannelIdentity,
    safe_headers: &[(String, String)],
    query: Option<&str>,
) -> AttemptResult {
    let resp = match send_request(attempt, channel, identity, safe_headers, query).await {
        Ok(r) => r,
        Err(f) => return AttemptResult::Failure(f),
    };
    let status = resp.status().as_u16();
    if status >= 400 {
        let retry_after = retry_after_from_headers(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        return AttemptResult::Failure(failure_from_upstream(
            status,
            &text,
            retry_after.map(|d| d.as_secs()),
        ));
    }
    // T06 M-2: preserve safely-passthrough response headers (credentials and
    // hop-by-hop fields dropped) so the non-stream executor boundary retains
    // upstream headers and the handler can forward them downstream.
    // Snapshot headers for the success path AND the decode-failure diagnostics
    // before `bytes()` consumes the response.
    let response_headers = safe_response_headers(resp.headers());
    let retry_after = retry_after_from_headers(resp.headers()).map(|d| d.as_secs());
    let content_encoding = resp
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    // Read the raw bytes once; parse JSON ourselves so the bytes stay available
    // for diagnostics when the decode fails.
    let bytes = resp.bytes().await.unwrap_or_default();
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            // A 2xx that fails to parse is either non-JSON content (HTML/plain
            // text) or an undecoded compressed body (gzip etc.) — the distinction
            // only shows up in the bytes and headers, so log them before failing.
            let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).to_string();
            tracing::warn!(
                url = %final_url(
                    &attempt.native_base_url,
                    &endpoint_path(&attempt.upstream_protocol, &attempt.upstream_endpoint),
                    query
                ),
                status = status,
                content_encoding = ?content_encoding,
                content_type = ?content_type,
                body_len = bytes.len(),
                body_preview = %preview,
                err = %e,
                "2xx upstream body failed JSON decode"
            );
            // Draft-test probes only: a gateway that ignores `stream:false` and
            // ALWAYS returns SSE is still speaking the endpoint's protocol, so a
            // probe must not fail it just because the body is SSE-framed rather
            // than a single JSON document.  Extract the first `data:` JSON frame
            // and treat that as the probe body.  Gating on the route_group (not
            // content-type) keeps this robust against gateways that emit SSE
            // under an `application/json` header.  Production requests never
            // take this path (their route_group is not `draft_test/...`).
            if attempt.route_group.starts_with("draft_test/") {
                match first_sse_data_json(&bytes) {
                    Some(v) => v,
                    None => {
                        return AttemptResult::Failure(undecodable_body(
                            status,
                            bytes.len(),
                            content_type.as_deref(),
                            e,
                        ))
                    }
                }
            } else {
                return AttemptResult::Failure(undecodable_body(
                    status,
                    bytes.len(),
                    content_type.as_deref(),
                    e,
                ));
            }
        }
    };
    if let Some(mut failure) = semantic_failure(&attempt.upstream_protocol, &body) {
        failure.retry_after = retry_after;
        return AttemptResult::Failure(failure);
    }
    // Legacy Gemini override: the upstream `generateContent` response must be
    // converted back to OpenAI Chat (this is the ONLY executor that converts
    // responses; it is selected exclusively via identity override).
    if identity.legacy_executor_override.as_deref() == Some("gemini_native") {
        let converted = convert_gemini_to_chat(&body, &attempt.upstream_model);
        return AttemptResult::Success(AttemptSuccess {
            status,
            body: converted.clone(),
            usage: extract_usage("openai", "chat_completions", &converted),
            downstream_events: None,
            upstream_model: Some(attempt.upstream_model.clone()),
            response_headers,
        });
    }
    match decode_non_stream(downstream, attempt, &body) {
        Ok((out_body, usage)) => AttemptResult::Success(AttemptSuccess {
            status,
            body: out_body,
            usage,
            downstream_events: None,
            upstream_model: Some(attempt.upstream_model.clone()),
            response_headers,
        }),
        Err(f) => AttemptResult::Failure(f),
    }
}

/// Decode one upstream body through the request-scoped codec plan.
///
/// The factory preserves the exact `ConversionContext` created while encoding
/// this attempt.  It also returns observed usage as part of the same decode,
/// avoiding a second protocol-specific pass for quota accounting.
fn decode_non_stream(
    downstream: EndpointKind,
    attempt: &PreparedAttempt,
    body: &Value,
) -> Result<(Value, Option<TokenUsage>), AttemptFailure> {
    let Some(codec) = attempt.prepared_codec.as_ref() else {
        return match downstream {
            EndpointKind::CountTokens | EndpointKind::Embeddings => Ok((
                body.clone(),
                extract_usage(&attempt.upstream_protocol, &attempt.upstream_endpoint, body),
            )),
            // Endpoint draft probes intentionally construct a minimal attempt
            // without request preparation.  They exercise upstream reachability
            // only and never serve a client response, so retain their historical
            // raw-body inspection path while production remains fail-closed.
            _ if attempt.route_group.starts_with("draft_test/") => Ok((
                body.clone(),
                extract_usage(&attempt.upstream_protocol, &attempt.upstream_endpoint, body),
            )),
            _ => Err(AttemptFailure {
                failure_class: FailureClass::UpstreamProtocolError,
                message: "three-protocol attempt is missing its prepared codec".to_string(),
                status_code: Some(502),
                retry_after: None,
            }),
        };
    };
    let decoded = codec
        .new_non_stream_decoder()
        .decode(body)
        .map_err(|error| AttemptFailure {
            failure_class: FailureClass::UpstreamProtocolError,
            message: format!("upstream response could not be decoded: {error}"),
            status_code: Some(502),
            retry_after: None,
        })?;
    Ok((
        decoded.body,
        decoded.usage.map(|usage| TokenUsage {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            total_tokens: usage.input_tokens + usage.output_tokens,
            cached_tokens: usage.cache_read_input_tokens,
        }),
    ))
}

/// Extract token usage from the RAW upstream body (before any response decode).
pub fn extract_usage(protocol: &str, endpoint: &str, body: &Value) -> Option<TokenUsage> {
    let usage = body.get("usage")?;
    if protocol == "anthropic" {
        let input = usage.get("input_tokens").and_then(Value::as_u64)?;
        let output = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cached = usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return Some(TokenUsage {
            prompt_tokens: input,
            completion_tokens: output,
            total_tokens: input + output,
            cached_tokens: cached,
        });
    }
    if endpoint == "responses" {
        let input = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("prompt_tokens").and_then(Value::as_u64))?;
        let output = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .or_else(|| usage.get("completion_tokens").and_then(Value::as_u64))
            .unwrap_or(0);
        // OpenAI Responses API returns cached tokens inside input_tokens_details.cached_tokens
        let cached = usage
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| usage.get("cache_read_input_tokens").and_then(Value::as_u64))
            .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
            .unwrap_or(0);
        return Some(TokenUsage {
            prompt_tokens: input,
            completion_tokens: output,
            total_tokens: input + output,
            cached_tokens: cached,
        });
    }
    let prompt = usage.get("prompt_tokens").and_then(Value::as_u64)?;
    let completion = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt + completion);
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| {
            // DeepSeek-compatible upstreams use their own cache-hit field.
            usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64)
        })
        .unwrap_or(0);
    Some(TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_tokens: cached,
    })
}

// ---------------------------------------------------------------------------
// Streaming execution
// ---------------------------------------------------------------------------

/// Run one streaming attempt's connect phase: send the request and, on a 2xx,
/// return the raw byte stream.  No downstream bytes are committed here — the
/// stream driver performs first-frame validation before committing.
pub async fn dispatch_stream_executor(
    _downstream: EndpointKind,
    attempt: &PreparedAttempt,
    channel: &Channel,
    identity: &ChannelIdentity,
    safe_headers: &[(String, String)],
    query: Option<&str>,
) -> StreamAttemptResult {
    let resp = match send_request(attempt, channel, identity, safe_headers, query).await {
        Ok(r) => r,
        Err(f) => return StreamAttemptResult::Failure(f),
    };
    let status = resp.status().as_u16();
    if status >= 400 {
        let retry_after = retry_after_from_headers(resp.headers());
        let text = resp.text().await.unwrap_or_default();
        return StreamAttemptResult::Failure(failure_from_upstream(
            status,
            &text,
            retry_after.map(|d| d.as_secs()),
        ));
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("text/event-stream")
        .to_string();
    let headers = safe_response_headers(resp.headers());
    let body = resp
        .bytes_stream()
        .map(|r| r.map_err(std::io::Error::other))
        .boxed();
    StreamAttemptResult::Connected(UpstreamStream {
        content_type,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_retry_after_delta_seconds() {
        assert_eq!(parse_retry_after(Some("120")), Some(120));
        assert_eq!(parse_retry_after(Some("0")), Some(0));
        assert_eq!(parse_retry_after(Some("  30  ")), Some(30));
    }

    #[test]
    fn parse_retry_after_http_date() {
        // A future date ~120 seconds from now.
        let future = Utc::now() + chrono::Duration::seconds(120);
        let rfc2822 = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let parsed = parse_retry_after(Some(&rfc2822));
        assert!(parsed.is_some(), "should parse HTTP-date: {rfc2822}");
        let secs = parsed.unwrap();
        assert!(secs >= 115 && secs <= 125, "should be ~120s, got {secs}");
    }

    #[test]
    fn parse_retry_after_invalid() {
        assert_eq!(parse_retry_after(None), None);
        assert_eq!(parse_retry_after(Some("garbage")), None);
        assert_eq!(parse_retry_after(Some("")), None);
    }

    #[test]
    fn retry_after_from_headers_caps_and_jitters() {
        // A huge value (3600s) should be capped to RETRY_AFTER_CAP_SECS (5).
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "3600".parse().unwrap());
        let dur = retry_after_from_headers(&headers).unwrap();
        // With ±20% jitter on 5s, range is [4, 6].
        assert!(
            dur.as_secs() >= 4 && dur.as_secs() <= 6,
            "capped+jit={:?}",
            dur
        );
    }

    #[test]
    fn retry_after_from_headers_absent() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(retry_after_from_headers(&headers).is_none());
    }

    #[test]
    fn final_url_joins_cleanly() {
        assert_eq!(
            final_url("https://api.openai.com/v1", "chat/completions", None),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            final_url("https://api.anthropic.com/v1", "/messages", None),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            final_url("http://localhost:11434", "api/chat", Some("stream=true")),
            "http://localhost:11434/api/chat?stream=true"
        );
        // trailing slash on base + leading slash on endpoint
        assert_eq!(
            final_url("http://localhost:11434/", "/api/chat", None),
            "http://localhost:11434/api/chat"
        );
    }

    #[test]
    fn auth_account_body_always_forces_responses_streaming() {
        let forced = force_responses_stream(&serde_json::json!({"model":"m","stream":false}));
        assert_eq!(forced["stream"], true);
    }

    #[test]
    fn auth_schemes() {
        assert_eq!(
            auth_headers(AuthScheme::Bearer, "k"),
            vec![("authorization".to_string(), "Bearer k".to_string())]
        );
        assert_eq!(
            auth_headers(AuthScheme::XApiKey, "k"),
            vec![("x-api-key".to_string(), "k".to_string())]
        );
        assert_eq!(auth_headers(AuthScheme::QueryKey, "k"), vec![]);
        assert_eq!(auth_headers(AuthScheme::OptionalBearer, ""), vec![]);
        assert_eq!(
            auth_headers(AuthScheme::OptionalBearer, "k"),
            vec![("authorization".to_string(), "Bearer k".to_string())]
        );
    }

    #[test]
    fn anthropic_error_envelopes_include_overload_and_ignore_messages_success() {
        let error = serde_json::json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "Selected model is at capacity."}
        });
        assert_eq!(
            anthropic_error_envelope_message(&error).as_deref(),
            Some("overloaded_error: Selected model is at capacity.")
        );
        assert!(anthropic_error_envelope_message(&serde_json::json!({
            "type": "message", "content": [], "stop_reason": "end_turn"
        }))
        .is_none());
    }

    #[test]
    fn anthropic_stream_error_envelope_is_detected_before_commit() {
        let record = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n";
        assert_eq!(
            anthropic_stream_error_message(record).as_deref(),
            Some("overloaded_error: busy")
        );
    }

    #[test]
    fn semantic_failure_covers_responses_and_openai_compatible_shapes() {
        let failed =
            serde_json::json!({"status":"failed","error":{"code":"server_error","message":"busy"}});
        assert_eq!(
            semantic_failure("openai", &failed).unwrap().failure_class,
            FailureClass::Retryable
        );
        let cancelled = serde_json::json!({"status":"cancelled"});
        assert!(semantic_failure("openai", &cancelled).is_some());
        let compat = serde_json::json!({"error":{"type":"rate_limit_error","message":"slow down"}});
        assert_eq!(
            semantic_failure("openai", &compat).unwrap().failure_class,
            FailureClass::Retryable
        );
        let success = serde_json::json!({"object":"chat.completion","message":"error text"});
        assert!(semantic_failure("openai", &success).is_none());
        let blocked = serde_json::json!({
            "response": {
                "promptFeedback": {"blockReason": "SAFETY", "blockReasonMessage": "hate"},
                "candidates": []
            }
        });
        let failure = semantic_failure("gemini", &blocked).unwrap();
        assert_eq!(failure.failure_class, FailureClass::CallerTerminal);
        assert!(failure.message.contains("SAFETY"));
        let ok =
            serde_json::json!({"response":{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}});
        assert!(semantic_failure("gemini", &ok).is_none());
    }

    #[test]
    fn responses_failed_sse_envelope_is_detected() {
        let record = br#"event: response.failed
data: {"type":"response.failed","response":{"status":"failed","error":{"message":"capacity"}}}

"#;
        assert!(semantic_stream_failure("openai", record).is_some());
    }

    #[test]
    fn endpoint_path_matrix() {
        assert_eq!(
            endpoint_path("openai", "chat_completions"),
            "chat/completions"
        );
        assert_eq!(endpoint_path("openai", "responses"), "responses");
        assert_eq!(endpoint_path("openai", "embeddings"), "embeddings");
        assert_eq!(endpoint_path("anthropic", "messages"), "/messages");
        assert_eq!(
            endpoint_path("anthropic", "count_tokens"),
            "/messages/count_tokens"
        );
        assert_eq!(endpoint_path("ollama", "api_chat"), "api/chat");
    }

    #[test]
    fn status_classification_matches_t00() {
        // 401/403 are channel-auth-terminal (never a local key error).
        assert_eq!(
            classify_upstream_status(401, "{}"),
            Some(FailureClass::ChannelAuthTerminal)
        );
        assert_eq!(
            classify_upstream_status(403, "{}"),
            Some(FailureClass::ChannelAuthTerminal)
        );
        // 400/422 caller terminal.
        assert_eq!(
            classify_upstream_status(400, "{}"),
            Some(FailureClass::CallerTerminal)
        );
        assert_eq!(
            classify_upstream_status(422, "{}"),
            Some(FailureClass::CallerTerminal)
        );
        // 404 only endpoint_unsupported when the body proves path-not-found.
        assert_eq!(
            classify_upstream_status(404, r#"{"error":{"message":"endpoint does not exist"}}"#),
            Some(FailureClass::EndpointUnsupported)
        );
        // A bare 404 (e.g. missing model) is NOT endpoint_unsupported; it stays
        // degradable so it can fail over, never a caller-terminal 4xx.
        assert_eq!(
            classify_upstream_status(404, r#"{"error":{"message":"model not found"}}"#),
            Some(FailureClass::Retryable)
        );
        // 405/501 endpoint unsupported.
        assert_eq!(
            classify_upstream_status(405, "{}"),
            Some(FailureClass::EndpointUnsupported)
        );
        assert_eq!(
            classify_upstream_status(501, "{}"),
            Some(FailureClass::EndpointUnsupported)
        );
        // retryable classes.
        assert_eq!(
            classify_upstream_status(429, "{}"),
            Some(FailureClass::Retryable)
        );
        assert_eq!(
            classify_upstream_status(503, "{}"),
            Some(FailureClass::Retryable)
        );
        assert_eq!(
            classify_upstream_status(500, "{}"),
            Some(FailureClass::Retryable)
        );
    }

    #[test]
    fn failure_from_upstream_never_leaks_channel_auth() {
        let f = failure_from_upstream(401, r#"{"error":{"message":"bad channel key"}}"#, None);
        assert_eq!(f.failure_class, FailureClass::ChannelAuthTerminal);
        assert_eq!(
            f.status_code,
            Some(502),
            "channel auth must map to 502 downstream"
        );
    }

    #[test]
    fn auth_upstream_keeps_real_401_status() {
        // Auth accounts own their OAuth credentials: the caller must see the
        // real 401 to know re-login fixes it (failover class unchanged).
        let f = failure_from_auth_upstream(401, r#"{"error":{"message":"invalid token"}}"#, None);
        assert_eq!(f.failure_class, FailureClass::ChannelAuthTerminal);
        assert_eq!(f.status_code, Some(401));
        let f = failure_from_auth_upstream(403, "forbidden", None);
        assert_eq!(f.failure_class, FailureClass::ChannelAuthTerminal);
        assert_eq!(f.status_code, Some(403));
        // Non-auth failures behave exactly like the channel variant.
        let f = failure_from_auth_upstream(500, "boom", None);
        assert_eq!(f.failure_class, FailureClass::Retryable);
        assert_eq!(f.status_code, Some(500));
    }

    #[test]
    fn provider_unauthorized_maps_to_401() {
        let f = provider_failure(crate::auth_provider::ProviderError::Unauthorized);
        assert_eq!(f.failure_class, FailureClass::ChannelAuthTerminal);
        assert_eq!(f.status_code, Some(401));
        let f = provider_failure(crate::auth_provider::ProviderError::Retryable);
        assert_eq!(f.status_code, Some(502));
    }

    #[test]
    fn gemini_override_url_and_body() {
        let url = gemini_url(
            "https://generativelanguage.googleapis.com",
            "gemini-2.0-flash",
            "k&ey",
            false,
        );
        assert!(url.contains("/v1beta/models/gemini-2.0-flash:generateContent?key=k%26ey"));
        let body = convert_chat_to_gemini(&serde_json::json!({
            "model": "gemini-2.0-flash",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"}
            ],
            "temperature": 0.5,
            "max_tokens": 16,
        }));
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
        assert_eq!(body["generationConfig"]["temperature"], 0.5);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 16);
    }

    #[test]
    fn extract_usage_matrix() {
        let openai = serde_json::json!({"usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}});
        assert_eq!(
            extract_usage("openai", "chat_completions", &openai)
                .unwrap()
                .total_tokens,
            15
        );
        let anth = serde_json::json!({"usage": {"input_tokens": 10, "output_tokens": 5}});
        let u = extract_usage("anthropic", "messages", &anth).unwrap();
        assert_eq!(u.prompt_tokens, 10);
        assert_eq!(u.total_tokens, 15);
        let resp = serde_json::json!({"usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7}});
        assert_eq!(
            extract_usage("openai", "responses", &resp)
                .unwrap()
                .prompt_tokens,
            3
        );
        assert_eq!(
            extract_usage("openai", "responses", &resp)
                .unwrap()
                .completion_tokens,
            4
        );
        assert!(extract_usage("openai", "embeddings", &serde_json::json!({})).is_none());
    }
}
