//! Single source of truth for channel protocol identity (T02).
//!
//! `resolve_channel_identity(row)` is the ONLY entry point used by DTOs,
//! dispatchers, tests, imports and exports to derive the normalized protocol
//! identity of a channel row.
//!
//! Rules (T00 decision 2/3/10, T02 spec, design 5.1 / 11.3):
//!   * A row is "legacy-uninitialized" when `identity_revision == 0`, when
//!     `protocol`/`provider` are NULL/empty, or when `native_endpoints` is
//!     empty and no preset can explain it. In that case the identity is
//!     inferred from the legacy `type/base_url/config` fields.
//!   * New writes dual-write both the new identity fields AND the legacy
//!     `type`/`base_url` (see `new_to_legacy`), so old binaries keep working.
//!   * `executor_kind` is DERIVED from the protocol identity — it is never
//!     persisted for new channels. Only legacy Gemini keeps
//!     `legacy_executor_override = "gemini_native"`.

use crate::channel_presets::{
    all_channel_presets, ChannelPreset, ChannelProtocol, ChannelProvider, NativeEndpoint,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical OpenAI-compatible hosts (host of `base_url`). Anything else maps
/// to provider `custom` instead of `openai` so private gateways are never
/// mislabeled as the official vendor (T02 spec: openai rule).
const OPENAI_CANONICAL_HOSTS: &[&str] = &["api.openai.com"];
/// Canonical Anthropic host for provider inference.
const ANTHROPIC_CANONICAL_HOSTS: &[&str] = &["api.anthropic.com"];

/// Executor kind derived from identity. Persisted only for legacy Gemini
/// (`legacy_executor_override`), never as a second runtime truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    /// OpenAI / Anthropic / Ollama native executors, derived from protocol.
    ChatCompletions,
    Messages,
    ApiChat,
    /// Legacy Gemini native executor (generateContent?key=). Only for rows
    /// with `legacy_executor_override == "gemini_native"`.
    GeminiNative,
    /// Legacy fallback executor (`/chat/completions` via custom adaptor).
    FallbackChat,
}

/// Normalized channel identity, returned by `resolve_channel_identity`.
/// Always non-empty in a resolved output; only `native_endpoints` may be empty
/// when the identity is unknown/uninitialized and nothing can be inferred.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub protocol: String,
    pub provider: String,
    /// Canonical new-protocol root URL. Empty when unknown.
    pub native_base_url: String,
    /// Upstream native capability list (strings; keep the frozen enum strings).
    pub native_endpoints: Vec<String>,
    pub identity_revision: i64,
    /// Persisted only for legacy Gemini; empty for everything else.
    pub legacy_executor_override: Option<String>,
    /// Derived executor kind.
    pub executor_kind: String,
    /// True when the identity was live-inferred from legacy fields rather than
    /// read verbatim from the new columns (revision 0 / missing fields).
    pub inferred: bool,
}

/// Field-level input used by `resolve_channel_identity`.
///
/// `row` here is intentionally not the storage `Channel` struct so this module
/// stays decoupled and trivially testable with fixtures.
#[derive(Debug, Clone, Default)]
pub struct ChannelIdentityRow {
    pub channel_type: String,
    pub base_url: String,
    pub config: Value,
    pub protocol: Option<String>,
    pub provider: Option<String>,
    pub native_base_url: Option<String>,
    pub native_endpoints: Option<String>,
    pub preset_revision: Option<String>,
    pub identity_revision: i64,
    pub legacy_executor_override: Option<String>,
}

impl From<&crate::db::models::Channel> for ChannelIdentityRow {
    fn from(c: &crate::db::models::Channel) -> Self {
        ChannelIdentityRow {
            channel_type: c.channel_type.clone(),
            base_url: c.base_url.clone(),
            config: serde_json::from_str(&c.config).unwrap_or(Value::Object(Default::default())),
            protocol: c.protocol.clone(),
            provider: c.provider.clone(),
            native_base_url: c.native_base_url.clone(),
            native_endpoints: c.native_endpoints.clone(),
            preset_revision: c.preset_revision.clone(),
            identity_revision: c.identity_revision,
            legacy_executor_override: c.legacy_executor_override.clone(),
        }
    }
}

