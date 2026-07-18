use crate::db::models::RequestLog;
use crate::db::repository::Repository;
use crate::AppState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct LogDto {
    pub id: String,
    pub api_key_name: Option<String>,
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
    pub is_stream: bool,
    pub is_retry: bool,
    pub created_at: String,
    pub request_body: Option<String>,
}

impl From<RequestLog> for LogDto {
    fn from(l: RequestLog) -> Self {
        LogDto {
            id: l.id,
            api_key_name: l.api_key_name,
            channel_name: l.channel_name,
            model: l.model,
            upstream_model: l.upstream_model,
            mode: l.mode,
            status_code: l.status_code,
            prompt_tokens: l.prompt_tokens,
            completion_tokens: l.completion_tokens,
            total_tokens: l.total_tokens,
            duration_ms: l.duration_ms,
            error_message: l.error_message,
            is_stream: l.is_stream == 1,
            is_retry: l.is_retry == 1,
            created_at: l.created_at,
            request_body: l.request_body,
        }
    }
}

#[tauri::command]
pub async fn get_logs(
    limit: Option<i64>,
    offset: Option<i64>,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<LogDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let limit = limit.unwrap_or(50);
    let offset = offset.unwrap_or(0);
    repo.get_logs(limit, offset).await.map_err(|e| e.to_string()).map(|ls| ls.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn get_log(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<LogDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_log(&id).await.map_err(|e| e.to_string()).map(Into::into)
}

#[tauri::command]
pub async fn delete_log(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_log(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_logs_before(
    before_date: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<u64, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_logs_before(&before_date).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_all_logs(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<u64, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_all_logs().await.map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogStatsDto {
    pub date: String,
    pub count: i64,
    pub total_tokens: i64,
}

#[tauri::command]
pub async fn get_log_stats(days: Option<i64>, state: tauri::State<'_, std::sync::Arc<AppState>>) -> Result<Vec<LogStatsDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let days = days.unwrap_or(7);
    repo.get_log_stats(days).await.map_err(|e| e.to_string()).map(|ss| ss.into_iter().map(|s| LogStatsDto { date: s.date, count: s.count, total_tokens: s.total_tokens }).collect())
}
