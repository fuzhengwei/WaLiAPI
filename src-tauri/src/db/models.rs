use chrono::Utc;
use serde::{Deserialize, Serialize};

/// A single API key belonging to a channel (migration 023: channel_api_keys).
/// Multiple keys per channel enable load balancing and failover.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChannelApiKey {
    pub id: String,
    pub channel_id: String,
    pub api_key: String,
    pub weight: i64,
    pub status: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Input for creating/updating a channel API key entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelApiKeyInput {
    /// 已存在从 Key 的数据库 ID（编辑保存时由前端回传）。列表 DTO 的 api_key
    /// 是掩码值，更新路径据此把「值仍为该 ID 存量掩码」的提交识别为未修改，
    /// 用库中真实值替换，避免掩码串覆盖真实 Key（仅更新路径消费此字段）。
    #[serde(default)]
    pub id: Option<String>,
    pub api_key: String,
    #[serde(default)]
    pub weight: Option<i64>,
    #[serde(default)]
    pub status: Option<i64>,
}

/// 渠道级自定义上游请求头。请求头默认启用，也可以单独禁用。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelRequestHeaderInput {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub status: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub channel_type: String,
    pub base_url: String,
    pub api_key: String,
    pub models: String,
    pub status: i64,
    pub priority: i64,
    pub weight: i64,
    pub config: String,
    pub model_mapping: String,
    /// 被关闭的映射对（迁移 041）：JSON 数组，元素为 [from, to]。
    /// 空数组 = 全部映射开启。
    #[sqlx(default)]
    pub model_mapping_disabled: String,
    pub timeout_secs: i64,
    // --- T02 protocol identity columns (migration 015) ---
    pub protocol: Option<String>,
    pub provider: Option<String>,
    pub native_base_url: Option<String>,
    pub native_endpoints: Option<String>,
    pub preset_revision: Option<String>,
    pub identity_revision: i64,
    pub legacy_executor_override: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<i64>,
    /// 主动健康探测（迁移 033）：NULL = 从未探测（排序视为健康）。
    /// default：部分集成测试只迁移到 015，行中无这些列时按「未探测」处理。
    #[sqlx(default)]
    pub last_probe_at: Option<String>,
    #[sqlx(default)]
    pub last_probe_ok: Option<i64>,
    #[sqlx(default)]
    pub probe_latency_ms: Option<i64>,
    /// 主 Key 是否参与负载均衡调度（迁移 044）。Some(0) = 停用；
    /// None/Some(1) = 启用。None 兼容只迁移到早期版本的集成测试行。
    #[sqlx(default)]
    pub api_key_enabled: Option<i64>,
}

impl Channel {
    /// 主 Key（channels.api_key）是否参与调度（迁移 044 列 api_key_enabled）。
    /// None（早期迁移的测试行/历史数据）视为启用，保持向后兼容。
    pub fn primary_key_enabled(&self) -> bool {
        self.api_key_enabled.unwrap_or(1) != 0
    }

    /// 返回规范化但尚未应用禁用列表的原始模型映射，供管理面与导出使用。
    pub fn normalized_model_mapping(&self) -> serde_json::Value {
        let mut mapping: serde_json::Value =
            serde_json::from_str(&self.model_mapping).unwrap_or_default();
        normalize_model_mapping(&mut mapping);
        mapping
    }

    /// 返回规范化后的禁用映射对，供管理面与导出使用。
    pub fn normalized_model_mapping_disabled(&self) -> serde_json::Value {
        let mut disabled =
            serde_json::from_str::<serde_json::Value>(if self.model_mapping_disabled.is_empty() {
                "[]"
            } else {
                &self.model_mapping_disabled
            })
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
        normalize_model_mapping_disabled(&mut disabled);
        disabled
    }

