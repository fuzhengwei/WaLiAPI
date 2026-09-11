use crate::db::models::{ApiKey, ApiKeyStats, CreateApiKeyInput};
use crate::db::repository::Repository;
use crate::AppState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiKeyDto {
    pub id: String,
    pub name: String,
    pub key: String,
    pub status: i64,
    pub allowed_models: Vec<String>,
    pub allowed_channels: Vec<String>,
    pub denied_models: Vec<String>,
    pub denied_channels: Vec<String>,
    pub quota_limit: i64,
    pub quota_used: i64,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<ApiKey> for ApiKeyDto {
    fn from(k: ApiKey) -> Self {
        ApiKeyDto {
            id: k.id,
            name: k.name,
            // FIX-13：列表/详情只回掩码，全量经 get_api_key_full 按需获取。
            key: crate::utils::secret::mask_secret(&k.key),
            status: k.status,
            allowed_models: serde_json::from_str(&k.allowed_models).unwrap_or_default(),
            allowed_channels: serde_json::from_str(&k.allowed_channels).unwrap_or_default(),
            denied_models: serde_json::from_str(&k.denied_models).unwrap_or_default(),
            denied_channels: serde_json::from_str(&k.denied_channels).unwrap_or_default(),
            quota_limit: k.quota_limit,
            quota_used: k.quota_used,
            expires_at: k.expires_at,
            created_at: k.created_at,
            updated_at: k.updated_at,
        }
    }
}

#[tauri::command]
pub async fn get_api_keys(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<ApiKeyDto>, String> {
    get_api_keys_impl(&*state).await
}

pub async fn get_api_keys_impl(state: &std::sync::Arc<AppState>) -> Result<Vec<ApiKeyDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_all_api_keys()
        .await
        .map_err(|e| e.to_string())
        .map(|ks| ks.into_iter().map(Into::into).collect())
}

/// FIX-13：按 id 按需取回完整密钥（复制/生成示例代码等显式动作）。
#[tauri::command]
pub async fn get_api_key_full(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
) -> Result<String, String> {
    get_api_key_full_impl(&*state, &id).await
}

pub async fn get_api_key_full_impl(
    state: &std::sync::Arc<AppState>,
    id: &str,
) -> Result<String, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_api_key_by_id(id)
        .await
        .map_err(|e| e.to_string())
        .map(|k| k.key)
}

#[tauri::command]
pub async fn create_api_key(
    input: CreateApiKeyInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<ApiKeyDto, String> {
    create_api_key_impl(input, &*state).await
}

pub async fn create_api_key_impl(
    input: CreateApiKeyInput,
    state: &std::sync::Arc<AppState>,
) -> Result<ApiKeyDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.create_api_key(&input)
        .await
        .map_err(|e| e.to_string())
        .map(Into::into)
}

#[derive(Debug, Deserialize)]
pub struct UpdateApiKeyInput {
    pub id: String,
    pub name: Option<String>,
    pub quota_limit: Option<i64>,
    pub status: Option<i64>,
    pub allowed_models: Option<Vec<String>>,
    pub allowed_channels: Option<Vec<String>>,
    pub denied_models: Option<Vec<String>>,
    pub denied_channels: Option<Vec<String>>,
}

#[tauri::command]
pub async fn update_api_key(
    input: UpdateApiKeyInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    update_api_key_impl(input, &*state).await
}

pub async fn update_api_key_impl(
    input: UpdateApiKeyInput,
    state: &std::sync::Arc<AppState>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    if let Some(name) = &input.name {
        repo.update_api_key_name(&input.id, name)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(quota_limit) = input.quota_limit {
        repo.update_api_key_quota(&input.id, quota_limit)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(status) = input.status {
        repo.update_api_key_status(&input.id, status)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(models) = &input.allowed_models {
        repo.update_api_key_allowed_models(&input.id, models)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(channels) = &input.allowed_channels {
        repo.update_api_key_allowed_channels(&input.id, channels)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(models) = &input.denied_models {
        repo.update_api_key_denied_models(&input.id, models)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(channels) = &input.denied_channels {
        repo.update_api_key_denied_channels(&input.id, channels)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_api_key(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    delete_api_key_impl(&id, &*state).await
}

pub async fn delete_api_key_impl(id: &str, state: &std::sync::Arc<AppState>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_api_key(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_api_key_stats(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<ApiKeyStats>, String> {
    get_api_key_stats_impl(&*state).await
}

pub async fn get_api_key_stats_impl(
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<ApiKeyStats>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_api_key_stats().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_api_key_knowledge_access(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
) -> Result<Vec<String>, String> {
    crate::server::knowledge_access::get_grants(&state.db.pool, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_api_key_knowledge_access(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
    kb_ids: Vec<String>,
) -> Result<(), String> {
    crate::server::knowledge_access::set_grants(&state.db.pool, &id, &kb_ids)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct KnowledgeConnectionTest {
    rest_status: u16,
    rest_ok: bool,
    mcp_status: u16,
    mcp_ok: bool,
}

/// 经实际监听端口验证授权，不调用模型，也不回传密钥。
#[tauri::command]
pub async fn test_api_key_knowledge_access(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
    kb_id: String,
) -> Result<KnowledgeConnectionTest, String> {
    if !state
        .server_running
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("请先启动 WaLiAPI 服务".into());
    }
    let key = Repository::new(state.db.pool.clone())
        .get_api_key_by_id(&id)
        .await
        .map_err(|_| "密钥不存在")?;
    let port = *state.server_port.read().await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法创建连接测试")?;
    let mut url =
        url::Url::parse(&format!("http://127.0.0.1:{port}/api/kb/")).map_err(|_| "服务地址无效")?;
    url.path_segments_mut()
        .map_err(|_| "服务地址无效")?
        .pop_if_empty()
        .push(&kb_id);
    let rest = client
        .get(url)
        .bearer_auth(&key.key)
        .send()
        .await
        .map_err(|_| "无法连接本机 WaLiAPI 服务")?;
    let rest_status = rest.status().as_u16();
    let rest_body = rest.json::<serde_json::Value>().await.unwrap_or_default();
    let mcp = client.post(format!("http://127.0.0.1:{port}/mcp")).bearer_auth(&key.key)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_knowledge_base_stats","arguments":{"kb_id":kb_id}}}))
        .send().await.map_err(|_| "无法连接本机 MCP 服务")?;
    let mcp_status = mcp.status().as_u16();
    let mcp_body = mcp.json::<serde_json::Value>().await.unwrap_or_default();
    Ok(KnowledgeConnectionTest {
        rest_status,
        rest_ok: rest_status == 200 && rest_body["id"] == kb_id,
        mcp_status,
        mcp_ok: mcp_status == 200 && mcp_body["result"]["isError"] == false,
    })
}
