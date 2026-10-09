use crate::adaptor::{get_adaptor, ChannelConfig};
use crate::channel_presets::ProtocolPresetGroup;
use crate::core::channel_identity::{
    resolve_channel_identity, ChannelIdentity, ChannelIdentityRow,
};
use crate::db::models::{
    Channel, ChannelApiKey, ChannelStats, CreateChannelInput, UpdateChannelInput,
};
use crate::db::repository::Repository;
use crate::services::channel_test::{
    self, DraftChannelTestInput, DraftChannelTestResult, SaveReceiptCheck,
};
use crate::services::upstream_models::UpstreamModelsResult;
use crate::AppState;
use serde::{Deserialize, Serialize};

/// Output DTO for a channel. Always returns the NORMALIZED protocol identity
/// (via `resolve_channel_identity`), including the previously omitted
/// `timeout_secs` (design 11.4). API key stays masked.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelDto {
    pub id: String,
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
    /// 被关闭的映射对（迁移 041）：[from, to][] 数组。
    pub model_mapping_disabled: serde_json::Value,
    pub timeout_secs: i64,
    // --- normalized protocol identity (T02) ---
    pub protocol: String,
    pub provider: String,
    pub native_base_url: String,
    pub native_endpoints: Vec<String>,
    pub identity_revision: i64,
    /// preset registry revision recorded at save time (traceability only).
    pub preset_revision: Option<String>,
    pub legacy_executor_override: Option<String>,
    pub executor_kind: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<i64>,
    /// 主动健康探测（迁移 033）：探测列优先展示，无探测数据回退手动测试列。
    pub last_probe_at: Option<String>,
    pub last_probe_ok: Option<i64>,
    pub probe_latency_ms: Option<i64>,
    /// 主 Key 是否参与负载均衡（迁移 044）。1 = 启用，0 = 停用。
    pub api_key_enabled: i64,
    // --- Multi-key: extra API keys for load balancing (migration 023) ---
    pub extra_keys: Vec<ChannelKeyDto>,
    /// 渠道级自定义上游请求头（敏感值原样返回给已进入管理面的前端）。
    pub request_headers: Vec<crate::db::models::ChannelRequestHeaderInput>,
}

/// Masked DTO for a channel API key entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelKeyDto {
    pub id: String,
    pub api_key: String,
    pub weight: i64,
    pub status: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl From<ChannelApiKey> for ChannelKeyDto {
    fn from(k: ChannelApiKey) -> Self {
        ChannelKeyDto {
            id: k.id,
            api_key: mask_key(&k.api_key),
            weight: k.weight,
            status: k.status,
            created_at: k.created_at,
            updated_at: k.updated_at,
        }
    }
}

impl From<Channel> for ChannelDto {
    fn from(c: Channel) -> Self {
        let identity: ChannelIdentity = resolve_channel_identity(&ChannelIdentityRow::from(&c));
        let model_mapping = c.normalized_model_mapping();
        let model_mapping_disabled = c.normalized_model_mapping_disabled();
        ChannelDto {
            id: c.id,
            name: c.name,
            channel_type: c.channel_type,
            base_url: c.base_url,
            api_key: mask_key(&c.api_key),
            models: serde_json::from_str(&c.models).unwrap_or_default(),
            status: c.status,
            priority: c.priority,
            weight: c.weight,
            config: serde_json::from_str(&c.config)
                .unwrap_or(serde_json::Value::Object(Default::default())),
            model_mapping,
            model_mapping_disabled,
            timeout_secs: c.timeout_secs,
            protocol: identity.protocol,
            provider: identity.provider,
            native_base_url: identity.native_base_url,
            native_endpoints: identity.native_endpoints,
            identity_revision: identity.identity_revision,
            preset_revision: c.preset_revision.clone(),
            legacy_executor_override: identity.legacy_executor_override,
            executor_kind: identity.executor_kind,
            created_at: c.created_at,
            updated_at: c.updated_at,
            last_test_at: c.last_test_at,
            last_test_ok: c.last_test_ok,
            last_probe_at: c.last_probe_at,
            last_probe_ok: c.last_probe_ok,
            probe_latency_ms: c.probe_latency_ms,
            api_key_enabled: c.api_key_enabled.unwrap_or(1),
            extra_keys: Vec::new(), // populated by to_dto_with_keys
            request_headers: serde_json::from_str::<serde_json::Value>(&c.config)
                .ok()
                .and_then(|v| v.get("request_headers").cloned())
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
        }
    }
}

