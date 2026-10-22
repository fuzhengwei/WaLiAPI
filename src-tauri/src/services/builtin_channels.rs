//! 内置免费渠道种子：OpenCode 免费车道 / Kilo 免费池（零配置开箱即用）。
//!
//! 启动时把两条免 Key 免费渠道预建进渠道管理，用户无需任何配置即可调用：
//! - OpenCode（`opencode.ai/inference/v1`）：凭据固定公开令牌 `public`；
//! - Kilo（`api.kilo.ai/api/gateway`）：免费池免 Key，凭据留空。
//!
//! 渠道字段完全取自 [`crate::channel_presets`] 的 preset（单一数据源），
//! preset 更新后新生效的种子字段随之更新（仅对尚未种子的实例）。
//!
//! 执行纪律：每个安装实例仅执行一次（settings 标记）；用户事后删除的种子
//! 渠道不会被再次自动加回（尊重用户意图）。种子失败不置位标记，下次启动
//! 重试；标记丢失时靠 `channel_provider_exists` 幂等兜底，不会产生重复渠道。

use crate::channel_presets::{all_channel_presets, ChannelProvider};
use crate::db::models::CreateChannelInput;
use crate::db::repository::Repository;
use crate::settings_store::SettingsStore;

/// settings 标记：种子是否已执行过。
const SEED_FLAG: &str = "channels.builtin_free_seeded_v1";

