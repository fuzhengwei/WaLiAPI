//! 渠道主动健康探测（C-04）：后台循环对启用中的 API 渠道发廉价探测
//! （GET {base_url}/models，带渠道 Key，短超时），结果每轮落渠道探测列；
//! 审计日志只反映**状态翻转**：正常→异常新增一条 `mode='probe'`/502；异常→正常不新增行，
//! 而是把那条失败行就地标记为已恢复；稳态（持续正常、持续故障）一行都不写
//! （探测行无正文、统计口径又一律排除，逐轮写只会淹没真实流量）。
//! 探测失败的渠道在候选排序中**沉底不剔除**——
//! 保守策略，误杀渠道的代价高于排序损失。OAuth/Auth 账号渠道零探测
//! （账号表不参与本循环，成本与配额敏感）。
//!
//! 与既有 `channel_mode_health` 被动冷却**正交**：那是请求驱动的
//! （渠道×端点×流式）模式级冷却；本模块是渠道级的主动信号，
//! GET /models 可达 ≠ 聊天模式健康，互不写对方的表。

use crate::db::repository::Repository;
use crate::settings_store::SettingsStore;
use sqlx::SqlitePool;
use std::time::Duration;

const DEFAULT_INTERVAL_SECS: u64 = 300;
/// 探测超时：比普通请求更严——探测的意义就是快速判定可达性。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// 单渠道探测结果。
#[derive(Debug, Clone, Copy)]
pub struct ProbeOutcome {
    pub ok: bool,
    pub latency_ms: i64,
}