/// The one and only entry point: resolve the normalized identity of a channel row.
///
/// Legacy-uninitialized means: `identity_revision == 0`, or `protocol`/`provider`
/// missing, or `native_endpoints` empty and no preset can explain the row.
/// In all those cases we fall back to inferring from `type/base_url/config`.
pub fn resolve_channel_identity(row: &ChannelIdentityRow) -> ChannelIdentity {
    let identity_revision = row.identity_revision;
    let protocol_ok = row
        .protocol
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let provider_ok = row
        .provider
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let endpoints = parse_endpoints(row.native_endpoints.as_deref());
    let native_base_ok = row
        .native_base_url
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    // A row written by the new dual-write path (revision > 0) with a coherent
    // identity is trusted verbatim.
    if identity_revision > 0
        && protocol_ok
        && provider_ok
        && native_base_ok
        && !endpoints.is_empty()
    {
        let protocol = row.protocol.as_deref().unwrap_or("").to_string();
        let provider = row.provider.as_deref().unwrap_or("").to_string();
        let native_base_url = normalize_native_base_url(
            &protocol,
            row.native_base_url.as_deref().unwrap_or_default(),
        );
        return ChannelIdentity {
            protocol: protocol.clone(),
            provider: provider.clone(),
            native_base_url,
            native_endpoints: endpoints,
            identity_revision,
            legacy_executor_override: row.legacy_executor_override.clone(),
            executor_kind: executor_kind_for(&protocol, row.legacy_executor_override.as_deref())
                .to_string(),
            inferred: false,
        };
    }

    // Legacy / revision-0 / partially-initialized: live-infer from old fields.
    infer_legacy(
        &row.channel_type,
        &row.base_url,
        &row.config,
        row.legacy_executor_override.as_deref(),
        identity_revision,
    )
}

pub(crate) fn normalize_native_base_url(protocol: &str, native_base_url: &str) -> String {
    if protocol != "anthropic" {
        return native_base_url.to_string();
    }
    let trimmed = native_base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() || trimmed.ends_with("/v1") {
        return trimmed.to_string();
    }
    format!("{trimmed}/v1")
}

