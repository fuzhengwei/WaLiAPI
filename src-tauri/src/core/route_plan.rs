//! Model-first RoutePlan (T05).
//!
//! Replaces the flat candidate queue of the legacy `Dispatcher` with a grouped,
//! model-first plan:
//!
//!   model candidates → native protocol group (G1) → in-group priority tier →
//!   same-tier weight sampling → conversion group (G2) when the native group has
//!   no candidate or only degradation-permitted failures.
//!
//! Single facade: [`authorize_and_plan`] — the ONLY entry point used by every
//! public endpoint and both stream/non-stream paths (design 6.0.1 / 11.3).
//!
//! Group matrix (design 6.0.1 table):
//!   * Chat      G1 = OpenAI `chat_completions`;  G2 = Anthropic `messages` codec.
//!   * Responses G1 = OpenAI native `responses`;   G2 = explicit `responses_via_chat_v1`.
//!   * Messages  G1 = Anthropic `messages`;        G2 = OpenAI `chat_completions` codec.
//!   * CountTokens = Anthropic `count_tokens` only.
//!   * Embeddings  = OpenAI `embeddings` only.
//!
//! Native Ollama `/api/chat` is NOT in the current matrix (T06).

use crate::core::channel_identity::{
    resolve_channel_identity, ChannelIdentity, ChannelIdentityRow,
};
use crate::core::feature_flags::FeatureFlags;
use crate::db::models::{ApiKey, AuthAccount, Channel};
use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};

/// Default per-group attempt budget (T00 decision 4).
pub const DEFAULT_MAX_ATTEMPTS_PER_GROUP: usize = 3;
/// Default whole-request attempt budget (T00 decision 4).
pub const DEFAULT_MAX_ATTEMPTS_TOTAL: usize = 6;

/// Non-stream framing fixed by a resolved auth route profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthNonStreamFraming {
    /// Ordinary request/response JSON (Kimi Chat and Anthropic Messages).
    Json,
    /// Upstream Responses endpoint forces SSE, which the executor aggregates.
    ForcedResponsesSse,
}

/// The downstream endpoint being routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum EndpointKind {
    ChatCompletions,
    Responses,
    Messages,
    CountTokens,
    Embeddings,
}

impl EndpointKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EndpointKind::ChatCompletions => "chat_completions",
            EndpointKind::Responses => "responses",
            EndpointKind::Messages => "messages",
            EndpointKind::CountTokens => "count_tokens",
            EndpointKind::Embeddings => "embeddings",
        }
    }

    /// Map a gate `DownstreamProtocol` to a routable endpoint kind.
    /// `Completions` / `Images` / `Audio` are NOT routed by the model-first plan
    /// (they keep their existing handlers).
    pub fn from_downstream_protocol(
        protocol: crate::security::gate::DownstreamProtocol,
    ) -> Option<EndpointKind> {
        use crate::security::gate::DownstreamProtocol::*;
        match protocol {
            ChatCompletions => Some(EndpointKind::ChatCompletions),
            Responses => Some(EndpointKind::Responses),
            Messages => Some(EndpointKind::Messages),
            CountTokens => Some(EndpointKind::CountTokens),
            Embeddings => Some(EndpointKind::Embeddings),
            Completions | Images | Audio => None,
        }
    }
}

/// Upstream wire protocol of a route group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum UpstreamProtocol {
    OpenAI,
    Anthropic,
    Ollama,
    /// The fixed Codex account adapter speaks the backend Responses wire format.
    Responses,
}

impl UpstreamProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpstreamProtocol::OpenAI => "openai",
            UpstreamProtocol::Anthropic => "anthropic",
            UpstreamProtocol::Ollama => "ollama",
            UpstreamProtocol::Responses => "responses",
        }
    }
}

/// Whether a group is the native (passthrough) tier or an explicit conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GroupTier {
    Native,
    Conversion,
}

impl GroupTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            GroupTier::Native => "native",
            GroupTier::Conversion => "conversion",
        }
    }
}

/// A safe, routable upstream candidate.  Auth account payloads remain in the
/// database model for the provider adapter, but are never exposed by this type's
/// Debug implementation or RoutePlan snapshots.
#[derive(Clone)]
pub enum RouteCandidate {
    Channel {
        channel: Channel,
        identity: ChannelIdentity,
    },
    AuthAccount(AuthAccount),
}

impl std::fmt::Debug for RouteCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteCandidate")
            .field("id", &self.id())
            .field("name", &self.name())
            .field("upstream_type", &self.upstream_type())
            .field("provider", &self.provider())
            .field("priority", &self.priority())
            .field("weight", &self.weight())
            .finish()
    }
}

impl RouteCandidate {
    pub fn id(&self) -> &str {
        match self {
            Self::Channel { channel, .. } => &channel.id,
            Self::AuthAccount(account) => &account.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Channel { channel, .. } => &channel.name,
            Self::AuthAccount(account) => &account.label,
        }
    }

    pub fn priority(&self) -> i64 {
        match self {
            Self::Channel { channel, .. } => channel.priority,
            Self::AuthAccount(account) => account.priority,
        }
    }

    pub fn weight(&self) -> i64 {
        match self {
            Self::Channel { channel, .. } => channel.weight,
            Self::AuthAccount(account) => account.weight,
        }
    }

    pub fn upstream_type(&self) -> &'static str {
        match self {
            Self::Channel { .. } => "channel",
            Self::AuthAccount(_) => "auth_account",
        }
    }

    pub fn provider(&self) -> String {
        match self {
            Self::Channel { identity, .. } => identity.provider.clone(),
            Self::AuthAccount(account) => account.provider.clone(),
        }
    }

    pub fn native_base_url(&self) -> String {
        match self {
            Self::Channel { identity, .. } => identity.native_base_url.clone(),
            // This is descriptive routing metadata only.  The account adapter
            // owns its fixed backend URL and never accepts a caller override.
            Self::AuthAccount(_) => "https://chatgpt.com/backend-api/codex".to_string(),
        }
    }

    pub fn identity_revision(&self) -> i64 {
        match self {
            Self::Channel { identity, .. } => identity.identity_revision,
            Self::AuthAccount(_) => 0,
        }
    }

    pub fn channel(&self) -> Option<&Channel> {
        match self {
            Self::Channel { channel, .. } => Some(channel),
            Self::AuthAccount(_) => None,
        }
    }

    pub fn auth_account(&self) -> Option<&AuthAccount> {
        match self {
            Self::Channel { .. } => None,
            Self::AuthAccount(account) => Some(account),
        }
    }

    /// Unified accessor: returns the candidate's `model_mapping` as a JSON Value.
    /// Works for both channels and auth accounts.
    pub fn mapping_json(&self) -> Value {
        match self {
            Self::Channel { channel, .. } => {
                serde_json::from_str(&channel.model_mapping).unwrap_or_default()
            }
            Self::AuthAccount(account) => account.model_mapping().unwrap_or_default(),
        }
    }
}

/// Immutable per-model wire profile for an auth account, resolved from the
/// provider `/models` snapshot (never from renderer, headers, or body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRouteProfile {
    pub provider: String,
    pub native_base_url: String,
    pub upstream_protocol: UpstreamProtocol,
    pub upstream_endpoint: String,
    pub non_stream_framing: AuthNonStreamFraming,
}

/// One candidate that survived model matching and endpoint-capability filtering.
#[derive(Debug, Clone)]
pub struct RouteGroupCandidate {
    pub candidate: RouteCandidate,
    pub tier: GroupTier,
    pub upstream_protocol: UpstreamProtocol,
    pub upstream_endpoint: String,
    /// Registered provider string for auth candidates (`codex`, `kimi`);
    /// `None` for regular channels.
    pub auth_provider: Option<String>,
    /// Frozen base URL the executor must use.  Auth candidates carry the
    /// per-model profile base; channels carry their identity base.
    pub native_base_url: String,
    /// Non-stream framing for auth candidates; `None` for channels (which use
    /// the codec-bound framing).
    pub auth_non_stream_framing: Option<AuthNonStreamFraming>,
}

/// A named bucket of candidates sharing one upstream protocol/endpoint and an
/// independent retry budget.
#[derive(Debug, Clone)]
pub struct RouteGroup {
    pub id: String,
    pub tier: GroupTier,
    pub downstream: EndpointKind,
    pub upstream_protocol: UpstreamProtocol,
    pub upstream_endpoint: String,
    pub candidates: Vec<RouteGroupCandidate>,
    /// Effective per-group attempt budget (≤ candidate count).
    pub max_attempts: usize,
}

/// GAP-08：把设置中心的「失败自动重试策略」映射为 RoutePlan 尝试预算。
/// 组内 = 次数+1（与 legacy 轨 `max_attempts = retry_times+1` 一致）、
/// 总量 = 组内×2（与既有硬编码默认 3/6 一致——双候选组语义）；关闭重试
/// 时 (1, 1)，主路径真正不重试。
pub fn retry_budget_from_settings(retry_enabled: bool, retry_times: i32) -> (usize, usize) {
    if !retry_enabled {
        return (1, 1);
    }
    let per_group = retry_times.max(0) as usize + 1;
    (per_group, per_group * 2)
}

impl RoutePlan {
    /// 按重试设置重写尝试预算（GAP-08：设置此前只作用于 legacy 轨）。
    /// 组内预算与构建期同语义：受组内候选数物理限制（≤ 候选数）；总量
    /// 受各组预算之和限制。non_idempotent 请求不放宽（保持 1，无自动重试）。
    pub fn apply_retry_budget(&mut self, per_group: usize, total: usize) {
        if self.non_idempotent {
            return;
        }
        for group in &mut self.groups {
            group.max_attempts = per_group.min(group.candidates.len()).max(1);
        }
        self.max_attempts_total = total
            .min(self.groups.iter().map(|g| g.max_attempts).sum::<usize>())
            .max(1);
    }
}

/// The full routing plan for one request.  `groups` is already ordered
/// native-first; conversion priority/weight can never leapfrog the native group.
#[derive(Debug, Clone)]
pub struct RoutePlan {
    pub endpoint: EndpointKind,
    /// The downstream requested model (mapping source name).
    pub model: String,
    pub groups: Vec<RouteGroup>,
    /// Whole-request attempt budget.
    pub max_attempts_total: usize,
    pub flags: FeatureFlags,
    /// Channels dropped for identity/config problems (logged, never routed).
    pub config_errors: Vec<String>,
    /// Responses requests carrying `background`/`store` disable automatic retry.
    pub non_idempotent: bool,
}

