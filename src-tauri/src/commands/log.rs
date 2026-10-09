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
    pub cached_tokens: i64,
    pub duration_ms: i64,
    pub error_message: Option<String>,
    pub is_stream: bool,
    pub is_retry: bool,
    pub created_at: String,
    pub request_body: Option<String>,
    pub response_choices: Option<String>,
    pub risk_level: String,
    pub risk_score: i64,
    pub risk_summary: Option<String>,
    pub security_action: String,
    pub sanitized: bool,
    pub blocked_reason: Option<String>,
    pub trace_id: Option<String>,
    pub reasoning_effort: Option<String>,
    // --- T09 observability fields (nullable; legacy rows are NULL) ---
    pub downstream_protocol: Option<String>,
    pub downstream_endpoint: Option<String>,
    pub route_group: Option<String>,
    pub upstream_protocol: Option<String>,
    pub upstream_endpoint: Option<String>,
    pub provider: Option<String>,
    pub codec_version: Option<String>,
    pub failure_class: Option<String>,
    pub identity_revision: Option<i64>,
    pub client_cancelled: Option<bool>,
    pub stream_committed: Option<bool>,
    pub upstream_type: String,
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
            cached_tokens: l.cached_tokens,
            duration_ms: l.duration_ms,
            error_message: l.error_message,
            is_stream: l.is_stream == 1,
            is_retry: l.is_retry == 1,
            created_at: l.created_at,
            request_body: l.request_body,
            response_choices: l.response_choices,
            risk_level: l.risk_level,
            risk_score: l.risk_score,
            risk_summary: l.risk_summary,
            security_action: l.security_action,
            sanitized: l.sanitized == 1,
            blocked_reason: l.blocked_reason,
            trace_id: l.trace_id,
            reasoning_effort: l.reasoning_effort,
            downstream_protocol: l.downstream_protocol,
            downstream_endpoint: l.downstream_endpoint,
            route_group: l.route_group,
            upstream_protocol: l.upstream_protocol,
            upstream_endpoint: l.upstream_endpoint,
            provider: l.provider,
            codec_version: l.codec_version,
            failure_class: l.failure_class,
            identity_revision: l.identity_revision,
            client_cancelled: l.client_cancelled.map(|v| v == 1),
            stream_committed: l.stream_committed.map(|v| v == 1),
            upstream_type: l.upstream_type,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogSummaryDto {
    #[serde(flatten)]
    pub log: LogDto,
    pub detail_level: String,
    pub detail_available: bool,
    pub started_at: Option<String>,
    pub request_body_bytes: i64,
    pub response_choices_bytes: i64,
}