    /// 解析 `model_mapping` 并剔除被关闭的映射对（迁移 041）。
    /// 返回「当前生效」的映射 JSON；路由匹配、上游模型解析、
    /// `/v1/models` 聚合都必须使用本方法而非直接读原始列。
    pub fn active_model_mapping(&self) -> serde_json::Value {
        let mapping = self.normalized_model_mapping();
        let disabled = self.normalized_model_mapping_disabled();
        filter_disabled_mapping(
            mapping,
            &serde_json::to_string(&disabled).unwrap_or_else(|_| "[]".to_owned()),
        )
    }
}

/// 规范化模型映射 JSON 中的模型名。
///
/// * 去除 key 与字符串 value 两端空格；
/// * 删除空 key 与去空格后为空的 value；
/// * 数组 value 逐项处理，只剩一项时折叠为字符串。
pub fn normalize_model_mapping(mapping: &mut serde_json::Value) {
    let Some(obj) = mapping.as_object_mut() else {
        return;
    };
    let keys: Vec<String> = obj.keys().cloned().collect();
    let mut trimmed_entries: Vec<(String, serde_json::Value)> = Vec::new();
    for key in &keys {
        let trimmed_key = key.trim().to_string();
        if trimmed_key.is_empty() {
            obj.remove(key);
            continue;
        }
        if let Some(value) = obj.remove(key) {
            let new_value = match value {
                serde_json::Value::String(s) => {
                    let ts = s.trim().to_string();
                    if ts.is_empty() {
                        continue;
                    }
                    serde_json::Value::String(ts)
                }
                serde_json::Value::Array(arr) => {
                    let trimmed: Vec<serde_json::Value> = arr
                        .into_iter()
                        .filter_map(|v| {
                            v.as_str()
                                .map(|s| {
                                    let ts = s.trim().to_string();
                                    if ts.is_empty() {
                                        None
                                    } else {
                                        Some(serde_json::Value::String(ts))
                                    }
                                })
                                .unwrap_or(Some(v))
                        })
                        .collect();
                    match trimmed.len() {
                        0 => continue,
                        1 => trimmed.into_iter().next().unwrap(),
                        _ => serde_json::Value::Array(trimmed),
                    }
                }
                other => other,
            };
            trimmed_entries.push((trimmed_key, new_value));
        }
    }
    for (k, v) in trimmed_entries {
        obj.insert(k, v);
    }
}

/// 去除禁用映射对两端空格，并删除空映射对。
pub fn normalize_model_mapping_disabled(mapping: &mut serde_json::Value) {
    let Some(pairs) = mapping.as_array_mut() else {
        return;
    };
    pairs.retain_mut(|pair| {
        let Some(pair) = pair.as_array_mut() else {
            return true;
        };
        for value in pair.iter_mut() {
            if let Some(name) = value.as_str() {
                *value = serde_json::Value::String(name.trim().to_owned());
            }
        }
        pair.len() == 2
            && pair.iter().all(|value| {
                value
                    .as_str()
                    .map(|name| !name.trim().is_empty())
                    .unwrap_or(true)
            })
    });
}

