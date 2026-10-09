//! 审计日志策略：控制正文是否落库，并按保留期清理历史记录。

use crate::{db::models::RequestLog, settings_store::SettingsStore};
use sqlx::SqlitePool;
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogDetailLevel {
    /// 不保存请求与响应正文。
    Basic,
    /// 保存响应正文，但请求的消息列表只留最新 [`BRIEF_MESSAGE_KEEP`] 条。
    Brief,
    /// 原样保存请求与响应正文。
    Detailed,
}

/// 「简要」级别下，请求消息列表保留的条数（按数组尾部计数，不区分角色）。
pub const BRIEF_MESSAGE_KEEP: usize = 3;

/// 「简要」截断后写回请求 JSON 顶层的标记字段，供详情面板说明省略了多少。
/// 前导下划线避开任何厂商真实字段名。
pub const BRIEF_MARKER_KEY: &str = "_wali_brief";

impl LogDetailLevel {
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("detailed") {
            Self::Detailed
        } else if value.eq_ignore_ascii_case("brief") {
            Self::Brief
        } else {
            Self::Basic
        }
    }

    /// 落库与前端识别用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Brief => "brief",
            Self::Detailed => "detailed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPolicy {
    pub detail_level: LogDetailLevel,
    /// 0 means retain forever.
    pub retention_days: u64,
}

impl Default for LogPolicy {
    fn default() -> Self {
        Self {
            detail_level: LogDetailLevel::Basic,
            retention_days: 7,
        }
    }
}

static POLICY: OnceLock<RwLock<LogPolicy>> = OnceLock::new();

fn policy_cell() -> &'static RwLock<LogPolicy> {
    // Library consumers and migration/integration tests may create a
    // Repository without booting AppState. Preserve the historical detailed
    // write behavior until the application startup explicitly applies the
    // persisted policy (whose product default is basic).
    POLICY.get_or_init(|| {
        RwLock::new(LogPolicy {
            detail_level: LogDetailLevel::Detailed,
            retention_days: 7,
        })
    })
}

pub fn policy_from_settings(settings: &SettingsStore) -> LogPolicy {
    let defaults = LogPolicy::default();
    let retention_days = settings.get_u64("logs.retention_days", defaults.retention_days);
    LogPolicy {
        detail_level: LogDetailLevel::parse(&settings.get_str("logs.detail_level", "basic")),
        retention_days: normalize_retention_days(retention_days),
    }
}

/// Supported retention values shared by UI and backend. Unknown persisted
/// values fall back to the safe default instead of creating an unbounded
/// cleanup interval or overflowing chrono's duration conversion.
pub fn normalize_retention_days(value: u64) -> u64 {
    match value {
        0 | 1 | 7 | 30 | 90 => value,
        _ => LogPolicy::default().retention_days,
    }
}

/// UI 展示与持久化共用的合法级别字符串；未知/损坏的持久化值回落到 `basic`，
/// 与 [`policy_from_settings`] 的判定保持一致（宁可不存正文，也不悄悄多占空间）。
pub fn normalize_detail_level(value: &str) -> &'static str {
    LogDetailLevel::parse(value).as_str()
}

pub fn apply_settings(settings: &SettingsStore) {
    let policy = policy_from_settings(settings);
    if let Ok(mut current) = policy_cell().write() {
        *current = policy;
    }
}

pub fn current_policy() -> LogPolicy {
    policy_cell()
        .read()
        .map(|policy| *policy)
        .unwrap_or_default()
}

/// Apply the active policy immediately before persistence. This centralizes
/// all request-log write paths because they already share Repository::create_log.
pub fn effective_log(log: &RequestLog) -> RequestLog {
    effective_log_with_policy(log, current_policy())
}