/// Authorization / planning failure.  These all fail closed BEFORE any upstream
/// access (design 11.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    KeyDisabled,
    KeyExpired,
    /// 配额用尽，携带 (quota_used, quota_limit)：错误文案直接给出差额信息，
    /// 客户端与管理端无需再查库（C-01）。
    QuotaExceeded(i64, i64),
    ModelNotAllowed(String),
    NoChannels,
    NoCandidateForModel(String),
    /// Model candidates exist but none supports this endpoint.
    /// Carries the endpoint so the HTTP status follows design 6.3:
    /// 503 for chat_completions/responses/messages (gateway-wide unavailability),
    /// 501 for count_tokens/embeddings (capability not offered).
    NoEndpointSupported(EndpointKind, String),
}

impl PlanError {
    pub fn http_status(&self) -> u16 {
        match self {
            PlanError::KeyDisabled => 401,
            PlanError::KeyExpired => 401,
            PlanError::QuotaExceeded(..) => 429,
            PlanError::ModelNotAllowed(_) => 403,
            PlanError::NoChannels => 503,
            PlanError::NoCandidateForModel(_) => 503,
            PlanError::NoEndpointSupported(endpoint, _) => match endpoint {
                // Design 6.3: Chat/Responses/Messages unavailable → 503.
                EndpointKind::ChatCompletions
                | EndpointKind::Responses
                | EndpointKind::Messages => 503,
                // Design 6.3: CountTokens keeps its current 501 semantics;
                // Embeddings likewise (no codec/conversion path exists).
                EndpointKind::CountTokens | EndpointKind::Embeddings => 501,
            },
        }
    }