fn parse_endpoints(raw: Option<&str>) -> Vec<String> {
    match raw {
        Some(s) => serde_json::from_str::<Vec<String>>(s).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Infer identity from legacy `type`/`base_url`/`config` (T02 spec identity rules).
fn infer_legacy(
    channel_type: &str,
    base_url: &str,
    config: &Value,
    legacy_override: Option<&str>,
    identity_revision: i64,
) -> ChannelIdentity {
    let t = channel_type.trim();
    let (protocol, provider, native_base_url, endpoints, override_used, inferred_executor) = match t
    {
        "openai" => {
            let provider = if host_in(base_url, OPENAI_CANONICAL_HOSTS) {
                ChannelProvider::OpenAI
            } else {
                ChannelProvider::Custom
            };
            // T00 decision 2 / design 11.2: an old openai record must NOT be
            // reported natively capable of /responses just because it is
            // typed openai. The Responses-via-Chat legacy debt is only
            // recorded per-row in config.legacy_capabilities.  (A revision-0
            // legacy row without the debt flag is given the debt path at
            // ROUTING time in route_plan::has_responses_debt — see there —
            // NOT by claiming native /responses here.)
            let has_responses_debt = config
                .get("legacy_capabilities")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .any(|s| s.as_str() == Some("responses_via_chat_v1"))
                })
                .unwrap_or(false);
            let eps = if has_responses_debt {
                vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses]
            } else {
                vec![NativeEndpoint::ChatCompletions]
            };
            (
                ChannelProtocol::OpenAI,
                provider,
                base_url.to_string(),
                eps,
                None,
                ExecutorKind::ChatCompletions,
            )
        }
        "deepseek" => (
            ChannelProtocol::OpenAI,
            ChannelProvider::DeepSeek,
            base_url.to_string(),
            vec![NativeEndpoint::ChatCompletions],
            None,
            ExecutorKind::ChatCompletions,
        ),
        "claude" => {
            let provider = infer_claude_provider(base_url);
            // main 分支约定：legacy claude base_url 已是带 /v1 的旧适配器根
            // （如 api.anthropic.com/v1），executor 端点只补 /messages。native
            // root 保持 base_url 原样，不再剥 /v1（剥除会造成 .../v1//messages）。
            let native = base_url.trim_end_matches('/').to_string();
            // T06 I-4 (leader adjudication 2026-08-05): legacy `type ==
            // "claude"` served /v1/messages/count_tokens under the old
            // predicate, so a revision-0 claude row must infer
            // [Messages, CountTokens] — NOT [Messages] only.  The canonical
            // Anthropic preset declares count_tokens, and legacy claude is
            // Anthropic by definition; granting count_tokens preserves the
            // pre-refactor behavior exactly.  (Contrast with the T02 F2
            // openai-responses ruling: legacy openai never served native
            // responses, so NOT granting preserved old behavior there.)
            (
                ChannelProtocol::Anthropic,
                provider,
                native,
                vec![NativeEndpoint::Messages, NativeEndpoint::CountTokens],
                None,
                ExecutorKind::Messages,
            )
        }
        // Legacy Gemini keeps its original URL, query-key auth and the
        // native executor until the user explicitly applies the Google
        // OpenAI-compat preset (design 11.1).
        "gemini" => {
            let override_value = legacy_override
                .filter(|o| *o == "gemini_native")
                .unwrap_or("gemini_native");
            (
                ChannelProtocol::OpenAI,
                ChannelProvider::Google,
                base_url.to_string(),
                Vec::new(),
                Some(override_value.to_string()),
                ExecutorKind::GeminiNative,
            )
        }
        "qwen" => (
            ChannelProtocol::OpenAI,
            ChannelProvider::Qwen,
            base_url.to_string(),
            vec![NativeEndpoint::ChatCompletions],
            None,
            ExecutorKind::ChatCompletions,
        ),
        "zhipu" => (
            ChannelProtocol::OpenAI,
            ChannelProvider::Zhipu,
            base_url.to_string(),
            vec![NativeEndpoint::ChatCompletions],
            None,
            ExecutorKind::ChatCompletions,
        ),
        "moonshot" => (
            ChannelProtocol::OpenAI,
            ChannelProvider::Moonshot,
            base_url.to_string(),
            vec![NativeEndpoint::ChatCompletions],
            None,
            ExecutorKind::ChatCompletions,
        ),
        "doubao" => (
            ChannelProtocol::OpenAI,
            ChannelProvider::Doubao,
            base_url.to_string(),
            vec![NativeEndpoint::ChatCompletions],
            None,
            ExecutorKind::ChatCompletions,
        ),
        // Old Ollama: strip ONLY an exact trailing "/v1" for the native
        // base. Never produces "/v1/api/chat" (design 11.1). Trim trailing
        // '/' first so "…:11434/v1/" also collapses to "…:11434" instead of
        // yielding "/v1/api/chat".
        "ollama" => {
            let trimmed = base_url.trim_end_matches('/');
            let native = trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string();
            (
                ChannelProtocol::Ollama,
                ChannelProvider::Ollama,
                native,
                vec![NativeEndpoint::ApiChat],
                None,
                ExecutorKind::ApiChat,
            )
        }
        // "custom" and anything unknown: OpenAI/custom with the legacy
        // fallback adaptor which hits /chat/completions. Preserve the
        // original URL, do not fake a vendor (T02 spec custom rule).
        _ => {
            let has_responses_debt = config
                .get("legacy_capabilities")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .any(|s| s.as_str() == Some("responses_via_chat_v1"))
                })
                .unwrap_or(false);
            let eps = if has_responses_debt {
                vec![NativeEndpoint::ChatCompletions, NativeEndpoint::Responses]
            } else {
                vec![NativeEndpoint::ChatCompletions]
            };
            (
                ChannelProtocol::OpenAI,
                ChannelProvider::Custom,
                base_url.to_string(),
                eps,
                None,
                ExecutorKind::FallbackChat,
            )
        }
    };

    let endpoints: Vec<String> = endpoints
        .into_iter()
        .map(|e| e.as_str().to_string())
        .collect();

    let executor_kind = if let Some(override_str) = &override_used {
        if override_str == "gemini_native" {
            ExecutorKind::GeminiNative
        } else {
            inferred_executor
        }
    } else {
        inferred_executor
    };

    ChannelIdentity {
        protocol: protocol.as_str().to_string(),
        provider: provider.as_str().to_string(),
        native_base_url,
        native_endpoints: endpoints,
        // Revision 0 => the resolver keeps reporting 0; callers must NOT
        // assume a revision here. The output DTO exposes the raw revision.
        identity_revision,
        legacy_executor_override: override_used,
        executor_kind: executor_kind.to_string(),
        inferred: true,
    }
}

