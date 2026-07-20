use crate::db::models::{RequestLog, RequestSecurityFinding};
use crate::db::repository::Repository;
use crate::AppState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct LogDto {
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
    pub duration_ms: i64,
    pub error_message: Option<String>,
    pub is_stream: bool,
    pub is_retry: bool,
    pub created_at: String,
    pub request_body: Option<String>,
    pub risk_level: String,
    pub risk_score: i64,
    pub risk_summary: Option<String>,
    pub security_action: String,
    pub sanitized: bool,
    pub blocked_reason: Option<String>,
}

impl From<RequestLog> for LogDto {
    fn from(l: RequestLog) -> Self {
        LogDto {
            id: l.id,
            seq: l.seq,
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
            risk_level: l.risk_level,
            risk_score: l.risk_score,
            risk_summary: l.risk_summary,
            security_action: l.security_action,
            sanitized: l.sanitized == 1,
            blocked_reason: l.blocked_reason,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SecurityFindingDto {
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
    pub action: Option<String>,
    pub created_at: String,
}

impl From<RequestSecurityFinding> for SecurityFindingDto {
    fn from(f: RequestSecurityFinding) -> Self {
        Self {
            id: f.id,
            log_id: f.log_id,
            phase: f.phase,
            category: f.category,
            rule_id: f.rule_id,
            severity: f.severity,
            title: f.title,
            description: f.description,
            location: f.location,
            evidence_masked: f.evidence_masked,
            action: f.action,
            created_at: f.created_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetLogsInput {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub keyword: Option<String>,
    pub api_key_name: Option<String>,
    pub channel_name: Option<String>,
    pub model: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
}

#[tauri::command]
pub async fn get_logs(
    input: GetLogsInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<LogDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let limit = input.limit.unwrap_or(50);
    let offset = input.offset.unwrap_or(0);

    let has_search = input.keyword.is_some()
        || input.api_key_name.is_some()
        || input.channel_name.is_some()
        || input.model.is_some()
        || input.date_from.is_some()
        || input.date_to.is_some();

    let logs = if has_search {
        repo.search_logs(
            input.keyword.as_deref(),
            input.api_key_name.as_deref(),
            input.channel_name.as_deref(),
            input.model.as_deref(),
            input.date_from.as_deref(),
            input.date_to.as_deref(),
            limit,
            offset,
        ).await
    } else {
        repo.get_logs(limit, offset).await
    };

    logs.map_err(|e| e.to_string()).map(|ls| ls.into_iter().map(Into::into).collect())
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
pub async fn get_log_security_findings(
    log_id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<SecurityFindingDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_security_findings(&log_id).await.map_err(|e| e.to_string()).map(|fs| fs.into_iter().map(Into::into).collect())
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