    pub fn message(&self) -> String {
        match self {
            PlanError::KeyDisabled => "API key is disabled".to_string(),
            PlanError::KeyExpired => "API key has expired".to_string(),
            PlanError::QuotaExceeded(used, limit) => {
                format!("Quota exceeded (used {} / limit {})", used, limit)
            }
            PlanError::ModelNotAllowed(m) => {
                format!("Model '{}' is not allowed for this API key", m)
            }
            PlanError::NoChannels => "No available upstream candidate".to_string(),
            PlanError::NoCandidateForModel(m) => {
                format!("No available upstream candidate for model: {}", m)
            }
            PlanError::NoEndpointSupported(_endpoint, m) => {
                format!(
                    "No available upstream candidate supports this endpoint for model: {}",
                    m
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Weighted ordering (shared with the legacy Dispatcher so semantics stay exact)
// ---------------------------------------------------------------------------

/// Anything with a `priority` (tier) and `weight` (same-tier sampling).
pub trait HasPriorityWeight {
    fn priority(&self) -> i64;
    fn weight(&self) -> i64;
}

impl HasPriorityWeight for Channel {
    fn priority(&self) -> i64 {
        self.priority
    }
    fn weight(&self) -> i64 {
        self.weight
    }
}

impl HasPriorityWeight for RouteGroupCandidate {
    fn priority(&self) -> i64 {
        self.candidate.priority()
    }
    fn weight(&self) -> i64 {
        self.candidate.weight()
    }
}

/// Order candidates by priority descending; within a priority tier, sample by
/// weight WITHOUT replacement (same semantics as the legacy Dispatcher).
pub fn order_by_priority_weight<T, R>(mut candidates: Vec<T>, rng: &mut R) -> Vec<T>
where
    T: HasPriorityWeight + Clone,
    R: Rng + ?Sized,
{
    if candidates.is_empty() {
        return candidates;
    }
    candidates.sort_by_key(|c| std::cmp::Reverse(c.priority()));
    let mut ordered = Vec::with_capacity(candidates.len());
    let mut start = 0;
    while start < candidates.len() {
        let priority = candidates[start].priority();
        let mut end = start;
        while end < candidates.len() && candidates[end].priority() == priority {
            end += 1;
        }
        let mut group = candidates[start..end].to_vec();
        while !group.is_empty() {
            let total_weight: i64 = group.iter().map(|c| c.weight().max(0)).sum();
            let index = if total_weight > 0 {
                let mut point = rng.random_range(0..total_weight);
                let mut selected = 0;
                for (idx, c) in group.iter().enumerate() {
                    point -= c.weight().max(0);
                    if point < 0 {
                        selected = idx;
                        break;
                    }
                }
                selected
            } else {
                0
            };
            ordered.push(group.remove(index));
        }
        start = end;
    }
    ordered
}

fn route_bucket(candidate: &RouteGroupCandidate, flags: &FeatureFlags) -> usize {
    let is_auth = candidate.candidate.auth_account().is_some();
    let is_same_protocol = candidate.tier == GroupTier::Native;
    match (flags.prefer_auth_accounts, flags.prefer_same_protocol) {
        (true, true) => match (is_auth, is_same_protocol) {
            (true, true) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 3,
        },
        (true, false) => {
            if is_auth {
                0
            } else {
                1
            }
        }
        (false, true) => {
            if is_same_protocol {
                0
            } else {
                1
            }
        }
        (false, false) => 0,
    }
}

fn push_route_group(
    groups: &mut Vec<RouteGroup>,
    endpoint: EndpointKind,
    candidates: Vec<RouteGroupCandidate>,
    per_group: usize,
) {
    if candidates.is_empty() {
        return;
    }
    let max_attempts = per_group.min(candidates.len()).max(1);
    let first = &candidates[0];
    groups.push(RouteGroup {
        id: format!(
            "{}_g{}_{}",
            endpoint.as_str(),
            groups.len() + 1,
            first.tier.as_str()
        ),
        tier: first.tier,
        downstream: endpoint,
        upstream_protocol: first.upstream_protocol,
        upstream_endpoint: first.upstream_endpoint.clone(),
        max_attempts,
        candidates,
    });
}

fn build_ordered_route_groups<R>(
    endpoint: EndpointKind,
    candidates: Vec<RouteGroupCandidate>,
    flags: &FeatureFlags,
    per_group: usize,
    rng: &mut R,
) -> Vec<RouteGroup>
where
    R: Rng + ?Sized,
{
    let mut buckets: Vec<Vec<RouteGroupCandidate>> =
        vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for candidate in candidates {
        buckets[route_bucket(&candidate, flags)].push(candidate);
    }

    let mut groups = Vec::new();
    for bucket in buckets.into_iter().filter(|bucket| !bucket.is_empty()) {
        let ordered = order_by_priority_weight(bucket, rng);
        let mut current: Vec<RouteGroupCandidate> = Vec::new();
        let mut current_tier: Option<GroupTier> = None;
        for candidate in ordered {
            if current_tier.is_some_and(|tier| tier != candidate.tier) {
                push_route_group(&mut groups, endpoint, current, per_group);
                current = Vec::new();
            }
            current_tier = Some(candidate.tier);
            current.push(candidate);
        }
        push_route_group(&mut groups, endpoint, current, per_group);
    }
    groups
}

// ---------------------------------------------------------------------------
// Model mapping resolution (sampled EXACTLY ONCE per attempt)
// ---------------------------------------------------------------------------

/// Resolve the upstream model from a channel's `model_mapping`.
///
/// * string value  → used verbatim;
/// * array value   → sampled WITHOUT replacement exactly once per attempt;
/// * no mapping    → the requested model.
///
/// The returned model is the single source of truth for the attempt body, logs
/// and stats (design 11.4: "解析后的 upstream_model 同时传给适配器、日志和统计").
pub fn resolve_upstream_model<R: Rng + ?Sized>(
    mapping: &Value,
    model: &str,
    rng: &mut R,
) -> String {
    if let Some(mapped) = mapping.get(model) {
        if let Some(s) = mapped.as_str() {
            return s.to_string();
        }
        if let Some(arr) = mapped.as_array() {
            let models: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if !models.is_empty() {
                let idx = rng.random_range(0..models.len());
                return models[idx].clone();
            }
        }
    }
    model.to_string()
}

// ---------------------------------------------------------------------------
// Authorization (design 11.3)
// ---------------------------------------------------------------------------

/// Authorize a request before any candidate construction.  Empty allowed arrays
/// mean "no restriction" (T00 decision 3).
///
/// `model` is the downstream request model / mapping source name.
pub fn authorize_request(api_key: &ApiKey, model: &str) -> Result<(), PlanError> {
    if api_key.status != 1 {
        return Err(PlanError::KeyDisabled);
    }
    if let Some(expires) = api_key.expires_at.as_deref() {
        if !expires.trim().is_empty() && is_expired(expires) {
            return Err(PlanError::KeyExpired);
        }
    }
    if api_key.quota_limit > 0 && api_key.quota_used >= api_key.quota_limit {
        return Err(PlanError::QuotaExceeded(
            api_key.quota_used,
            api_key.quota_limit,
        ));
    }
    let allowed: Vec<String> = serde_json::from_str(&api_key.allowed_models).unwrap_or_default();
    if !allowed.is_empty() && !allowed.iter().any(|m| m == model) {
        return Err(PlanError::ModelNotAllowed(model.to_string()));
    }
    let denied_models: Vec<String> =
        serde_json::from_str(&api_key.denied_models).unwrap_or_default();
    if denied_models.iter().any(|m| m == model) {
        return Err(PlanError::ModelNotAllowed(model.to_string()));
    }
    Ok(())
}

pub(crate) fn is_expired(iso: &str) -> bool {
    use chrono::{DateTime, NaiveDateTime, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(iso) {
        return dt.with_timezone(&Utc) < Utc::now();
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%S%.f") {
        return dt < Utc::now().naive_utc();
    }
    if let Ok(dt) = chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d") {
        return dt
            .and_hms_opt(23, 59, 59)
            .unwrap_or(dt.and_hms_opt(0, 0, 0).unwrap())
            < Utc::now().naive_utc();
    }
    // Unparseable expiry is treated as "not expired" (fail open on format,
    // fail closed on the check itself).
    false
}

// ---------------------------------------------------------------------------
// Model candidates (design 6.0.1 / 11.3)
// ---------------------------------------------------------------------------

/// Keep only channels that are enabled, allowed by the API key, and match the
/// requested model (either `models` contains it, `model_mapping` has it as a
/// source name, or legacy `models=[]` wildcard).
pub fn resolve_model_candidates<'a>(
    channels: &'a [Channel],
    model: &str,
    api_key: &ApiKey,
) -> Vec<&'a Channel> {
    let allowed_channels: Vec<String> =
        serde_json::from_str(&api_key.allowed_channels).unwrap_or_default();
    let denied_channels: Vec<String> =
        serde_json::from_str(&api_key.denied_channels).unwrap_or_default();
    channels
        .iter()
        .filter(|c| c.status == 1)
        // allowed_channels filter happens BEFORE model matching (design 11.3).
        .filter(|c| allowed_channels.is_empty() || allowed_channels.contains(&c.id))
        .filter(|c| !denied_channels.contains(&c.id))
        .filter(|c| channel_accepts_model(c, model))
        .collect()
}

/// Resolve the mixed candidate pool.  Channels retain their legacy wildcard
/// and API-key allow-list behavior; Auth accounts are deliberately exempt from
/// `allowed_channels`, require an exact non-empty model snapshot hit, and never
/// inherit Channel model mappings.
pub fn resolve_route_candidates(
    channels: &[Channel],
    accounts: &[AuthAccount],
    model: &str,
    api_key: &ApiKey,
) -> Vec<RouteCandidate> {
    let mut candidates = resolve_model_candidates(channels, model, api_key)
        .into_iter()
        .map(|channel| RouteCandidate::Channel {
            identity: resolve_channel_identity(&ChannelIdentityRow::from(channel)),
            channel: channel.clone(),
        })
        .collect::<Vec<_>>();

    candidates.extend(
        accounts
            .iter()
            .filter(|account| auth_account_accepts_model(account, model))
            .cloned()
            .map(RouteCandidate::AuthAccount),
    );
    candidates
}

/// Check if a model name matches any source key in a mapping JSON.
/// Used by both `channel_accepts_model` and `auth_account_accepts_model`.
fn mapping_contains_source(mapping_json: &str, model: &str) -> bool {
    let mapping: Value = serde_json::from_str(mapping_json).unwrap_or_default();
    mapping
        .as_object()
        .map(|o| o.contains_key(model))
        .unwrap_or(false)
}

fn channel_accepts_model(channel: &Channel, model: &str) -> bool {
    let models: Vec<String> = serde_json::from_str(&channel.models).unwrap_or_default();
    if models.is_empty() {
        // T00 decision 3: empty models = wildcard (accepts any request model).
        return true;
    }
    if models.iter().any(|m| m == model) {
        return true;
    }
    // Mapping source names also count as hits.
    mapping_contains_source(&channel.model_mapping, model)
}

fn auth_account_accepts_model(account: &AuthAccount, model: &str) -> bool {
    if account.disabled != 0 || account.status != "active" {
        return false;
    }
    if account_quota_unavailable(account) {
        return false;
    }
    // Direct model hit in the account's synced model snapshot.
    let direct_hit = account
        .model_states()
        .ok()
        .map(|states| {
            states
                .models
                .iter()
                .any(|state| state.id == model && state.status == "available" && !state.unavailable)
        })
        .unwrap_or(false);
    if direct_hit {
        return true;
    }
    // Mapping source names also count as hits (shared helper).
    mapping_contains_source(&account.model_mapping_json, model)
}

/// The repository clears expired quota before returning route accounts.  This
/// duplicate check keeps direct planner callers fail-closed for malformed and
/// future quota snapshots while still accepting an already-expired recovery.
fn account_quota_unavailable(account: &AuthAccount) -> bool {
    let Ok(Some(quota)) = account.quota_state() else {
        return account.quota_json.is_some();
    };
    if !quota.exceeded {
        return false;
    }
    let Some(recover_at) = quota.next_recover_at.as_deref() else {
        return true;
    };
    chrono::DateTime::parse_from_rfc3339(recover_at)
        .map(|recover_at| recover_at > chrono::Utc::now())
        .unwrap_or(true)
}

// ---------------------------------------------------------------------------
// Auth route profiles (per-model wire profile, frozen at plan time)
// ---------------------------------------------------------------------------

/// Fixed mapping for a single model snapshot entry to a wire profile.
///
/// The `protocol` value is the provider `/models` catalog's non-secret routing
/// metadata.  `missing`/`kimi` → Kimi Chat; `anthropic` → Kimi Messages beta;
/// any other non-empty value fails closed (not routable as Chat).
fn profile_for_model_state(provider: &str, protocol: Option<&str>) -> Option<AuthRouteProfile> {
    // Missing OR empty protocol is the Kimi Chat-compatible default.
    let protocol = match protocol {
        Some("") | None => "kimi",
        Some(value) => value,
    };
    match (provider, protocol) {
        ("codex", _) => Some(AuthRouteProfile {
            provider: "codex".into(),
            native_base_url: "https://chatgpt.com/backend-api/codex".into(),
            upstream_protocol: UpstreamProtocol::Responses,
            upstream_endpoint: "responses".into(),
            non_stream_framing: AuthNonStreamFraming::ForcedResponsesSse,
        }),
        ("kimi", "kimi") => Some(AuthRouteProfile {
            provider: "kimi".into(),
            native_base_url: "https://api.kimi.com/coding/v1".into(),
            upstream_protocol: UpstreamProtocol::OpenAI,
            upstream_endpoint: "chat_completions".into(),
            non_stream_framing: AuthNonStreamFraming::Json,
        }),
        ("kimi", "anthropic") => Some(AuthRouteProfile {
            provider: "kimi".into(),
            native_base_url: "https://api.kimi.com/coding".into(),
            upstream_protocol: UpstreamProtocol::Anthropic,
            upstream_endpoint: "messages_beta".into(),
            non_stream_framing: AuthNonStreamFraming::Json,
        }),
        // Unknown provider or unknown non-empty Kimi protocol: fail closed.
        _ => None,
    }
}

/// The wire profile for one auth account routing `requested_model`.
///
/// A direct snapshot hit uses that model's own `protocol`.  An alias mapping
/// (`model_mapping[requested_model]` = one or more upstream model names) is
/// allowed only when every target resolves to the *same* profile; a mixed
/// `kimi`/`anthropic` mapping must fail closed rather than randomly change the
/// group protocol.  An unknown provider, unknown Kimi protocol, missing target,
/// or malformed snapshot also returns `None` (fail closed, never Codex URL or
/// Kimi Chat fallback).
pub fn resolve_auth_route_profile(
    account: &AuthAccount,
    requested_model: &str,
) -> Option<AuthRouteProfile> {
    let provider = account.provider.clone();

    // Direct hit on the synced snapshot.
    let states = account.model_states().ok()?;
    if let Some(state) = states
        .models
        .iter()
        .find(|state| state.id == requested_model)
    {
        // A present-but-unavailable model is not routable (fail closed).
        if state.status != "available" || state.unavailable {
            return None;
        }
        return profile_for_model_state(&provider, state.protocol.as_deref());
    }

    // Alias mapping: every target must resolve to an identical profile.
    let mapping: Value = serde_json::from_str(&account.model_mapping_json).unwrap_or_default();
    let Some(targets) = mapping.get(requested_model) else {
        return None;
    };
    let target_names: Vec<String> = match targets {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => return None,
    };
    if target_names.is_empty() {
        return None;
    }
    let mut profiles: Vec<AuthRouteProfile> = Vec::new();
    for name in &target_names {
        let Some(state) = states.models.iter().find(|state| &state.id == name) else {
            return None;
        };
        if state.status != "available" || state.unavailable {
            return None;
        }
        profiles.push(profile_for_model_state(
            &provider,
            state.protocol.as_deref(),
        )?);
    }
    // Mixed profiles fail closed: require every target to agree exactly.
    if profiles.windows(2).any(|w| w[0] != w[1]) {
        return None;
    }
    profiles.into_iter().next()
}

// ---------------------------------------------------------------------------
// Group building
// ---------------------------------------------------------------------------

/// True for Responses requests that carry a remote side effect and must not be
/// retried automatically (T00 decision 5).
///
/// Only a TRUTHY `background: true` / `store: true` disables retry: a present
/// but false `store: false` is not a remote side effect.  T00's broader "具有
/// 远端副作用的" beyond background/store is not generically detectable from the
/// request body — documented limitation (the known non-idempotent knobs are
/// background and store).
pub fn is_non_idempotent_responses(endpoint: EndpointKind, body: &Value) -> bool {
    if endpoint != EndpointKind::Responses {
        return false;
    }
    body.get("background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || body.get("store").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Whether the channel's legacy config records the Responses→Chat debt
/// (`config.legacy_capabilities=["responses_via_chat_v1"]`, design 11.2).
pub fn has_responses_debt(channel: &Channel) -> bool {
    let config: Value = serde_json::from_str(&channel.config).unwrap_or_default();
    config
        .get("legacy_capabilities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .any(|s| s.as_str() == Some("responses_via_chat_v1"))
        })
        .unwrap_or(false)
}

/// Classify a channel into (tier, upstream protocol, upstream endpoint) for the
/// given downstream endpoint, or `None` if the channel cannot serve it.
fn classify_channel(
    endpoint: EndpointKind,
    id: &ChannelIdentity,
    channel: &Channel,
    flags: &FeatureFlags,
) -> Option<(GroupTier, UpstreamProtocol, String)> {
    let has = |ep: &str| id.native_endpoints.iter().any(|e| e == ep);
    match endpoint {
        EndpointKind::ChatCompletions => {
            if id.protocol == "openai" && has("chat_completions") {
                // Native OpenAI-compatible chat (incl. OpenAI-compat Ollama).
                Some((
                    GroupTier::Native,
                    UpstreamProtocol::OpenAI,
                    "chat_completions".into(),
                ))
            } else if id.protocol == "ollama" && has("api_chat") && flags.ollama_native {
                // Native Ollama `/api/chat` (T06 executor).  OFF until the
                // executor + downstream Chat chain pass their tests.
                Some((
                    GroupTier::Native,
                    UpstreamProtocol::Ollama,
                    "api_chat".into(),
                ))
            } else if id.protocol == "anthropic" && has("messages") && flags.cross_protocol_codec {
                Some((
                    GroupTier::Conversion,
                    UpstreamProtocol::Anthropic,
                    "messages".into(),
                ))
            } else if id.protocol == "openai" && has("responses") && flags.cross_protocol_codec {
                // opencode Chat → OpenAI Responses conversion.  This keeps
                // OpenAI-compatible channels that expose only `/responses`
                // reachable from Chat clients.  A channel that also declares
                // `chat_completions` stays native (first branch), never
                // silently downgraded to the Responses hop.
                Some((
                    GroupTier::Conversion,
                    UpstreamProtocol::OpenAI,
                    "responses".into(),
                ))
            } else {
                None
            }
        }
        EndpointKind::Responses => {
            // A channel carries the Responses→Chat debt when its config records
            // it explicitly OR when it is a legacy-inferred openai/custom row
            // (revision-0 era) that predates the native /responses path.  The
            // latter restores the pre-refactor de facto behavior (design 11.2).
            let debt = has_responses_debt(channel)
                || (id.inferred
                    && id.identity_revision == 0
                    && id.protocol == "openai"
                    && !has("responses"));
            if !debt && id.protocol == "openai" && has("responses") && flags.native_responses {
                // Native /responses passthrough.
                Some((
                    GroupTier::Native,
                    UpstreamProtocol::OpenAI,
                    "responses".into(),
                ))
            } else if id.protocol == "anthropic" && has("messages") && flags.cross_protocol_codec {
                // Codex Responses → Anthropic Messages conversion (design §4.3).
                Some((
                    GroupTier::Conversion,
                    UpstreamProtocol::Anthropic,
                    "messages".into(),
                ))
            } else if (debt
                || (id.protocol == "openai" && has("chat_completions") && !has("responses")))
                && flags.cross_protocol_codec
            {
                // Responses→Chat conversion.  `debt` covers explicit
                // legacy_capabilities and revision-0 inferred rows; the
                // openai+chat_completions-without-native-responses arm restores
                // the pre-refactor de facto behavior for NEW openai/custom
                // channels (DeepSeek, Zhipu, Moonshot, Doubao, …) that only
                // offer chat_completions upstream (product decision 2026-08-11).
                // A channel that natively declares `responses` is never degraded
                // this way (native_responses OFF → 503, not silent downgrade).
                Some((
                    GroupTier::Conversion,
                    UpstreamProtocol::OpenAI,
                    "chat_completions".into(),
                ))
            } else {
                None
            }
        }
        EndpointKind::Messages => {
            if id.protocol == "anthropic" && has("messages") {
                Some((
                    GroupTier::Native,
                    UpstreamProtocol::Anthropic,
                    "messages".into(),
                ))
            } else if id.protocol == "openai"
                && has("chat_completions")
                && flags.cross_protocol_codec
            {
                Some((
                    GroupTier::Conversion,
                    UpstreamProtocol::OpenAI,
                    "chat_completions".into(),
                ))
            } else if id.protocol == "openai" && has("responses") && flags.cross_protocol_codec {
                // Claude Messages → OpenAI Responses conversion.  This keeps
                // OpenAI-compatible channels that expose only `/responses`
                // reachable from Claude clients.
                Some((
                    GroupTier::Conversion,
                    UpstreamProtocol::OpenAI,
                    "responses".into(),
                ))
            } else {
                None
            }
        }
        EndpointKind::CountTokens => {
            if id.protocol == "anthropic" && has("count_tokens") {
                Some((
                    GroupTier::Native,
                    UpstreamProtocol::Anthropic,
                    "count_tokens".into(),
                ))
            } else {
                None
            }
        }
        EndpointKind::Embeddings => {
            if id.protocol == "openai" && has("embeddings") {
                Some((
                    GroupTier::Native,
                    UpstreamProtocol::OpenAI,
                    "embeddings".into(),
                ))
            } else {
                None
            }
        }
    }
}

/// Classify an auth account against its resolved per-model profile for the
/// given downstream endpoint.
///
/// The profile is the single source of the upstream protocol/endpoint/framing
/// (never the rollout capability flags — auth accounts keep their existing
/// conversion groups even when `cross_protocol_codec` is off).  CountTokens and
/// Embeddings have no account adapter in v1, so they never form a group.
fn classify_auth_account(
    endpoint: EndpointKind,
    profile: &AuthRouteProfile,
) -> Option<(GroupTier, UpstreamProtocol, String)> {
    if matches!(
        endpoint,
        EndpointKind::CountTokens | EndpointKind::Embeddings
    ) {
        return None;
    }
    // Native means the downstream endpoint expects this profile's protocol
    // with no conversion: Chat↔OpenAI, Messages↔Anthropic, Responses↔Responses.
    let native = match endpoint {
        EndpointKind::ChatCompletions => profile.upstream_protocol == UpstreamProtocol::OpenAI,
        EndpointKind::Responses => profile.upstream_protocol == UpstreamProtocol::Responses,
        EndpointKind::Messages => profile.upstream_protocol == UpstreamProtocol::Anthropic,
        EndpointKind::CountTokens | EndpointKind::Embeddings => false,
    };
    let tier = if native {
        GroupTier::Native
    } else {
        GroupTier::Conversion
    };
    Some((
        tier,
        profile.upstream_protocol,
        profile.upstream_endpoint.clone(),
    ))
}

/// Build the ordered group plan from the surviving model candidates.
fn build_route_plan<R: Rng + ?Sized>(
    endpoint: EndpointKind,
    model: &str,
    candidates: Vec<RouteCandidate>,
    flags: &FeatureFlags,
    body: &Value,
    rng: &mut R,
) -> Result<RoutePlan, PlanError> {
    let mut routed: Vec<RouteGroupCandidate> = Vec::new();
    let mut config_errors = Vec::new();

    for candidate in candidates {
        match candidate {
            RouteCandidate::Channel { channel, identity } => {
                if identity.native_base_url.is_empty() && identity.native_endpoints.is_empty() {
                    config_errors.push(format!(
                        "channel '{}' ({}): native identity not inferable",
                        channel.name, channel.id
                    ));
                    continue;
                }
                let native_base_url = identity.native_base_url.clone();
                if let Some((tier, proto, ep)) =
                    classify_channel(endpoint, &identity, &channel, flags)
                {
                    routed.push(RouteGroupCandidate {
                        candidate: RouteCandidate::Channel { channel, identity },
                        tier,
                        upstream_protocol: proto,
                        upstream_endpoint: ep,
                        auth_provider: None,
                        native_base_url,
                        auth_non_stream_framing: None,
                    });
                }
            }
            RouteCandidate::AuthAccount(account) => {
                // The per-model wire profile is resolved ONLY from the provider
                // `/models` snapshot.  Unknown provider, unknown Kimi protocol,
                // missing target, or mixed-profile alias fails closed here.
                let Some(profile) = resolve_auth_route_profile(&account, model) else {
                    config_errors.push(format!(
                        "auth account '{}' ({}): model '{}' has no resolvable wire profile",
                        account.label, account.id, model
                    ));
                    continue;
                };
                if let Some((tier, proto, ep)) = classify_auth_account(endpoint, &profile) {
                    routed.push(RouteGroupCandidate {
                        candidate: RouteCandidate::AuthAccount(account),
                        tier,
                        upstream_protocol: proto,
                        upstream_endpoint: ep,
                        auth_provider: Some(profile.provider),
                        native_base_url: profile.native_base_url,
                        auth_non_stream_framing: Some(profile.non_stream_framing),
                    });
                }
            }
        }
    }

    // Responses with remote side effects: no automatic retry (T00 decision 5).
    let non_idempotent = is_non_idempotent_responses(endpoint, body);
    let per_group = if non_idempotent {
        1
    } else {
        DEFAULT_MAX_ATTEMPTS_PER_GROUP
    };

    let groups = build_ordered_route_groups(endpoint, routed, flags, per_group, rng);

    if groups.is_empty() {
        return Err(PlanError::NoEndpointSupported(endpoint, model.to_string()));
    }

    let total = if non_idempotent {
        1
    } else {
        DEFAULT_MAX_ATTEMPTS_TOTAL
    };
    let total_attempts = total
        .min(groups.iter().map(|g| g.max_attempts).sum::<usize>())
        .max(1);

    Ok(RoutePlan {
        endpoint,
        model: model.to_string(),
        groups,
        max_attempts_total: total_attempts,
        flags: *flags,
        config_errors,
        non_idempotent,
    })
}

// ---------------------------------------------------------------------------
// The facade (design 6.0.1 / 11.3): authorize_and_plan
// ---------------------------------------------------------------------------

/// THE single routing facade used by every public endpoint and both stream and
/// non-stream paths.
///
/// Order of operations (design 6.0.1 / 11.3):
/// 1. `authorize_request` — status / expires_at / quota / allowed_models;
/// 2. `allowed_channels` filter (before model matching);
/// 3. model candidates (`models` hit / `model_mapping` source hit / wildcard);
/// 4. protocol grouping (native G1 first, then conversion G2);
/// 5. endpoint capability filtering;
/// 6. in-group priority tier + same-tier weight sampling (no replacement).
///
/// `body` is the gate's forwarded request JSON (used for non-idempotency
/// detection on Responses).  `rng` is injected so tests can seed a deterministic
/// RNG; production passes `&mut rand::rng()`.
pub fn authorize_and_plan<R: Rng + ?Sized>(
    api_key: &ApiKey,
    model: &str,
    endpoint: EndpointKind,
    channels: &[Channel],
    flags: &FeatureFlags,
    body: &Value,
    rng: &mut R,
) -> Result<RoutePlan, PlanError> {
    authorize_and_plan_with_accounts(api_key, model, endpoint, channels, &[], flags, body, rng)
}

/// Mixed-pool variant used by the production auth rollout gate.  The original
/// facade remains as a Channel-only compatibility wrapper for existing callers
/// and tests; both paths share the exact same planning implementation.
pub fn authorize_and_plan_with_accounts<R: Rng + ?Sized>(
    api_key: &ApiKey,
    model: &str,
    endpoint: EndpointKind,
    channels: &[Channel],
    accounts: &[AuthAccount],
    flags: &FeatureFlags,
    body: &Value,
    rng: &mut R,
) -> Result<RoutePlan, PlanError> {
    authorize_request(api_key, model)?;
    if channels.is_empty() && accounts.is_empty() {
        return Err(PlanError::NoChannels);
    }
    let candidates = resolve_route_candidates(channels, accounts, model, api_key);
    if candidates.is_empty() {
        return Err(PlanError::NoCandidateForModel(model.to_string()));
    }
    build_route_plan(endpoint, model, candidates, flags, body, rng)
}

impl RoutePlan {
    /// Sanitized JSON snapshot for logs/reports.  NEVER serializes channel
    /// api_keys or secrets — only id/name/priority/weight.
    pub fn debug_json(&self) -> Value {
        json!({
            "endpoint": self.endpoint.as_str(),
            "model": self.model,
            "max_attempts_total": self.max_attempts_total,
            "non_idempotent": self.non_idempotent,
            "flags": {
                "new_routeplan": self.flags.new_routeplan,
                "cross_protocol_codec": self.flags.cross_protocol_codec,
                "native_responses": self.flags.native_responses,
                "ollama_native": self.flags.ollama_native,
            },
            "config_errors": self.config_errors,
            "groups": self.groups.iter().map(|g| {
                json!({
                    "id": g.id,
                    "tier": g.tier.as_str(),
                    "upstream_protocol": g.upstream_protocol.as_str(),
                    "upstream_endpoint": g.upstream_endpoint,
                    "max_attempts": g.max_attempts,
                    "candidates": g.candidates.iter().map(|c| {
                        json!({
                            "id": c.candidate.id(),
                            "name": c.candidate.name(),
                            "type": c.candidate.upstream_type(),
                            "provider": c.candidate.provider(),
                            "priority": c.candidate.priority(),
                            "weight": c.candidate.weight(),
                        })
                    }).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    /// GAP-08 真值表：重试设置 → 主路径尝试预算（与 legacy 对齐）。
    #[test]
    fn retry_budget_mapping_truth_table() {
        // 默认（开，2 次）→ 3/6，与既有硬编码默认逐字一致（零回归点）。
        assert_eq!(retry_budget_from_settings(true, 2), (3, 6));
        // 关闭 → 1/1（修复：此前主路径无视该设置继续重试）。
        assert_eq!(retry_budget_from_settings(false, 2), (1, 1));
        assert_eq!(retry_budget_from_settings(false, 0), (1, 1));
        // 调大/调小按 次数+1 / ×2 映射。
        assert_eq!(retry_budget_from_settings(true, 5), (6, 12));
        assert_eq!(retry_budget_from_settings(true, 0), (1, 2));
        // 负数（异常输入）clamp 到 0 次 → 1。
        assert_eq!(retry_budget_from_settings(true, -3), (1, 2));
    }

    fn api_key(allowed_models: &[&str], allowed_channels: &[&str]) -> ApiKey {
        ApiKey {
            id: "key-1".into(),
            name: "test".into(),
            key: "sk-test".into(),
            status: 1,
            allowed_models: serde_json::to_string(allowed_models).unwrap(),
            allowed_channels: serde_json::to_string(allowed_channels).unwrap(),
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
        config: &str,
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
            config: config.into(),
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

    /// A channel written by the new dual-write path (identity_revision > 0)
    /// with explicit native endpoints.  Needed because legacy rows only report
    /// native `responses`/`count_tokens` when their legacy debt/resolver says so.
    #[allow(clippy::too_many_arguments)]
    fn new_channel(
        id: &str,
        protocol: &str,
        provider: &str,
        native_base_url: &str,
        native_endpoints: &[&str],
        priority: i64,
        weight: i64,
    ) -> Channel {
        Channel {
            id: id.into(),
            name: format!("ch-{}", id),
            channel_type: if protocol == "anthropic" {
                "claude"
            } else {
                "openai"
            }
            .into(),
            base_url: native_base_url.into(),
            api_key: "sk-test".into(),
            models: json!(["m"]).to_string(),
            status: 1,
            priority,
            weight,
            config: "{}".into(),
            model_mapping: "{}".into(),
            timeout_secs: 30,
            protocol: Some(protocol.into()),
            provider: Some(provider.into()),
            native_base_url: Some(native_base_url.into()),
            native_endpoints: Some(serde_json::to_string(native_endpoints).unwrap()),
            preset_revision: Some("2026-08-04".into()),
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

    fn seeded() -> StdRng {
        StdRng::seed_from_u64(0x5EED)
    }

    fn auth_account(id: &str, model: &str, priority: i64, weight: i64) -> AuthAccount {
        AuthAccount {
            id: id.into(),
            provider: "codex".into(),
            label: format!("account-{id}"),
            account_id: format!("remote-{id}"),
            status: "active".into(),
            disabled: 0,
            priority,
            weight,
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
            payload_json:
                "{\"access_token\":\"route-secret\",\"refresh_token\":\"refresh-secret\"}".into(),
            last_refreshed_at: None,
            last_models_sync_at: None,
            next_refresh_after: None,
            next_retry_after: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn flags(codec_on: bool) -> FeatureFlags {
        FeatureFlags {
            new_routeplan: true,
            cross_protocol_codec: codec_on,
            native_responses: true,
            ollama_native: false,
            prefer_auth_accounts: false,
            prefer_same_protocol: true,
        }
    }

    // --- authorization ---

    #[test]
    fn authorize_empty_allowed_models_is_unrestricted() {
        let key = api_key(&[], &[]);
        assert_eq!(authorize_request(&key, "gpt-4o"), Ok(()));
    }

    #[test]
    fn authorize_rejects_model_outside_allowed() {
        let key = api_key(&["gpt-4o"], &[]);
        assert_eq!(
            authorize_request(&key, "claude-sonnet-4-6"),
            Err(PlanError::ModelNotAllowed("claude-sonnet-4-6".into()))
        );
    }

    #[test]
    fn authorize_accepts_model_in_allowed() {
        let key = api_key(&["gpt-4o"], &[]);
        assert_eq!(authorize_request(&key, "gpt-4o"), Ok(()));
    }

    #[test]
    fn authorize_rejects_disabled_key() {
        let mut key = api_key(&[], &[]);
        key.status = 0;
        assert_eq!(authorize_request(&key, "m"), Err(PlanError::KeyDisabled));
    }

    #[test]
    fn authorize_rejects_quota() {
        let mut key = api_key(&[], &[]);
        key.quota_limit = 100;
        key.quota_used = 100;
        assert_eq!(
            authorize_request(&key, "m"),
            Err(PlanError::QuotaExceeded(100, 100))
        );
    }

    #[test]
    fn authorize_rejects_expired_key() {
        let mut key = api_key(&[], &[]);
        key.expires_at = Some("2000-01-01T00:00:00Z".into());
        assert_eq!(authorize_request(&key, "m"), Err(PlanError::KeyExpired));
    }

    #[test]
    fn authorize_ignores_empty_expiry() {
        let mut key = api_key(&[], &[]);
        key.expires_at = Some("".into());
        assert_eq!(authorize_request(&key, "m"), Ok(()));
    }

    // --- model candidates ---

    #[test]
    fn wildcard_models_and_allowed_channels_filter() {
        let c1 = channel("c1", "openai", "https://api.openai.com/v1", &[], 1, 1, "{}");
        let c2 = channel(
            "c2",
            "openai",
            "https://api.openai.com/v1",
            &["gpt-4o"],
            1,
            1,
            "{}",
        );
        let key = api_key(&[], &["c1"]);
        let all = vec![c1.clone(), c2.clone()];
        let cands = resolve_model_candidates(&all, "gpt-4o", &key);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].id, "c1");
        assert_eq!(cands[0].models, "[]");
    }

    #[test]
    fn model_mapping_source_name_hits() {
        let c1 = channel(
            "c1",
            "openai",
            "https://api.openai.com/v1",
            &["other"],
            1,
            1,
            "{}",
        );
        let mut c1 = c1;
        c1.model_mapping = serde_json::json!({ "alias-x": "gpt-4o" }).to_string();
        let key = api_key(&[], &[]);
        let all = vec![c1];
        let cands = resolve_model_candidates(&all, "alias-x", &key);
        assert_eq!(cands.len(), 1);
    }

    // --- ordering ---

    #[test]
    fn higher_priority_first_but_conversion_never_leapfrogs_native() {
        // Native candidate low priority; conversion candidate high priority.
        let native = channel(
            "n1",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            1,
            1,
            "{}",
        );
        let conv = channel(
            "c1",
            "claude",
            "https://api.anthropic.com/v1",
            &["m"],
            100,
            100,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[native, conv],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 2);
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(plan.groups[1].tier, GroupTier::Conversion);
        // The first attempt must be the native candidate, not the higher-prio
        // conversion one.
        assert_eq!(plan.groups[0].candidates[0].candidate.id(), "n1");
        // Conversion group keeps its own priority ordering internally.
        assert_eq!(plan.groups[1].candidates[0].candidate.id(), "c1");
    }

    #[test]
    fn no_native_candidate_goes_straight_to_conversion() {
        let conv = channel(
            "c1",
            "claude",
            "https://api.anthropic.com/v1",
            &["m"],
            5,
            5,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[conv],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Conversion);
    }

    #[test]
    fn weight_sampling_is_deterministic_with_seed() {
        let channels: Vec<Channel> = (0..4)
            .map(|i| {
                channel(
                    &format!("c{}", i),
                    "openai",
                    "https://api.openai.com/v1",
                    &["m"],
                    10,
                    10,
                    "{}",
                )
            })
            .collect();
        let key = api_key(&[], &[]);
        let plan_a = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &channels,
            &flags(true),
            &json!({}),
            &mut StdRng::seed_from_u64(42),
        )
        .unwrap();
        let plan_b = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &channels,
            &flags(true),
            &json!({}),
            &mut StdRng::seed_from_u64(42),
        )
        .unwrap();
        let ids_a: Vec<&str> = plan_a.groups[0]
            .candidates
            .iter()
            .map(|c| c.candidate.id())
            .collect();
        let ids_b: Vec<&str> = plan_b.groups[0]
            .candidates
            .iter()
            .map(|c| c.candidate.id())
            .collect();
        assert_eq!(ids_a, ids_b);
        // With equal weights every channel appears exactly once.
        assert_eq!(ids_a.len(), 4);
    }

    #[test]
    fn different_seeds_differ_in_weight_order() {
        let channels: Vec<Channel> = (0..4)
            .map(|i| {
                channel(
                    &format!("c{}", i),
                    "openai",
                    "https://api.openai.com/v1",
                    &["m"],
                    10,
                    10,
                    "{}",
                )
            })
            .collect();
        let key = api_key(&[], &[]);
        let plan_a = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &channels,
            &flags(true),
            &json!({}),
            &mut StdRng::seed_from_u64(1),
        )
        .unwrap();
        let plan_b = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &channels,
            &flags(true),
            &json!({}),
            &mut StdRng::seed_from_u64(2),
        )
        .unwrap();
        let ids_a: Vec<&str> = plan_a.groups[0]
            .candidates
            .iter()
            .map(|c| c.candidate.id())
            .collect();
        let ids_b: Vec<&str> = plan_b.groups[0]
            .candidates
            .iter()
            .map(|c| c.candidate.id())
            .collect();
        assert_ne!(
            ids_a, ids_b,
            "two seeds should produce different weight orders"
        );
    }

    #[test]
    fn priority_tiers_respect_priority_desc() {
        let c_hi = channel(
            "hi",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            50,
            1,
            "{}",
        );
        let c_mid = channel(
            "mid",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            30,
            1,
            "{}",
        );
        let c_lo = channel(
            "lo",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            10,
            1,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[c_lo, c_hi, c_mid],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        let ids: Vec<&str> = plan.groups[0]
            .candidates
            .iter()
            .map(|c| c.candidate.id())
            .collect();
        // High priority must come before low priority regardless of weight.
        assert_eq!(ids[0], "hi");
        assert_eq!(ids[1], "mid");
        assert_eq!(ids[2], "lo");
    }

    // --- Responses matrix ---

    #[test]
    fn responses_native_group_gated_by_native_responses_flag() {
        let native = new_channel(
            "n1",
            "openai",
            "openai",
            "https://api.openai.com/v1",
            &["chat_completions", "responses"],
            1,
            1,
        );
        let key = api_key(&[], &[]);
        let on = flags(true);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            std::slice::from_ref(&native),
            &on,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(plan.groups[0].upstream_endpoint, "responses");
        // native_responses OFF → the native Responses group disappears.
        let off = FeatureFlags {
            native_responses: false,
            ..flags(true)
        };
        let err = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[native],
            &off,
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            PlanError::NoEndpointSupported(EndpointKind::Responses, "m".into())
        );
        // F6: Responses unavailability → 503 (design 6.3), not 501.
        assert_eq!(err.http_status(), 503);
    }

    #[test]
    fn legacy_openai_row_gets_responses_debt_at_routing() {
        // A revision-0 openai/custom row (no native identity, no explicit
        // legacy_capabilities flag) must still route /v1/responses through the
        // Responses→Chat debt path (G2), restoring the pre-refactor behavior
        // (design 11.2) instead of being silently dropped.
        let legacy = channel(
            "legacy",
            "openai",
            "https://gw.example.com/v1",
            &["m"],
            1,
            1,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[legacy],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Conversion);
        assert_eq!(plan.groups[0].upstream_endpoint, "chat_completions");
    }

    #[test]
    fn responses_anthropic_channel_goes_to_conversion_messages() {
        // A native anthropic channel (identity declares `messages`) serves a
        // codex Responses request through the V5 Responses→Messages conversion
        // (design §4.3): Conversion tier, Anthropic upstream, "messages" endpoint.
        let anthropic = new_channel(
            "a1",
            "anthropic",
            "anthropic",
            "https://api.anthropic.com/v1",
            &["messages"],
            1,
            1,
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            std::slice::from_ref(&anthropic),
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Conversion);
        assert_eq!(
            plan.groups[0].upstream_protocol,
            UpstreamProtocol::Anthropic
        );
        assert_eq!(plan.groups[0].upstream_endpoint, "messages");

        // cross_protocol_codec OFF → the anthropic conversion group disappears
        // and the Responses request fails with the 503 NoEndpointSupported.
        let err = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[anthropic],
            &flags(false),
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            PlanError::NoEndpointSupported(EndpointKind::Responses, "m".into())
        );
        assert_eq!(err.http_status(), 503);
    }

    #[test]
    fn responses_debt_channel_goes_to_g2_not_g1() {
        let debt = channel(
            "legacy",
            "openai",
            "https://gw.example.com/v1",
            &["m"],
            1,
            1,
            r#"{"legacy_capabilities":["responses_via_chat_v1"]}"#,
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[debt],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Conversion);
        assert_eq!(plan.groups[0].upstream_endpoint, "chat_completions");
    }

    #[test]
    fn non_idempotent_responses_disable_retries() {
        let n1 = new_channel(
            "n1",
            "openai",
            "openai",
            "https://api.openai.com/v1",
            &["chat_completions", "responses"],
            1,
            1,
        );
        let n2 = new_channel(
            "n2",
            "openai",
            "openai",
            "https://api.openai.com/v1",
            &["chat_completions", "responses"],
            1,
            1,
        );
        let key = api_key(&[], &[]);
        // background=true → single attempt even though two candidates exist.
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[n1.clone(), n2.clone()],
            &flags(true),
            &json!({ "background": true }),
            &mut seeded(),
        )
        .unwrap();
        assert!(plan.non_idempotent);
        assert_eq!(plan.max_attempts_total, 1);
        assert_eq!(plan.groups[0].max_attempts, 1);

        // No side effect → retries allowed (2 candidates → 2 attempts).
        let plan2 = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[n1.clone(), n2.clone()],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert!(!plan2.non_idempotent);
        assert_eq!(plan2.max_attempts_total, 2);

        // F5: `store: false` is NOT a remote side effect → retries stay on.
        let plan3 = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[n1.clone(), n2.clone()],
            &flags(true),
            &json!({ "store": false }),
            &mut seeded(),
        )
        .unwrap();
        assert!(!plan3.non_idempotent, "store:false must stay retryable");
        assert_eq!(plan3.max_attempts_total, 2);

        // `store: true` IS a side effect → retries disabled.
        let plan4 = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &[n1, n2],
            &flags(true),
            &json!({ "store": true }),
            &mut seeded(),
        )
        .unwrap();
        assert!(plan4.non_idempotent);
        assert_eq!(plan4.max_attempts_total, 1);
    }

    #[test]
    fn endpoint_unavailable_status_is_503_for_chat_and_501_for_count_tokens() {
        // F6 (leader-ratified, design 6.3): no channel supporting the endpoint
        // → 503 for Chat/Responses/Messages; 501 only for CountTokens.
        let key = api_key(&[], &[]);
        // Chat with only an Anthropic channel, codec OFF → no group at all.
        let ant = channel(
            "a1",
            "claude",
            "https://api.anthropic.com/v1",
            &["m"],
            1,
            1,
            "{}",
        );
        let off_codec = FeatureFlags {
            cross_protocol_codec: false,
            native_responses: true,
            ..FeatureFlags::all_on()
        };
        let err = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[ant],
            &off_codec,
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PlanError::NoEndpointSupported(EndpointKind::ChatCompletions, _)
        ));
        assert_eq!(err.http_status(), 503);

        // CountTokens with no anthropic count_tokens channel → 501.
        let oai = channel(
            "o1",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            1,
            1,
            "{}",
        );
        let err = authorize_and_plan(
            &key,
            "m",
            EndpointKind::CountTokens,
            &[oai],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PlanError::NoEndpointSupported(EndpointKind::CountTokens, _)
        ));
        assert_eq!(err.http_status(), 501);
    }

    // --- Ollama native (T06) ---

    #[test]
    fn ollama_native_chat_group_is_gated_by_flag() {
        let ollama = new_channel(
            "o1",
            "ollama",
            "ollama",
            "http://localhost:11434",
            &["api_chat"],
            1,
            1,
        );
        let key = api_key(&[], &[]);
        // Flag OFF → no candidate (deferred until executor+codec tests pass).
        let off = FeatureFlags {
            ollama_native: false,
            ..flags(true)
        };
        let err = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            std::slice::from_ref(&ollama),
            &off,
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PlanError::NoEndpointSupported(EndpointKind::ChatCompletions, _)
        ));

        // Flag ON → native Ollama `/api/chat` group (G1, same tier as OpenAI).
        let on = FeatureFlags {
            ollama_native: true,
            ..flags(true)
        };
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[ollama],
            &on,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(plan.groups[0].upstream_protocol, UpstreamProtocol::Ollama);
        assert_eq!(plan.groups[0].upstream_endpoint, "api_chat");
    }

    #[test]
    fn ollama_native_does_not_serve_count_tokens() {
        // Ollama `/api/chat` must never satisfy CountTokens (no codec path).
        let ollama = new_channel(
            "o1",
            "ollama",
            "ollama",
            "http://localhost:11434",
            &["api_chat"],
            1,
            1,
        );
        let key = api_key(&[], &[]);
        let on = FeatureFlags {
            ollama_native: true,
            ..flags(true)
        };
        let err = authorize_and_plan(
            &key,
            "m",
            EndpointKind::CountTokens,
            &[ollama],
            &on,
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PlanError::NoEndpointSupported(EndpointKind::CountTokens, _)
        ));
        assert_eq!(err.http_status(), 501);
    }

    // --- Messages / CountTokens / Embeddings ---

    #[test]
    fn messages_native_anthropic_then_openai_conversion() {
        let ant = channel(
            "a1",
            "claude",
            "https://api.anthropic.com/v1",
            &["m"],
            1,
            1,
            "{}",
        );
        let oai = channel(
            "o1",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            100,
            100,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Messages,
            &[ant, oai],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 2);
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(plan.groups[0].candidates[0].candidate.id(), "a1");
        assert_eq!(plan.groups[1].tier, GroupTier::Conversion);
        assert_eq!(plan.groups[1].candidates[0].candidate.id(), "o1");
    }

    #[test]
    fn count_tokens_only_anthropic() {
        let ant = new_channel(
            "a1",
            "anthropic",
            "anthropic",
            "https://api.anthropic.com",
            &["messages", "count_tokens"],
            1,
            1,
        );
        let oai = channel(
            "o1",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            100,
            100,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::CountTokens,
            &[ant, oai],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(plan.groups[0].candidates[0].candidate.id(), "a1");
    }

    #[test]
    fn config_error_channel_is_dropped_and_reported() {
        // A gemini row with an empty base_url yields an identity with neither a
        // base URL nor endpoints — it must be dropped and reported, never routed.
        let bad = channel("bad", "gemini", "", &["m"], 1, 1, "{}");
        let good = channel(
            "good",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            1,
            1,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[bad, good],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups[0].candidates.len(), 1);
        assert_eq!(plan.groups[0].candidates[0].candidate.id(), "good");
        assert!(!plan.config_errors.is_empty());
    }

    #[test]
    fn upstream_model_string_and_array_sampling() {
        let mapping = json!({
            "alias": "mapped-single",
            "alias-arr": ["a", "b", "c"]
        });
        let mut rng = seeded();
        assert_eq!(
            resolve_upstream_model(&mapping, "alias", &mut rng),
            "mapped-single"
        );
        let v = resolve_upstream_model(&mapping, "alias-arr", &mut rng);
        assert!(["a", "b", "c"].contains(&v.as_str()));
        // Deterministic for the same seed: two freshly-seeded RNGs produce the
        // same first sample.
        let mut rng_a = StdRng::seed_from_u64(42);
        let mut rng_b = StdRng::seed_from_u64(42);
        assert_eq!(
            resolve_upstream_model(&mapping, "alias-arr", &mut rng_a),
            resolve_upstream_model(&mapping, "alias-arr", &mut rng_b)
        );
        // No mapping → requested model.
        assert_eq!(
            resolve_upstream_model(&mapping, "unknown", &mut rng),
            "unknown"
        );
    }

    #[test]
    fn debug_json_never_leaks_api_key() {
        let native = channel(
            "n1",
            "openai",
            "https://api.openai.com/v1",
            &["m"],
            1,
            1,
            "{}",
        );
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[native],
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        let s = serde_json::to_string(&plan.debug_json()).unwrap();
        assert!(
            !s.contains("sk-test"),
            "api_key must never leak into plan debug output"
        );
        assert!(s.contains("chat_completions_g1_native"));
    }

    #[test]
    fn auth_accounts_share_the_responses_native_pool_and_keep_independent_weighted_candidates() {
        let key = api_key(&[], &["only-channel"]);
        let channel = new_channel(
            "only-channel",
            "openai",
            "openai",
            "https://api.openai.com/v1",
            &["responses"],
            5,
            1,
        );
        let a1 = auth_account("auth-1", "m", 10, 1);
        let a2 = auth_account("auth-2", "m", 10, 9);
        let plan = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Responses,
            &[channel],
            &[a1, a2],
            &flags(false),
            &json!({}),
            &mut StdRng::seed_from_u64(9),
        )
        .unwrap();
        let candidates = &plan.groups[0].candidates;
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(candidates.len(), 3);
        assert!(matches!(
            &candidates[0].candidate,
            RouteCandidate::AuthAccount(_)
        ));
        assert!(candidates.iter().any(|c| c.candidate.id() == "auth-1"));
        assert!(candidates.iter().any(|c| c.candidate.id() == "auth-2"));
        assert!(candidates
            .iter()
            .any(|c| c.candidate.id() == "only-channel"));
        assert!(candidates
            .iter()
            .all(|c| c.candidate.id() != "remote-auth-1"));
    }

    #[test]
    fn prefer_auth_accounts_orders_auth_before_higher_priority_channels() {
        let key = api_key(&[], &[]);
        let channel = new_channel(
            "high-priority-channel",
            "openai",
            "custom",
            "https://example.test/v1",
            &["chat_completions"],
            1000,
            1,
        );
        let account = auth_account("auth-low-priority", "m", 0, 1);
        let mut normal_flags = flags(true);
        normal_flags.prefer_auth_accounts = false;
        let normal = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Messages,
            std::slice::from_ref(&channel),
            std::slice::from_ref(&account),
            &normal_flags,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(
            normal.groups[0].candidates[0].candidate.id(),
            "high-priority-channel"
        );

        let mut prefer_auth = normal_flags;
        prefer_auth.prefer_auth_accounts = true;
        let preferred = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Messages,
            &[channel],
            &[account],
            &prefer_auth,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(
            preferred.groups[0].candidates[0].candidate.id(),
            "auth-low-priority"
        );
    }

    #[test]
    fn prefer_auth_and_same_protocol_prioritizes_same_protocol_auth_first() {
        let key = api_key(&[], &[]);
        let same_protocol_channel = new_channel(
            "same-protocol-channel",
            "openai",
            "openai",
            "https://api.openai.com/v1",
            &["responses"],
            1000,
            1,
        );
        let account = auth_account("same-protocol-auth", "m", 0, 1);
        let mut flags = flags(true);
        flags.prefer_auth_accounts = true;
        flags.prefer_same_protocol = true;

        let plan = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Responses,
            &[same_protocol_channel],
            &[account],
            &flags,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();

        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(
            plan.groups[0].candidates[0].candidate.id(),
            "same-protocol-auth"
        );
    }

    #[test]
    fn auth_accounts_filter_models_lifecycle_and_quota_but_allow_expired_recovery() {
        let key = api_key(&[], &["unrelated-channel"]);
        let available = auth_account("available", "m", 1, 1);
        let mut empty_models = auth_account("empty", "m", 1, 1);
        empty_models.model_states_json = json!({"version": 1, "models": []}).to_string();
        let mut disabled = auth_account("disabled", "m", 1, 1);
        disabled.disabled = 1;
        let mut invalid = auth_account("invalid", "m", 1, 1);
        invalid.status = "invalid".into();
        let mut future_quota = auth_account("future", "m", 1, 1);
        future_quota.quota_json = Some(
            json!({
                "version": 1, "exceeded": true, "reason": "quota",
                "next_recover_at": "2999-01-01T00:00:00Z", "backoff_level": 0, "limits": []
            })
            .to_string(),
        );
        let mut recovered_quota = auth_account("recovered", "m", 1, 1);
        recovered_quota.quota_json = Some(
            json!({
                "version": 1, "exceeded": true, "reason": "quota",
                "next_recover_at": "2000-01-01T00:00:00Z", "backoff_level": 0, "limits": []
            })
            .to_string(),
        );
        let candidates = resolve_route_candidates(
            &[],
            &[
                available,
                empty_models,
                disabled,
                invalid,
                future_quota,
                recovered_quota,
            ],
            "m",
            &key,
        );
        let ids = candidates
            .iter()
            .map(RouteCandidate::id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["available", "recovered"]);
    }

    #[test]
    fn auth_classification_ignores_channel_flags_and_never_serves_unsupported_endpoints() {
        let account = auth_account("auth", "m", 1, 1);
        let key = api_key(&[], &[]);
        let flags_off = FeatureFlags {
            new_routeplan: false,
            cross_protocol_codec: false,
            native_responses: false,
            ollama_native: false,
            prefer_auth_accounts: false,
            prefer_same_protocol: true,
        };
        let responses = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Responses,
            &[],
            &[account.clone()],
            &flags_off,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(responses.groups[0].tier, GroupTier::Native);
        assert_eq!(
            responses.groups[0].upstream_protocol,
            UpstreamProtocol::Responses
        );
        let chat = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[],
            &[account.clone()],
            &flags_off,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(chat.groups[0].tier, GroupTier::Conversion);
        let messages = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Messages,
            &[],
            &[account.clone()],
            &flags_off,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(messages.groups[0].tier, GroupTier::Conversion);
        assert!(matches!(
            authorize_and_plan_with_accounts(
                &key,
                "m",
                EndpointKind::CountTokens,
                &[],
                &[account],
                &flags_off,
                &json!({}),
                &mut seeded(),
            ),
            Err(PlanError::NoEndpointSupported(EndpointKind::CountTokens, _))
        ));
    }

    #[test]
    fn auth_debug_snapshot_is_metadata_only() {
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Responses,
            &[],
            &[auth_account("auth", "m", 1, 1)],
            &flags(false),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        let snapshot = plan.debug_json().to_string();
        assert!(snapshot.contains("auth_account"));
        assert!(snapshot.contains("codex"));
        assert!(!snapshot.contains("route-secret"));
        assert!(!snapshot.contains("refresh-secret"));
        assert!(!snapshot.contains("payload_json"));
    }

    /// Repro: a newly-created preset OpenAI/DeepSeek channel (identity_revision
    /// 1, native chat_completions only, no responses_via_chat debt) must not 503
    /// on a codex Responses request — the upstream can serve it via the
    /// Responses→Chat codec (design 11.2: "新建渠道是否允许该降级由产品预设明确").
    #[test]
    fn new_preset_openai_chat_only_channel_serves_responses_via_chat_debt() {
        // Mirrors the DeepSeek preset: protocol=openai, native [chat_completions].
        let mut deepseek = new_channel(
            "deepseek",
            "openai",
            "deepseek",
            "https://api.deepseek.com",
            &["chat_completions"],
            1,
            1,
        );
        deepseek.models = json!(["deepseek-v4-flash"]).to_string();
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "deepseek-v4-flash",
            EndpointKind::Responses,
            std::slice::from_ref(&deepseek),
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Conversion);
        assert_eq!(plan.groups[0].upstream_endpoint, "chat_completions");
    }