/// Build a ChannelDto with extra keys populated from the database.
pub(crate) async fn to_dto_with_keys(repo: &Repository, c: Channel) -> Result<ChannelDto, String> {
    let keys = repo
        .get_channel_api_keys(&c.id)
        .await
        .map_err(|e| e.to_string())?;
    let mut dto: ChannelDto = c.into();
    dto.extra_keys = keys.into_iter().map(ChannelKeyDto::from).collect();
    Ok(dto)
}

fn mask_key(key: &str) -> String {
    // FIX-23：共享字符边界安全实现（此前按字节切片，多字节密钥 panic）。
    crate::utils::secret::mask_secret(key)
}

fn to_dto(c: Channel) -> ChannelDto {
    c.into()
}

#[tauri::command]
pub async fn get_channels(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<ChannelDto>, String> {
    get_channels_impl(&*state).await
}

pub async fn get_channels_impl(
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<ChannelDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let channels = repo.get_all_channels().await.map_err(|e| e.to_string())?;
    let mut dtos = Vec::with_capacity(channels.len());
    for c in channels {
        dtos.push(to_dto_with_keys(&repo, c).await?);
    }
    Ok(dtos)
}

#[tauri::command]
pub async fn get_channel(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<ChannelDto, String> {
    get_channel_impl(&id, &*state).await
}

pub async fn get_channel_impl(
    id: &str,
    state: &std::sync::Arc<AppState>,
) -> Result<ChannelDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    let c = repo.get_channel(id).await.map_err(|e| e.to_string())?;
    to_dto_with_keys(&repo, c).await
}

/// 只读：返回全部协议及其 provider 模板，`groups[n].presets[0]` 恒为 custom option。
/// 不落库、不访问网络。
#[tauri::command]
pub fn get_channel_presets() -> Result<Vec<ProtocolPresetGroup>, String> {
    Ok(crate::channel_presets::groups_for_protocols())
}

#[tauri::command]
pub async fn get_channel_api_key(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<String, String> {
    get_channel_api_key_impl(&id, &*state).await
}

pub async fn get_channel_api_key_impl(
    id: &str,
    state: &std::sync::Arc<AppState>,
) -> Result<String, String> {
    let repo = Repository::new(state.db.pool.clone());
    let channel = repo.get_channel(id).await.map_err(|e| e.to_string())?;
    Ok(channel.api_key)
}

#[tauri::command]
pub async fn create_channel(
    input: CreateChannelInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<ChannelDto, String> {
    create_channel_impl(input, &*state).await
}

pub async fn create_channel_impl(
    input: CreateChannelInput,
    state: &std::sync::Arc<AppState>,
) -> Result<ChannelDto, String> {
    // T07: validate the save-time receipt (test_run_id + draft_fingerprint +
    // force_save). Returns Ok(None) for legacy payloads without these fields.
    let receipt_check = validate_create_receipt(&input, &state)?;
    let repo = Repository::new(state.db.pool.clone());
    let channel = repo
        .create_channel(&input)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(check) = receipt_check {
        repo.update_channel_test_result(&channel.id, check.all_passed)
            .await
            .map_err(|e| e.to_string())?;
        let channel = repo
            .get_channel(&channel.id)
            .await
            .map_err(|e| e.to_string())?;
        to_dto_with_keys(&repo, channel).await
    } else {
        to_dto_with_keys(&repo, channel).await
    }
}

#[tauri::command]
pub async fn update_channel(
    input: UpdateChannelInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<ChannelDto, String> {
    update_channel_impl(input, &*state).await
}

pub async fn update_channel_impl(
    input: UpdateChannelInput,
    state: &std::sync::Arc<AppState>,
) -> Result<ChannelDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    // T07: validate the save-time receipt against the EFFECTIVE draft (None
    // fields resolve to the existing row, mirroring the repository update).
    let receipt_check = {
        let existing = repo
            .get_channel(&input.id)
            .await
            .map_err(|e| e.to_string())?;
        let eff_protocol = input.protocol.clone().or_else(|| existing.protocol.clone());
        let eff_provider = input.provider.clone().or_else(|| existing.provider.clone());
        let eff_native_base = input
            .native_base_url
            .clone()
            .or_else(|| existing.native_base_url.clone());
        let eff_eps = input.native_endpoints.clone().or_else(|| {
            serde_json::from_str::<Vec<String>>(
                existing.native_endpoints.as_deref().unwrap_or("[]"),
            )
            .ok()
        });
        let eff_models = input.models.clone().unwrap_or_else(|| {
            serde_json::from_str::<Vec<String>>(&existing.models).unwrap_or_default()
        });
        let eff_timeout = input.timeout_secs.unwrap_or(existing.timeout_secs);
        let eff_key = if let Some(k) = input.api_key.clone() {
            k
        } else if input.clear_api_key == Some(true) {
            String::new()
        } else {
            // 与测试侧 resolve_draft_api_key 同规则：主 Key 停用时指纹绑定
            // 第一个启用的从 Key，保证测试与保存指纹一致（T07 门禁不误判）。
            channel_test::effective_probe_key(&repo, &input.id, &existing.api_key).await
        };
        let eff_override = input
            .legacy_executor_override
            .clone()
            .or_else(|| existing.legacy_executor_override.clone());
        let computed = channel_test::fingerprint_for_draft(
            eff_protocol.as_deref(),
            eff_provider.as_deref(),
            eff_native_base.as_deref(),
            eff_eps.as_deref(),
            &eff_models,
            eff_timeout,
            &eff_key,
            &existing.channel_type,
            &existing.base_url,
            &serde_json::from_str::<serde_json::Value>(&existing.config)
                .unwrap_or_else(|_| serde_json::Value::Object(Default::default())),
            eff_override.as_deref(),
            input.test_models.as_ref(),
        );
        validate_save_receipt(&state, &input, &computed)?
    };
    repo.update_channel(&input)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(check) = receipt_check {
        repo.update_channel_test_result(&input.id, check.all_passed)
            .await
            .map_err(|e| e.to_string())?;
        let channel = repo
            .get_channel(&input.id)
            .await
            .map_err(|e| e.to_string())?;
        to_dto_with_keys(&repo, channel).await
    } else {
        let channel = repo
            .get_channel(&input.id)
            .await
            .map_err(|e| e.to_string())?;
        to_dto_with_keys(&repo, channel).await
    }
}

/// T07 create-path save-receipt validation.
fn validate_create_receipt(
    input: &CreateChannelInput,
    state: &std::sync::Arc<AppState>,
) -> Result<Option<SaveReceiptCheck>, String> {
    if input.test_run_id.is_none() {
        return Ok(None); // legacy path
    }
    let computed = channel_test::fingerprint_for_draft(
        input.protocol.as_deref(),
        input.provider.as_deref(),
        input.native_base_url.as_deref(),
        input.native_endpoints.as_deref(),
        &input.models,
        input.timeout_secs.unwrap_or(300),
        &input.api_key,
        &input.channel_type,
        &input.base_url,
        input
            .config
            .as_ref()
            .unwrap_or(&serde_json::Value::Object(Default::default())),
        input.legacy_executor_override.as_deref(),
        input.test_models.as_ref(),
    );
    validate_save_receipt(state, input, &computed)
}

/// Shared save-receipt validation: `input` carries the T07 receipt fields.
fn validate_save_receipt<I: ReceiptFields>(
    state: &std::sync::Arc<AppState>,
    input: &I,
    computed_fingerprint: &str,
) -> Result<Option<SaveReceiptCheck>, String> {
    channel_test::validate_save_receipt(
        &state.test_receipts,
        input.test_run_id(),
        input.draft_fingerprint(),
        computed_fingerprint,
        input.force_save().unwrap_or(false),
    )
}

/// Minimal accessor seam so create/update share one validator.
trait ReceiptFields {
    fn test_run_id(&self) -> Option<&str>;
    fn draft_fingerprint(&self) -> Option<&str>;
    fn force_save(&self) -> Option<bool>;
}

impl ReceiptFields for CreateChannelInput {
    fn test_run_id(&self) -> Option<&str> {
        self.test_run_id.as_deref()
    }
    fn draft_fingerprint(&self) -> Option<&str> {
        self.draft_fingerprint.as_deref()
    }
    fn force_save(&self) -> Option<bool> {
        self.force_save
    }
}

impl ReceiptFields for UpdateChannelInput {
    fn test_run_id(&self) -> Option<&str> {
        self.test_run_id.as_deref()
    }
    fn draft_fingerprint(&self) -> Option<&str> {
        self.draft_fingerprint.as_deref()
    }
    fn force_save(&self) -> Option<bool> {
        self.force_save
    }
}

/// T07: save-time, no-DB-write connectivity test for an UNSAVED draft.
///
/// Returns one independent result per selected endpoint, plus the
/// `draft_fingerprint` / `tested_at` / `test_run_id` wrapper.  Does NOT create
/// or update a channel, does NOT count quota, and does NOT write a production
/// request log; the only persisted side effect is a short-lived in-process
/// receipt used by the save step.
#[tauri::command]
pub async fn test_channel_draft(
    input: DraftChannelTestInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<DraftChannelTestResult, String> {
    test_channel_draft_impl(input, &*state).await
}

pub async fn test_channel_draft_impl(
    input: DraftChannelTestInput,
    state: &std::sync::Arc<AppState>,
) -> Result<DraftChannelTestResult, String> {
    let repo = Repository::new(state.db.pool.clone());
    let api_key = channel_test::resolve_draft_api_key(&input, &repo).await?;
    let config = channel_test::DraftTestConfig::default();
    channel_test::run_draft_test(&input, &api_key, &state.test_receipts, &config).await
}

/// 拉取上游模型列表（T14）。返回模型 ID 数组 + 判定协议 + 根 URL，供编辑页
/// 弹窗勾选后合并进模型列表。**绝不写库**：不创建/更新渠道、不写 request log、
/// 不覆盖已有模型列表。API Key 复用草稿测试的解析语义（编辑留空回填已存 Key）。
#[tauri::command]
pub async fn sync_upstream_models(
    input: DraftChannelTestInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<UpstreamModelsResult, String> {
    sync_upstream_models_impl(input, &*state).await
}

pub async fn sync_upstream_models_impl(
    input: DraftChannelTestInput,
    state: &std::sync::Arc<AppState>,
) -> Result<UpstreamModelsResult, String> {
    let repo = Repository::new(state.db.pool.clone());
    let api_key = channel_test::resolve_draft_api_key(&input, &repo).await?;
    let timeout = input.timeout_secs.unwrap_or(300).max(1) as u64;
    crate::services::upstream_models::fetch_upstream_models(&input, &api_key, timeout).await
}

#[tauri::command]
pub async fn toggle_channel(
    id: String,
    status: i64,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    toggle_channel_impl(&id, status, &*state).await
}

pub async fn toggle_channel_impl(
    id: &str,
    status: i64,
    state: &std::sync::Arc<AppState>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.update_channel_status(id, status)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_channel(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    delete_channel_impl(&id, &*state).await
}

pub async fn delete_channel_impl(id: &str, state: &std::sync::Arc<AppState>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_channel(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_channel_stats(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<ChannelStats>, String> {
    get_channel_stats_impl(&*state).await
}

pub async fn get_channel_stats_impl(
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<ChannelStats>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_channel_stats().await.map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestChannelResult {
    pub success: bool,
    pub message: String,
    pub latency_ms: u64,
}

#[tauri::command]
pub async fn test_channel(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<TestChannelResult, String> {
    test_channel_impl(&id, &*state).await
}

pub async fn test_channel_impl(
    id: &str,
    state: &std::sync::Arc<AppState>,
) -> Result<TestChannelResult, String> {
    let repo = Repository::new(state.db.pool.clone());
    let mut channel = repo.get_channel(id).await.map_err(|e| e.to_string())?;
    // 主 Key 停用时走第一个启用的从 Key（与真实调度语义一致）。
    channel.api_key = channel_test::effective_probe_key(&repo, id, &channel.api_key).await;

    let config = ChannelConfig {
        base_url: channel.base_url.clone(),
        api_key: channel.api_key.clone(),
        models: serde_json::from_str(&channel.models).unwrap_or_default(),
        model_mapping: channel.active_model_mapping(),
        extra: serde_json::from_str(&channel.config)
            .unwrap_or(serde_json::Value::Object(Default::default())),
        timeout_secs: channel.timeout_secs.max(1) as u64,
        // 渠道测试同样走渠道级出站代理（与真实转发路径一致）。
        proxy: {
            let extra: serde_json::Value = serde_json::from_str(&channel.config)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            crate::adaptor::ProxySetting::from_extra(&extra)
        },
    };

    let adaptor = get_adaptor(&channel.channel_type);
    let result = adaptor.test(&config).await.map_err(|e| e.to_string())?;

    repo.update_channel_test_result(id, result.success)
        .await
        .map_err(|e| e.to_string())?;

    Ok(TestChannelResult {
        success: result.success,
        message: result.message,
        latency_ms: result.latency_ms,
    })
}

#[tauri::command]
pub async fn reorder_channels(
    ordered_ids: Vec<String>,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    reorder_channels_impl(&ordered_ids, &*state).await
}

pub async fn reorder_channels_impl(
    ordered_ids: &[String],
    state: &std::sync::Arc<AppState>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.reorder_channels(ordered_ids)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get the full (unmasked) extra API keys for a channel. Used by the frontend
/// to show full key values on demand, similar to get_channel_api_key.
#[tauri::command]
pub async fn get_channel_extra_keys(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<ChannelKeyDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let keys = repo
        .get_channel_api_keys(&id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(keys.into_iter().map(ChannelKeyDto::from).collect())
}

/// Get a single full (unmasked) extra API key by its id.
#[tauri::command]
pub async fn get_channel_extra_key_value(
    key_id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<String, String> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT api_key FROM channel_api_keys WHERE id = ?")
            .bind(&key_id)
            .fetch_optional(&state.db.pool)
            .await
            .map_err(|e| e.to_string())?;
    row.map(|(k,)| k).ok_or_else(|| "Key not found".to_string())
}

/// Toggle a channel API key's enabled/disabled status.
#[tauri::command]
pub async fn toggle_channel_extra_key(
    key_id: String,
    status: i64,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.toggle_channel_api_key(&key_id, status)
        .await
        .map_err(|e| e.to_string())
}

/// 启用/停用渠道主 Key（#1，channels.api_key）。停用后不参与负载均衡；
/// 主 Key 与全部额外 Key 均停用时该渠道整体跳过。
#[tauri::command]
pub async fn toggle_channel_primary_key(
    id: String,
    enabled: bool,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    toggle_channel_primary_key_impl(&id, enabled, &*state).await
}

pub async fn toggle_channel_primary_key_impl(
    id: &str,
    enabled: bool,
    state: &std::sync::Arc<AppState>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.toggle_channel_primary_key(id, enabled)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a channel API key.
#[tauri::command]
pub async fn delete_channel_extra_key(
    key_id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_channel_api_key(&key_id)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel_fixture() -> Channel {
        Channel {
            id: "channel-1".into(),
            name: "Channel".into(),
            channel_type: "openai".into(),
            base_url: "https://example.test/v1".into(),
            api_key: "secret".into(),
            models: "[]".into(),
            status: 1,
            priority: 0,
            weight: 1,
            config: "{}".into(),
            model_mapping: json!({" alias ": " upstream "}).to_string(),
            model_mapping_disabled: json!([[" alias ", " upstream "]]).to_string(),
            timeout_secs: 60,
            protocol: Some("openai".into()),
            provider: Some("custom".into()),
            native_base_url: Some("https://example.test/v1".into()),
            native_endpoints: Some(json!(["chat_completions"]).to_string()),
            preset_revision: None,
            identity_revision: 1,
            legacy_executor_override: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            last_test_at: None,
            last_test_ok: None,
            last_probe_at: None,
            last_probe_ok: None,
            probe_latency_ms: None,
            api_key_enabled: Some(1),
        }
    }

    #[test]
    fn channel_dto_normalizes_historical_model_mapping_names() {
        let dto = ChannelDto::from(channel_fixture());

        assert_eq!(dto.model_mapping, json!({"alias": "upstream"}));
        assert_eq!(dto.model_mapping_disabled, json!([["alias", "upstream"]]));
    }
}