impl From<crate::db::models::RequestLogSummary> for LogSummaryDto {
    fn from(s: crate::db::models::RequestLogSummary) -> Self {
        // 「简要」下请求正文已被裁短、响应正文完整，详情同样可看。
        let detail_available = (s.detail_level == "detailed" || s.detail_level == "brief")
            && (s.request_body_bytes > 0 || s.response_choices_bytes > 0);
        let log = LogDto {
            id: s.id,
            seq: s.seq,
            api_key_name: s.api_key_name,
            channel_name: s.channel_name,
            model: s.model,
            upstream_model: s.upstream_model,
            mode: s.mode,
            status_code: s.status_code,
            prompt_tokens: s.prompt_tokens,
            completion_tokens: s.completion_tokens,
            total_tokens: s.total_tokens,
            cached_tokens: s.cached_tokens,
            duration_ms: s.duration_ms,
            error_message: s.error_message,
            is_stream: s.is_stream == 1,
            is_retry: s.is_retry == 1,
            created_at: s.created_at,
            request_body: None,
            response_choices: None,
            risk_level: s.risk_level,
            risk_score: s.risk_score,
            risk_summary: s.risk_summary,
            security_action: s.security_action,
            sanitized: s.sanitized == 1,
            blocked_reason: s.blocked_reason,
            trace_id: s.trace_id,
            reasoning_effort: s.reasoning_effort,
            downstream_protocol: s.downstream_protocol,
            downstream_endpoint: s.downstream_endpoint,
            route_group: s.route_group,
            upstream_protocol: s.upstream_protocol,
            upstream_endpoint: s.upstream_endpoint,
            provider: s.provider,
            codec_version: s.codec_version,
            failure_class: s.failure_class,
            identity_revision: s.identity_revision,
            client_cancelled: s.client_cancelled.map(|v| v == 1),
            stream_committed: s.stream_committed.map(|v| v == 1),
            upstream_type: s.upstream_type,
        };
        Self {
            log,
            detail_level: s.detail_level,
            detail_available,
            started_at: s.started_at,
            request_body_bytes: s.request_body_bytes,
            response_choices_bytes: s.response_choices_bytes,
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
    pub trace_id: Option<String>,
    pub upstream_type: Option<String>,
}

#[tauri::command]
pub async fn get_logs(
    input: GetLogsInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<LogSummaryDto>, String> {
    get_logs_impl(input, &*state).await
}

pub async fn get_logs_impl(
    input: GetLogsInput,
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<LogSummaryDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let limit = input.limit.unwrap_or(50);
    let offset = input.offset.unwrap_or(0);

    let has_search = input.keyword.is_some()
        || input.api_key_name.is_some()
        || input.channel_name.is_some()
        || input.model.is_some()
        || input.date_from.is_some()
        || input.date_to.is_some()
        || input.trace_id.is_some()
        || input.upstream_type.is_some();

    let logs = if has_search {
        repo.search_log_summaries(
            input.keyword.as_deref(),
            input.api_key_name.as_deref(),
            input.channel_name.as_deref(),
            input.model.as_deref(),
            input.date_from.as_deref(),
            input.date_to.as_deref(),
            input.trace_id.as_deref(),
            input.upstream_type.as_deref(),
            limit,
            offset,
        )
        .await
    } else {
        repo.get_log_summaries(limit, offset).await
    };

    logs.map_err(|e| e.to_string())
        .map(|ls| ls.into_iter().map(LogSummaryDto::from).collect())
}

#[tauri::command]
pub async fn count_logs(
    input: GetLogsInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<i64, String> {
    count_logs_impl(input, &*state).await
}

pub async fn count_logs_impl(
    input: GetLogsInput,
    state: &std::sync::Arc<AppState>,
) -> Result<i64, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.count_log_summaries(
        input.keyword.as_deref(),
        input.api_key_name.as_deref(),
        input.channel_name.as_deref(),
        input.model.as_deref(),
        input.date_from.as_deref(),
        input.date_to.as_deref(),
        input.trace_id.as_deref(),
        input.upstream_type.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_log(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<LogDto, String> {
    get_log_impl(&id, &*state).await
}

pub async fn get_log_impl(id: &str, state: &std::sync::Arc<AppState>) -> Result<LogDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_log(id)
        .await
        .map_err(|e| e.to_string())
        .map(Into::into)
}

#[tauri::command]
pub async fn get_log_security_findings(
    log_id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<SecurityFindingDto>, String> {
    get_log_security_findings_impl(&log_id, &*state).await
}

pub async fn get_log_security_findings_impl(
    log_id: &str,
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<SecurityFindingDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_security_findings(log_id)
        .await
        .map_err(|e| e.to_string())
        .map(|fs| fs.into_iter().map(Into::into).collect())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StreamSegmentDto {
    pub seq: i64,
    pub content: String,
}

#[tauri::command]
pub async fn get_log_stream_segments(
    log_id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<StreamSegmentDto>, String> {
    get_log_stream_segments_impl(&log_id, &*state).await
}

pub async fn get_log_stream_segments_impl(
    log_id: &str,
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<StreamSegmentDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_stream_segments(log_id)
        .await
        .map_err(|e| e.to_string())
        .map(|segments| {
            segments
                .into_iter()
                .map(|(seq, content)| StreamSegmentDto { seq, content })
                .collect()
        })
}

#[tauri::command]
pub async fn delete_log(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    delete_log_impl(&id, &*state).await
}

pub async fn delete_log_impl(id: &str, state: &std::sync::Arc<AppState>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_log(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_logs_before(
    before_date: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<u64, String> {
    delete_logs_before_impl(&before_date, &*state).await
}

pub async fn delete_logs_before_impl(
    before_date: &str,
    state: &std::sync::Arc<AppState>,
) -> Result<u64, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_logs_before(before_date)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_all_logs(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<u64, String> {
    delete_all_logs_impl(&*state).await
}

pub async fn delete_all_logs_impl(state: &std::sync::Arc<AppState>) -> Result<u64, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_all_logs().await.map_err(|e| e.to_string())
}

/// 多条件组合清理日志(Task 4 颗粒度重构)。所有条件 AND 组合,空条件 = 清理全部。
/// `clear_stats=false`(默认)只删日志,统计表不动 —— 清理不影响首页统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteLogsInput {
    /// 清理该时刻之前的日志(RFC3339)。
    pub before_date: Option<String>,
    /// 清理该时刻之后的日志(与 before_date 组合成区间)。
    pub after_date: Option<String>,
    /// 保留最近 N 天,清理更早的(与 before_date 互斥,二者都传时取交集)。
    pub keep_recent_days: Option<u64>,
    /// 仅清理指定状态码的日志。
    pub status_code: Option<i64>,
    /// true=仅清理 2xx,false=仅清理非 2xx。
    pub is_success: Option<bool>,
    /// 仅清理指定渠道的日志。
    pub channel_id: Option<String>,
    /// 仅清理指定 Key 的日志。
    pub api_key_id: Option<String>,
    /// 仅清理指定模型的日志。
    pub model: Option<String>,
    /// true 时同步清除对应的 usage_stats(默认 false)。
    pub clear_stats: Option<bool>,
    /// true 时只返回匹配行数不执行删除,供前端确认弹窗展示预计影响。
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteLogsReport {
    pub dry_run: bool,
    pub matched_logs: u64,
    pub matched_stats: u64,
}

#[tauri::command]
pub async fn delete_logs(
    input: DeleteLogsInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<DeleteLogsReport, String> {
    delete_logs_impl(&input, &*state).await
}

pub async fn delete_logs_impl(
    input: &DeleteLogsInput,
    state: &std::sync::Arc<AppState>,
) -> Result<DeleteLogsReport, String> {
    let repo = Repository::new(state.db.pool.clone());
    let dry_run = input.dry_run.unwrap_or(false);
    let (matched_logs, matched_stats) = repo
        .delete_logs_matching(input, dry_run)
        .await
        .map_err(|e| e.to_string())?;
    Ok(DeleteLogsReport {
        dry_run,
        matched_logs,
        matched_stats,
    })
}

/// 独立清除历史统计数据(不删日志)。条件与 delete_logs 的统计侧一致。
#[tauri::command]
pub async fn clear_usage_stats(
    input: DeleteLogsInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<u64, String> {
    clear_usage_stats_impl(&input, &*state).await
}

pub async fn clear_usage_stats_impl(
    input: &DeleteLogsInput,
    state: &std::sync::Arc<AppState>,
) -> Result<u64, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.clear_usage_stats_matching(input)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogStatsDto {
    pub date: String,
    pub count: i64,
    pub total_tokens: i64,
}

#[tauri::command]
pub async fn get_log_stats(
    days: Option<i64>,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<LogStatsDto>, String> {
    get_log_stats_impl(days, &*state).await
}

pub async fn get_log_stats_impl(
    days: Option<i64>,
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<LogStatsDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let days = days.unwrap_or(7);
    repo.get_log_stats(days)
        .await
        .map_err(|e| e.to_string())
        .map(|ss| {
            ss.into_iter()
                .map(|s| LogStatsDto {
                    date: s.date,
                    count: s.count,
                    total_tokens: s.total_tokens,
                })
                .collect()
        })
}