/// 对单个渠道发一次廉价探测。GET {base_url}/models（2xx 即健康）。
pub async fn probe_channel(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> ProbeOutcome {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let started = std::time::Instant::now();
    let ok = client
        .get(&url)
        .bearer_auth(api_key)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map(|response| response.status().is_success())
        .unwrap_or(false);
    ProbeOutcome {
        ok,
        latency_ms: started.elapsed().as_millis() as i64,
    }
}

/// 单轮探测步骤（测试与循环共用）：探测全部启用渠道 → 每轮写探测列，
/// 但审计日志只反映状态翻转：故障新增一行 `probe`/502，恢复则就地标记那条失败行、
/// 不新增行（详见 [`Repository::record_channel_probe`]）。
/// 探测关闭（`probe.enabled=false`）时零网络请求零写库。
pub async fn probe_step(
    pool: &SqlitePool,
    settings: &SettingsStore,
) -> Vec<(String, ProbeOutcome)> {
    if !settings.get_bool("probe.enabled", true) {
        return Vec::new();
    }
    let repo = Repository::new(pool.clone());
    let channels = match repo.get_enabled_channels().await {
        Ok(channels) => channels,
        Err(error) => {
            tracing::warn!("[探测] 拉取启用渠道失败: {error}");
            return Vec::new();
        }
    };
    let client = reqwest::Client::new();
    let mut outcomes = Vec::new();
    for channel in channels {
        let outcome = probe_channel(&client, &channel.base_url, &channel.api_key).await;
        tracing::debug!(
            "[探测] 渠道 {} ({}) -> ok={} latency={}ms",
            channel.name,
            channel.id,
            outcome.ok,
            outcome.latency_ms
        );
        if let Err(error) = repo
            .record_channel_probe(
                &channel.id,
                &channel.name,
                outcome,
                &crate::db::models::now_iso(),
            )
            .await
        {
            tracing::warn!("[探测] 写入渠道 {} 探测结果失败: {error}", channel.id);
        }
        outcomes.push((channel.id, outcome));
    }
    outcomes
}

/// 后台探测循环：默认 300s 一轮，可整体关闭（关闭时零后台流量）。
pub async fn run_probe_loop(pool: SqlitePool, settings: SettingsStore) {
    // 启动期先清一次历史噪音：0.3.1 及更早版本每轮探测都写一行“探测成功”，存量可达上千
    // 条且无人读取，会一直霸占审计日志前几页。放在循环前、且不受 probe.enabled 影响 ——
    // 噪音是既成的，即使把探测关掉也该清掉。只删 is_probe=1 的 2xx 行，审计记录不受影响。
    match Repository::new(pool.clone())
        .purge_successful_probe_logs()
        .await
    {
        Ok(0) => {}
        Ok(deleted) => tracing::info!("[探测] 启动清理：删除历史探测成功日志 {deleted} 条"),
        Err(error) => tracing::warn!("[探测] 启动清理历史探测日志失败: {error}"),
    }

    loop {
        let interval = Duration::from_secs(
            settings
                .get_u64("probe.interval_secs", DEFAULT_INTERVAL_SECS)
                .max(30),
        );
        let outcomes = probe_step(&pool, &settings).await;
        if !outcomes.is_empty() {
            let failed = outcomes.iter().filter(|(_, o)| !o.ok).count();
            if failed > 0 {
                tracing::info!(
                    "[探测] 本轮 {} 渠道，{} 个不健康（排序沉底）",
                    outcomes.len(),
                    failed
                );
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_db() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn settings_at(dir: &std::path::Path, enabled: bool) -> SettingsStore {
        let store = SettingsStore::file(dir.join("settings.json"));
        store
            .set_many(&[("probe.enabled".to_string(), serde_json::json!(enabled))])
            .unwrap();
        store
    }

    async fn seed_channel(pool: &SqlitePool, id: &str, base_url: &str) {
        sqlx::query(
            "INSERT INTO channels (id, name, type, base_url, api_key, models, status, priority, \
             weight, config, model_mapping, timeout_secs, identity_revision, created_at, updated_at) \
             VALUES (?, ?, 'openai', ?, 'sk-x', '[]', 1, 1, 1, '{}', '{}', 60, 0, ?, ?)",
        )
        .bind(id)
        .bind(id)
        .bind(base_url)
        .bind(crate::db::models::now_iso())
        .bind(crate::db::models::now_iso())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn mock_models_endpoint() -> String {
        let app = axum::Router::new().route("/models", axum::routing::get(|| async { "[]" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn probe_step_updates_columns_but_logs_only_new_failures() {
        let pool = memory_db().await;
        let dir = std::env::temp_dir().join(format!("waliapi-probe-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = settings_at(&dir, true);

        let healthy_base = mock_models_endpoint().await;
        seed_channel(&pool, "ch-ok", &healthy_base).await;
        // 不可达端口（保留地址，必然连接失败）
        seed_channel(&pool, "ch-bad", "http://127.0.0.1:1").await;
        let outcomes = probe_step(&pool, &settings).await;
        assert_eq!(outcomes.len(), 2, "两个启用渠道都应被探测");

        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT last_probe_ok, probe_latency_ms FROM channels WHERE id = 'ch-ok'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, Some(1), "健康渠道 last_probe_ok=1");

        let row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT last_probe_ok, probe_latency_ms FROM channels WHERE id = 'ch-bad'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, Some(0), "不可达渠道 last_probe_ok=0");

        // 探测状态列每轮都更新（调度排序要用），但日志行只在“正常 → 异常”这条边上写：
        // 成功渠道一条都不写，失败渠道写一条 mode=probe / 502。
        let probes: Vec<(String, String, i64)> = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT channel_name, mode, status_code FROM request_logs WHERE is_probe = 1",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            probes,
            vec![("ch-bad".to_string(), "probe".to_string(), 502)],
            "成功探测不得入库；只有失败的那条留痕"
        );
    }

    /// 渠道持续故障时只留第一条异常，不逐轮累积（300s 一轮 × 一个故障渠道
    /// 一天能刷出 288 行，那是同一种噪音的另一副面孔）。
    #[tokio::test]
    async fn repeated_probe_failure_records_only_first_edge() {
        let pool = memory_db().await;
        let dir = std::env::temp_dir().join(format!("waliapi-probe-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = settings_at(&dir, true);
        seed_channel(&pool, "ch-bad", "http://127.0.0.1:1").await;

        for _ in 0..3 {
            probe_step(&pool, &settings).await;
        }

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE is_probe = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "连续失败只记第一条边");
        // 但调度状态必须每轮都刷新，否则探测功能本身失效
        let ok: Option<i64> =
            sqlx::query_scalar("SELECT last_probe_ok FROM channels WHERE id = 'ch-bad'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ok, Some(0));
    }

    /// 已落库的探测日志行（mode, status_code, upstream_model），按 seq 升序。
    async fn probe_log_rows(pool: &SqlitePool) -> Vec<(String, i64, Option<String>)> {
        sqlx::query_as::<_, (String, i64, Option<String>)>(
            "SELECT mode, status_code, upstream_model FROM request_logs WHERE is_probe = 1 \
             ORDER BY seq ASC",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// 状态翻转的两条边：故障新增一行；恢复**不新增行**，只就地标记那条故障行。
    /// 稳态（持续正常、持续故障）什么都不写。
    #[tokio::test]
    async fn probe_state_transitions_are_recorded() {
        let pool = memory_db().await;
        let dir = std::env::temp_dir().join(format!("waliapi-probe-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = settings_at(&dir, true);
        seed_channel(&pool, "ch-flaky", "http://127.0.0.1:1").await;

        // 1) 正常 → 异常，2) 持续故障：只应有第一条
        probe_step(&pool, &settings).await;
        probe_step(&pool, &settings).await;
        assert_eq!(
            probe_log_rows(&pool).await,
            vec![("probe".to_string(), 502, None::<String>)],
            "持续故障不得逐轮累积"
        );

        // 3) 异常 → 正常：行数不变，只把那条失败行就地标记
        let healthy = mock_models_endpoint().await;
        sqlx::query("UPDATE channels SET base_url = ? WHERE id = 'ch-flaky'")
            .bind(&healthy)
            .execute(&pool)
            .await
            .unwrap();
        probe_step(&pool, &settings).await;
        assert_eq!(
            probe_log_rows(&pool).await,
            vec![(
                "probe_recovered".to_string(),
                502,
                Some("已恢复".to_string())
            )],
            "恢复须就地标记：行数不变、状态仍是 502（不篡改历史）、标记文案写在 upstream_model"
        );

        // 4) 持续正常：不写也不改
        probe_step(&pool, &settings).await;
        assert_eq!(probe_log_rows(&pool).await.len(), 1, "持续正常不写日志");

        // 5) 再次故障：新增一行，且只标记最近一条未标记的失败行
        sqlx::query("UPDATE channels SET base_url = 'http://127.0.0.1:1' WHERE id = 'ch-flaky'")
            .execute(&pool)
            .await
            .unwrap();
        probe_step(&pool, &settings).await;
        assert_eq!(
            probe_log_rows(&pool).await,
            vec![
                (
                    "probe_recovered".to_string(),
                    502,
                    Some("已恢复".to_string())
                ),
                ("probe".to_string(), 502, None),
            ],
            "再故障应新增一行，旧标记行不受影响"
        );
    }

    /// 恢复时若库里没有待标记的失败行（故障发生在老版本、或已被启动清理删掉），
    /// 必须静默跳过，绝不为恢复事件补写一行 —— 那正是本次要消灭的东西。
    #[tokio::test]
    async fn probe_recovery_without_failure_row_writes_nothing() {
        let pool = memory_db().await;
        let dir = std::env::temp_dir().join(format!("waliapi-probe-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = settings_at(&dir, true);
        let healthy = mock_models_endpoint().await;
        seed_channel(&pool, "ch-ok", &healthy).await;
        // 把渠道置成“上一轮异常”，但库里没有对应的失败日志行
        sqlx::query("UPDATE channels SET last_probe_ok = 0 WHERE id = 'ch-ok'")
            .execute(&pool)
            .await
            .unwrap();

        probe_step(&pool, &settings).await;
        assert_eq!(
            probe_log_rows(&pool).await,
            Vec::new(),
            "无待标记行时不得新增任何探测日志"
        );
        // 但调度用的状态列要正常复位
        let ok: Option<i64> =
            sqlx::query_scalar("SELECT last_probe_ok FROM channels WHERE id = 'ch-ok'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ok, Some(1));
    }

    /// 启动清理的边界：所有 2xx 的探测行都该清掉（0.3.1 的“探测成功”、以及中间版本为
    /// 恢复补写的 200 行）；真实审计记录、探测失败行、就地标记为已恢复的 502 行都不动。
    #[tokio::test]
    async fn purge_removes_only_successful_probe_rows() {
        let pool = memory_db().await;
        let repo = Repository::new(pool.clone());
        let now = crate::db::models::now_iso();
        // (id, mode, status_code, is_probe)
        for (id, mode, status, is_probe) in [
            ("real", "chat", 200, 0),               // 真实请求：保留
            ("ok-probe", "probe", 200, 1),          // 0.3.1 成功噪音：删
            ("bad-probe", "probe", 502, 1),         // 探测失败：保留
            ("interim", "probe_recovered", 200, 1), // 中间版本的恢复行：删
            ("marked", "probe_recovered", 502, 1),  // 就地标记的失败行：保留
        ] {
            sqlx::query(
                "INSERT INTO request_logs (id, seq, model, mode, status_code, duration_ms, \
                 is_stream, is_retry, created_at, risk_level, security_action, upstream_type, \
                 is_probe) \
                 VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM request_logs), 'm', ?, \
                 ?, 10, 0, 0, ?, 'low', 'audit', 'channel', ?)",
            )
            .bind(id)
            .bind(mode)
            .bind(status)
            .bind(&now)
            .bind(is_probe)
            .execute(&pool)
            .await
            .unwrap();
        }

        let deleted = repo.purge_successful_probe_logs().await.unwrap();
        assert_eq!(
            deleted, 2,
            "应删掉两条 2xx 探测行（成功噪音 + 中间版本恢复行）"
        );

        let left: Vec<(String, String, i64, i64)> =
            sqlx::query_as::<_, (String, String, i64, i64)>(
                "SELECT id, mode, status_code, is_probe FROM request_logs ORDER BY id",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left.len(), 3);
        assert!(
            left.iter().any(|r| r.0 == "real" && r.3 == 0),
            "真实审计记录必须保留"
        );
        assert!(
            left.iter().any(|r| r.0 == "bad-probe" && r.2 == 502),
            "探测失败记录必须保留"
        );
        assert!(
            left.iter().any(|r| r.0 == "marked" && r.2 == 502),
            "就地标记为已恢复的失败行必须保留（它是 502，不是 2xx）"
        );
    }

    #[tokio::test]
    async fn probe_disabled_means_zero_traffic_and_writes() {
        let pool = memory_db().await;
        let dir = std::env::temp_dir().join(format!("waliapi-probe-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = settings_at(&dir, false);

        let base = mock_models_endpoint().await;
        seed_channel(&pool, "ch-x", &base).await;

        let outcomes = probe_step(&pool, &settings).await;
        assert!(outcomes.is_empty(), "关闭时零探测");
        let probes: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE is_probe = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(probes, 0, "关闭时零日志写入");
        let never: Option<i64> =
            sqlx::query_scalar("SELECT last_probe_ok FROM channels WHERE id = 'ch-x'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(never.is_none(), "关闭时探测列保持 NULL（从未探测）");
    }

    #[tokio::test]
    async fn unhealthy_channels_sink_in_candidate_ordering() {
        let pool = memory_db().await;
        let repo = Repository::new(pool.clone());
        // 高优先级但探测失败 / 中优先级从未探测 / 低优先级探测健康
        for (id, priority) in [("sink", 9), ("mid", 5), ("ok", 1)] {
            sqlx::query(
                "INSERT INTO channels (id, name, type, base_url, api_key, models, status, priority, \
                 weight, config, model_mapping, timeout_secs, identity_revision, created_at, updated_at) \
                 VALUES (?, ?, 'openai', 'http://127.0.0.1:1', 'sk-x', '[]', 1, ?, 1, '{}', '{}', 60, 0, ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(priority)
            .bind(crate::db::models::now_iso())
            .bind(crate::db::models::now_iso())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("UPDATE channels SET last_probe_ok = 0 WHERE id = 'sink'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE channels SET last_probe_ok = 1 WHERE id = 'ok'")
            .execute(&pool)
            .await
            .unwrap();

        let ordered = repo.get_enabled_channels().await.unwrap();
        let ids: Vec<&str> = ordered.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mid", "ok", "sink"],
            "探测失败渠道沉底（不剔除）；健康档内按优先级/权重，从未探测视为健康"
        );
    }

    #[tokio::test]
    async fn usage_stats_exclude_probe_rows() {
        let pool = memory_db().await;
        let repo = Repository::new(pool.clone());
        let now = crate::db::models::now_iso();
        // 一条正常请求行 + 一条探测行（model 都为 m）
        for (model, is_probe) in [("m", 0), ("m", 1)] {
            sqlx::query(
                "INSERT INTO request_logs (id, seq, model, mode, status_code, duration_ms, \
                 is_stream, is_retry, created_at, risk_level, security_action, upstream_type, \
                 is_probe, total_tokens) \
                 VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM request_logs), ?, 'chat', 200, \
                 10, 0, 0, ?, 'low', 'audit', 'channel', ?, 100)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(model)
            .bind(&now)
            .bind(is_probe)
            .execute(&pool)
            .await
            .unwrap();
        }

        let model_stats = repo.get_model_stats().await.unwrap();
        let row = model_stats
            .iter()
            .find(|s| s.model == "m")
            .expect("正常行应计入模型统计");
        assert_eq!(
            row.request_count, 1,
            "探测行不计入用量统计（is_probe=0 过滤）"
        );

        let dashboard = repo.get_dashboard_stats().await.unwrap();
        assert_eq!(dashboard.total_requests, 1, "仪表盘请求数排除探测行");
    }

    /// 被动反哺：探测失败的渠道经一次 mark_probe_ok 立即恢复排序位
    /// （验收标准「一次真实请求成功 → 恢复正常排序」）。
    #[tokio::test]
    async fn mark_probe_ok_recovers_ordering_after_real_success() {
        let pool = memory_db().await;
        let repo = Repository::new(pool.clone());
        for (id, priority) in [("sink", 9), ("healthy", 1)] {
            sqlx::query(
                "INSERT INTO channels (id, name, type, base_url, api_key, models, status, priority, \
                 weight, config, model_mapping, timeout_secs, identity_revision, created_at, updated_at) \
                 VALUES (?, ?, 'openai', 'http://127.0.0.1:1', 'sk-x', '[]', 1, ?, 1, '{}', '{}', 60, 0, ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(priority)
            .bind(crate::db::models::now_iso())
            .bind(crate::db::models::now_iso())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("UPDATE channels SET last_probe_ok = 0 WHERE id = 'sink'")
            .execute(&pool)
            .await
            .unwrap();

        // 反哺前：sink 沉底
        let ids: Vec<String> = repo
            .get_enabled_channels()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["healthy", "sink"]);

        // 真实请求成功路径调用的反哺 → 立即恢复高优先级位
        repo.mark_probe_ok("sink").await;
        let ids: Vec<String> = repo
            .get_enabled_channels()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["sink", "healthy"], "被动反哺后应恢复优先级排序");
    }

    /// 两轨覆盖：driver 轨入口的 get_enabled_channels_for_mode 同样吃沉底排序键
    /// （且与 mode_health 冷却并存时，冷却剔除优先、健康排序作用于剩余候选）。
    #[tokio::test]
    async fn mode_query_also_sinks_unhealthy_candidates() {
        let pool = memory_db().await;
        let repo = Repository::new(pool.clone());
        for (id, priority, probe_ok) in [("hot", 9, 0), ("mid", 5, 1), ("low", 1, 1)] {
            sqlx::query(
                "INSERT INTO channels (id, name, type, base_url, api_key, models, status, priority, \
                 weight, config, model_mapping, timeout_secs, identity_revision, created_at, updated_at, \
                 last_probe_ok) \
                 VALUES (?, ?, 'openai', 'http://127.0.0.1:1', 'sk-x', '[]', 1, ?, 1, '{}', '{}', 60, 0, ?, ?, ?)",
            )
            .bind(id)
            .bind(id)
            .bind(priority)
            .bind(crate::db::models::now_iso())
            .bind(crate::db::models::now_iso())
            .bind(probe_ok)
            .execute(&pool)
            .await
            .unwrap();
        }

        let ids: Vec<String> = repo
            .get_enabled_channels_for_mode("chat_completions", false, &crate::db::models::now_iso())
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(
            ids,
            vec!["mid", "low", "hot"],
            "for_mode 查询同样沉底探测失败渠道（两轨一致的排序语义）"
        );
    }
}
