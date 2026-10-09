use super::models::*;
use sqlx::{Row, SqlitePool};

/// 从 RFC3339 `created_at` 提取 UTC 小时桶字符串(YYYY-MM-DDTHH:00:00.000Z)。
/// created_at 由上游写入时统一为 UTC,字符串切片即可,不做时区换算。
fn hour_bucket(created_at: &str) -> String {
    if created_at.len() >= 13 {
        format!("{}:00:00.000Z", &created_at[..13])
    } else {
        String::new()
    }
}

/// 由一条请求日志构造 usage_stats 累加增量(迁移 046)。
/// channel_id / api_key_id 为空串归一,便于聚合;hour 取 UTC 小时桶。
pub fn usage_delta_from_log(log: &super::models::RequestLog) -> UsageStatsDelta {
    UsageStatsDelta {
        hour: hour_bucket(&log.created_at),
        model: log.model.clone(),
        channel_id: log.channel_id.clone().unwrap_or_default(),
        api_key_id: log.api_key_id.clone().unwrap_or_default(),
        success: (200..300).contains(&log.status_code),
        prompt_tokens: log.prompt_tokens,
        completion_tokens: log.completion_tokens,
        total_tokens: log.total_tokens,
        cached_tokens: log.cached_tokens,
        duration_ms: log.duration_ms,
    }
}

/// 由 `keep_recent_days` 反推清理截止时刻(RFC3339 UTC),供日志侧过滤。
fn cutoff_from_keep_recent_days(days: u64) -> Option<String> {
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::days(days as i64);
    Some(cutoff.to_rfc3339())
}

/// 拼接 request_logs 的多条件删除 WHERE(条件已由调用方归一,此处只拼 AND 子句)。
/// 返回 `(where_clause, bind 顺序表)`;binds 中 Option<String> 恒为 Some(条件值)。
fn build_log_delete_where(
    input: &crate::commands::log::DeleteLogsInput,
) -> (String, Vec<Option<String>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Option<String>> = Vec::new();

    // 时间门限:before 与 keep_recent_days 二选一取更早者;after 独立成区间。
    let before = match (&input.before_date, input.keep_recent_days) {
        (Some(b), _) => Some(b.clone()),
        (None, Some(d)) => cutoff_from_keep_recent_days(d),
        (None, None) => None,
    };
    if let Some(b) = before {
        clauses.push("created_at < ?".to_string());
        binds.push(Some(b));
    }
    if let Some(a) = &input.after_date {
        clauses.push("created_at > ?".to_string());
        binds.push(Some(a.clone()));
    }
    if let Some(sc) = input.status_code {
        clauses.push("status_code = ?".to_string());
        binds.push(Some(sc.to_string()));
    }
    match input.is_success {
        Some(true) => clauses.push("status_code >= 200 AND status_code < 300".to_string()),
        Some(false) => clauses.push("NOT (status_code >= 200 AND status_code < 300)".to_string()),
        None => {}
    }
    if let Some(c) = &input.channel_id {
        clauses.push("channel_id = ?".to_string());
        binds.push(Some(c.clone()));
    }
    if let Some(k) = &input.api_key_id {
        clauses.push("api_key_id = ?".to_string());
        binds.push(Some(k.clone()));
    }
    if let Some(m) = &input.model {
        clauses.push("model = ?".to_string());
        binds.push(Some(m.clone()));
    }
    let where_clause = if clauses.is_empty() {
        "1=1".to_string()
    } else {
        clauses.join(" AND ")
    };
    (where_clause, binds)
}

/// 拼接 usage_stats 的多条件删除 WHERE。注意:
/// - 时间条件映射到小时桶(hour 前缀),粒度比日志秒级粗,属可接受口径;
/// - status_code / is_success 仅存在于日志侧,统计为聚合行(一行含成功与失败),
///   无法按状态拆行,故统计侧忽略这两个条件(与 delete_logs_matching 注释一致)。
fn build_stats_delete_where(
    input: &crate::commands::log::DeleteLogsInput,
) -> (String, Vec<Option<String>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Option<String>> = Vec::new();

    let before = match (&input.before_date, input.keep_recent_days) {
        (Some(b), _) => Some(b.clone()),
        (None, Some(d)) => cutoff_from_keep_recent_days(d),
        (None, None) => None,
    };
    if let Some(b) = before {
        // hour 为整点桶起点;日志 created_at < b 的行,其 hour <= b 的整点桶才可能包含,
        // 用 hour < 下一个整点(= 桶起点 < b 的整点)保证覆盖:直接 hour < b 的字符串比较
        // 会漏掉 b 所在整点桶内的旧行,故取 b 的小时桶起点作为严格下界。
        clauses.push("hour < ?".to_string());
        binds.push(Some(hour_bucket(&b)));
    }
    if let Some(a) = &input.after_date {
        clauses.push("hour > ?".to_string());
        binds.push(Some(hour_bucket(a)));
    }
    if let Some(c) = &input.channel_id {
        clauses.push("channel_id = ?".to_string());
        binds.push(Some(c.clone()));
    }
    if let Some(k) = &input.api_key_id {
        clauses.push("api_key_id = ?".to_string());
        binds.push(Some(k.clone()));
    }
    if let Some(m) = &input.model {
        clauses.push("model = ?".to_string());
        binds.push(Some(m.clone()));
    }
    let where_clause = if clauses.is_empty() {
        "1=1".to_string()
    } else {
        clauses.join(" AND ")
    };
    (where_clause, binds)
}

// 稳态真实请求只补健康时间戳；主动探测和失败后的恢复不受此间隔限制。
const PASSIVE_PROBE_REFRESH_SECS: u64 = 30;
const PASSIVE_PROBE_NEEDS_REFRESH: &str =
    "COALESCE(last_probe_ok, 0) != 1 OR julianday(last_probe_at) IS NULL \
     OR julianday(last_probe_at) <= julianday('now', ?1) \
     OR julianday(last_probe_at) > julianday('now')";
const CHANNEL_MODE_IS_HEALTHY: &str = "consecutive_failures = 0 AND cooldown_until IS NULL \
     AND last_failure_at IS NULL AND last_failure_reason IS NULL";