/// 从映射 JSON 中剔除被关闭的 [from, to] 对。
///
/// * 字符串值命中禁用对 → 整个 key 移除；
/// * 数组值逐项过滤 → 全部被禁用则移除 key，只剩一项时折叠为字符串；
/// * 禁用列表为空或解析失败 → 原样返回（fail-open 与历史行为一致）。
pub fn filter_disabled_mapping(
    mut mapping: serde_json::Value,
    disabled_json: &str,
) -> serde_json::Value {
    let disabled: Vec<(String, String)> = serde_json::from_str::<Vec<Vec<String>>>(disabled_json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|pair| {
            let mut it = pair.into_iter();
            let from = it.next()?;
            let to = it.next()?;
            Some((from, to))
        })
        .collect();
    if disabled.is_empty() {
        return mapping;
    }
    let Some(obj) = mapping.as_object_mut() else {
        return mapping;
    };
    let is_disabled = |from: &str, to: &str| disabled.iter().any(|(df, dt)| df == from && dt == to);
    let keys: Vec<String> = obj.keys().cloned().collect();
    for from in keys {
        let removed = match obj.get(&from) {
            Some(serde_json::Value::String(s)) => {
                if is_disabled(&from, s) {
                    obj.remove(&from);
                    true
                } else {
                    false
                }
            }
            Some(serde_json::Value::Array(arr)) => {
                let kept: Vec<serde_json::Value> = arr
                    .iter()
                    .filter(|v| v.as_str().map(|s| !is_disabled(&from, s)).unwrap_or(true))
                    .cloned()
                    .collect();
                match kept.len() {
                    0 => {
                        obj.remove(&from);
                        true
                    }
                    1 => {
                        obj.insert(from.clone(), kept.into_iter().next().unwrap());
                        true
                    }
                    _ => {
                        obj.insert(from.clone(), serde_json::Value::Array(kept));
                        true
                    }
                }
            }
            _ => false,
        };
        let _ = removed;
    }
    mapping
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CreateChannelInput {
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub priority: Option<i64>,
    pub weight: Option<i64>,
    pub config: Option<serde_json::Value>,
    pub model_mapping: Option<serde_json::Value>,
    /// 被关闭的映射对：JSON 数组，元素为 [from, to]（迁移 041）。
    #[serde(default)]
    pub model_mapping_disabled: Option<serde_json::Value>,
    pub timeout_secs: Option<i64>,
    // --- T02 protocol identity fields (all Option + serde(default)) ---
    // Missing => legacy inference from type/base_url/config.
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub native_base_url: Option<String>,
    /// Serialized JSON array of endpoint strings; missing => legacy inference.
    #[serde(default)]
    pub native_endpoints: Option<Vec<String>>,
    #[serde(default)]
    pub preset_revision: Option<String>,
    #[serde(default)]
    pub legacy_executor_override: Option<String>,
    // --- T07 draft-test receipt. Backend validates these against the current
    // draft when present; force_save saves despite failed/skipped tests as long
    // as the same draft was tested at least once. Legacy payloads omit them. ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_run_id: Option<String>,
    /// 本次按端点选择的测试模型，仅校验回执，不持久化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_models: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_save: Option<bool>,
    // --- Multi-key: additional API keys for load balancing (migration 023) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_keys: Option<Vec<ChannelApiKeyInput>>,
    // --- Custom upstream request headers (stored in config for compatibility) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Vec<ChannelRequestHeaderInput>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateChannelInput {
    pub id: String,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub channel_type: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub models: Option<Vec<String>>,
    pub status: Option<i64>,
    pub priority: Option<i64>,
    pub weight: Option<i64>,
    pub config: Option<serde_json::Value>,
    pub model_mapping: Option<serde_json::Value>,
    /// 被关闭的映射对：JSON 数组，元素为 [from, to]（迁移 041）。None = 保持不变。
    #[serde(default)]
    pub model_mapping_disabled: Option<serde_json::Value>,
    pub timeout_secs: Option<i64>,
    // --- T02 protocol identity fields. None = keep current value. ---
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub native_base_url: Option<String>,
    /// None = keep; explicit empty Vec is REJECTED (must be non-empty or absent).
    #[serde(default)]
    pub native_endpoints: Option<Vec<String>>,
    #[serde(default)]
    pub preset_revision: Option<String>,
    #[serde(default)]
    pub legacy_executor_override: Option<String>,
    /// Distinguish "edit leave-blank = keep key" from "Ollama explicitly clear
    /// key": true => persist an empty api_key (clears the stored key).
    #[serde(default)]
    pub clear_api_key: Option<bool>,
    // --- T07 draft-test receipt (see CreateChannelInput). ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_run_id: Option<String>,
    /// 本次按端点选择的测试模型，仅校验回执，不持久化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_models: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_save: Option<bool>,
    // --- Multi-key: replacement for extra keys (full replace semantics) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_keys: Option<Vec<ChannelApiKeyInput>>,
    // --- Custom upstream request headers (full replace semantics) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Vec<ChannelRequestHeaderInput>>,
}

/// Import-write input (T09).  Unlike `CreateChannelInput` (whose repository
/// writer hard-codes status=1 and a default timeout), this input carries the
/// full business field set so import/export round-trips are per-field exact:
/// status, priority, weight, timeout_secs, config unknown keys, URL, key,
/// models and array model_mapping.  Identity columns are `Option`: a v1 /
/// legacy import passes `None` (identity_revision 0) so the resolver live-infers;
/// a v2 import passes the validated identity verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImportChannelInput {
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub status: i64,
    pub priority: i64,
    pub weight: i64,
    pub config: serde_json::Value,
    pub model_mapping: serde_json::Value,
    /// 被关闭的映射对：JSON 数组，元素为 [from, to]（迁移 041）。
    #[serde(default)]
    pub model_mapping_disabled: Option<serde_json::Value>,
    pub timeout_secs: i64,
    // --- T02 protocol identity columns (None => legacy-infer on read) ---
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub native_base_url: Option<String>,
    #[serde(default)]
    pub native_endpoints: Option<Vec<String>>,
    #[serde(default)]
    pub preset_revision: Option<String>,
    #[serde(default)]
    pub identity_revision: i64,
    #[serde(default)]
    pub legacy_executor_override: Option<String>,
    // --- test-status fields (preserved so an exported test badge survives) ---
    #[serde(default)]
    pub last_test_at: Option<String>,
    #[serde(default)]
    pub last_test_ok: Option<i64>,
    /// 主 Key 是否参与负载均衡（迁移 044；v1 文件缺省 = 启用）。
    #[serde(default)]
    pub api_key_enabled: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub key: String,
    pub status: i64,
    pub allowed_models: String,
    pub allowed_channels: String,
    pub denied_models: String,
    pub denied_channels: String,
    pub quota_limit: i64,
    pub quota_used: i64,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateApiKeyInput {
    pub name: String,
    /// 可选自定义密钥。留空则自动生成 sk-waliapi-<uuid>。
    #[serde(default)]
    pub key: Option<String>,
    pub allowed_models: Option<Vec<String>>,
    pub allowed_channels: Option<Vec<String>>,
    pub denied_models: Option<Vec<String>>,
    pub denied_channels: Option<Vec<String>>,
    pub quota_limit: Option<i64>,
    pub expires_at: Option<String>,
}

/// A single model in an Auth Account's provider-synchronized snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelState {
    pub id: String,
    pub status: String,
    pub unavailable: bool,
    pub next_retry_after: Option<String>,
    pub last_error: Option<String>,
    /// Per-model wire protocol metadata sourced only from the provider's
    /// `/models` catalog (e.g. `kimi` or `anthropic`).  Backward-compatible:
    /// old snapshots serialize without this field and deserialize to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelStates {
    pub version: i64,
    pub models: Vec<ModelState>,
}