    /// A Claude Messages request must be able to use an OpenAI-compatible
    /// channel that declares only a native `/responses` endpoint.  The codec
    /// matrix already implements Messages→Responses; this guards the planner
    /// branch that makes that conversion reachable.
    #[test]
    fn messages_request_serves_openai_responses_only_channel() {
        let mut deepseek = new_channel(
            "deepseek",
            "openai",
            "deepseek",
            "https://api.deepseek.com",
            &["responses"],
            1,
            1,
        );
        deepseek.models = json!(["deepseek-v4-flash"]).to_string();
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "deepseek-v4-flash",
            EndpointKind::Messages,
            std::slice::from_ref(&deepseek),
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .expect("messages request should route through the Messages→Responses codec");

        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Conversion);
        assert_eq!(plan.groups[0].upstream_protocol, UpstreamProtocol::OpenAI);
        assert_eq!(plan.groups[0].upstream_endpoint, "responses");
    }

    /// An opencode Chat request must be able to use an OpenAI-compatible
    /// channel that declares only a native `/responses` endpoint.  The codec
    /// matrix already implements Chat→Responses; this guards the planner
    /// branch that makes that conversion reachable.
    #[test]
    fn chat_request_serves_openai_responses_only_channel() {
        let mut deepseek = new_channel(
            "deepseek",
            "openai",
            "deepseek",
            "https://api.deepseek.com",
            &["responses"],
            1,
            1,
        );
        deepseek.models = json!(["deepseek-v4-flash"]).to_string();
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "deepseek-v4-flash",
            EndpointKind::ChatCompletions,
            std::slice::from_ref(&deepseek),
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .expect("chat request should route through the Chat→Responses codec");

        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Conversion);
        assert_eq!(plan.groups[0].upstream_protocol, UpstreamProtocol::OpenAI);
        assert_eq!(plan.groups[0].upstream_endpoint, "responses");
    }

    /// A channel declaring both `chat_completions` and `responses` must stay
    /// native for a Chat request; the Responses hop is only a conversion
    /// fallback, never a silent downgrade.
    #[test]
    fn chat_request_prefers_native_chat_over_responses_conversion() {
        let mut deepseek = new_channel(
            "deepseek",
            "openai",
            "deepseek",
            "https://api.deepseek.com",
            &["chat_completions", "responses"],
            1,
            1,
        );
        deepseek.models = json!(["deepseek-v4-flash"]).to_string();
        let key = api_key(&[], &[]);
        let plan = authorize_and_plan(
            &key,
            "deepseek-v4-flash",
            EndpointKind::ChatCompletions,
            std::slice::from_ref(&deepseek),
            &flags(true),
            &json!({}),
            &mut seeded(),
        )
        .expect("chat request should stay native on a dual-endpoint channel");

        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier, GroupTier::Native);
        assert_eq!(plan.groups[0].upstream_protocol, UpstreamProtocol::OpenAI);
        assert_eq!(plan.groups[0].upstream_endpoint, "chat_completions");
    }

    #[test]
    fn auth_account_model_mapping_source_name_hits() {
        let key = api_key(&[], &[]);
        let mut account = auth_account("auth-1", "gpt-4o", 1, 1);
        account.model_mapping_json = json!({"auto": "gpt-4o"}).to_string();
        let candidates = resolve_route_candidates(&[], &[account], "auto", &key);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id(), "auth-1");
    }

    #[test]
    fn auth_account_model_mapping_does_not_match_unknown_alias() {
        let key = api_key(&[], &[]);
        let mut account = auth_account("auth-1", "gpt-4o", 1, 1);
        account.model_mapping_json = json!({"auto": "gpt-4o"}).to_string();
        let candidates = resolve_route_candidates(&[], &[account], "unknown-alias", &key);
        assert_eq!(candidates.len(), 0);
    }

    // --- C5: per-model auth route profiles ---

    fn kimi_account(id: &str, model: &str, protocol: &str) -> AuthAccount {
        let mut account = auth_account(id, model, 1, 1);
        account.provider = "kimi".into();
        account.model_states_json = json!({
            "version": 1,
            "models": [{
                "id": model,
                "status": "available",
                "unavailable": false,
                "next_retry_after": null,
                "last_error": null,
                "protocol": protocol
            }]
        })
        .to_string();
        account
    }

    fn account_with_models(id: &str, provider: &str, models: Value, mapping: Value) -> AuthAccount {
        let mut account = auth_account(id, "unused", 1, 1);
        account.provider = provider.into();
        account.model_states_json = models.to_string();
        account.model_mapping_json = mapping.to_string();
        account
    }

    #[test]
    fn kimi_missing_protocol_resolves_chat_profile() {
        let account = kimi_account("k1", "kimi-k2.5", "");
        let profile = resolve_auth_route_profile(&account, "kimi-k2.5").unwrap();
        assert_eq!(profile.provider, "kimi");
        assert_eq!(profile.native_base_url, "https://api.kimi.com/coding/v1");
        assert_eq!(profile.upstream_protocol, UpstreamProtocol::OpenAI);
        assert_eq!(profile.upstream_endpoint, "chat_completions");
        assert_eq!(profile.non_stream_framing, AuthNonStreamFraming::Json);
    }

    #[test]
    fn kimi_kimi_protocol_is_chat_native_for_chat_endpoint() {
        let key = api_key(&[], &[]);
        let account = kimi_account("k1", "kimi-k2.5", "kimi");
        let plan = authorize_and_plan_with_accounts(
            &key,
            "kimi-k2.5",
            EndpointKind::ChatCompletions,
            &[],
            &[account],
            &flags(false),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        let group = &plan.groups[0];
        assert_eq!(group.tier, GroupTier::Native);
        assert_eq!(group.upstream_protocol, UpstreamProtocol::OpenAI);
        assert_eq!(group.upstream_endpoint, "chat_completions");
        assert_eq!(
            group.candidates[0].auth_non_stream_framing,
            Some(AuthNonStreamFraming::Json)
        );
        assert_eq!(group.candidates[0].auth_provider.as_deref(), Some("kimi"));
        assert_eq!(
            group.candidates[0].native_base_url,
            "https://api.kimi.com/coding/v1"
        );
    }

    #[test]
    fn kimi_chat_profile_converts_responses_and_messages_to_chat() {
        let key = api_key(&[], &[]);
        for endpoint in [EndpointKind::Responses, EndpointKind::Messages] {
            let account = kimi_account("k1", "kimi-k2.5", "kimi");
            let plan = authorize_and_plan_with_accounts(
                &key,
                "kimi-k2.5",
                endpoint,
                &[],
                &[account],
                &flags(false),
                &json!({}),
                &mut seeded(),
            )
            .unwrap();
            let group = &plan.groups[0];
            assert_eq!(group.tier, GroupTier::Conversion);
            assert_eq!(group.upstream_protocol, UpstreamProtocol::OpenAI);
            assert_eq!(group.upstream_endpoint, "chat_completions");
        }
    }

    #[test]
    fn kimi_anthropic_profile_is_messages_beta_native() {
        let key = api_key(&[], &[]);
        let account = kimi_account("k2", "kimi-anthropic", "anthropic");
        let plan = authorize_and_plan_with_accounts(
            &key,
            "kimi-anthropic",
            EndpointKind::Messages,
            &[],
            &[account],
            &flags(false),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        let group = &plan.groups[0];
        assert_eq!(group.tier, GroupTier::Native);
        assert_eq!(group.upstream_protocol, UpstreamProtocol::Anthropic);
        assert_eq!(group.upstream_endpoint, "messages_beta");
        assert_eq!(
            group.candidates[0].auth_non_stream_framing,
            Some(AuthNonStreamFraming::Json)
        );
        assert_eq!(
            group.candidates[0].native_base_url,
            "https://api.kimi.com/coding"
        );
    }

    #[test]
    fn kimi_anthropic_profile_converts_chat_and_responses_to_messages_beta() {
        let key = api_key(&[], &[]);
        for endpoint in [EndpointKind::ChatCompletions, EndpointKind::Responses] {
            let account = kimi_account("k2", "kimi-anthropic", "anthropic");
            let plan = authorize_and_plan_with_accounts(
                &key,
                "kimi-anthropic",
                endpoint,
                &[],
                &[account],
                &flags(false),
                &json!({}),
                &mut seeded(),
            )
            .unwrap();
            let group = &plan.groups[0];
            assert_eq!(group.tier, GroupTier::Conversion);
            assert_eq!(group.upstream_protocol, UpstreamProtocol::Anthropic);
            assert_eq!(group.upstream_endpoint, "messages_beta");
        }
    }

    #[test]
    fn kimi_unknown_protocol_fails_closed_no_candidate() {
        let key = api_key(&[], &[]);
        let account = kimi_account("k3", "kimi-mars", "mars");
        let plan = authorize_and_plan_with_accounts(
            &key,
            "kimi-mars",
            EndpointKind::ChatCompletions,
            &[],
            &[account],
            &flags(false),
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert_eq!(
            plan,
            PlanError::NoEndpointSupported(EndpointKind::ChatCompletions, "kimi-mars".into())
        );
    }

    #[test]
    fn kimi_unavailable_model_never_routes() {
        let key = api_key(&[], &[]);
        let mut account = kimi_account("k4", "kimi-k2.5", "kimi");
        account.model_states_json = json!({
            "version": 1,
            "models": [{
                "id": "kimi-k2.5",
                "status": "unavailable",
                "unavailable": true,
                "next_retry_after": null,
                "last_error": "unsupported wire protocol",
                "protocol": "mars"
            }]
        })
        .to_string();
        let candidates = resolve_route_candidates(&[], &[account], "kimi-k2.5", &key);
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn kimi_alias_all_same_profile_routes() {
        let account = account_with_models(
            "k5",
            "kimi",
            json!({
                "version": 1,
                "models": [
                    {"id": "kimi-k2.5", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null, "protocol": "kimi"},
                    {"id": "kimi-k2.5-alt", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null, "protocol": "kimi"}
                ]
            }),
            json!({"auto": ["kimi-k2.5", "kimi-k2.5-alt"]}),
        );
        let profile = resolve_auth_route_profile(&account, "auto").unwrap();
        assert_eq!(profile.upstream_protocol, UpstreamProtocol::OpenAI);
        assert_eq!(profile.upstream_endpoint, "chat_completions");
    }

    #[test]
    fn kimi_alias_mixed_profiles_fail_closed() {
        let key = api_key(&[], &[]);
        let account = account_with_models(
            "k6",
            "kimi",
            json!({
                "version": 1,
                "models": [
                    {"id": "kimi-k2.5", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null, "protocol": "kimi"},
                    {"id": "kimi-anthropic", "status": "available", "unavailable": false,
                     "next_retry_after": null, "last_error": null, "protocol": "anthropic"}
                ]
            }),
            json!({"auto": ["kimi-k2.5", "kimi-anthropic"]}),
        );
        // Mixed profile fails closed in the profile resolution itself.
        assert!(resolve_auth_route_profile(&account, "auto").is_none());
        // And in the planner: no group is formed, no RNG flip changes protocol.
        let err = authorize_and_plan_with_accounts(
            &key,
            "auto",
            EndpointKind::ChatCompletions,
            &[],
            &[account],
            &flags(false),
            &json!({}),
            &mut seeded(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            PlanError::NoEndpointSupported(EndpointKind::ChatCompletions, "auto".into())
        );
    }

    #[test]
    fn kimi_count_tokens_and_embeddings_never_form_auth_group() {
        let key = api_key(&[], &[]);
        for endpoint in [EndpointKind::CountTokens, EndpointKind::Embeddings] {
            let account = kimi_account("k7", "kimi-k2.5", "kimi");
            let err = authorize_and_plan_with_accounts(
                &key,
                "kimi-k2.5",
                endpoint,
                &[],
                &[account],
                &flags(false),
                &json!({}),
                &mut seeded(),
            )
            .unwrap_err();
            assert!(matches!(
                err,
                PlanError::NoEndpointSupported(e, _) if e == endpoint
            ));
        }
    }

    #[test]
    fn codex_profile_still_responses_with_forced_sse_framing() {
        let key = api_key(&[], &[]);
        let account = auth_account("codex-1", "m", 1, 1);
        let plan = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::Responses,
            &[],
            &[account],
            &flags(false),
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        let group = &plan.groups[0];
        assert_eq!(group.tier, GroupTier::Native);
        assert_eq!(group.upstream_protocol, UpstreamProtocol::Responses);
        assert_eq!(
            group.candidates[0].auth_non_stream_framing,
            Some(AuthNonStreamFraming::ForcedResponsesSse)
        );
        assert_eq!(group.candidates[0].auth_provider.as_deref(), Some("codex"));
        assert_eq!(
            group.candidates[0].native_base_url,
            "https://chatgpt.com/backend-api/codex"
        );
    }

    #[test]
    fn prefer_same_protocol_picks_kimi_chat_over_codex_for_chat() {
        let key = api_key(&[], &[]);
        let codex = auth_account("codex", "m", 0, 1);
        let kimi = kimi_account("kimi", "m", "kimi");
        let mut f = flags(false);
        f.prefer_same_protocol = true;
        let plan = authorize_and_plan_with_accounts(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &[],
            &[codex, kimi],
            &f,
            &json!({}),
            &mut seeded(),
        )
        .unwrap();
        // Same-protocol (OpenAI chat) Kimi group comes first.
        let first = &plan.groups[0];
        assert_eq!(first.tier, GroupTier::Native);
        assert_eq!(first.upstream_protocol, UpstreamProtocol::OpenAI);
        assert!(first.candidates.iter().any(|c| c.candidate.id() == "kimi"));
    }
}