pub fn effective_log_with_policy(log: &RequestLog, policy: LogPolicy) -> RequestLog {
    let mut effective = log.clone();
    match policy.detail_level {
        LogDetailLevel::Basic => {
            effective.request_body = None;
            effective.response_choices = None;
        }
        LogDetailLevel::Brief => {
            // 只截请求里的消息列表；响应正文与其它摘要字段一律不动。
            if let Some(body) = effective.request_body.as_deref() {
                if let Some(truncated) = truncate_brief_body(body) {
                    effective.request_body = Some(truncated);
                }
            }
        }
        LogDetailLevel::Detailed => {}
    }
    effective
}

/// 把请求正文里的消息列表裁到最新 [`BRIEF_MESSAGE_KEEP`] 条，并在顶层补一个
/// [`BRIEF_MARKER_KEY`] 说明省略了多少、原始多大。
///
/// 覆盖三种下游协议的形态：
/// - Chat Completions / Anthropic Messages：`messages` 数组
/// - Responses：`input` 数组（`input` 为纯字符串时不属消息列表，原样保留）
///
/// 返回 `None` 表示无需改动（不是 JSON、没有消息数组、或本来就没超过保留条数），
/// 调用方据此保留原文 —— 宁可少省一点，也不把无法解析的正文弄成半截。
fn truncate_brief_body(body: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(body).ok()?;
    let object = value.as_object_mut()?;
    let key = if matches!(object.get("messages"), Some(v) if v.is_array()) {
        "messages"
    } else if matches!(object.get("input"), Some(v) if v.is_array()) {
        "input"
    } else {
        return None;
    };
    let total = object.get(key).and_then(|v| v.as_array())?.len();
    if total <= BRIEF_MESSAGE_KEEP {
        return None;
    }
    let omitted = total - BRIEF_MESSAGE_KEEP;
    // 不区分角色：按数组尾部保留最新 BRIEF_MESSAGE_KEEP 条。
    object[key].as_array_mut()?.drain(..omitted);
    object.insert(
        BRIEF_MARKER_KEY.to_string(),
        serde_json::json!({
            "omitted_messages": omitted,
            "kept_messages": BRIEF_MESSAGE_KEEP,
            "original_bytes": body.len(),
        }),
    );
    serde_json::to_string(&value).ok()
}

/// Delete expired rows in small transactions so cleanup does not hold the
/// SQLite write lock for the entire history.
pub async fn cleanup_expired_logs(
    pool: &SqlitePool,
    retention_days: u64,
) -> Result<u64, sqlx::Error> {
    if retention_days == 0 {
        return Ok(0);
    }
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
    let mut deleted = 0;
    loop {
        let mut tx = pool.begin().await?;
        let findings = sqlx::query(
            "DELETE FROM request_security_findings WHERE log_id IN \
             (SELECT id FROM request_logs WHERE created_at < ? LIMIT 500)",
        )
        .bind(&cutoff)
        .execute(&mut *tx)
        .await?;
        let segments = sqlx::query(
            "DELETE FROM stream_segments WHERE log_id IN \
             (SELECT id FROM request_logs WHERE created_at < ? LIMIT 500)",
        )
        .bind(&cutoff)
        .execute(&mut *tx)
        .await?;
        let rows = sqlx::query(
            "DELETE FROM request_logs WHERE id IN \
             (SELECT id FROM request_logs WHERE created_at < ? LIMIT 500)",
        )
        .bind(&cutoff)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        deleted += rows.rows_affected();
        let _ = (findings, segments);
        if rows.rows_affected() == 0 {
            break;
        }
    }
    Ok(deleted)
}