impl Default for ModelStates {
    fn default() -> Self {
        Self {
            version: 1,
            models: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaWindow {
    pub used_percent: Option<f64>,
    pub window_minutes: Option<i64>,
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaLimit {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub primary: Option<QuotaWindow>,
    pub secondary: Option<QuotaWindow>,
    pub credits: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaState {
    pub version: i64,
    pub exceeded: bool,
    pub reason: Option<String>,
    pub next_recover_at: Option<String>,
    pub backoff_level: i64,
    pub limits: Vec<QuotaLimit>,
}

impl Default for QuotaState {
    fn default() -> Self {
        Self {
            version: 1,
            exceeded: false,
            reason: None,
            next_recover_at: None,
            backoff_level: 0,
            limits: Vec::new(),
        }
    }
}

/// Persisted generic provider account. `payload_json` is intentionally kept in
/// the database model only; command DTOs must expose a redacted summary.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AuthAccount {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub account_id: String,
    pub status: String,
    pub disabled: i64,
    pub priority: i64,
    pub weight: i64,
    /// 手动排序权重（拖拽排序），默认 0 表示未手动排序。
    #[sqlx(default)]
    pub sort_order: i64,
    pub quota_json: Option<String>,
    pub model_states_json: String,
    pub model_mapping_json: String,
    /// 被关闭的映射对（迁移 042）：JSON 数组，元素为 [from, to]。
    /// 空数组 = 全部映射开启。`#[sqlx(default)]` 兼容仅迁移到旧版本的测试库。
    #[sqlx(default)]
    pub model_mapping_disabled: String,
    pub attributes_json: String,
    pub payload_json: String,
    pub last_refreshed_at: Option<String>,
    pub last_models_sync_at: Option<String>,
    pub next_refresh_after: Option<String>,
    pub next_retry_after: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 重置卡消费的本地审计状态。完整卡 ID 永不落库。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AuthResetOperation {
    pub id: String,
    pub account_id: String,
    pub credit_id_hash: String,
    pub redeem_request_id: String,
    pub status: String,
    pub upstream_code: Option<String>,
    pub error_class: Option<String>,
    pub quota_refresh_status: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl AuthAccount {
    pub fn model_states(&self) -> Result<ModelStates, serde_json::Error> {
        serde_json::from_str(&self.model_states_json)
    }

    pub fn quota_state(&self) -> Result<Option<QuotaState>, serde_json::Error> {
        self.quota_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
    }

    pub fn model_mapping(&self) -> Result<serde_json::Value, serde_json::Error> {
        if self.model_mapping_json.is_empty() {
            return Ok(serde_json::json!({}));
        }
        serde_json::from_str(&self.model_mapping_json)
    }

    /// 返回规范化但尚未应用禁用列表的原始模型映射，供管理面使用。
    pub fn normalized_model_mapping(&self) -> Result<serde_json::Value, serde_json::Error> {
        let mut mapping = self.model_mapping()?;
        normalize_model_mapping(&mut mapping);
        Ok(mapping)
    }

    /// 返回规范化后的禁用映射对，供管理面使用。
    pub fn normalized_model_mapping_disabled(&self) -> serde_json::Value {
        let mut disabled =
            serde_json::from_str::<serde_json::Value>(if self.model_mapping_disabled.is_empty() {
                "[]"
            } else {
                &self.model_mapping_disabled
            })
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
        normalize_model_mapping_disabled(&mut disabled);
        disabled
    }

    /// 解析 `model_mapping` 并剔除被关闭的映射对（迁移 042，与渠道
    /// `Channel::active_model_mapping` 对齐）。路由匹配、别名目标解析、
    /// `/v1/models` 聚合都必须使用本方法而非直接读原始列。
    pub fn active_model_mapping(&self) -> serde_json::Value {
        let mapping = self.normalized_model_mapping().unwrap_or_default();
        let disabled = self.normalized_model_mapping_disabled();
        filter_disabled_mapping(
            mapping,
            &serde_json::to_string(&disabled).unwrap_or_else(|_| "[]".to_owned()),
        )
    }
}

/// Login/import input used for an atomic provider/account-id upsert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthAccountUpsert {
    pub provider: String,
    pub label: String,
    pub account_id: String,
    pub attributes: serde_json::Value,
    pub payload: serde_json::Value,
    pub last_refreshed_at: Option<String>,
    pub next_refresh_after: Option<String>,
    pub next_retry_after: Option<String>,
}

/// A persisted request-log row.  All T09 observability columns are NULLABLE
/// (migration 016) so legacy rows and old queries keep working. Its manual
/// `Default` supplies `upstream_type = "channel"` for legacy write paths.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestLog {
    pub id: String,
    pub seq: Option<i64>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub model: String,
    pub upstream_model: Option<String>,
    pub mode: String,
    pub status_code: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub duration_ms: i64,
    pub error_message: Option<String>,
    pub is_stream: i64,
    pub is_retry: i64,
    pub created_at: String,
    pub request_body: Option<String>,
    pub response_choices: Option<String>,
    pub risk_level: String,
    pub risk_score: i64,
    pub risk_summary: Option<String>,
    pub security_action: String,
    pub sanitized: i64,
    pub blocked_reason: Option<String>,
    pub trace_id: Option<String>,
    pub reasoning_effort: Option<String>,
    // --- T09 observability (migration 016; all nullable) ---
    pub downstream_protocol: Option<String>,
    pub downstream_endpoint: Option<String>,
    pub route_group: Option<String>,
    pub upstream_protocol: Option<String>,
    pub upstream_endpoint: Option<String>,
    pub provider: Option<String>,
    pub codec_version: Option<String>,
    pub failure_class: Option<String>,
    pub identity_revision: Option<i64>,
    pub client_cancelled: Option<i64>,
    pub stream_committed: Option<i64>,
    /// `channel` for legacy API channels and `auth_account` for provider
    /// accounts. The database default makes upgraded historical rows channel.
    pub upstream_type: String,
    /// Prompt tokens served from upstream cache (migration 026).
    pub cached_tokens: i64,
}

/// Lightweight row used by the audit-log list. Large request/response bodies
/// are intentionally excluded from this contract and loaded only by id.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestLogSummary {
    pub id: String,
    pub seq: Option<i64>,
    pub api_key_name: Option<String>,
    pub channel_name: Option<String>,
    pub model: String,
    pub upstream_model: Option<String>,
    pub mode: String,
    pub status_code: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cached_tokens: i64,
    pub duration_ms: i64,
    pub error_message: Option<String>,
    pub is_stream: i64,
    pub is_retry: i64,
    pub created_at: String,
    pub risk_level: String,
    pub risk_score: i64,
    pub risk_summary: Option<String>,
    pub security_action: String,
    pub sanitized: i64,
    pub blocked_reason: Option<String>,
    pub trace_id: Option<String>,
    pub reasoning_effort: Option<String>,
    pub downstream_protocol: Option<String>,
    pub downstream_endpoint: Option<String>,
    pub route_group: Option<String>,
    pub upstream_protocol: Option<String>,
    pub upstream_endpoint: Option<String>,
    pub provider: Option<String>,
    pub codec_version: Option<String>,
    pub failure_class: Option<String>,
    pub identity_revision: Option<i64>,
    pub client_cancelled: Option<i64>,
    pub stream_committed: Option<i64>,
    pub upstream_type: String,
    pub detail_level: String,
    pub started_at: Option<String>,
    pub request_body_bytes: i64,
    pub response_choices_bytes: i64,
    pub has_request_body: bool,
}

impl Default for RequestLog {
    fn default() -> Self {
        Self {
            id: String::new(),
            seq: None,
            api_key_id: None,
            api_key_name: None,
            channel_id: None,
            channel_name: None,
            model: String::new(),
            upstream_model: None,
            mode: String::new(),
            status_code: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            duration_ms: 0,
            error_message: None,
            is_stream: 0,
            is_retry: 0,
            created_at: String::new(),
            request_body: None,
            response_choices: None,
            risk_level: String::new(),
            risk_score: 0,
            risk_summary: None,
            security_action: String::new(),
            sanitized: 0,
            blocked_reason: None,
            trace_id: None,
            reasoning_effort: None,
            downstream_protocol: None,
            downstream_endpoint: None,
            route_group: None,
            upstream_protocol: None,
            upstream_endpoint: None,
            provider: None,
            codec_version: None,
            failure_class: None,
            identity_revision: None,
            client_cancelled: None,
            stream_committed: None,
            upstream_type: "channel".into(),
            cached_tokens: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardStats {
    pub today_requests: i64,
    pub today_total_tokens: i64,
    pub today_cached_tokens: i64,
    pub today_prompt_tokens: i64,
    pub total_cached_tokens: i64,
    pub total_prompt_tokens: i64,
    pub active_channels: i64,
    pub avg_latency_ms: f64,
    pub total_channels: i64,
    pub active_auth_accounts: i64,
    pub total_auth_accounts: i64,
    pub total_api_keys: i64,
    pub total_requests: i64,
    pub total_tokens: i64,
    pub total_knowledge_bases: i64,
    pub total_kb_documents: i64,
    pub total_kb_chunks: i64,
    pub total_wiki_projects: i64,
    pub total_wiki_pages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct LogStats {
    pub date: String,
    pub count: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChannelStats {
    pub channel_id: String,
    pub total_calls: i64,
    pub success_calls: i64,
    pub failed_calls: i64,
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub avg_latency_ms: f64,
    pub last_call_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ApiKeyStats {
    pub api_key_id: String,
    pub total_calls: i64,
    pub success_calls: i64,
    pub failed_calls: i64,
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Prompt tokens served from upstream cache (migration 026).
    pub cached_tokens: i64,
    pub avg_latency_ms: f64,
    pub last_call_at: Option<String>,
}

pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ModelStats {
    pub model: String,
    pub request_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub total_tokens: i64,
    pub success_rate: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TokenTrendPoint {
    pub hour: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
}

/// 单条请求对 `usage_stats` 的累加增量(迁移 046)。由 create_log 漏斗在
/// 同一事务内 UPSERT,日志清理不触碰此表,统计因此不受审计日志保留期影响。
#[derive(Debug, Clone)]
pub struct UsageStatsDelta {
    pub hour: String, // YYYY-MM-DDTHH:00:00.000Z(UTC 小时桶)
    pub model: String,
    pub channel_id: String, // '' 表示无渠道(Auth 账号等)
    pub api_key_id: String, // '' 表示无 Key
    pub success: bool,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cached_tokens: i64,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestSecurityFinding {
    pub id: String,
    pub log_id: String,
    pub phase: String,
    pub category: String,
    pub rule_id: String,
    pub severity: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub evidence_masked: Option<String>,
    pub evidence_hash: Option<String>,
    pub action: Option<String>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_codex_snapshot_without_protocol_deserializes() {
        let json = r#"{"id":"gpt-5","status":"available","unavailable":false,"next_retry_after":null,"last_error":null}"#;
        let model: ModelState = serde_json::from_str(json).expect("old snapshot must parse");
        assert_eq!(model.id, "gpt-5");
        assert_eq!(model.protocol, None);
    }

    #[test]
    fn model_protocol_serialization_round_trip() {
        let model = ModelState {
            id: "kimi-k2.5".into(),
            status: "available".into(),
            unavailable: false,
            next_retry_after: None,
            last_error: None,
            protocol: Some("kimi".into()),
        };
        let json = serde_json::to_string(&model).unwrap();
        let parsed: ModelState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.protocol.as_deref(), Some("kimi"));
    }

    #[test]
    fn model_protocol_omitted_when_none() {
        let bare = ModelState {
            id: "gpt-5".into(),
            status: "available".into(),
            unavailable: false,
            next_retry_after: None,
            last_error: None,
            protocol: None,
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(
            !json.contains("protocol"),
            "None protocol must not serialize"
        );
    }

    #[test]
    fn model_states_round_trip_with_protocol() {
        let states = ModelStates {
            version: 1,
            models: vec![ModelState {
                id: "kimi-a".into(),
                status: "available".into(),
                unavailable: false,
                next_retry_after: None,
                last_error: None,
                protocol: Some("anthropic".into()),
            }],
        };
        let json = serde_json::to_string(&states).unwrap();
        let parsed: ModelStates = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.models[0].protocol.as_deref(), Some("anthropic"));
    }
}

#[cfg(test)]
mod mapping_disabled_tests {
    use super::*;

    #[test]
    fn empty_disabled_list_returns_mapping_verbatim() {
        let mapping = serde_json::json!({"auto": "m-a", "alias": ["m-b", "m-c"]});
        let out = filter_disabled_mapping(mapping, "[]");
        assert_eq!(out["auto"], "m-a");
        assert_eq!(out["alias"], serde_json::json!(["m-b", "m-c"]));
    }

    #[test]
    fn disabled_string_pair_removes_key() {
        let mapping = serde_json::json!({"auto": "m-a", "keep": "m-z"});
        let out = filter_disabled_mapping(mapping, r#"[["auto","m-a"]]"#);
        assert!(out.get("auto").is_none());
        assert_eq!(out["keep"], "m-z");
    }

    #[test]
    fn disabled_array_pair_filters_targets_and_collapses_single() {
        let mapping = serde_json::json!({"auto": ["m-a", "m-b", "m-c"]});
        let out = filter_disabled_mapping(mapping, r#"[["auto","m-a"],["auto","m-c"]]"#);
        assert_eq!(out["auto"], "m-b");

        let mapping = serde_json::json!({"auto": ["m-a", "m-b"]});
        let out = filter_disabled_mapping(mapping, r#"[["auto","m-b"]]"#);
        assert_eq!(out["auto"], "m-a");

        let mapping = serde_json::json!({"auto": ["m-a"]});
        let out = filter_disabled_mapping(mapping, r#"[["auto","m-a"]]"#);
        assert!(out.get("auto").is_none());
    }

    #[test]
    fn malformed_disabled_list_fails_open() {
        let mapping = serde_json::json!({"auto": "m-a"});
        let out = filter_disabled_mapping(mapping, "not-json");
        assert_eq!(out["auto"], "m-a");
    }

    #[test]
    fn normalize_model_mapping_trims_keys_and_values() {
        let mut mapping = serde_json::json!({
            " alias ": " upstream-a ",
            " array ": [" upstream-a ", " upstream-b ", " ", 1],
            " empty ": " ",
            " ": "ignored"
        });
        crate::db::models::normalize_model_mapping(&mut mapping);
        assert_eq!(mapping["alias"], "upstream-a");
        assert_eq!(
            mapping["array"],
            serde_json::json!(["upstream-a", "upstream-b", 1])
        );
        assert!(mapping.get("empty").is_none());
        assert!(mapping.get(" ").is_none());
    }

    #[test]
    fn active_model_mapping_excludes_disabled_pairs() {
        let mut ch = Channel {
            id: "c1".into(),
            name: "t".into(),
            channel_type: "openai".into(),
            base_url: "https://x/v1".into(),
            api_key: "k".into(),
            models: "[]".into(),
            status: 1,
            priority: 0,
            weight: 1,
            config: "{}".into(),
            model_mapping: serde_json::json!({"auto": ["m-a", "m-b"]}).to_string(),
            model_mapping_disabled: r#"[["auto","m-a"]]"#.into(),
            timeout_secs: 300,
            protocol: None,
            provider: None,
            native_base_url: None,
            native_endpoints: None,
            preset_revision: None,
            identity_revision: 1,
            legacy_executor_override: None,
            created_at: String::new(),
            updated_at: String::new(),
            last_test_at: None,
            last_test_ok: None,
            last_probe_at: None,
            last_probe_ok: None,
            probe_latency_ms: None,
            api_key_enabled: Some(1),
        };
        assert_eq!(ch.active_model_mapping()["auto"], "m-b");

        // 关闭列表清空后恢复全部映射
        ch.model_mapping_disabled = "[]".into();
        assert_eq!(
            ch.active_model_mapping()["auto"],
            serde_json::json!(["m-a", "m-b"])
        );

        // 历史数据即使保存了首尾空格，也必须按规范化后的名称参与路由。
        ch.model_mapping = serde_json::json!({" auto ": [" m-a ", " m-b "]}).to_string();
        ch.model_mapping_disabled = r#"[[" auto "," m-a "]]"#.into();
        assert_eq!(ch.active_model_mapping()["auto"], "m-b");
    }
}
