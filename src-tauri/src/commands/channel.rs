use crate::db::models::{Channel, CreateChannelInput, UpdateChannelInput};
use crate::db::repository::Repository;
use crate::AppState;
use crate::adaptor::{get_adaptor, ChannelConfig};
use serde::{Deserialize, Serialize};

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
    pub created_at: String,
    pub updated_at: String,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<i64>,
}

impl From<Channel> for ChannelDto {
    fn from(c: Channel) -> Self {
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
            config: serde_json::from_str(&c.config).unwrap_or(serde_json::Value::Object(Default::default())),
            model_mapping: serde_json::from_str(&c.model_mapping).unwrap_or(serde_json::Value::Object(Default::default())),
            created_at: c.created_at,
            updated_at: c.updated_at,
            last_test_at: c.last_test_at,
            last_test_ok: c.last_test_ok,
        }
    }
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".to_string();
    }
    format!("{}...{}", &key[..4], &key[key.len()-4..])
}

fn to_dto(c: Channel) -> ChannelDto {
    c.into()
}

#[tauri::command]
pub async fn get_channels(state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<Vec<ChannelDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_all_channels().await.map_err(|e| e.to_string()).map(|cs| cs.into_iter().map(to_dto).collect())
}

#[tauri::command]
pub async fn get_channel(id: String, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<ChannelDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_channel(&id).await.map_err(|e| e.to_string()).map(to_dto)
}

#[tauri::command]
pub async fn create_channel(input: CreateChannelInput, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<ChannelDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.create_channel(&input).await.map_err(|e| e.to_string()).map(to_dto)
}

#[tauri::command]
pub async fn update_channel(input: UpdateChannelInput, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<ChannelDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.update_channel(&input).await.map_err(|e| e.to_string()).map(to_dto)
}

#[tauri::command]
pub async fn toggle_channel(id: String, status: i64, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.update_channel_status(&id, status).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_channel(id: String, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_channel(&id).await.map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestChannelResult {
    pub success: bool,
    pub message: String,
    pub latency_ms: u64,
}

#[tauri::command]
pub async fn test_channel(id: String, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<TestChannelResult, String> {
    let repo = Repository::new(state.db.pool.clone());
    let channel = repo.get_channel(&id).await.map_err(|e| e.to_string())?;
    
    let config = ChannelConfig {
        base_url: channel.base_url.clone(),
        api_key: channel.api_key.clone(),
        models: serde_json::from_str(&channel.models).unwrap_or_default(),
        model_mapping: serde_json::from_str(&channel.model_mapping).unwrap_or(serde_json::Value::Object(Default::default())),
        extra: serde_json::from_str(&channel.config).unwrap_or(serde_json::Value::Object(Default::default())),
    };

    let adaptor = get_adaptor(&channel.channel_type);
    let result = adaptor.test(&config).await.map_err(|e| e.to_string())?;
    
    repo.update_channel_test_result(&id, result.success).await.map_err(|e| e.to_string())?;
    
    Ok(TestChannelResult {
        success: result.success,
        message: result.message,
        latency_ms: result.latency_ms,
    })
}