/// 启动时预建内置免费渠道。失败仅告警，不阻断启动。
pub async fn seed_builtin_free_channels(repo: &Repository, settings: &SettingsStore) {
    if settings.get_bool(SEED_FLAG, false) {
        return;
    }
    let presets = all_channel_presets();
    for provider in [ChannelProvider::Opencode, ChannelProvider::Kilo] {
        let Some(preset) = presets.iter().find(|p| p.provider == provider) else {
            continue;
        };
        // 已有同 protocol+provider 的渠道（含用户自建）则跳过，保证幂等。
        match repo
            .channel_provider_exists(preset.protocol.as_str(), provider.as_str())
            .await
        {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    "[内置渠道] 查询 {} 是否存在失败，跳过本轮种子: {error}",
                    preset.display_name
                );
                continue;
            }
        }
        // OpenCode 免费车道凭据为固定公开令牌；Kilo 免费池完全免 Key。
        let api_key = match provider {
            ChannelProvider::Opencode => "public",
            _ => "",
        };
        let input = CreateChannelInput {
            name: preset.display_name.clone(),
            channel_type: preset.legacy_type.clone(),
            base_url: preset.legacy_base_url.clone(),
            api_key: api_key.to_string(),
            models: preset
                .model_suggestions
                .iter()
                .map(|m| m.id.clone())
                .collect(),
            priority: Some(0),
            weight: Some(1),
            // 标记内置种子来源（前端可据此识别；不阻断任何编辑操作）。
            config: Some(serde_json::json!({ "builtin_seed": provider.as_str() })),
            model_mapping: None,
            model_mapping_disabled: None,
            timeout_secs: Some(300),
            protocol: Some(preset.protocol.as_str().to_string()),
            provider: Some(provider.as_str().to_string()),
            native_base_url: Some(preset.native_base_url.clone()),
            native_endpoints: Some(
                preset
                    .native_endpoints
                    .iter()
                    .map(|e| e.as_str().to_string())
                    .collect(),
            ),
            preset_revision: Some(preset.preset_revision.clone()),
            legacy_executor_override: None,
            test_run_id: None,
            test_models: None,
            draft_fingerprint: None,
            force_save: None,
            extra_keys: None,
            request_headers: None,
        };
        match repo.create_channel(&input).await {
            Ok(channel) => {
                tracing::info!(
                    "[内置渠道] 已预建免费渠道: {} ({})",
                    channel.name,
                    channel.id
                );
            }
            // 失败即返回、不置位标记，下次启动重试（幂等保护防重复插入）。
            Err(error) => {
                tracing::warn!("[内置渠道] 预建 {} 失败: {error}", preset.display_name);
                return;
            }
        }
    }
    if let Err(error) = settings.set_many(&[(SEED_FLAG.to_string(), serde_json::Value::Bool(true))])
    {
        tracing::warn!(
            "[内置渠道] 种子标记写入失败（下次启动重试时靠存在性检查兜底防重复）: {error}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// 进程内 settings 后端（无需落盘）。
    struct MemBackend(Mutex<serde_json::Map<String, Value>>);

    impl crate::settings_store::SettingsBackend for MemBackend {
        fn get(&self, key: &str) -> Option<Value> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set_many(&self, entries: &[(String, Value)]) -> Result<(), String> {
            let mut map = self.0.lock().unwrap();
            for (k, v) in entries {
                map.insert(k.clone(), v.clone());
            }
            Ok(())
        }
    }

    async fn fixture() -> (Repository, SettingsStore) {
        let dir = std::env::temp_dir().join(format!("waliapi-builtin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.join("test.db").display()
            ))
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let settings = SettingsStore::new(Arc::new(MemBackend(Mutex::new(Default::default()))));
        (Repository::new(pool), settings)
    }

    async fn provider_count(repo: &Repository) -> i64 {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM channels WHERE provider IN ('opencode', 'kilo')")
                .fetch_one(repo.pool())
                .await
                .unwrap();
        row.0
    }

    #[tokio::test]
    async fn seeds_both_free_channels_once() {
        let (repo, settings) = fixture().await;
        seed_builtin_free_channels(&repo, &settings).await;
        assert_eq!(provider_count(&repo).await, 2);

        let opencode: (String, String, String, String) = sqlx::query_as(
            "SELECT name, api_key, native_base_url, models FROM channels WHERE provider = 'opencode'",
        )
        .fetch_one(repo.pool())
        .await
        .unwrap();
        assert_eq!(opencode.0, "OpenCode 免费车道");
        assert_eq!(opencode.1, "public");
        assert_eq!(opencode.2, "https://opencode.ai/inference/v1");
        assert!(
            serde_json::from_str::<Vec<String>>(&opencode.3)
                .unwrap()
                .len()
                >= 3
        );

        let kilo: (String, String) =
            sqlx::query_as("SELECT name, api_key FROM channels WHERE provider = 'kilo'")
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(kilo.0, "Kilo 免费池");
        assert_eq!(kilo.1, "");

        // 种子标记已置位
        assert!(settings.get_bool(SEED_FLAG, false));
    }

    #[tokio::test]
    async fn rerun_is_idempotent() {
        let (repo, settings) = fixture().await;
        seed_builtin_free_channels(&repo, &settings).await;
        seed_builtin_free_channels(&repo, &settings).await;
        assert_eq!(provider_count(&repo).await, 2);
    }

    #[tokio::test]
    async fn deleted_seeded_channel_is_not_readded() {
        let (repo, settings) = fixture().await;
        seed_builtin_free_channels(&repo, &settings).await;
        sqlx::query("DELETE FROM channels WHERE provider IN ('opencode', 'kilo')")
            .execute(repo.pool())
            .await
            .unwrap();
        // 标记已置位：用户删除是主动行为，不再自动加回。
        seed_builtin_free_channels(&repo, &settings).await;
        assert_eq!(provider_count(&repo).await, 0);
    }

    #[tokio::test]
    async fn existing_user_channel_blocks_duplication() {
        let (repo, settings) = fixture().await;
        // 用户已自建同 provider 渠道（即使标记未置位）也不产生重复。
        sqlx::query(
            "INSERT INTO channels (id, name, type, base_url, api_key, created_at, updated_at) \
             VALUES ('u1', '我的 OpenCode', 'openai', 'https://opencode.ai/inference/v1', \
             'sk-user', '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z')",
        )
        .execute(repo.pool())
        .await
        .unwrap();
        sqlx::query("UPDATE channels SET protocol='openai', provider='opencode' WHERE id='u1'")
            .execute(repo.pool())
            .await
            .unwrap();
        seed_builtin_free_channels(&repo, &settings).await;
        let opencode_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM channels WHERE provider = 'opencode'")
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(opencode_count.0, 1);
        // Kilo 仍正常种子
        assert_eq!(provider_count(&repo).await, 2);
    }
}