/// Parse the stored JSON endpoint list back into a Vec, or None when empty/absent.
fn parse_eps(raw: &Option<String>) -> Option<Vec<String>> {
    let s = raw.as_deref()?;
    let parsed: Vec<String> = serde_json::from_str(s).unwrap_or_default();
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

pub struct Repository {
    pool: SqlitePool,
}

impl Repository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 将请求头写入 config，避免为一个可选配置破坏旧版数据库兼容性。
    fn config_with_request_headers(
        mut config: serde_json::Value,
        headers: Option<&[ChannelRequestHeaderInput]>,
    ) -> String {
        if let Some(headers) = headers {
            let sanitized: Vec<serde_json::Value> = headers
                .iter()
                .filter(|h| !h.name.trim().is_empty())
                .map(|h| {
                    serde_json::json!({
                        "name": h.name.trim(),
                        "value": h.value,
                        "status": h.status.unwrap_or(1),
                    })
                })
                .collect();
            if let Some(object) = config.as_object_mut() {
                object.insert(
                    "request_headers".to_string(),
                    serde_json::Value::Array(sanitized),
                );
            }
        }
        serde_json::to_string(&config).unwrap_or_else(|_| "{}".to_string())
    }

    // ==================== Channel ====================

    pub async fn get_all_channels(&self) -> Result<Vec<Channel>, sqlx::Error> {
        sqlx::query_as::<_, Channel>(
            "SELECT * FROM channels ORDER BY priority DESC, created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_channel(&self, id: &str) -> Result<Channel, sqlx::Error> {
        sqlx::query_as::<_, Channel>("SELECT * FROM channels WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn get_enabled_channels(&self) -> Result<Vec<Channel>, sqlx::Error> {
        // 主动探测（C-04）：探测失败的渠道沉底不剔除（NULL=从未探测，视为健康）。
        // 同健康档内保持既有优先级/权重序；两轨候选查询共用此排序语义。
        sqlx::query_as::<_, Channel>(
            "SELECT * FROM channels WHERE status = 1 \
             ORDER BY COALESCE(last_probe_ok, 1) DESC, priority DESC, weight DESC",
        )
        .fetch_all(&self.pool)
        .await
    }

    /// 记录一次主动探测结果：更新渠道探测三列（调度排序依据，每轮都写）；
    /// 审计日志只反映**状态发生翻转的那条边**，且恢复不新增行：
    /// - 正常 → 异常：新增一行 `mode='probe'`、`status_code=502`
    /// - 异常 → 正常：就地标记那条失败行为已恢复（见 [`Self::mark_probe_recovered`]）
    /// - 持续正常 / 持续故障：不写
    ///
    /// 为什么成功探测不写：探测行不携带请求/响应正文，统计口径又一律 `is_probe = 0` 排除，
    /// 所以成功探测行没有任何读取方，留在审计日志里只会淹没真实流量 —— 默认 300 秒一轮
    /// × 每个启用渠道，实测某库最近 40 行里 31 行是这类噪音（占全库 31.5%）。
    /// 为什么恢复也不补一行：恢复是失败事件的**后续状态**，不是一次新请求；为它写一行
    /// 200 会让列表里重新出现“看起来像成功探测”的条目，与上面的目标直接冲突。
    /// 标记只改既有列（mode + upstream_model），状态码保持 502，不篡改审计历史。
    /// 直接 SQL 最小列集，不走 create_log 漏斗（探测行无正文、不受明细级别影响）。
    pub async fn record_channel_probe(
        &self,
        channel_id: &str,
        channel_name: &str,
        outcome: crate::health_probe::ProbeOutcome,
        now: &str,
    ) -> Result<(), sqlx::Error> {
        // 先取上一轮状态再更新，才能认出状态翻转。
        // COALESCE 把“从未探测”按健康处理：首次探测就失败也算一次新异常。
        let previous_ok: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(last_probe_ok, 1) FROM channels WHERE id = ?",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or(1);

        sqlx::query(
            "UPDATE channels SET last_probe_at = ?, last_probe_ok = ?, probe_latency_ms = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(now)
        .bind(i64::from(outcome.ok))
        .bind(outcome.latency_ms)
        .bind(now)
        .bind(channel_id)
        .execute(&self.pool)
        .await?;

        // 只有状态翻转才动日志，且**恢复绝不新增行**：
        //   正常 → 异常：新增一行 mode='probe' / 502（故障事件）
        //   异常 → 正常：就地标记该渠道最近一条待标记的故障行，不新增行
        //   稳态（持续正常、持续故障）：什么都不做
        match (previous_ok == 0, outcome.ok) {
            (false, false) => {
                sqlx::query(
                    "INSERT INTO request_logs (id, seq, channel_id, channel_name, model, mode, \
                     status_code, duration_ms, is_stream, is_retry, created_at, risk_level, \
                     security_action, upstream_type, is_probe) \
                     VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM request_logs), ?, ?, ?, \
                     'probe', 502, ?, 0, 0, ?, 'low', 'audit', 'channel', 1)",
                )
                .bind(crate::utils::id::new_id())
                .bind(channel_id)
                .bind(channel_name)
                .bind(channel_name)
                .bind(outcome.latency_ms)
                .bind(now)
                .execute(&self.pool)
                .await?;
            }
            (true, true) => {
                self.mark_probe_recovered(channel_id).await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// 把某渠道最近一条“尚未标记恢复”的探测失败行**就地标记**为已恢复。
    ///
    /// 只改两个既有列：`mode` 由 'probe' 变 'probe_recovered'（机器可读标记），
    /// `upstream_model` 写标记文案（前端模型列本就渲染 `model → upstream_model`，
    /// 于是列表里直接显示 `渠道名 → 已恢复`）。**状态码保持 502** —— 那一刻确实
    /// 失败了，不篡改审计历史；也不新增任何行。
    /// 标记后该行不再匹配 `mode='probe'`，所以再故障→写新行、再恢复→标记新行，天然幂等。
    async fn mark_probe_recovered(&self, channel_id: &str) -> Result<(), sqlx::Error> {
        const MARK: &str = "已恢复";
        let result = sqlx::query(
            "UPDATE request_logs SET mode = 'probe_recovered', upstream_model = ? \
             WHERE id = (SELECT id FROM request_logs WHERE channel_id = ? AND is_probe = 1 \
                         AND mode = 'probe' AND status_code = 502 \
                         ORDER BY seq DESC LIMIT 1)",
        )
        .bind(MARK)
        .bind(channel_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            // 没有待标记的故障行（故障发生在老版本、或已被启动清理删掉）。
            // 按“零新增行”的要求什么都不做，不为恢复事件补写日志。
            tracing::debug!("[探测] 渠道 {channel_id} 已恢复，但库中没有待标记的探测失败行");
        }
        Ok(())
    }

    /// 启动期一次性清理：删掉历史上写入的“成功形态”探测行。
    ///
    /// 覆盖两种来源：0.3.1 及更早每轮探测都写一行的“探测成功”；以及中间版本为“渠道已恢复”
    /// 补写的 200 行（该设计已改为就地标记失败行，不再新增行）。新语义下任何 `is_probe = 1`
    /// 且 2xx 的行都不该存在，所以条件就是这一条 —— 真实审计记录（`is_probe = 0`）与探测
    /// 失败行（502，含已标记恢复的）一律不动。
    pub async fn purge_successful_probe_logs(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM request_logs \
             WHERE is_probe = 1 AND status_code >= 200 AND status_code < 300",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// 被动反哺：失败或未知状态在真实请求成功后立即恢复。
    /// 持续成功时，健康时间戳至多每 30 秒刷新一次；表示最近落库的健康证据，
    /// 不再表示每次请求时间。主动探测仍每轮写入，探测延迟保持原值。
    /// 数据库状态决定是否刷新，不依赖进程缓存，重启和并发调用共用同一条件。
    /// 与 mode_health 的成功清除相互独立、各管各的表。
    pub async fn mark_probe_ok(&self, channel_id: &str) {
        if let Err(error) = self.mark_probe_ok_if_needed(channel_id).await {
            tracing::warn!("[探测] 被动反哺失败（channel {channel_id}）: {error}");
        }
    }

    async fn mark_probe_ok_if_needed(&self, channel_id: &str) -> Result<u64, sqlx::Error> {
        let refresh_modifier = format!("-{PASSIVE_PROBE_REFRESH_SECS} seconds");
        // 先读持久状态，稳态请求不提交 UPDATE，避免 no-op UPDATE 也争夺写锁。
        // 无法解析/未来的时间戳也刷新，避免旧数据或时钟回拨造成无限跳过。
        let needs_refresh = sqlx::query_scalar::<_, bool>(&format!(
            "SELECT ({PASSIVE_PROBE_NEEDS_REFRESH}) FROM channels WHERE id = ?2"
        ))
        .bind(&refresh_modifier)
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?;
        if !needs_refresh.unwrap_or(false) {
            return Ok(0);
        }

        // 多个读取同时发现需要刷新时，由同一个 SQL 条件合并为一次实际写入。
        // 时间取 SQL 执行时刻，排队的成功请求不会用较旧时间覆盖较新健康证据。
        let result = sqlx::query(&format!(
            "UPDATE channels SET last_probe_ok = 1, \
             last_probe_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?2 AND ({PASSIVE_PROBE_NEEDS_REFRESH})"
        ))
        .bind(&refresh_modifier)
        .bind(channel_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Return channels that are enabled and not cooling down for this exact
    /// endpoint + transport mode. Stream and non-stream health are deliberately
    /// independent: a broken JSON response must not disable an otherwise healthy
    /// SSE route.
    pub async fn get_enabled_channels_for_mode(
        &self,
        endpoint: &str,
        is_stream: bool,
        now: &str,
    ) -> Result<Vec<Channel>, sqlx::Error> {
        sqlx::query_as::<_, Channel>(
            "SELECT c.*
             FROM channels c
             LEFT JOIN channel_mode_health h
               ON h.channel_id = c.id
              AND h.endpoint = ?
              AND h.is_stream = ?
             WHERE c.status = 1
               AND (h.cooldown_until IS NULL OR h.cooldown_until <= ?)
             ORDER BY COALESCE(c.last_probe_ok, 1) DESC, c.priority DESC, c.weight DESC",
        )
        .bind(endpoint)
        .bind(i64::from(is_stream))
        .bind(now)
        .fetch_all(&self.pool)
        .await
    }

    /// 成功后清除对应端点、流式模式的故障状态，保留轻量恢复记录。
    /// 全健康行先读后跳过写入；缺行首次插入，故障立即恢复，不使用请求间缓存。
    pub async fn record_channel_mode_success(
        &self,
        channel_id: &str,
        endpoint: &str,
        is_stream: bool,
    ) -> Result<(), sqlx::Error> {
        let healthy = sqlx::query_scalar::<_, bool>(&format!(
            "SELECT ({CHANNEL_MODE_IS_HEALTHY}) FROM channel_mode_health \
             WHERE channel_id = ? AND endpoint = ? AND is_stream = ?"
        ))
        .bind(channel_id)
        .bind(endpoint)
        .bind(i64::from(is_stream))
        .fetch_optional(&self.pool)
        .await?;
        if healthy == Some(true) {
            return Ok(());
        }

        // 并发读取缺行或故障后，仅第一条成功真正插入/恢复该行。
        sqlx::query(&format!(
            "INSERT INTO channel_mode_health
                (channel_id, endpoint, is_stream, consecutive_failures, cooldown_until, last_failure_at, last_failure_reason)
             VALUES (?, ?, ?, 0, NULL, NULL, NULL)
             ON CONFLICT(channel_id, endpoint,is_stream) DO UPDATE SET
                consecutive_failures = 0,
                cooldown_until = NULL,
                last_failure_at = NULL,
                last_failure_reason = NULL
             WHERE NOT ({CHANNEL_MODE_IS_HEALTHY})"
        ))
        .bind(channel_id)
        .bind(endpoint)
        .bind(i64::from(is_stream))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A second consecutive transport/protocol failure temporarily removes only
    /// this mode from routing. The caller supplies a short, redacted reason.
    pub async fn record_channel_mode_failure(
        &self,
        channel_id: &str,
        endpoint: &str,
        is_stream: bool,
        failure_at: &str,
        cooldown_until: &str,
        reason: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO channel_mode_health
                (channel_id, endpoint, is_stream, consecutive_failures, cooldown_until, last_failure_at, last_failure_reason)
             VALUES (?, ?, ?, 1, NULL, ?, ?)
             ON CONFLICT(channel_id, endpoint,is_stream) DO UPDATE SET
                consecutive_failures = channel_mode_health.consecutive_failures + 1,
                cooldown_until = CASE
                    WHEN channel_mode_health.consecutive_failures + 1 >= 2 THEN ?
                    ELSE NULL
                END,
                last_failure_at = excluded.last_failure_at,
                last_failure_reason = excluded.last_failure_reason",
        )
        .bind(channel_id)
        .bind(endpoint)
        .bind(i64::from(is_stream))
        .bind(failure_at)
        .bind(reason)
        .bind(cooldown_until)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Resolve the full identity to persist for a create/update, and the
    /// legacy dual-write pair (type, base_url).
    ///
    /// * New fields all present (protocol/provider/native_base_url/native
    ///   endpoints) => identity written from them, dual-write via
    ///   `new_to_legacy`, revision = max(current, 1).
    /// * Otherwise => live-infer from legacy fields; identity revision stays 0.
    fn plan_channel_identity(
        protocol: &Option<String>,
        provider: &Option<String>,
        native_base_url: &Option<String>,
        native_endpoints: &Option<Vec<String>>,
        current_revision: i64,
        legacy_type: &str,
        legacy_base_url: &str,
        config_json: &str,
    ) -> (
        crate::core::channel_identity::ChannelIdentity,
        String,
        String,
        String,
    ) {
        use crate::core::channel_identity::{
            resolve_channel_identity, ChannelIdentity, ChannelIdentityRow,
        };

        let protocol_ok = protocol
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let provider_ok = provider
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let base_ok = native_base_url
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let eps_ok = native_endpoints
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false);

        // Determine which legacy fields to infer from. If the caller supplied a
        // legacy type/base_url (e.g. old frontend payload), use those; else the
        // current row values.
        let (identity, legacy_type_out, legacy_base_out) =
            if protocol_ok && provider_ok && base_ok && eps_ok {
                let protocol = protocol.clone().unwrap_or_default();
                let identity = ChannelIdentity {
                    protocol: protocol.clone(),
                    provider: provider.clone().unwrap_or_default(),
                    native_base_url: crate::core::channel_identity::normalize_native_base_url(
                        protocol.as_str(),
                        native_base_url.as_deref().unwrap_or_default(),
                    ),
                    native_endpoints: native_endpoints.clone().unwrap_or_default(),
                    identity_revision: current_revision.max(1),
                    legacy_executor_override: None,
                    executor_kind: crate::core::channel_identity::derive_executor_kind(&protocol)
                        .to_string(),
                    inferred: false,
                };
                let (lt, lb) = crate::core::channel_identity::new_to_legacy(&identity);
                (identity, lt, lb)
            } else {
                // Legacy infer from the legacy fields (old payload or current row).
                let row = ChannelIdentityRow {
                    channel_type: legacy_type.to_string(),
                    base_url: legacy_base_url.to_string(),
                    config: serde_json::from_str(config_json)
                        .unwrap_or(serde_json::Value::Object(Default::default())),
                    protocol: protocol.clone(),
                    provider: provider.clone(),
                    native_base_url: native_base_url.clone(),
                    native_endpoints: native_endpoints
                        .as_ref()
                        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())),
                    preset_revision: None,
                    identity_revision: 0,
                    legacy_executor_override: None,
                };
                let identity = resolve_channel_identity(&row);
                let lt = legacy_type.to_string();
                let lb = legacy_base_url.to_string();
                (identity, lt, lb)
            };

        let endpoints_json =
            serde_json::to_string(&identity.native_endpoints).unwrap_or_else(|_| "[]".to_string());
        (identity, legacy_type_out, legacy_base_out, endpoints_json)
    }

    pub async fn create_channel(&self, input: &CreateChannelInput) -> Result<Channel, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        let models = serde_json::to_string(&input.models).unwrap_or_else(|_| "[]".to_string());
        let config = Self::config_with_request_headers(
            input
                .config
                .clone()
                .unwrap_or_else(|| serde_json::json!({})),
            input.request_headers.as_deref(),
        );
        let model_mapping = input
            .model_mapping
            .as_ref()
            .map(|v| {
                let mut normalized = v.clone();
                crate::db::models::normalize_model_mapping(&mut normalized);
                serde_json::to_string(&normalized).unwrap_or_else(|_| "{}".to_string())
            })
            .unwrap_or_else(|| "{}".to_string());
        let model_mapping_disabled = input
            .model_mapping_disabled
            .as_ref()
            .map(|v| {
                let mut normalized = v.clone();
                crate::db::models::normalize_model_mapping_disabled(&mut normalized);
                serde_json::to_string(&normalized).unwrap_or_else(|_| "[]".to_string())
            })
            .unwrap_or_else(|| "[]".to_string());

        let (identity, legacy_type, legacy_base, endpoints_json) = Self::plan_channel_identity(
            &input.protocol,
            &input.provider,
            &input.native_base_url,
            &input.native_endpoints,
            0,
            &input.channel_type,
            &input.base_url,
            &config,
        );

        sqlx::query(
            "INSERT INTO channels (
                id, name, type, base_url, api_key, models, status, priority, weight,
                config, model_mapping, model_mapping_disabled, timeout_secs,
                protocol, provider, native_base_url, native_endpoints,
                preset_revision, identity_revision, legacy_executor_override,
                created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&input.name)
        .bind(&legacy_type)
        .bind(&legacy_base)
        .bind(&input.api_key)
        .bind(&models)
        .bind(input.priority.unwrap_or(0))
        .bind(input.weight.unwrap_or(1))
        .bind(&config)
        .bind(&model_mapping)
        .bind(&model_mapping_disabled)
        .bind(input.timeout_secs.unwrap_or(300))
        .bind(&identity.protocol)
        .bind(&identity.provider)
        .bind(&identity.native_base_url)
        .bind(&endpoints_json)
        .bind(&input.preset_revision)
        .bind(identity.identity_revision)
        .bind(&identity.legacy_executor_override)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        // Insert extra API keys for load balancing (migration 023).
        if let Some(extra) = &input.extra_keys {
            if !extra.is_empty() {
                self.replace_channel_api_keys(&id, extra).await?;
            }
        }

        self.get_channel(&id).await
    }

    /// Import a channel with FULL business-field fidelity (T09).
    ///
    /// Deliberately NOT `create_channel`: that writer hard-codes `status = 1`
    /// and a default `timeout_secs`, which would silently corrupt round-trips
    /// of disabled/slow channels.  This narrow import-write API persists every
    /// business field verbatim (status/priority/weight/timeout_secs/config
    /// unknown keys/URL/key/models/array model_mapping) and copies the identity
    /// columns exactly as resolved by `commands::import_export`.
    pub async fn import_channel(&self, input: &ImportChannelInput) -> Result<Channel, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        let models = serde_json::to_string(&input.models).unwrap_or_else(|_| "[]".to_string());
        let config = serde_json::to_string(&input.config).unwrap_or_else(|_| "{}".to_string());
        let model_mapping = {
            let mut normalized = input.model_mapping.clone();
            crate::db::models::normalize_model_mapping(&mut normalized);
            serde_json::to_string(&normalized).unwrap_or_else(|_| "{}".to_string())
        };
        let model_mapping_disabled = input
            .model_mapping_disabled
            .as_ref()
            .map(|v| {
                let mut normalized = v.clone();
                crate::db::models::normalize_model_mapping_disabled(&mut normalized);
                serde_json::to_string(&normalized).unwrap_or_else(|_| "[]".to_string())
            })
            .unwrap_or_else(|| "[]".to_string());
        let endpoints_json = input
            .native_endpoints
            .as_ref()
            .map(|eps| serde_json::to_string(eps).unwrap_or_else(|_| "[]".to_string()))
            .unwrap_or_else(|| "[]".to_string());

        sqlx::query(
            "INSERT INTO channels (
                id, name, type, base_url, api_key, models, status, priority, weight,
                config, model_mapping, model_mapping_disabled, timeout_secs,
                protocol, provider, native_base_url, native_endpoints,
                preset_revision, identity_revision, legacy_executor_override,
                created_at, updated_at, last_test_at, last_test_ok, api_key_enabled)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&input.name)
        .bind(&input.channel_type)
        .bind(&input.base_url)
        .bind(&input.api_key)
        .bind(&models)
        .bind(input.status)
        .bind(input.priority)
        .bind(input.weight)
        .bind(&config)
        .bind(&model_mapping)
        .bind(&model_mapping_disabled)
        .bind(input.timeout_secs)
        .bind(&input.protocol)
        .bind(&input.provider)
        .bind(&input.native_base_url)
        .bind(&endpoints_json)
        .bind(&input.preset_revision)
        .bind(input.identity_revision)
        .bind(&input.legacy_executor_override)
        .bind(&now)
        .bind(&now)
        .bind(&input.last_test_at)
        .bind(input.last_test_ok)
        .bind(input.api_key_enabled.unwrap_or(1))
        .execute(&self.pool)
        .await?;

        self.get_channel(&id).await
    }

    pub async fn update_channel(&self, input: &UpdateChannelInput) -> Result<Channel, sqlx::Error> {
        // Explicit empty native endpoints are rejected (T02 DTO contract):
        // None = keep, empty Vec = invalid configuration.
        if let Some(eps) = &input.native_endpoints {
            if eps.is_empty() {
                return Err(sqlx::Error::Protocol(
                    "native_endpoints must not be explicitly empty; omit it to keep current value"
                        .to_string(),
                ));
            }
        }

        let now = now_iso();
        let mut tx = self.pool.begin().await?;
        let existing_config: serde_json::Value =
            sqlx::query_scalar("SELECT config FROM channels WHERE id = ?")
                .bind(&input.id)
                .fetch_one(&mut *tx)
                .await
                .ok()
                .and_then(|raw: String| serde_json::from_str(&raw).ok())
                .unwrap_or_else(|| serde_json::json!({}));

        // STEP 1: write the legacy/business fields exactly as the payload
        // provides them (old frontend payloads). Naming type/base_url/config in
        // this UPDATE fires the invalidation trigger (revision 0), which is what
        // makes an old binary's legacy edit re-infer identity on next read.
        let mut q = sqlx::QueryBuilder::new("UPDATE channels SET updated_at = ");

        q.push_bind(&now);

        if let Some(name) = &input.name {
            q.push(", name = ").push_bind(name);
        }
        if let Some(ct) = &input.channel_type {
            q.push(", type = ").push_bind(ct);
        }
        if let Some(base_url) = &input.base_url {
            q.push(", base_url = ").push_bind(base_url);
        }
        if let Some(api_key) = &input.api_key {
            q.push(", api_key = ").push_bind(api_key);
        }
        if input.clear_api_key == Some(true) {
            q.push(", api_key = ").push_bind("");
        }
        if let Some(models) = &input.models {
            let m = serde_json::to_string(models).unwrap_or_else(|_| "[]".to_string());
            q.push(", models = ").push_bind(m);
        }
        if let Some(status) = input.status {
            q.push(", status = ").push_bind(status);
        }
        if let Some(priority) = input.priority {
            q.push(", priority = ").push_bind(priority);
        }
        if let Some(weight) = input.weight {
            q.push(", weight = ").push_bind(weight);
        }
        if input.config.is_some() || input.request_headers.is_some() {
            let config = input.config.clone().unwrap_or(existing_config.clone());
            let c = Self::config_with_request_headers(config, input.request_headers.as_deref());
            q.push(", config = ").push_bind(c);
        }
        if let Some(mapping) = &input.model_mapping {
            let mut normalized = mapping.clone();
            crate::db::models::normalize_model_mapping(&mut normalized);
            let m = serde_json::to_string(&normalized).unwrap_or_else(|_| "{}".to_string());
            q.push(", model_mapping = ").push_bind(m);
        }
        if let Some(disabled) = &input.model_mapping_disabled {
            let mut normalized = disabled.clone();
            crate::db::models::normalize_model_mapping_disabled(&mut normalized);
            let d = serde_json::to_string(&normalized).unwrap_or_else(|_| "[]".to_string());
            q.push(", model_mapping_disabled = ").push_bind(d);
        }
        if let Some(timeout_secs) = input.timeout_secs {
            q.push(", timeout_secs = ").push_bind(timeout_secs);
        }

        q.push(" WHERE id = ").push_bind(&input.id);
        q.build().execute(&mut *tx).await?;

        // Read the row as it now stands (post-legacy-write, post-trigger) so the
        // identity plan starts from the persisted state: if the trigger fired,
        // identity fields are NULL/revision 0 and we re-infer.
        let row: Channel = sqlx::query_as("SELECT * FROM channels WHERE id = ?")
            .bind(&input.id)
            .fetch_one(&mut *tx)
            .await?;

        // Effective fields: what was written this UPDATE, else the row's value.
        let eff_type = input
            .channel_type
            .clone()
            .unwrap_or_else(|| row.channel_type.clone());
        let eff_base = input
            .base_url
            .clone()
            .unwrap_or_else(|| row.base_url.clone());
        let eff_config = input
            .config
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| row.config.clone());
        let eff_protocol = input.protocol.clone().or_else(|| row.protocol.clone());
        let eff_provider = input.provider.clone().or_else(|| row.provider.clone());
        let eff_native_base = input
            .native_base_url
            .clone()
            .or_else(|| row.native_base_url.clone());
        let eff_eps = input
            .native_endpoints
            .clone()
            .or_else(|| parse_eps(&row.native_endpoints));
        let eff_preset_revision = input
            .preset_revision
            .clone()
            .or_else(|| row.preset_revision.clone());

        // Compute the full identity plan. On a full identity write this yields
        // the DERIVED legacy dual-write pair (type/base_url from new_to_legacy).
        let (identity, legacy_type, legacy_base, endpoints_json) = Self::plan_channel_identity(
            &eff_protocol,
            &eff_provider,
            &eff_native_base,
            &eff_eps,
            row.identity_revision,
            &eff_type,
            &eff_base,
            &eff_config,
        );

        // STEP 1b: write the DERIVED legacy dual-write pair in a separate
        // statement (never merged with the identity write — 不得单条 UPDATE
        // 同时写新旧). On a full-identity write this repairs a raw/empty
        // base_url to the old-code compat root (F1); on a legacy write it is a
        // no-op equal to the effective fields.
        sqlx::query("UPDATE channels SET type = ?, base_url = ? WHERE id = ?")
            .bind(&legacy_type)
            .bind(&legacy_base)
            .bind(&input.id)
            .execute(&mut *tx)
            .await?;

        // STEP 2: final UPDATE writes the complete new identity + current
        // revision. If the identity plan fell back to legacy inference, the
        // revision stays 0 and the resolver live-infers on read.
        sqlx::query(
            "UPDATE channels SET
                protocol = ?, provider = ?, native_base_url = ?, native_endpoints = ?,
                preset_revision = ?, identity_revision = ?, legacy_executor_override = ?
             WHERE id = ?",
        )
        .bind(&identity.protocol)
        .bind(&identity.provider)
        .bind(&identity.native_base_url)
        .bind(&endpoints_json)
        .bind(&eff_preset_revision)
        .bind(identity.identity_revision)
        .bind(&identity.legacy_executor_override)
        .bind(&input.id)
        .execute(&mut *tx)
        .await?;

        // Multi-key: replace extra keys if provided (full-replace semantics).
        if let Some(extra) = &input.extra_keys {
            // 掩码写回防护：列表 DTO 的 api_key 是掩码值，编辑表单里未进入
            // 编辑态的存量 Key 提交的仍是掩码串。按前端回传的 id 找到库中
            // 真实值，凡提交值 == 该 Key 的掩码形式即视为「未修改」，用真实
            // 值落库，避免掩码串覆盖真实凭证。
            let existing_values: std::collections::HashMap<String, String> =
                match sqlx::query_as::<_, (String, String)>(
                    "SELECT id, api_key FROM channel_api_keys WHERE channel_id = ?",
                )
                .bind(&input.id)
                .fetch_all(&mut *tx)
                .await
                {
                    Ok(rows) => rows.into_iter().collect(),
                    Err(_) => Default::default(),
                };
            // Delete + re-insert within the same transaction.
            sqlx::query("DELETE FROM channel_api_keys WHERE channel_id = ?")
                .bind(&input.id)
                .execute(&mut *tx)
                .await?;
            let now_k = &now;
            for k in extra {
                let real_value = match k.id.as_deref().and_then(|id| existing_values.get(id)) {
                    Some(stored) if k.api_key == crate::utils::secret::mask_secret(stored) => {
                        stored.clone()
                    }
                    _ => k.api_key.clone(),
                };
                let kid = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO channel_api_keys (id, channel_id, api_key, weight, status, created_at, updated_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&kid)
                .bind(&input.id)
                .bind(&real_value)
                .bind(k.weight.unwrap_or(1))
                .bind(k.status.unwrap_or(1))
                .bind(now_k)
                .bind(now_k)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;

        self.get_channel(&input.id).await
    }

    pub async fn update_channel_status(&self, id: &str, status: i64) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE channels SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_channel(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM channels WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ==================== Channel API Keys (multi-key) ====================

    /// Get all extra API keys for a channel (excluding the primary key stored
    /// in channels.api_key). Returns enabled keys first, ordered by weight desc.
    pub async fn get_channel_api_keys(
        &self,
        channel_id: &str,
    ) -> Result<Vec<ChannelApiKey>, sqlx::Error> {
        sqlx::query_as::<_, ChannelApiKey>(
            "SELECT * FROM channel_api_keys WHERE channel_id = ? ORDER BY status DESC, weight DESC, created_at ASC",
        )
        .bind(channel_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Replace all extra keys for a channel. Called during create/update to
    /// implement full-replace semantics (frontend sends the complete key list).
    pub async fn replace_channel_api_keys(
        &self,
        channel_id: &str,
        keys: &[ChannelApiKeyInput],
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM channel_api_keys WHERE channel_id = ?")
            .bind(channel_id)
            .execute(&mut *tx)
            .await?;

        for k in keys {
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO channel_api_keys (id, channel_id, api_key, weight, status, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(channel_id)
            .bind(&k.api_key)
            .bind(k.weight.unwrap_or(1))
            .bind(k.status.unwrap_or(1))
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// Toggle a single channel API key's enabled/disabled status.
    pub async fn toggle_channel_api_key(
        &self,
        key_id: &str,
        status: i64,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE channel_api_keys SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(key_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 启用/停用渠道主 Key（迁移 044 窄列更新）。
    /// 刻意不触碰 config 列——015 的身份失效触发器由 UPDATE OF config 触发，
    /// 窄列更新可完全绕开身份重建。
    pub async fn toggle_channel_primary_key(
        &self,
        channel_id: &str,
        enabled: bool,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE channels SET api_key_enabled = ?, updated_at = ? WHERE id = ?")
            .bind(if enabled { 1_i64 } else { 0_i64 })
            .bind(&now)
            .bind(channel_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete a single channel API key.
    pub async fn delete_channel_api_key(&self, key_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM channel_api_keys WHERE id = ?")
            .bind(key_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_channel_test_result(&self, id: &str, ok: bool) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE channels SET last_test_at = ?, last_test_ok = ? WHERE id = ?")
            .bind(&now)
            .bind(if ok { 1 } else { 0 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn reorder_channels(&self, ordered_ids: &[String]) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let mut tx = self.pool.begin().await?;
        for (i, id) in ordered_ids.iter().enumerate() {
            let priority = (ordered_ids.len() - i) as i64;
            sqlx::query("UPDATE channels SET priority = ?, updated_at = ? WHERE id = ?")
                .bind(priority)
                .bind(&now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ==================== API Key ====================

    pub async fn get_all_api_keys(&self) -> Result<Vec<ApiKey>, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_api_key_by_key(&self, key: &str) -> Result<ApiKey, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys WHERE key = ? AND status = 1")
            .bind(key)
            .fetch_one(&self.pool)
            .await
    }

    /// FIX-13：按 id 取回完整密钥记录（管理面按需 reveal）。
    pub async fn get_api_key_by_id(&self, id: &str) -> Result<ApiKey, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn create_api_key(&self, input: &CreateApiKeyInput) -> Result<ApiKey, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        let key = match &input.key {
            Some(custom) => {
                let trimmed = custom.trim();
                if trimmed.is_empty() {
                    format!("sk-waliapi-{}", uuid::Uuid::new_v4().simple())
                } else {
                    // 校验格式：只允许字母、数字、连字符、下划线
                    if !trimmed
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        return Err(sqlx::Error::Protocol(
                            "密钥只能包含字母、数字、连字符和下划线".to_string(),
                        ));
                    }
                    // 校验长度
                    if trimmed.len() < 8 || trimmed.len() > 128 {
                        return Err(sqlx::Error::Protocol(
                            "密钥长度需在 8-128 个字符之间".to_string(),
                        ));
                    }
                    // 校验唯一性
                    let exists: Option<(String,)> =
                        sqlx::query_as("SELECT id FROM api_keys WHERE key = ?")
                            .bind(trimmed)
                            .fetch_optional(&self.pool)
                            .await?;
                    if exists.is_some() {
                        return Err(sqlx::Error::Protocol(
                            "该密钥已存在，请更换后重试".to_string(),
                        ));
                    }
                    trimmed.to_string()
                }
            }
            None => format!("sk-waliapi-{}", uuid::Uuid::new_v4().simple()),
        };
        let allowed_models =
            serde_json::to_string(&input.allowed_models.clone().unwrap_or_default())
                .unwrap_or_else(|_| "[]".to_string());
        let allowed_channels =
            serde_json::to_string(&input.allowed_channels.clone().unwrap_or_default())
                .unwrap_or_else(|_| "[]".to_string());
        let denied_models = serde_json::to_string(&input.denied_models.clone().unwrap_or_default())
            .unwrap_or_else(|_| "[]".to_string());
        let denied_channels =
            serde_json::to_string(&input.denied_channels.clone().unwrap_or_default())
                .unwrap_or_else(|_| "[]".to_string());

        // 创建和默认授权共用写事务，避免并发新建知识库时漏掉授权或留下半次创建。
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO api_keys (id, name, key, status, allowed_models, allowed_channels, denied_models, denied_channels, quota_limit, quota_used, created_at, updated_at)
             VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, 0, ?, ?)"
        )
        .bind(&id)
        .bind(&input.name)
        .bind(&key)
        .bind(&allowed_models)
        .bind(&allowed_channels)
        .bind(&denied_models)
        .bind(&denied_channels)
        .bind(input.quota_limit.unwrap_or(-1))
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO api_key_knowledge_access (api_key_id, kb_id)
             SELECT ?, id FROM kb_knowledge_bases",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;

        let api_key = sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys WHERE id = ?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(api_key)
    }

    pub async fn update_api_key_status(&self, id: &str, status: i64) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE api_keys SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_api_key_allowed_models(
        &self,
        id: &str,
        models: &[String],
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let json = serde_json::to_string(models).unwrap_or_else(|_| "[]".to_string());
        sqlx::query("UPDATE api_keys SET allowed_models = ?, updated_at = ? WHERE id = ?")
            .bind(&json)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_api_key_allowed_channels(
        &self,
        id: &str,
        channels: &[String],
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let json = serde_json::to_string(channels).unwrap_or_else(|_| "[]".to_string());
        sqlx::query("UPDATE api_keys SET allowed_channels = ?, updated_at = ? WHERE id = ?")
            .bind(&json)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_api_key_denied_models(
        &self,
        id: &str,
        models: &[String],
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let json = serde_json::to_string(models).unwrap_or_else(|_| "[]".to_string());
        sqlx::query("UPDATE api_keys SET denied_models = ?, updated_at = ? WHERE id = ?")
            .bind(&json)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_api_key_denied_channels(
        &self,
        id: &str,
        channels: &[String],
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let json = serde_json::to_string(channels).unwrap_or_else(|_| "[]".to_string());
        sqlx::query("UPDATE api_keys SET denied_channels = ?, updated_at = ? WHERE id = ?")
            .bind(&json)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_api_key_name(&self, id: &str, name: &str) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE api_keys SET name = ?, updated_at = ? WHERE id = ?")
            .bind(name)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_api_key_quota(
        &self,
        id: &str,
        quota_limit: i64,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE api_keys SET quota_limit = ?, updated_at = ? WHERE id = ?")
            .bind(quota_limit)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_api_key(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn increment_quota(&self, id: &str, tokens: i64) -> Result<(), sqlx::Error> {
        // 配额递增带封顶（C-01）：quota_limit > 0 时 quota_used 不越过上限；
        // -1/0 表示不限，纯加法。检查发生在请求前、递增在响应后，并发窗口内
        // 多个在途请求可同时放行——封顶保证越界幅度有界，属已知可接受行为。
        sqlx::query(
            "UPDATE api_keys SET quota_used = CASE WHEN quota_limit > 0 \
             THEN MIN(quota_used + ?, quota_limit) ELSE quota_used + ? END WHERE id = ?",
        )
        .bind(tokens)
        .bind(tokens)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ==================== Auth Account ====================

    pub async fn list_auth_accounts(&self) -> Result<Vec<AuthAccount>, sqlx::Error> {
        // sort_order 越大越靠前（手动拖拽排序）；同值时按创建时间倒序（新账号在前）。
        sqlx::query_as::<_, AuthAccount>(
            "SELECT * FROM auth_accounts ORDER BY sort_order DESC, created_at DESC, id DESC",
        )
        .fetch_all(&self.pool)
        .await
    }

    /// 手动拖拽排序：按传入顺序重写 sort_order（倒序赋值，首元素最大）。
    pub async fn reorder_auth_accounts(&self, ordered_ids: &[String]) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let mut tx = self.pool.begin().await?;
        for (i, id) in ordered_ids.iter().enumerate() {
            let sort_order = (ordered_ids.len() - i) as i64;
            sqlx::query("UPDATE auth_accounts SET sort_order = ?, updated_at = ? WHERE id = ?")
                .bind(sort_order)
                .bind(&now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_auth_account(&self, id: &str) -> Result<AuthAccount, sqlx::Error> {
        sqlx::query_as::<_, AuthAccount>("SELECT * FROM auth_accounts WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn delete_auth_account(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM auth_accounts WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Atomically save a login/import result. On a `(provider, account_id)`
    /// conflict, credential/display attributes are refreshed while user-owned
    /// route settings (`id`, label, priority, weight, disabled) stay intact.
    pub async fn upsert_by_provider_account_id(
        &self,
        input: &AuthAccountUpsert,
    ) -> Result<AuthAccount, sqlx::Error> {
        if input.provider.trim().is_empty()
            || input.account_id.trim().is_empty()
            || input.label.trim().is_empty()
        {
            return Err(sqlx::Error::Protocol(
                "provider, account_id, and label must not be empty".into(),
            ));
        }
        let now = now_iso();
        let attributes_json = serde_json::to_string(&input.attributes)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let payload_json = serde_json::to_string(&input.payload)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let id = crate::utils::id::new_id();

        sqlx::query(
            "INSERT INTO auth_accounts (id, provider, label, account_id, payload_json, attributes_json, last_refreshed_at, next_refresh_after, next_retry_after, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(provider, account_id) DO UPDATE SET
                payload_json = excluded.payload_json,
                attributes_json = excluded.attributes_json,
                last_refreshed_at = excluded.last_refreshed_at,
                next_refresh_after = excluded.next_refresh_after,
                next_retry_after = excluded.next_retry_after,
                status = 'active',
                updated_at = excluded.updated_at",
        )
        .bind(id)
        .bind(&input.provider)
        .bind(&input.label)
        .bind(&input.account_id)
        .bind(payload_json)
        .bind(attributes_json)
        .bind(&input.last_refreshed_at)
        .bind(&input.next_refresh_after)
        .bind(&input.next_retry_after)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        sqlx::query_as::<_, AuthAccount>(
            "SELECT * FROM auth_accounts WHERE provider = ? AND account_id = ?",
        )
        .bind(&input.provider)
        .bind(&input.account_id)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn update_auth_account(
        &self,
        id: &str,
        label: &str,
        priority: i64,
        weight: i64,
        model_mapping_json: &str,
        model_mapping_disabled_json: &str,
    ) -> Result<(), sqlx::Error> {
        if label.trim().is_empty() || priority < 0 || weight < 1 {
            return Err(sqlx::Error::Protocol(
                "label must be non-empty, priority >= 0, and weight >= 1".into(),
            ));
        }
        let mut model_mapping = serde_json::from_str(model_mapping_json)
            .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));
        crate::db::models::normalize_model_mapping(&mut model_mapping);
        let model_mapping_json =
            serde_json::to_string(&model_mapping).unwrap_or_else(|_| "{}".to_owned());
        let mut model_mapping_disabled = serde_json::from_str(model_mapping_disabled_json)
            .unwrap_or_else(|_| serde_json::Value::Array(Default::default()));
        crate::db::models::normalize_model_mapping_disabled(&mut model_mapping_disabled);
        let model_mapping_disabled_json =
            serde_json::to_string(&model_mapping_disabled).unwrap_or_else(|_| "[]".to_owned());
        sqlx::query(
            "UPDATE auth_accounts SET label = ?, priority = ?, weight = ?, model_mapping_json = ?, model_mapping_disabled = ?, updated_at = ? WHERE id = ?",
        )
        .bind(label)
        .bind(priority)
        .bind(weight)
        .bind(&model_mapping_json)
        .bind(&model_mapping_disabled_json)
        .bind(now_iso())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 映射对快捷开启/关闭（迁移 042）：只重写 model_mapping_disabled，
    /// 不触碰 label/priority/weight/model_mapping。前端乐观更新后调用。
    pub async fn update_auth_account_mapping_disabled(
        &self,
        id: &str,
        model_mapping_disabled_json: &str,
    ) -> Result<(), sqlx::Error> {
        let mut disabled = serde_json::from_str(model_mapping_disabled_json)
            .unwrap_or_else(|_| serde_json::Value::Array(Default::default()));
        crate::db::models::normalize_model_mapping_disabled(&mut disabled);
        let normalized_json = serde_json::to_string(&disabled).unwrap_or_else(|_| "[]".to_owned());
        sqlx::query(
            "UPDATE auth_accounts SET model_mapping_disabled = ?, updated_at = ? WHERE id = ?",
        )
        .bind(normalized_json)
        .bind(now_iso())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_auth_account_disabled(
        &self,
        id: &str,
        disabled: bool,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE auth_accounts SET disabled = ?, updated_at = ? WHERE id = ?")
            .bind(i64::from(disabled))
            .bind(now_iso())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_tokens(
        &self,
        id: &str,
        payload: &serde_json::Value,
        last_refreshed_at: Option<&str>,
        next_refresh_after: Option<&str>,
        next_retry_after: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let payload_json = serde_json::to_string(payload)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        sqlx::query(
            "UPDATE auth_accounts
             SET payload_json = ?, last_refreshed_at = ?, next_refresh_after = ?, next_retry_after = ?, status = 'active', updated_at = ?
             WHERE id = ?",
        )
        .bind(payload_json)
        .bind(last_refreshed_at)
        .bind(next_refresh_after)
        .bind(next_retry_after)
        .bind(now_iso())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically overwrite re-login credentials on an existing local account.
    ///
    /// Unlike the generic `(provider, account_id)` conflict upsert, this update
    /// is keyed by true local `id` and guarded by optimistic preconditions on
    /// `provider` and `account_id`.  A zero-row update means the account was
    /// deleted or its identity moved concurrently (e.g. by refresh rotation),
    /// which the caller must treat as a fail-closed precondition failure.
    ///
    /// The same statement clears `model_states_json` and `last_models_sync_at`
    /// so a successfully re-logged account cannot route against a stale model
    /// catalog until the follow-up `/models` sync succeeds and rewrites the
    /// snapshot.
    pub async fn replace_auth_account(
        &self,
        id: &str,
        expected_account_id: &str,
        input: &AuthAccountUpsert,
    ) -> Result<AuthAccount, sqlx::Error> {
        let now = now_iso();
        let attributes_json = serde_json::to_string(&input.attributes)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let payload_json = serde_json::to_string(&input.payload)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let result = sqlx::query(
            "UPDATE auth_accounts
             SET provider = ?, label = ?, account_id = ?, payload_json = ?, attributes_json = ?,
                 model_states_json = ?, last_models_sync_at = NULL,
                 last_refreshed_at = ?, next_refresh_after = ?, next_retry_after = ?,
                 status = 'active', updated_at = ?
             WHERE id = ? AND provider = ? AND account_id = ?",
        )
        .bind(&input.provider)
        .bind(&input.label)
        .bind(&input.account_id)
        .bind(payload_json)
        .bind(attributes_json)
        .bind(serde_json::to_string(&ModelStates::default()).unwrap())
        .bind(&input.last_refreshed_at)
        .bind(&input.next_refresh_after)
        .bind(&input.next_retry_after)
        .bind(&now)
        .bind(id)
        .bind(&input.provider)
        .bind(expected_account_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(sqlx::Error::RowNotFound);
        }
        self.get_auth_account(id).await
    }

    /// This is only called after a successful provider sync. Keeping the
    /// update separate makes failures naturally preserve the old snapshot and
    /// its timestamp.
    pub async fn update_models_if_success(
        &self,
        id: &str,
        models: &ModelStates,
        synced_at: &str,
    ) -> Result<(), sqlx::Error> {
        let model_states_json = serde_json::to_string(models)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        sqlx::query(
            "UPDATE auth_accounts SET model_states_json = ?, last_models_sync_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(model_states_json)
        .bind(synced_at)
        .bind(now_iso())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_quota(
        &self,
        id: &str,
        quota: Option<&QuotaState>,
    ) -> Result<(), sqlx::Error> {
        let quota_json = quota
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        sqlx::query("UPDATE auth_accounts SET quota_json = ?, updated_at = ? WHERE id = ?")
            .bind(quota_json)
            .bind(now_iso())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_auth_reset_operation(
        &self,
        operation: &AuthResetOperation,
    ) -> Result<AuthResetOperation, sqlx::Error> {
        sqlx::query(
            "INSERT INTO auth_reset_operations \
             (id, account_id, credit_id_hash, redeem_request_id, status, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&operation.id)
        .bind(&operation.account_id)
        .bind(&operation.credit_id_hash)
        .bind(&operation.redeem_request_id)
        .bind(&operation.status)
        .bind(&operation.created_at)
        .bind(&operation.updated_at)
        .execute(&self.pool)
        .await?;
        self.get_auth_reset_operation(&operation.account_id, &operation.id)
            .await
    }

    pub async fn get_auth_reset_operation(
        &self,
        account_id: &str,
        id: &str,
    ) -> Result<AuthResetOperation, sqlx::Error> {
        sqlx::query_as::<_, AuthResetOperation>(
            "SELECT * FROM auth_reset_operations WHERE account_id = ? AND id = ?",
        )
        .bind(account_id)
        .bind(id)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn get_auth_reset_operation_by_redeem(
        &self,
        account_id: &str,
        redeem_request_id: &str,
    ) -> Result<Option<AuthResetOperation>, sqlx::Error> {
        sqlx::query_as::<_, AuthResetOperation>(
            "SELECT * FROM auth_reset_operations WHERE account_id = ? AND redeem_request_id = ?",
        )
        .bind(account_id)
        .bind(redeem_request_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn get_open_auth_reset_operation(
        &self,
        account_id: &str,
    ) -> Result<Option<AuthResetOperation>, sqlx::Error> {
        sqlx::query_as::<_, AuthResetOperation>(
            "SELECT * FROM auth_reset_operations WHERE account_id = ? AND status IN ('pending', 'unknown') ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn update_auth_reset_operation(
        &self,
        id: &str,
        status: &str,
        upstream_code: Option<&str>,
        error_class: Option<&str>,
        quota_refresh_status: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE auth_reset_operations SET status = ?, upstream_code = ?, error_class = ?, \
             quota_refresh_status = ?, updated_at = ? WHERE id = ?",
        )
        .bind(status)
        .bind(upstream_code)
        .bind(error_class)
        .bind(quota_refresh_status)
        .bind(now_iso())
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn mark_invalid(
        &self,
        id: &str,
        next_retry_after: Option<&str>,
        reason: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        // Persist a stable, non-secret reason for the invalidation (e.g.
        // "payment_required" for an unusable subscription) alongside the status
        // flip.  Stored in attributes_json so no schema migration is needed,
        // and merged (not overwriting) so login metadata like email/plan_type
        // survives.  The DTO surfaces it again as `invalidation_reason`.
        let merged_attributes: Option<String> = if reason.is_some() {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT attributes_json FROM auth_accounts WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await?;
            row.map(|(attributes_json,)| {
                let mut value: serde_json::Value = serde_json::from_str(&attributes_json)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                value["invalidation_reason"] = reason.unwrap().into();
                serde_json::to_string(&value).unwrap_or(attributes_json)
            })
        } else {
            None
        };
        sqlx::query(
            "UPDATE auth_accounts SET status = 'invalid', next_retry_after = ?, attributes_json = COALESCE(?, attributes_json), updated_at = ? WHERE id = ?",
        )
        .bind(next_retry_after)
        .bind(merged_attributes.as_deref())
        .bind(now_iso())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load candidates safe for the route planner. Quota JSON is deliberately
    /// parsed here: corrupt persisted data fail-closes rather than admitting an
    /// account with unknown quota state. A passed recovery deadline clears only
    /// the quota exclusion, so recovery is not delayed until maintenance.
    pub async fn list_route_accounts(&self, now: &str) -> Result<Vec<AuthAccount>, sqlx::Error> {
        let accounts = sqlx::query_as::<_, AuthAccount>(
            "SELECT * FROM auth_accounts WHERE disabled = 0 AND status = 'active' ORDER BY priority ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let now_time = chrono::DateTime::parse_from_rfc3339(now)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let mut routeable = Vec::new();

        for mut account in accounts {
            match account.model_states() {
                Ok(models) if !models.models.is_empty() => {}
                _ => continue,
            }
            let Some(raw_quota) = account.quota_json.as_deref() else {
                routeable.push(account);
                continue;
            };
            let mut quota: QuotaState = match serde_json::from_str(raw_quota) {
                Ok(quota) => quota,
                Err(_) => continue,
            };
            if !quota.exceeded {
                routeable.push(account);
                continue;
            }
            let recovered = quota
                .next_recover_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|recover_at| recover_at <= now_time);
            if !recovered {
                continue;
            }

            quota.exceeded = false;
            quota.reason = None;
            quota.next_recover_at = None;
            let quota_json = serde_json::to_string(&quota)
                .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
            sqlx::query("UPDATE auth_accounts SET quota_json = ?, updated_at = ? WHERE id = ?")
                .bind(&quota_json)
                .bind(now_iso())
                .bind(&account.id)
                .execute(&self.pool)
                .await?;
            account.quota_json = Some(quota_json);
            routeable.push(account);
        }
        Ok(routeable)
    }

    /// Read-only list of active, enabled auth accounts (the same routeable filter
    /// as `list_route_accounts` but WITHOUT quota recovery writes).  Used by the
    /// `/v1/models` endpoint to surface auth-account-only models.
    pub async fn list_active_auth_accounts(&self) -> Result<Vec<AuthAccount>, sqlx::Error> {
        sqlx::query_as::<_, AuthAccount>(
            "SELECT * FROM auth_accounts WHERE disabled = 0 AND status = 'active' ORDER BY priority ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await
    }

    // ==================== Request Log ====================

    pub async fn create_log(&self, log: &RequestLog) -> Result<(), sqlx::Error> {
        self.create_log_with_policy(log, crate::audit_log::current_policy())
            .await
    }

    /// Persist a request log using an explicit policy. The normal runtime path
    /// uses `create_log`; this variant keeps policy application close to the
    /// INSERT and makes the behavior independently testable.
    pub async fn create_log_with_policy(
        &self,
        log: &RequestLog,
        policy: crate::audit_log::LogPolicy,
    ) -> Result<(), sqlx::Error> {
        let log = crate::audit_log::effective_log_with_policy(log, policy);
        // 日志落账与用量统计(迁移 046)必须在同一事务:共用一次提交/fsync,
        // 避免每请求两次提交的高频写放大;统计失败回滚整个事务 = 本请求日志未记,
        // 与 create_log 失败的既有容忍语义一致(调用方 eprintln/tracing::warn 后继续)。
        // Insert with seq auto-incremented via subquery (atomic, avoids race condition).
        // The 11 T09 observability columns (migration 016) are bound as Option<> so
        // legacy callers using `..Default::default()` persist NULLs for them.
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO request_logs (id, seq, api_key_id, api_key_name, channel_id, channel_name, model, upstream_model, mode, status_code, prompt_tokens, completion_tokens, total_tokens, cached_tokens, duration_ms, error_message, is_stream, is_retry, created_at, request_body, response_choices, risk_level, risk_score, risk_summary, security_action, sanitized, blocked_reason, trace_id, reasoning_effort, downstream_protocol, downstream_endpoint, route_group, upstream_protocol, upstream_endpoint, provider, codec_version, failure_class, identity_revision, client_cancelled, stream_committed, upstream_type, detail_level)
             VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM request_logs), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&log.id)
        .bind(&log.api_key_id)
        .bind(&log.api_key_name)
        .bind(&log.channel_id)
        .bind(&log.channel_name)
        .bind(&log.model)
        .bind(&log.upstream_model)
        .bind(&log.mode)
        .bind(log.status_code)
        .bind(log.prompt_tokens)
        .bind(log.completion_tokens)
        .bind(log.total_tokens)
        .bind(log.cached_tokens)
        .bind(log.duration_ms)
        .bind(&log.error_message)
        .bind(log.is_stream)
        .bind(log.is_retry)
        .bind(&log.created_at)
        .bind(&log.request_body)
        .bind(&log.response_choices)
        .bind(&log.risk_level)
        .bind(log.risk_score)
        .bind(&log.risk_summary)
        .bind(&log.security_action)
        .bind(log.sanitized)
        .bind(&log.blocked_reason)
        .bind(&log.trace_id)
        .bind(&log.reasoning_effort)
        .bind(&log.downstream_protocol)
        .bind(&log.downstream_endpoint)
        .bind(&log.route_group)
        .bind(&log.upstream_protocol)
        .bind(&log.upstream_endpoint)
        .bind(&log.provider)
        .bind(&log.codec_version)
        .bind(&log.failure_class)
        .bind(log.identity_revision)
        .bind(log.client_cancelled)
        .bind(log.stream_committed)
        .bind(&log.upstream_type)
        .bind(policy.detail_level.as_str())
        .execute(&mut *tx)
        .await?;
        // 用量统计 UPSERT:同一事务内累加,与日志同生共死。
        let delta = usage_delta_from_log(&log);
        let success = i64::from(delta.success);
        let fail = i64::from(!delta.success);
        sqlx::query(
            "INSERT INTO usage_stats (hour, model, channel_id, api_key_id, request_count, \
             success_count, fail_count, prompt_tokens, completion_tokens, total_tokens, \
             cached_tokens, total_duration_ms) \
             VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (hour, model, channel_id, api_key_id) DO UPDATE SET \
             request_count = request_count + 1, \
             success_count = success_count + excluded.success_count, \
             fail_count = fail_count + excluded.fail_count, \
             prompt_tokens = prompt_tokens + excluded.prompt_tokens, \
             completion_tokens = completion_tokens + excluded.completion_tokens, \
             total_tokens = total_tokens + excluded.total_tokens, \
             cached_tokens = cached_tokens + excluded.cached_tokens, \
             total_duration_ms = total_duration_ms + excluded.total_duration_ms",
        )
        .bind(&delta.hour)
        .bind(&delta.model)
        .bind(&delta.channel_id)
        .bind(&delta.api_key_id)
        .bind(success)
        .bind(fail)
        .bind(delta.prompt_tokens)
        .bind(delta.completion_tokens)
        .bind(delta.total_tokens)
        .bind(delta.cached_tokens)
        .bind(delta.duration_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        // 流式内容段（迁移 032）：detailed 策略下把流式累计内容同步落入溢出表，
        // 日志详情与后续续传能力按 log_id 寻址；basic 尊重用户存储选择不落段，
        // brief 只裁请求消息列表，响应内容仍需完整落段。
        // best-effort：段写入失败仅告警，不使主日志落账失败（主表行是权威记录）。
        // Responses 协议除外：driver 轨对该协议做**逐帧渐进持久化**（携带
        // response_id 续传锚点），本漏斗跳过以免同一流写两份内容。
        if log.is_stream == 1
            && log.mode != "responses"
            && policy.detail_level != crate::audit_log::LogDetailLevel::Basic
        {
            if let Some(content) = log.response_choices.as_deref() {
                if !content.is_empty() {
                    if let Err(error) = sqlx::query(
                        "INSERT INTO stream_segments (log_id, seq, content, created_at) VALUES (?, 1, ?, ?)",
                    )
                    .bind(&log.id)
                    .bind(content)
                    .bind(&log.created_at)
                    .execute(&self.pool)
                    .await
                    {
                        tracing::warn!("[流式段] 写入失败（不影响主日志）: {error}");
                    }
                }
            }
        }
        Ok(())
    }

    /// 读取某次流式请求的全部已生成内容段（按 seq 升序拼接即完整内容）。
    pub async fn get_stream_segments(
        &self,
        log_id: &str,
    ) -> Result<Vec<(i64, String)>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT seq, content FROM stream_segments WHERE log_id = ? ORDER BY seq ASC",
        )
        .bind(log_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get::<i64, _>("seq"), row.get::<String, _>("content")))
            .collect())
    }

    /// Responses 逐帧持久化：追加一帧下游 SSE 字节（best-effort，失败仅告警）。
    pub async fn append_stream_frame(
        &self,
        log_id: &str,
        seq: i64,
        response_id: Option<&str>,
        content: &str,
        created_at: &str,
    ) {
        let mut query = sqlx::query(
            "INSERT INTO stream_segments (log_id, seq, response_id, content, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(log_id)
        .bind(seq);
        query = match response_id {
            Some(id) => query.bind(id),
            None => query.bind(Option::<String>::None),
        };
        query = query.bind(content).bind(created_at);
        if let Err(error) = query.execute(&self.pool).await {
            tracing::warn!("[流式段] 逐帧写入失败（不影响流转发）: {error}");
        }
    }

    /// 按续传锚点（上游 response.id）取未消费帧：seq > offset 升序。
    pub async fn get_stream_frames_after(
        &self,
        response_id: &str,
        offset: i64,
    ) -> Result<Vec<(i64, String)>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT seq, content FROM stream_segments \
             WHERE response_id = ? AND seq > ? ORDER BY seq ASC",
        )
        .bind(response_id)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get::<i64, _>("seq"), row.get::<String, _>("content")))
            .collect())
    }

    /// 续传锚点 → 首帧时间（TTL 判定）与关联日志行 id（完成状态判定）。
    pub async fn find_stream_anchor(
        &self,
        response_id: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT log_id, created_at FROM stream_segments \
             WHERE response_id = ? ORDER BY seq ASC LIMIT 1",
        )
        .bind(response_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            (
                r.get::<String, _>("log_id"),
                r.get::<String, _>("created_at"),
            )
        }))
    }

    pub async fn create_security_findings(
        &self,
        log_id: &str,
        findings: &[crate::security::SecurityFinding],
        action: &str,
    ) -> Result<(), sqlx::Error> {
        for finding in findings {
            sqlx::query(
                "INSERT INTO request_security_findings (id, log_id, phase, category, rule_id, severity, title, description, location, evidence_masked, evidence_hash, action, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(crate::utils::id::new_id())
            .bind(log_id)
            .bind(&finding.phase)
            .bind(&finding.category)
            .bind(&finding.rule_id)
            .bind(finding.severity.as_str())
            .bind(&finding.title)
            .bind(&finding.description)
            .bind(&finding.location)
            .bind(&finding.evidence_masked)
            .bind(Option::<String>::None)
            .bind(action)
            .bind(crate::utils::time::now_iso())
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn get_security_findings(
        &self,
        log_id: &str,
    ) -> Result<Vec<RequestSecurityFinding>, sqlx::Error> {
        sqlx::query_as::<_, RequestSecurityFinding>(
            "SELECT * FROM request_security_findings WHERE log_id = ? ORDER BY created_at ASC",
        )
        .bind(log_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_log(&self, id: &str) -> Result<RequestLog, sqlx::Error> {
        sqlx::query_as::<_, RequestLog>("SELECT * FROM request_logs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn delete_logs_before(&self, before_date: &str) -> Result<u64, sqlx::Error> {
        sqlx::query("DELETE FROM request_security_findings WHERE log_id IN (SELECT id FROM request_logs WHERE created_at < ?)")
            .bind(before_date)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM stream_segments WHERE log_id IN (SELECT id FROM request_logs WHERE created_at < ?)")
            .bind(before_date)
            .execute(&self.pool)
            .await?;
        let result = sqlx::query("DELETE FROM request_logs WHERE created_at < ?")
            .bind(before_date)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn delete_all_logs(&self) -> Result<u64, sqlx::Error> {
        sqlx::query("DELETE FROM request_security_findings")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM stream_segments")
            .execute(&self.pool)
            .await?;
        let result = sqlx::query("DELETE FROM request_logs")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// 多条件组合删除日志(Task 4 颗粒度重构)。与 `delete_logs_before` 的区别:
    /// 支持状态码/成功与否/渠道/Key/模型过滤,并可选择是否同步清除对应 usage_stats。
    ///
    /// 返回 `(删除/匹配的日志行数, 同步删除/匹配的统计行数)`:
    /// - `dry_run=true` 只统计匹配行数不删除,供前端确认弹窗展示预计影响;
    /// - `clear_stats=false`(默认)统计表完全不动 —— 清理日志不影响首页统计;
    /// - `clear_stats=true` 时按同一条件映射删除 usage_stats(时间条件映射到 hour 桶,
    ///   小时桶粒度,比日志秒级精度粗,属可接受的展示口径)。
    pub async fn delete_logs_matching(
        &self,
        input: &crate::commands::log::DeleteLogsInput,
        dry_run: bool,
    ) -> Result<(u64, u64), sqlx::Error> {
        let (log_where, log_bind) = build_log_delete_where(input);

        if dry_run {
            let matched_logs =
                Self::count_matching(&self.pool, "request_logs", &log_where, &log_bind).await?;
            let matched_stats = if input.clear_stats.unwrap_or(false) {
                let (stats_where, stats_bind) = build_stats_delete_where(input);
                Self::count_matching(&self.pool, "usage_stats", &stats_where, &stats_bind).await?
            } else {
                0
            };
            return Ok((matched_logs, matched_stats));
        }

        // 级联删除子表(与 delete_logs_before 一致):findings + stream_segments。
        let mut tx = self.pool.begin().await?;
        let child_sql = format!("DELETE FROM request_security_findings WHERE log_id IN (SELECT id FROM request_logs WHERE {log_where})");
        let child_sql2 = format!("DELETE FROM stream_segments WHERE log_id IN (SELECT id FROM request_logs WHERE {log_where})");
        let mut q1 = sqlx::query(&child_sql);
        for b in &log_bind {
            q1 = q1.bind(b);
        }
        q1.execute(&mut *tx).await?;
        let mut q2 = sqlx::query(&child_sql2);
        for b in &log_bind {
            q2 = q2.bind(b);
        }
        q2.execute(&mut *tx).await?;

        let log_sql = format!("DELETE FROM request_logs WHERE {log_where}");
        let mut ql = sqlx::query(&log_sql);
        for b in &log_bind {
            ql = ql.bind(b);
        }
        let deleted_logs = ql.execute(&mut *tx).await?.rows_affected();

        let deleted_stats = if input.clear_stats.unwrap_or(false) {
            let (stats_where, stats_bind) = build_stats_delete_where(input);
            let stats_sql = format!("DELETE FROM usage_stats WHERE {stats_where}");
            let mut qs = sqlx::query(&stats_sql);
            for b in &stats_bind {
                qs = qs.bind(b);
            }
            qs.execute(&mut *tx).await?.rows_affected()
        } else {
            0
        };
        tx.commit().await?;
        Ok((deleted_logs, deleted_stats))
    }

    /// 从现存 request_logs 回填 usage_stats(迁移 046 上线时的历史续接)。
    /// 幂等:仅当统计表为空时执行。单条聚合 SQL(表空才跑、启动时一次),
    /// 口径与 create_log 的 UPSERT 完全一致(同 hour 桶 / COALESCE 归一 / 成功=2xx)。
    /// 探测行走 record_channel_probe 直接 SQL 不落 request_logs,is_probe=0 过滤天然一致。
    pub async fn backfill_usage_stats_if_empty(&self) -> Result<u64, sqlx::Error> {
        let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_stats")
            .fetch_one(&self.pool)
            .await?;
        if existing > 0 {
            return Ok(0);
        }
        let result = sqlx::query(
            "INSERT INTO usage_stats (hour, model, channel_id, api_key_id, request_count, \
             success_count, fail_count, prompt_tokens, completion_tokens, total_tokens, \
             cached_tokens, total_duration_ms) \
             SELECT substr(created_at, 1, 13) || ':00:00.000Z' as hour, model, \
                    COALESCE(channel_id, '') as channel_id, COALESCE(api_key_id, '') as api_key_id, \
                    COUNT(*) as request_count, \
                    SUM(CASE WHEN status_code >= 200 AND status_code < 300 THEN 1 ELSE 0 END) as success_count, \
                    SUM(CASE WHEN status_code >= 200 AND status_code < 300 THEN 0 ELSE 1 END) as fail_count, \
                    COALESCE(SUM(prompt_tokens), 0) as prompt_tokens, \
                    COALESCE(SUM(completion_tokens), 0) as completion_tokens, \
                    COALESCE(SUM(total_tokens), 0) as total_tokens, \
                    COALESCE(SUM(cached_tokens), 0) as cached_tokens, \
                    COALESCE(SUM(duration_ms), 0) as total_duration_ms \
             FROM request_logs WHERE is_probe = 0 \
             GROUP BY hour, model, channel_id, api_key_id \
             ON CONFLICT (hour, model, channel_id, api_key_id) DO UPDATE SET \
             request_count = excluded.request_count, \
             success_count = excluded.success_count, \
             fail_count = excluded.fail_count, \
             prompt_tokens = excluded.prompt_tokens, \
             completion_tokens = excluded.completion_tokens, \
             total_tokens = excluded.total_tokens, \
             cached_tokens = excluded.cached_tokens, \
             total_duration_ms = excluded.total_duration_ms",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// 独立清除历史统计数据(不删日志)。条件与 `delete_logs_matching` 的统计侧一致。
    pub async fn clear_usage_stats_matching(
        &self,
        input: &crate::commands::log::DeleteLogsInput,
    ) -> Result<u64, sqlx::Error> {
        let (stats_where, stats_bind) = build_stats_delete_where(input);
        let sql = format!("DELETE FROM usage_stats WHERE {stats_where}");
        let mut q = sqlx::query(&sql);
        for b in &stats_bind {
            q = q.bind(b);
        }
        let r = q.execute(&self.pool).await?;
        Ok(r.rows_affected())
    }

    async fn count_matching(
        pool: &SqlitePool,
        table: &str,
        where_clause: &str,
        binds: &[Option<String>],
    ) -> Result<u64, sqlx::Error> {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {where_clause}");
        let mut q = sqlx::query(&sql);
        for b in binds {
            q = q.bind(b.as_deref());
        }
        let n: i64 = q.fetch_one(pool).await?.get(0);
        Ok(n.max(0) as u64)
    }

    pub async fn delete_log(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM request_security_findings WHERE log_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM stream_segments WHERE log_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM request_logs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_logs(&self, limit: i64, offset: i64) -> Result<Vec<RequestLog>, sqlx::Error> {
        sqlx::query_as::<_, RequestLog>(
            "SELECT * FROM request_logs ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_log_summaries(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<crate::db::models::RequestLogSummary>, sqlx::Error> {
        self.search_log_summaries(
            None, None, None, None, None, None, None, None, limit, offset,
        )
        .await
    }

    pub async fn search_log_summaries(
        &self,
        keyword: Option<&str>,
        api_key_name: Option<&str>,
        channel_name: Option<&str>,
        model: Option<&str>,
        date_from: Option<&str>,
        date_to: Option<&str>,
        trace_id: Option<&str>,
        upstream_type: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<crate::db::models::RequestLogSummary>, sqlx::Error> {
        let mut q = sqlx::QueryBuilder::new(
            "SELECT id, seq, api_key_name, channel_name, model, upstream_model, mode, status_code, \
             prompt_tokens, completion_tokens, total_tokens, cached_tokens, duration_ms, error_message, \
             is_stream, is_retry, created_at, risk_level, risk_score, risk_summary, security_action, \
             sanitized, blocked_reason, trace_id, reasoning_effort, downstream_protocol, downstream_endpoint, \
             route_group, upstream_protocol, upstream_endpoint, provider, codec_version, failure_class, \
             identity_revision, client_cancelled, stream_committed, upstream_type, \
             COALESCE(detail_level, 'detailed') AS detail_level, started_at, \
             COALESCE(length(CAST(request_body AS BLOB)), 0) AS request_body_bytes, \
             COALESCE(length(CAST(response_choices AS BLOB)), 0) AS response_choices_bytes, \
             (request_body IS NOT NULL) AS has_request_body \
             FROM request_logs WHERE 1=1",
        );
        if let Some(kw) = keyword {
            let pattern = format!("%{}%", kw);
            q.push(" AND (api_key_name LIKE ")
                .push_bind(pattern.clone());
            q.push(" OR channel_name LIKE ").push_bind(pattern.clone());
            q.push(" OR model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ")
                .push_bind(pattern.clone());
            q.push(" OR api_key_id LIKE ").push_bind(pattern.clone());
            q.push(" OR id LIKE ").push_bind(pattern);
            q.push(")");
        }
        for (column, value) in [
            ("api_key_name", api_key_name),
            ("channel_name", channel_name),
        ] {
            if let Some(value) = value {
                q.push(" AND ")
                    .push(column)
                    .push(" LIKE ")
                    .push_bind(format!("%{}%", value));
            }
        }
        if let Some(value) = model {
            let pattern = format!("%{}%", value);
            q.push(" AND (model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ").push_bind(pattern);
            q.push(")");
        }
        if let Some(value) = date_from {
            q.push(" AND created_at >= ").push_bind(value);
        }
        if let Some(value) = date_to {
            q.push(" AND created_at <= ").push_bind(value);
        }
        if let Some(value) = trace_id {
            q.push(" AND trace_id LIKE ")
                .push_bind(format!("%{}%", value));
        }
        if let Some(value) = upstream_type {
            q.push(" AND upstream_type = ").push_bind(value);
        }
        q.push(" ORDER BY created_at DESC LIMIT ").push_bind(limit);
        q.push(" OFFSET ").push_bind(offset);
        q.build_query_as::<crate::db::models::RequestLogSummary>()
            .fetch_all(&self.pool)
            .await
    }

    pub async fn count_log_summaries(
        &self,
        keyword: Option<&str>,
        api_key_name: Option<&str>,
        channel_name: Option<&str>,
        model: Option<&str>,
        date_from: Option<&str>,
        date_to: Option<&str>,
        trace_id: Option<&str>,
        upstream_type: Option<&str>,
    ) -> Result<i64, sqlx::Error> {
        let mut q = sqlx::QueryBuilder::new("SELECT COUNT(*) FROM request_logs WHERE 1=1");
        if let Some(kw) = keyword {
            let pattern = format!("%{}%", kw);
            q.push(" AND (api_key_name LIKE ")
                .push_bind(pattern.clone());
            q.push(" OR channel_name LIKE ").push_bind(pattern.clone());
            q.push(" OR model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ")
                .push_bind(pattern.clone());
            q.push(" OR api_key_id LIKE ").push_bind(pattern.clone());
            q.push(" OR id LIKE ").push_bind(pattern);
            q.push(")");
        }
        for (column, value) in [
            ("api_key_name", api_key_name),
            ("channel_name", channel_name),
        ] {
            if let Some(value) = value {
                q.push(" AND ")
                    .push(column)
                    .push(" LIKE ")
                    .push_bind(format!("%{}%", value));
            }
        }
        if let Some(value) = model {
            let pattern = format!("%{}%", value);
            q.push(" AND (model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ").push_bind(pattern);
            q.push(")");
        }
        if let Some(value) = date_from {
            q.push(" AND created_at >= ").push_bind(value);
        }
        if let Some(value) = date_to {
            q.push(" AND created_at <= ").push_bind(value);
        }
        if let Some(value) = trace_id {
            q.push(" AND trace_id LIKE ")
                .push_bind(format!("%{}%", value));
        }
        if let Some(value) = upstream_type {
            q.push(" AND upstream_type = ").push_bind(value);
        }
        let (count,): (i64,) = q.build_query_as().fetch_one(&self.pool).await?;
        Ok(count)
    }

    pub async fn search_logs(
        &self,
        keyword: Option<&str>,
        api_key_name: Option<&str>,
        channel_name: Option<&str>,
        model: Option<&str>,
        date_from: Option<&str>,
        date_to: Option<&str>,
        trace_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<RequestLog>, sqlx::Error> {
        self.search_logs_by_upstream_type(
            keyword,
            api_key_name,
            channel_name,
            model,
            date_from,
            date_to,
            trace_id,
            None,
            limit,
            offset,
        )
        .await
    }

    pub async fn search_logs_by_upstream_type(
        &self,
        keyword: Option<&str>,
        api_key_name: Option<&str>,
        channel_name: Option<&str>,
        model: Option<&str>,
        date_from: Option<&str>,
        date_to: Option<&str>,
        trace_id: Option<&str>,
        upstream_type: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<RequestLog>, sqlx::Error> {
        let mut q = sqlx::QueryBuilder::new("SELECT * FROM request_logs WHERE 1=1");

        if let Some(kw) = keyword {
            let pattern = format!("%{}%", kw);
            q.push(" AND (api_key_name LIKE ")
                .push_bind(pattern.clone());
            q.push(" OR channel_name LIKE ").push_bind(pattern.clone());
            q.push(" OR model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ")
                .push_bind(pattern.clone());
            q.push(" OR api_key_id LIKE ").push_bind(pattern.clone());
            q.push(" OR id LIKE ").push_bind(pattern);
            q.push(")");
        }

        if let Some(name) = api_key_name {
            let pattern = format!("%{}%", name);
            q.push(" AND api_key_name LIKE ").push_bind(pattern);
        }

        if let Some(name) = channel_name {
            let pattern = format!("%{}%", name);
            q.push(" AND channel_name LIKE ").push_bind(pattern);
        }

        if let Some(m) = model {
            let pattern = format!("%{}%", m);
            q.push(" AND (model LIKE ").push_bind(pattern.clone());
            q.push(" OR upstream_model LIKE ").push_bind(pattern);
            q.push(")");
        }

        if let Some(from) = date_from {
            q.push(" AND created_at >= ").push_bind(from);
        }

        if let Some(to) = date_to {
            q.push(" AND created_at <= ").push_bind(to);
        }

        if let Some(tid) = trace_id {
            let pattern = format!("%{}%", tid);
            q.push(" AND trace_id LIKE ").push_bind(pattern);
        }

        if let Some(kind) = upstream_type {
            q.push(" AND upstream_type = ").push_bind(kind);
        }

        q.push(" ORDER BY created_at DESC LIMIT ").push_bind(limit);
        q.push(" OFFSET ").push_bind(offset);

        q.build_query_as::<RequestLog>().fetch_all(&self.pool).await
    }

    pub async fn get_dashboard_stats(&self) -> Result<DashboardStats, sqlx::Error> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let today_prefix = format!("{}%", today);

        // 请求类指标(迁移 046)改读 usage_stats 聚合:与审计日志解耦,
        // 清理日志不再影响已统计的调用量与 Token。口径与旧版实时聚合一致。
        let today_requests: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(request_count), 0) FROM usage_stats WHERE hour LIKE ?",
        )
        .bind(&today_prefix)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let today_total_tokens: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(total_tokens), 0) FROM usage_stats WHERE hour LIKE ?",
        )
        .bind(&today_prefix)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let today_cached_tokens: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(cached_tokens), 0) FROM usage_stats WHERE hour LIKE ?",
        )
        .bind(&today_prefix)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let today_prompt_tokens: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(prompt_tokens), 0) FROM usage_stats WHERE hour LIKE ?",
        )
        .bind(&today_prefix)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_cached_tokens: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(cached_tokens), 0) FROM usage_stats")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let total_prompt_tokens: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(prompt_tokens), 0) FROM usage_stats")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let total_requests: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(request_count), 0) FROM usage_stats")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let total_tokens: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(total_tokens), 0) FROM usage_stats")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        // 今日平均延迟 = 今日总耗时 / 今日请求数(与旧版 AVG(duration_ms) 口径一致)。
        let avg_latency: f64 = {
            let (dur, cnt): (i64, i64) = sqlx::query_as(
                "SELECT COALESCE(SUM(total_duration_ms), 0), COALESCE(SUM(request_count), 0) \
                 FROM usage_stats WHERE hour LIKE ?",
            )
            .bind(&today_prefix)
            .fetch_one(&self.pool)
            .await
            .unwrap_or((0, 0));
            if cnt > 0 {
                dur as f64 / cnt as f64
            } else {
                0.0
            }
        };

        // 服务可用率的健康口径（渠道健康度设计 v2）：
        // - 分母 = 启用渠道（status=1）；主动禁用的渠道不计入，避免"用户下线"拉低可用率
        // - 分子 = 启用且健康：last_probe_ok=1（探测通过）或 NULL（从未探测视为可用）
        //   只有真实探测失败（last_probe_ok=0）才会拉低可用率，如实反映上游故障。
        // 渠道健康探测循环见 health_probe.rs（默认 300s 一轮，仅探测启用渠道）。
        let active_channels: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM channels WHERE status = 1 AND COALESCE(last_probe_ok, 1) = 1",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_channels: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM channels WHERE status = 1")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        // Auth 账号无网络可达性探测（仅 12h token 刷新 + quota 探测），
        // 可用口径 = 未禁用且凭证有效；分母 = 未禁用账号（主动禁用的不计入）。
        let active_auth_accounts: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM auth_accounts WHERE disabled = 0 AND status = 'active'",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let total_auth_accounts: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM auth_accounts WHERE disabled = 0")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let total_api_keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let total_knowledge_bases: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM kb_knowledge_bases")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let total_kb_documents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kb_documents")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let total_kb_chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kb_chunks")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let total_wiki_projects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wiki_projects")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let total_wiki_pages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM wiki_pages")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        Ok(DashboardStats {
            today_requests,
            today_total_tokens,
            today_cached_tokens,
            today_prompt_tokens,
            total_cached_tokens,
            total_prompt_tokens,
            active_channels,
            avg_latency_ms: avg_latency,
            total_channels,
            active_auth_accounts,
            total_auth_accounts,
            total_api_keys,
            total_requests,
            total_tokens,
            total_knowledge_bases,
            total_kb_documents,
            total_kb_chunks,
            total_wiki_projects,
            total_wiki_pages,
        })
    }

    pub async fn get_channel_stats(&self) -> Result<Vec<ChannelStats>, sqlx::Error> {
        sqlx::query_as::<_, ChannelStats>(
            "SELECT\n                channel_id as channel_id,\n                SUM(request_count) as total_calls,\n                SUM(success_count) as success_calls,\n                SUM(fail_count) as failed_calls,\n                COALESCE(SUM(total_tokens), 0) as total_tokens,\n                COALESCE(SUM(prompt_tokens), 0) as prompt_tokens,\n                COALESCE(SUM(completion_tokens), 0) as completion_tokens,\n                CAST(COALESCE(SUM(total_duration_ms), 0) AS REAL) / NULLIF(SUM(request_count), 0) as avg_latency_ms,\n                MAX(hour) as last_call_at\n            FROM usage_stats\n            WHERE channel_id != ''\n            GROUP BY channel_id"
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_api_key_stats(&self) -> Result<Vec<ApiKeyStats>, sqlx::Error> {
        sqlx::query_as::<_, ApiKeyStats>(
            "SELECT\n                api_key_id as api_key_id,\n                SUM(request_count) as total_calls,\n                SUM(success_count) as success_calls,\n                SUM(fail_count) as failed_calls,\n                COALESCE(SUM(total_tokens), 0) as total_tokens,\n                COALESCE(SUM(prompt_tokens), 0) as prompt_tokens,\n                COALESCE(SUM(completion_tokens), 0) as completion_tokens,\n                COALESCE(SUM(cached_tokens), 0) as cached_tokens,\n                CAST(COALESCE(SUM(total_duration_ms), 0) AS REAL) / NULLIF(SUM(request_count), 0) as avg_latency_ms,\n                MAX(hour) as last_call_at\n            FROM usage_stats\n            WHERE api_key_id != ''\n            GROUP BY api_key_id"
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_log_stats(&self, days: i64) -> Result<Vec<LogStats>, sqlx::Error> {
        let since = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(days))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();

        sqlx::query_as::<_, LogStats>(
            "SELECT substr(hour, 1, 10) as date, COALESCE(SUM(request_count), 0) as count, COALESCE(SUM(total_tokens), 0) as total_tokens
             FROM usage_stats
             WHERE hour >= ?
             GROUP BY date
             ORDER BY date DESC"
        )
        .bind(&since)
        .fetch_all(&self.pool)
        .await
    }

    /// 按模型分组统计:请求次数、Token 消耗、成功率、平均延迟(读 usage_stats 聚合)。
    pub async fn get_model_stats(&self) -> Result<Vec<ModelStats>, sqlx::Error> {
        let sql = r#"
            SELECT
                model,
                COALESCE(SUM(request_count), 0) as request_count,
                COALESCE(SUM(prompt_tokens), 0) as input_tokens,
                COALESCE(SUM(completion_tokens), 0) as output_tokens,
                COALESCE(SUM(cached_tokens), 0) as cached_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                -- 口径与渠道/Key 统计一致:success/(success+failed)。
                -- 常规数据下 request_count = success+fail,两口径数值相等;
                -- 渠道清理的 reset_fail(仅清 fail_count、不动 request_count)后,
                -- 此口径会使模型成功率随之恢复到 ~100%,而非停滞在旧值。
                ROUND(CAST(COALESCE(SUM(success_count), 0) AS REAL) / NULLIF(SUM(success_count) + SUM(fail_count), 0), 4) as success_rate,
                CAST(COALESCE(SUM(total_duration_ms), 0) AS REAL) / NULLIF(SUM(request_count), 0) as avg_latency_ms
            FROM usage_stats
            GROUP BY model
            ORDER BY total_tokens DESC
        "#;
        sqlx::query_as::<_, ModelStats>(sql)
            .fetch_all(&self.pool)
            .await
    }

    /// 按小时粒度统计各模型 Token 趋势(读 usage_stats,hour 即 UTC 小时桶,天然对齐)。
    pub async fn get_token_trend(&self, hours: i64) -> Result<Vec<TokenTrendPoint>, sqlx::Error> {
        let since = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::hours(hours))
            .unwrap()
            .format("%Y-%m-%dT%H:00:00.000Z")
            .to_string();
        let sql = r#"
            SELECT
                hour,
                model,
                COALESCE(SUM(prompt_tokens), 0) as input_tokens,
                COALESCE(SUM(completion_tokens), 0) as output_tokens,
                COALESCE(SUM(cached_tokens), 0) as cached_tokens,
                COALESCE(SUM(total_tokens), 0) as total_tokens,
                SUM(request_count) as request_count
            FROM usage_stats
            WHERE hour >= ?
            GROUP BY hour, model
            ORDER BY hour ASC
        "#;
        sqlx::query_as::<_, TokenTrendPoint>(sql)
            .bind(&since)
            .fetch_all(&self.pool)
            .await
    }
}

#[cfg(test)]
#[path = "probe_success_tests.rs"]
mod probe_success_tests;