fn host_in(base_url: &str, hosts: &[&str]) -> bool {
    let host = url_host(base_url);
    hosts.iter().any(|h| host == *h)
}

fn url_host(base_url: &str) -> String {
    let s = base_url.trim();
    let rest = s
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(s)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    // strip port, e.g. "api.openai.com:443"
    rest.split(':').next().unwrap_or("").to_string()
}

fn infer_claude_provider(base_url: &str) -> ChannelProvider {
    if host_in(base_url, ANTHROPIC_CANONICAL_HOSTS) {
        ChannelProvider::Anthropic
    } else {
        // Match built-in Anthropic-compat presets by canonical host; anything
        // else is custom. Matching is done by URL prefix/host, never guessing.
        let lower = base_url.to_lowercase();
        let p = all_channel_presets()
            .into_iter()
            .filter(|p| {
                p.protocol == ChannelProtocol::Anthropic && p.provider != ChannelProvider::Custom
            })
            .find(|p| {
                !p.native_base_url.is_empty()
                    && lower.starts_with(&p.native_base_url.to_lowercase())
            })
            .map(|p| p.provider);
        p.unwrap_or(ChannelProvider::Custom)
    }
}

impl std::fmt::Display for ExecutorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ExecutorKind::ChatCompletions => "chat_completions",
            ExecutorKind::Messages => "messages",
            ExecutorKind::ApiChat => "api_chat",
            ExecutorKind::GeminiNative => "gemini_native",
            ExecutorKind::FallbackChat => "fallback_chat",
        })
    }
}

fn executor_kind_for(protocol: &str, legacy_override: Option<&str>) -> ExecutorKind {
    if let Some("gemini_native") = legacy_override {
        return ExecutorKind::GeminiNative;
    }
    match protocol {
        "anthropic" => ExecutorKind::Messages,
        "ollama" => ExecutorKind::ApiChat,
        _ => ExecutorKind::ChatCompletions,
    }
}

/// Public helper: derive the executor kind from a persisted protocol string
/// (used by the repository when it builds the identity for a new write).
pub fn derive_executor_kind(protocol: &str) -> ExecutorKind {
    executor_kind_for(protocol, None)
}

/// Find the preset for a (protocol, provider), for the new-to-legacy writer.
pub fn preset_for(protocol: &str, provider: &str) -> Option<ChannelPreset> {
    all_channel_presets()
        .into_iter()
        .find(|p| p.protocol.as_str() == protocol && p.provider.as_str() == provider)
}

/// New-config -> legacy dual-write mapping (design 5.1 table).
///
/// Given the normalized identity + the preset that produced it, return the
/// legacy `type` and the old-code-compatible `base_url` to persist alongside
/// the new identity fields.
pub fn new_to_legacy(identity: &ChannelIdentity) -> (String, String) {
    // Legacy Gemini rows are not dual-written through a preset; they keep the
    // original URL/type and the executor override.
    if identity.legacy_executor_override.as_deref() == Some("gemini_native") {
        return ("gemini".to_string(), identity.native_base_url.clone());
    }
    match preset_for(&identity.protocol, &identity.provider) {
        Some(p) if !p.legacy_base_url.is_empty() => {
            (p.legacy_type.clone(), p.legacy_base_url.clone())
        }
        Some(p) => {
            // Custom (or any preset whose legacy root is empty): the old code
            // appends its endpoint to base_url, so the legacy base must be the
            // user's native root — never an empty string.
            let legacy_type = if p.legacy_type.is_empty() {
                legacy_alias_for(&identity.protocol)
            } else {
                p.legacy_type.clone()
            };
            let legacy_base = if identity.native_base_url.is_empty() {
                p.legacy_base_url.clone()
            } else {
                legacy_base_for_native(&identity.protocol, &identity.native_base_url)
            };
            (legacy_type, legacy_base)
        }
        None => {
            // No preset (e.g. custom): keep type=protocol-derived legacy alias.
            let legacy_type = match identity.protocol.as_str() {
                "anthropic" => "claude",
                "ollama" => "openai",
                _ => "openai",
            };
            (
                legacy_type.to_string(),
                legacy_base_for_native(&identity.protocol, &identity.native_base_url),
            )
        }
    }
}

