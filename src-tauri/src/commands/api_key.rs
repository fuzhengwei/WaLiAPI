use crate::db::models::{ApiKey, CreateApiKeyInput, ApiKeyStats};
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
            key: k.key,
            status: k.status,
            allowed_models: serde_json::from_str(&k.allowed_models).unwrap_or_default(),
            allowed_channels: serde_json::from_str(&k.allowed_channels).unwrap_or_default(),
            quota_limit: k.quota_limit,
            quota_used: k.quota_used,
            expires_at: k.expires_at,
            created_at: k.created_at,
            updated_at: k.updated_at,
        }
    }
}

#[tauri::command]
pub async fn get_api_keys(state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<Vec<ApiKeyDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_all_api_keys().await.map_err(|e| e.to_string()).map(|ks| ks.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn create_api_key(input: CreateApiKeyInput, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<ApiKeyDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.create_api_key(&input).await.map_err(|e| e.to_string()).map(Into::into)
}

#[derive(Debug, Deserialize)]
pub struct UpdateApiKeyInput {
    pub id: String,
    pub status: Option<i64>,
}

#[tauri::command]
pub async fn update_api_key(input: UpdateApiKeyInput, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    if let Some(status) = input.status {
        repo.update_api_key_status(&input.id, status).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_api_key(id: String, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_api_key(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_api_key_stats(state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<Vec<ApiKeyStats>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_api_key_stats().await.map_err(|e| e.to_string())
}