pub async fn run_maintenance_loop(pool: SqlitePool, settings: SettingsStore) {
    apply_settings(&settings);
    let run = || async {
        let policy = policy_from_settings(&settings);
        // 历史统计续接(迁移 046):表空时从现存 request_logs 回填一次,幂等。
        // 放在自动清理之前执行,失败仅告警不阻断维护循环。
        if policy.retention_days != 0 {
            if let Err(error) = crate::db::repository::Repository::new(pool.clone())
                .backfill_usage_stats_if_empty()
                .await
            {
                tracing::warn!(%error, "用量统计历史回填失败");
            }
        }
        if let Err(error) = cleanup_expired_logs(&pool, policy.retention_days).await {
            tracing::warn!(%error, "审计日志自动清理失败");
        }
        apply_settings(&settings);
    };
    run().await;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
    loop {
        interval.tick().await;
        run().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::RequestLog;

    #[test]
    fn basic_policy_drops_large_payloads() {
        let mut log = RequestLog::default();
        log.request_body = Some("large".into());
        log.response_choices = Some("response".into());
        let effective = effective_log_with_policy(&log, LogPolicy::default());
        assert!(effective.request_body.is_none());
        assert!(effective.response_choices.is_none());
    }

    #[test]
    fn detail_level_parser_is_safe() {
        assert_eq!(LogDetailLevel::parse("detailed"), LogDetailLevel::Detailed);
        assert_eq!(LogDetailLevel::parse("brief"), LogDetailLevel::Brief);
        assert_eq!(LogDetailLevel::parse("BRIEF"), LogDetailLevel::Brief);
        assert_eq!(LogDetailLevel::parse("unexpected"), LogDetailLevel::Basic);
        assert_eq!(LogDetailLevel::Brief.as_str(), "brief");
    }

    fn body_with_messages(count: usize) -> String {
        // 内容带填充：贴近 Agent 流量里单条消息的真实体积（工具结果常上百 KB），
        // 否则几十字节的合成消息会被 _wali_brief 标记自身的体积吃掉，测不出省空间。
        let items: Vec<String> = (0..count)
            .map(|i| {
                format!(
                    r#"{{"role":"user","content":"m{} {}"}}"#,
                    i,
                    "x".repeat(400)
                )
            })
            .collect();
        format!(
            r#"{{"model":"gpt-4o","max_tokens":128,"messages":[{}]}}"#,
            items.join(",")
        )
    }

    fn log_with(body: Option<String>, response: Option<String>) -> RequestLog {
        RequestLog {
            request_body: body,
            response_choices: response,
            ..Default::default()
        }
    }

    fn brief_policy() -> LogPolicy {
        LogPolicy {
            detail_level: LogDetailLevel::Brief,
            retention_days: 7,
        }
    }

    fn brief_log(body: &str, response: Option<&str>) -> RequestLog {
        effective_log_with_policy(
            &log_with(Some(body.to_string()), response.map(|v| v.to_string())),
            brief_policy(),
        )
    }

    #[test]
    fn brief_keeps_only_the_last_three_messages() {
        let body = body_with_messages(5);
        let effective = brief_log(&body, None);
        let saved = effective.request_body.expect("简要应保留正文");
        let parsed: serde_json::Value = serde_json::from_str(&saved).unwrap();
        let kept = parsed["messages"].as_array().unwrap();
        assert_eq!(kept.len(), BRIEF_MESSAGE_KEEP, "只应保留最新 3 条");
        // 按尾部保留：被丢掉的是最旧的 m0/m1
        assert!(kept[0]["content"].as_str().unwrap().starts_with("m2"));
        assert!(kept[2]["content"].as_str().unwrap().starts_with("m4"));
        // 消息列表之外的字段必须原样保留
        assert_eq!(parsed["model"], serde_json::json!("gpt-4o"));
        assert_eq!(parsed["max_tokens"], serde_json::json!(128));
        let marker = &parsed[BRIEF_MARKER_KEY];
        assert_eq!(marker["omitted_messages"], serde_json::json!(2));
        assert_eq!(
            marker["kept_messages"],
            serde_json::json!(BRIEF_MESSAGE_KEEP as u64)
        );
        assert_eq!(marker["original_bytes"], serde_json::json!(body.len()));
        assert!(saved.len() < body.len(), "截断后应更小");
    }

    #[test]
    fn brief_does_not_touch_response_content() {
        let response = r#"[{"message":{"content":"full response"}}]"#;
        let effective = effective_log_with_policy(
            &log_with(Some(body_with_messages(9)), Some(response.to_string())),
            brief_policy(),
        );
        assert_eq!(
            effective.response_choices.as_deref(),
            Some(response),
            "「简要」下响应内容必须保持不变"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(effective.request_body.unwrap().as_str()).unwrap();
        assert_eq!(
            parsed["messages"].as_array().unwrap().len(),
            BRIEF_MESSAGE_KEEP
        );
    }

    #[test]
    fn brief_truncates_responses_input_array() {
        let body = r#"{"model":"gpt-5","input":[{"type":"message","role":"user","content":"i0"},{"type":"message","role":"assistant","content":"i1"},{"type":"message","role":"user","content":"i2"},{"type":"message","role":"assistant","content":"i3"},{"type":"message","role":"user","content":"i4"}]}"#;
        let effective = brief_log(body, None);
        let parsed: serde_json::Value =
            serde_json::from_str(effective.request_body.unwrap().as_str()).unwrap();
        let kept = parsed["input"].as_array().unwrap();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[2]["content"], serde_json::json!("i4"));
        assert_eq!(
            parsed[BRIEF_MARKER_KEY]["omitted_messages"],
            serde_json::json!(2)
        );
    }

    #[test]
    fn brief_leaves_short_and_unparseable_bodies_untouched() {
        // 不超过保留条数：原样保留，也不加标记
        let small = body_with_messages(3);
        assert_eq!(
            brief_log(&small, None).request_body.as_deref(),
            Some(small.as_str())
        );

        // Responses 的 input 允许是纯字符串：不属消息列表，原样保留
        let string_input = r#"{"model":"gpt-5","input":"hello"}"#;
        assert_eq!(
            brief_log(string_input, None).request_body.as_deref(),
            Some(string_input)
        );

        // 非 JSON：无法安全截断，原样保留
        let junk = "not-json";
        assert_eq!(brief_log(junk, None).request_body.as_deref(), Some(junk));

        // 没有消息数组：原样保留
        let no_msgs = r#"{"model":"gpt-4o","prompt":"x"}"#;
        assert_eq!(
            brief_log(no_msgs, None).request_body.as_deref(),
            Some(no_msgs)
        );
    }

    #[test]
    fn policy_from_settings_selects_each_level() {
        // 打通「持久化设置 → 落库策略」这条接线：单测 parse 不足以证明
        // 用户在设置页选「简要」后真的会生效。
        let dir = std::env::temp_dir().join(format!("waliapi-logpolicy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SettingsStore::file(dir.join("settings.json"));
        for (value, expect) in [
            ("basic", LogDetailLevel::Basic),
            ("brief", LogDetailLevel::Brief),
            ("detailed", LogDetailLevel::Detailed),
            // 未知/损坏的持久化值必须回落到最省空间的 basic，不能反向扩存储
            ("nonsense", LogDetailLevel::Basic),
        ] {
            store
                .set_many(&[("logs.detail_level".to_string(), serde_json::json!(value))])
                .unwrap();
            let policy = policy_from_settings(&store);
            assert_eq!(
                policy.detail_level, expect,
                "设置值 {} 未被正确识别为策略",
                value
            );
            assert_eq!(normalize_detail_level(value), expect.as_str());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn brief_still_drops_nothing_else_and_detailed_is_unchanged() {
        let body = body_with_messages(4);
        let log = log_with(Some(body.clone()), Some("resp".to_string()));

        let detailed = effective_log_with_policy(
            &log,
            LogPolicy {
                detail_level: LogDetailLevel::Detailed,
                retention_days: 7,
            },
        );
        assert_eq!(detailed.request_body.as_deref(), Some(body.as_str()));
        assert_eq!(detailed.response_choices.as_deref(), Some("resp"));

        let basic = effective_log_with_policy(&log, LogPolicy::default());
        assert!(basic.request_body.is_none() && basic.response_choices.is_none());

        // 简要：响应侧一字不动，只有请求正文被裁
        let brief = effective_log_with_policy(&log, brief_policy());
        assert_eq!(brief.response_choices.as_deref(), Some("resp"));
        assert_ne!(brief.request_body.as_deref(), Some(body.as_str()));
    }
}