/// Compute the old-code-compatible `base_url` for a *custom* new identity.
///
/// The legacy adapter appends its endpoint to `base_url` (claude adds
/// `/messages`, openai/custom adds `/chat/completions`).  The new native
/// executor serves Anthropic at `{root}/v1/messages` and Ollama's
/// OpenAI-compat layer at `{root}/v1/chat/completions`, so for those protocols
/// the legacy base must carry the `/v1` segment (mirroring the frontend
/// `deriveLegacyBaseUrl`); OpenAI custom roots already include `/v1` and are
/// left verbatim.
fn legacy_base_for_native(protocol: &str, native_base: &str) -> String {
    let needs_v1 = matches!(protocol, "anthropic" | "ollama")
        && !native_base.trim_end_matches('/').ends_with("/v1");
    if needs_v1 {
        format!("{}/v1", native_base.trim_end_matches('/'))
    } else {
        native_base.to_string()
    }
}

fn legacy_alias_for(protocol: &str) -> String {
    match protocol {
        "anthropic" => "claude",
        "ollama" => "openai",
        _ => "openai",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(channel_type: &str, base_url: &str, rev: i64) -> ChannelIdentityRow {
        let mut r = ChannelIdentityRow {
            channel_type: channel_type.to_string(),
            base_url: base_url.to_string(),
            identity_revision: rev,
            ..Default::default()
        };
        r.config = serde_json::json!({});
        r
    }

    #[test]
    fn openai_official_host_is_openai() {
        let id = resolve_channel_identity(&row("openai", "https://api.openai.com/v1", 0));
        assert_eq!(id.protocol, "openai");
        assert_eq!(id.provider, "openai");
        assert_eq!(id.native_base_url, "https://api.openai.com/v1");
        // T00 decision 2 / design 11.2: old openai rows are NOT natively
        // capable of /responses unless config records the legacy debt.
        assert_eq!(id.native_endpoints, vec!["chat_completions"]);
        assert!(id.inferred);
        assert_eq!(id.executor_kind, "chat_completions");
    }

    #[test]
    fn openai_private_gateway_is_custom() {
        let id = resolve_channel_identity(&row("openai", "https://gw.example.com/v1", 0));
        assert_eq!(id.protocol, "openai");
        assert_eq!(id.provider, "custom");
        assert_eq!(id.native_base_url, "https://gw.example.com/v1");
        assert_eq!(id.native_endpoints, vec!["chat_completions"]);
    }

    #[test]
    fn openai_with_responses_debt_keeps_both() {
        let mut r = row("openai", "https://api.openai.com/v1", 0);
        r.config = serde_json::json!({ "legacy_capabilities": ["responses_via_chat_v1"] });
        let id = resolve_channel_identity(&r);
        assert_eq!(id.native_endpoints, vec!["chat_completions", "responses"]);
    }

    /// 反向锁（StepFun 接入）：共享的 legacy openai 推断分支（非 api.openai.com
    /// 一律 custom，T02 规则）不得因新厂商预设而被改坏——revision-0 且 type=openai、
    /// base_url 指向 StepFun 主机的行，读时仍推断为 provider=custom。这条断言锁的
    /// 是「不改共享分支」这一决定本身。
    ///
    /// executor 附注：legacy openai 型行按协议派生 `chat_completions`
    /// （infer_legacy 的 "openai" 臂写死 ChatCompletions，见上文分支）；
    /// `fallback_chat` 只在 unknown/custom type 兜底臂出现。本测试不改该行为，
    /// 只是如实固定它。
    #[test]
    fn openai_stepfun_host_revision_zero_stays_custom() {
        let id = resolve_channel_identity(&row("openai", "https://api.stepfun.com/v1", 0));
        assert_eq!(id.protocol, "openai");
        assert_eq!(id.provider, "custom");
        assert_eq!(id.native_base_url, "https://api.stepfun.com/v1");
        assert_eq!(id.native_endpoints, vec!["chat_completions"]);
        assert_eq!(id.executor_kind, "chat_completions");
        assert!(id.inferred);
    }

    #[test]
    fn ollama_trailing_slash_v1_does_not_produce_v1_api_chat() {
        // "…:11434/v1/" (trailing slash after /v1) must collapse to the root,
        // not "…/v1", so /api/chat never becomes /v1/api/chat.
        let id = resolve_channel_identity(&row("ollama", "http://localhost:11434/v1/", 0));
        assert_eq!(id.native_base_url, "http://localhost:11434");
        assert_eq!(id.native_endpoints, vec!["api_chat"]);
    }

    #[test]
    fn claude_trailing_slash_v1_keeps_v1_root() {
        // "…/v1/" trims to "…/v1" (native root keeps the /v1 per main 约定),
        // so the executor builds exactly one "/v1/messages", not "/v1/v1/messages".
        let id = resolve_channel_identity(&row("claude", "https://api.anthropic.com/v1/", 0));
        assert_eq!(id.native_base_url, "https://api.anthropic.com/v1");
        // T06 I-4: legacy claude infers [messages, count_tokens].
        assert_eq!(id.native_endpoints, vec!["messages", "count_tokens"]);
    }

    #[test]
    fn deepseek_infers_openai_deepseek() {
        let id = resolve_channel_identity(&row("deepseek", "https://api.deepseek.com", 0));
        assert_eq!(id.protocol, "openai");
        assert_eq!(id.provider, "deepseek");
        assert_eq!(id.native_endpoints, vec!["chat_completions"]);
    }

    #[test]
    fn claude_canonical_is_anthropic() {
        let id = resolve_channel_identity(&row("claude", "https://api.anthropic.com/v1", 0));
        assert_eq!(id.protocol, "anthropic");
        assert_eq!(id.provider, "anthropic");
        // main 分支约定：legacy claude base 已是带 /v1 的根；native root 保持
        // 原样，executor 端点只补 /messages，最终 /v1/messages 恰一次。
        assert_eq!(id.native_base_url, "https://api.anthropic.com/v1");
        // T06 I-4: legacy claude infers [messages, count_tokens].
        assert_eq!(id.native_endpoints, vec!["messages", "count_tokens"]);
        assert_eq!(id.executor_kind, "messages");
    }

    #[test]
    fn claude_base_without_v1_is_custom_and_keeps_unchanged_native_root() {
        // 统一带 /v1 后，deepseek 预设 base 是 …/anthropic/v1；一个不带 /v1 的
        // vendor 根不再命中预设 → provider=custom，native root 保持原样（不伪造）。
        let id = resolve_channel_identity(&row("claude", "https://api.deepseek.com/anthropic", 0));
        assert_eq!(id.native_base_url, "https://api.deepseek.com/anthropic");
        assert_eq!(id.provider, "custom");
    }

    /// T06 I-4 (leader adjudication): legacy revision-0 `type == "claude"`
    /// MUST infer the count_tokens capability (the old predicate served
    /// /v1/messages/count_tokens), so the flag-OFF fallback keeps serving it.
    #[test]
    fn legacy_claude_infers_count_tokens() {
        let id = resolve_channel_identity(&row("claude", "https://api.anthropic.com/v1", 0));
        assert_eq!(id.protocol, "anthropic");
        assert_eq!(
            id.native_endpoints,
            vec!["messages", "count_tokens"],
            "legacy claude must retain count_tokens (no-regression contract)"
        );
        assert!(id.inferred);
    }

    #[test]
    fn claude_deepseek_compat_maps_to_deepseek() {
        // main 约定：deepseek anthropic 兼容 base 带 /v1 才命中预设。
        let id =
            resolve_channel_identity(&row("claude", "https://api.deepseek.com/anthropic/v1", 0));
        assert_eq!(id.protocol, "anthropic");
        assert_eq!(id.provider, "deepseek");
    }

    #[test]
    fn claude_unknown_gateway_is_custom() {
        let id = resolve_channel_identity(&row("claude", "https://gw.example.com/xyz", 0));
        assert_eq!(id.provider, "custom");
    }

    #[test]
    fn gemini_keeps_native_override() {
        let id = resolve_channel_identity(&row(
            "gemini",
            "https://generativelanguage.googleapis.com",
            0,
        ));
        assert_eq!(id.protocol, "openai");
        assert_eq!(id.provider, "google");
        assert_eq!(
            id.legacy_executor_override.as_deref(),
            Some("gemini_native")
        );
        assert_eq!(id.executor_kind, "gemini_native");
        assert_eq!(
            id.native_base_url,
            "https://generativelanguage.googleapis.com"
        );
    }

    #[test]
    fn qwen_zhipu_moonshot_doubao_map_to_openai() {
        for (t, prov) in [
            ("qwen", "qwen"),
            ("zhipu", "zhipu"),
            ("moonshot", "moonshot"),
            ("doubao", "doubao"),
        ] {
            let id = resolve_channel_identity(&row(t, "https://example.com/v1", 0));
            assert_eq!(id.protocol, "openai");
            assert_eq!(id.provider, prov);
            assert_eq!(id.native_endpoints, vec!["chat_completions"]);
        }
    }

    #[test]
    fn ollama_strips_exact_trailing_v1() {
        let id = resolve_channel_identity(&row("ollama", "http://localhost:11434/v1", 0));
        assert_eq!(id.protocol, "ollama");
        assert_eq!(id.provider, "ollama");
        assert_eq!(id.native_base_url, "http://localhost:11434");
        assert_eq!(id.native_endpoints, vec!["api_chat"]);
        assert_eq!(id.executor_kind, "api_chat");
    }

    #[test]
    fn ollama_without_v1_keeps_base() {
        let id = resolve_channel_identity(&row("ollama", "http://localhost:11434", 0));
        assert_eq!(id.native_base_url, "http://localhost:11434");
        assert_eq!(id.native_endpoints, vec!["api_chat"]);
    }

    #[test]
    fn ollama_never_produces_v1_api_chat() {
        // The native base must never keep a "/v1" suffix that an api_chat
        // executor would append to, producing "/v1/api/chat".
        let id = resolve_channel_identity(&row("ollama", "http://localhost:11434/v1", 0));
        assert!(
            !id.native_base_url.ends_with("/v1"),
            "{}",
            id.native_base_url
        );
    }

    #[test]
    fn custom_unknown_maps_to_openai_custom() {
        let id = resolve_channel_identity(&row("custom", "https://my.gateway/v1", 0));
        assert_eq!(id.protocol, "openai");
        assert_eq!(id.provider, "custom");
        assert_eq!(id.executor_kind, "fallback_chat");
        assert_eq!(id.native_endpoints, vec!["chat_completions"]);
    }

    #[test]
    fn custom_with_responses_debt_keeps_both() {
        let mut r = row("custom", "https://my.gateway/v1", 0);
        r.config = serde_json::json!({ "legacy_capabilities": ["responses_via_chat_v1"] });
        let id = resolve_channel_identity(&r);
        assert_eq!(id.native_endpoints, vec!["chat_completions", "responses"]);
    }

    #[test]
    fn revision_zero_triggers_inference_even_with_partial_new_fields() {
        // Simulates a row whose new fields were written by an older/newer
        // mixed path but revision stayed 0: resolver must NOT trust them.
        let mut r = row("openai", "https://api.openai.com/v1", 0);
        r.protocol = Some("anthropic".to_string());
        r.provider = Some("anthropic".to_string());
        r.native_base_url = Some("https://api.anthropic.com".to_string());
        r.native_endpoints = Some("[\"messages\"]".to_string());
        let id = resolve_channel_identity(&r);
        assert_eq!(id.protocol, "openai");
        assert_eq!(id.provider, "openai");
        assert!(id.inferred);
    }

    #[test]
    fn revision_one_trusts_new_fields() {
        let mut r = row("openai", "https://api.openai.com/v1", 1);
        r.protocol = Some("anthropic".to_string());
        r.provider = Some("zhipu".to_string());
        r.native_base_url = Some("https://open.bigmodel.cn/api/anthropic".to_string());
        r.native_endpoints = Some("[\"messages\"]".to_string());
        let id = resolve_channel_identity(&r);
        assert_eq!(id.protocol, "anthropic");
        assert_eq!(id.provider, "zhipu");
        assert_eq!(
            id.native_base_url,
            "https://open.bigmodel.cn/api/anthropic/v1"
        );
        assert!(!id.inferred);
    }

    #[test]
    fn new_to_legacy_anthropic_zhipu() {
        let mut r = row("claude", "https://open.bigmodel.cn/api/anthropic/v1", 0);
        r.protocol = None;
        let id = resolve_channel_identity(&r);
        let (lt, lb) = new_to_legacy(&id);
        assert_eq!(lt, "claude");
        assert_eq!(lb, "https://open.bigmodel.cn/api/anthropic/v1");
    }

    #[test]
    fn new_to_legacy_ollama_native() {
        let mut r = row("ollama", "http://localhost:11434/v1", 0);
        r.protocol = None;
        let id = resolve_channel_identity(&r);
        assert_eq!(id.protocol, "ollama");
        let (lt, lb) = new_to_legacy(&id);
        // Ollama native dual-writes type=openai + OpenAI-compat /v1 base
        assert_eq!(lt, "openai");
        assert_eq!(lb, "http://localhost:11434/v1");
    }

    #[test]
    fn new_to_legacy_anthropic_ollama() {
        let mut r = row("claude", "http://localhost:11434/v1", 0);
        r.protocol = None;
        let id = resolve_channel_identity(&r);
        // main 分支约定：claude 分支不再剥 /v1，native root 保持 11434/v1，
        // 经 starts_with 匹配 anthropic:ollama 预设。
        assert_eq!(id.protocol, "anthropic");
        assert_eq!(id.provider, "ollama");
        let (lt, lb) = new_to_legacy(&id);
        assert_eq!(lt, "claude");
        assert_eq!(lb, "http://localhost:11434/v1");
    }

    #[test]
    fn new_to_legacy_openai_google_is_openai_type() {
        let r = row(
            "openai",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            0,
        );
        let id = resolve_channel_identity(&r);
        // provider stays custom because the host is not api.openai.com
        let (lt, _) = new_to_legacy(&id);
        assert_eq!(lt, "openai");
    }

    #[test]
    fn new_to_legacy_custom_preset_uses_native_base_as_legacy_base() {
        // A new openai:custom channel has an empty preset legacy_base_url; the
        // legacy base must fall back to the user's native root so old code
        // still builds the correct /chat/completions URL.
        let identity = ChannelIdentity {
            protocol: "openai".to_string(),
            provider: "custom".to_string(),
            native_base_url: "https://gw.internal.example.com/v1".to_string(),
            native_endpoints: vec!["chat_completions".to_string()],
            identity_revision: 1,
            legacy_executor_override: None,
            executor_kind: "chat_completions".to_string(),
            inferred: false,
        };
        let (lt, lb) = new_to_legacy(&identity);
        assert_eq!(lt, "openai");
        assert_eq!(lb, "https://gw.internal.example.com/v1");
    }

    #[test]
    fn new_to_legacy_anthropic_custom_uses_claude_alias_and_native_root() {
        // main 分支约定：native root 自带 /v1（表单输入即 /v1 结尾）；custom
        // 渠道 legacy base 保持原样，旧适配器拼 /messages → …/v1/messages。
        let identity = ChannelIdentity {
            protocol: "anthropic".to_string(),
            provider: "custom".to_string(),
            native_base_url: "https://gw.internal.example.com/anthropic/v1".to_string(),
            native_endpoints: vec!["messages".to_string()],
            identity_revision: 1,
            legacy_executor_override: None,
            executor_kind: "messages".to_string(),
            inferred: false,
        };
        let (lt, lb) = new_to_legacy(&identity);
        assert_eq!(lt, "claude");
        // legacy base 保持 native root（已带 /v1），旧适配器拼 /messages 得到
        // {root}/v1/messages —— 与新 executor 的 {native}/messages 完全一致。
        assert_eq!(lb, "https://gw.internal.example.com/anthropic/v1");
    }
}
