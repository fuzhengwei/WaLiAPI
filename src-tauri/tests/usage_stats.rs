//! 用量统计聚合表(迁移 046)集成测试。
//!
//! 覆盖:
//!   * Task 2:create_log 在同一事务内 UPSERT usage_stats —— 同 (hour,model)
//!     不同渠道/Key 分别累加;探测行(record_channel_probe)不产生统计行;
//!   * Task 3:五个统计接口读 usage_stats,口径与等价 request_logs 聚合一致
//!     (dashboard 今日/累计、模型分布、趋势、按天、渠道/Key 统计);
//!   * Task 4:多条件组合清理 —— 时间+渠道、状态码、keep_recent_days;
//!     clear_stats=false 只删日志不动统计;true 同步减少;dry_run 只预览;
//!   * Task 5:回填幂等 —— 表空时从 request_logs 聚合,重复执行不重写。

use waliapi_lib::{
    commands::log::DeleteLogsInput,
    db::{models, repository::Repository},
};

fn now() -> String {
    models::now_iso()
}

/// In-memory SQLite with all migrations (incl. 046) applied.
async fn fresh_db() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory db");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrate fresh db");
    pool
}

fn log(
    id: &str,
    api_key_id: Option<&str>,
    channel_id: Option<&str>,
    model: &str,
    status: i64,
    prompt: i64,
    completion: i64,
    total: i64,
    duration: i64,
    created_at: &str,
) -> models::RequestLog {
    models::RequestLog {
        id: id.into(),
        seq: None,
        api_key_id: api_key_id.map(|s| s.to_string()),
        api_key_name: None,
        channel_id: channel_id.map(|s| s.to_string()),
        channel_name: None,
        model: model.into(),
        upstream_model: None,
        mode: "chat".into(),
        status_code: status,
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_tokens: 0,
        duration_ms: duration,
        error_message: None,
        is_stream: 0,
        is_retry: 0,
        created_at: created_at.into(),
        request_body: None,
        response_choices: None,
        risk_level: "clean".into(),
        risk_score: 0,
        risk_summary: None,
        security_action: "allow".into(),
        sanitized: 0,
        blocked_reason: None,
        trace_id: None,
        reasoning_effort: None,
        downstream_protocol: None,
        downstream_endpoint: None,
        route_group: None,
        upstream_protocol: None,
        upstream_endpoint: None,
        provider: None,
        codec_version: None,
        failure_class: None,
        identity_revision: None,
        client_cancelled: None,
        stream_committed: None,
        upstream_type: "channel".into(),
    }
}

/// Task 2:同 (hour, model) 不同渠道/Key 分别累加;成功/失败计数正确。
#[tokio::test]
async fn create_log_accumulates_usage_stats() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool.clone());

    let t = "2026-09-04T08:15:00.000Z";
    repo.create_log(&log(
        "a",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "b",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "c",
        Some("k2"),
        Some("c1"),
        "m1",
        500,
        10,
        0,
        10,
        5,
        t,
    ))
    .await
    .unwrap();
    repo.create_log(&log("d", Some("k1"), Some("c2"), "m2", 200, 1, 1, 2, 3, t))
        .await
        .unwrap();

    let rows: Vec<(
        String,
        String,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    )> = sqlx::query_as(
        "SELECT hour, model, channel_id, api_key_id, request_count, success_count, \
             fail_count, prompt_tokens, completion_tokens, total_tokens, cached_tokens, \
             total_duration_ms FROM usage_stats ORDER BY model, channel_id, api_key_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    // 同 (hour,m1,c1,k1) 两行合并为一行:2 次请求、2 成功、token 翻倍、耗时翻倍。
    assert_eq!(rows.len(), 3, "应按四维键聚合: {rows:?}");
    let (h, m, c, k, req, suc, fail, p, comp, tot, cached, dur) = &rows[0];
    assert_eq!(h, "2026-09-04T08:00:00.000Z", "hour 应落到小时桶: {h}");
    assert_eq!((m.as_str(), c.as_str(), k.as_str()), ("m1", "c1", "k1"));
    assert_eq!((req, suc, fail), (&2, &2, &0));
    assert_eq!((p, comp, tot, cached, dur), (&200, &40, &240, &0, &100));

    let (_, _, _, _, req, suc, fail, _, _, _, _, _) = &rows[1];
    assert_eq!((req, suc, fail), (&1, &0, &1), "500 应计 fail");

    // 探测行路径不产生统计行。
    repo.record_channel_probe(
        "c1",
        "chan",
        waliapi_lib::health_probe::ProbeOutcome {
            ok: true,
            latency_ms: 3,
        },
        &now(),
    )
    .await
    .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_stats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, 3, "探测不应新增统计行");

    pool.close().await;
}

/// Task 3:统计接口读 usage_stats,与等价 request_logs 聚合一致。
#[tokio::test]
async fn stats_read_from_usage_stats_matches_logs() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool.clone());

    // 今天的两条 + 昨天的一条(is_probe=0 语义下全部计入)。
    // 时间必须相对当前 UTC 动态生成,与 get_dashboard_stats 的 chrono::Utc::now()
    // 今日口径对齐;写死日期会在「今天≠测试写死日」时让今日断言失效。
    let today = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();
    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();
    repo.create_log(&log(
        "a",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        &today,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "b",
        Some("k1"),
        Some("c1"),
        "m1",
        500,
        10,
        0,
        10,
        5,
        &today,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "c",
        Some("k2"),
        Some("c2"),
        "m2",
        200,
        1,
        1,
        2,
        3,
        &yesterday,
    ))
    .await
    .unwrap();

    let dash = repo.get_dashboard_stats().await.unwrap();
    // 今日请求 2(today_requests)、今日 Token 130、累计请求 3、累计 Token 132。
    assert_eq!(dash.today_requests, 2);
    assert_eq!(dash.today_total_tokens, 130);
    assert_eq!(dash.total_requests, 3);
    assert_eq!(dash.total_tokens, 132);

    let model_stats = repo.get_model_stats().await.unwrap();
    assert_eq!(model_stats.len(), 2);
    let m1 = model_stats.iter().find(|s| s.model == "m1").unwrap();
    assert_eq!(m1.request_count, 2);
    assert_eq!(m1.total_tokens, 130);
    assert!((m1.success_rate - 0.5).abs() < 1e-4, "m1 成功率应 0.5");
    assert_eq!(m1.avg_latency_ms as i64, 27, "m1 平均延迟 (50+5)/2");

    // 趋势:今天 08 点桶 m1 一行(含 2 请求),昨天 22 点桶 m2 一行。
    let trend = repo.get_token_trend(48).await.unwrap();
    assert_eq!(trend.len(), 2);
    assert!(trend
        .iter()
        .any(|p| p.model == "m1" && p.request_count == 2));

    // 按天统计:今日、昨日各一天。
    let by_day = repo.get_log_stats(7).await.unwrap();
    assert_eq!(by_day.len(), 2);
    let today_day = &today[..10];
    assert_eq!(
        by_day.iter().find(|s| s.date == today_day).unwrap().count,
        2
    );

    // 渠道统计:排除空渠道;c1 2 次、c2 1 次。
    let channels = repo.get_channel_stats().await.unwrap();
    assert_eq!(channels.len(), 2);
    let c1 = channels.iter().find(|s| s.channel_id == "c1").unwrap();
    assert_eq!(c1.total_calls, 2);
    assert_eq!(c1.success_calls, 1);
    assert_eq!(c1.failed_calls, 1);

    // Key 统计:排除空 Key;k1 2 次、k2 1 次。
    let keys = repo.get_api_key_stats().await.unwrap();
    assert_eq!(keys.len(), 2);
    let k1 = keys.iter().find(|s| s.api_key_id == "k1").unwrap();
    assert_eq!(k1.total_calls, 2);

    pool.close().await;
}

/// Task 4:多条件组合清理 —— clear_stats=false 只删日志不动统计;dry_run 只预览。
#[tokio::test]
async fn delete_logs_multi_filter_keeps_stats_by_default() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool.clone());

    let t1 = "2026-09-01T08:15:00.000Z";
    let t2 = "2026-09-02T08:15:00.000Z";
    repo.create_log(&log(
        "a",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t1,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "b",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t2,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "c",
        Some("k1"),
        Some("c1"),
        "m1",
        500,
        10,
        0,
        10,
        5,
        t2,
    ))
    .await
    .unwrap();
    repo.create_log(&log("d", Some("k2"), Some("c2"), "m2", 200, 1, 1, 2, 3, t2))
        .await
        .unwrap();

    // dry_run:预览匹配行数,不删除。
    let preview = repo
        .delete_logs_matching(
            &DeleteLogsInput {
                before_date: Some("2026-09-02T00:00:00.000Z".into()),
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(preview.0, 1, "09-01 之前只有 a 一条");
    let logs_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(logs_left, 4, "dry_run 不得删除");

    // 按状态码清理 500:仅删 c;clear_stats=false → 统计行数不变。
    let (deleted, stats_deleted) = repo
        .delete_logs_matching(
            &DeleteLogsInput {
                status_code: Some(500),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(stats_deleted, 0, "默认不清理统计");
    let logs_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(logs_left, 3);
    let stats_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_stats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stats_left, 3, "统计不受日志清理影响");

    // 按渠道 c1 清理:剩 d(m2/c2)一条;再断言统计仍未动。
    let (deleted, _) = repo
        .delete_logs_matching(
            &DeleteLogsInput {
                channel_id: Some("c1".into()),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(deleted, 2, "c1 的 a/b 两条(500 的 c 已删)");
    let stats_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_stats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stats_left, 3, "统计仍不受影响");

    pool.close().await;
}

/// Task 4:clear_stats=true 时按同条件同步减少统计;clear_usage_stats 独立清统计。
#[tokio::test]
async fn clear_stats_follows_filter_and_independent_clear() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool.clone());

    let t1 = "2026-09-01T08:15:00.000Z";
    let t2 = "2026-09-02T08:15:00.000Z";
    repo.create_log(&log(
        "a",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t1,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "b",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t2,
    ))
    .await
    .unwrap();
    repo.create_log(&log("c", Some("k2"), Some("c2"), "m2", 200, 1, 1, 2, 3, t2))
        .await
        .unwrap();

    // clear_stats=true + before 09-02 → 删 a 及其统计行。
    let (deleted, stats_deleted) = repo
        .delete_logs_matching(
            &DeleteLogsInput {
                before_date: Some("2026-09-02T00:00:00.000Z".into()),
                clear_stats: Some(true),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(stats_deleted, 1, "09-01 桶统计行同步清除");
    let stats_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_stats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stats_left, 2);

    // 独立 clear_usage_stats:只清统计,不动日志。
    let cleared = repo
        .clear_usage_stats_matching(&DeleteLogsInput::default())
        .await
        .unwrap();
    assert_eq!(cleared, 2);
    let logs_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(logs_left, 2, "独立清统计不动日志");

    pool.close().await;
}

/// Task 5:回填幂等 —— 表空时从 request_logs 聚合,重复执行不重写。
#[tokio::test]
async fn backfill_is_idempotent_and_matches_logs() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool.clone());

    let t1 = "2026-09-01T08:15:00.000Z";
    let t2 = "2026-09-02T08:15:00.000Z";
    repo.create_log(&log(
        "a",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t1,
    ))
    .await
    .unwrap();
    repo.create_log(&log(
        "b",
        Some("k1"),
        Some("c1"),
        "m1",
        200,
        100,
        20,
        120,
        50,
        t2,
    ))
    .await
    .unwrap();

    // 清空统计表模拟「旧库升级后为空」。
    sqlx::query("DELETE FROM usage_stats")
        .execute(&pool)
        .await
        .unwrap();

    let filled = repo.backfill_usage_stats_if_empty().await.unwrap();
    assert_eq!(filled, 2, "两条日志 → 两个小时桶各一行");
    let (req, total): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(request_count),0), COALESCE(SUM(total_tokens),0) FROM usage_stats",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((req, total), (2, 240), "回填与日志聚合一致");

    // 幂等:再跑返回 0,不重写。
    let again = repo.backfill_usage_stats_if_empty().await.unwrap();
    assert_eq!(again, 0);
    let req_after: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(request_count),0) FROM usage_stats")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(req_after, 2, "重复回填不得翻倍");

    pool.close().await;
}

/// 服务可用率健康口径(渠道健康度设计 v2):
/// - 分母 = 启用渠道(status=1);分子 = 启用且健康(COALESCE(last_probe_ok,1)=1)
/// - 主动禁用的渠道不计入分子分母,不拉低可用率;
/// - 只有真实探测失败(last_probe_ok=0)才让可用率下降;
/// - 账号侧分母=未禁用,分子=未禁用且凭证 active。
#[tokio::test]
async fn service_availability_excludes_disabled_upstreams() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool.clone());
    let now = models::now_iso();

    async fn ins_channel(
        pool: &sqlx::SqlitePool,
        id: &str,
        status: i64,
        last_probe_ok: Option<i64>,
    ) {
        let now = models::now_iso();
        let lpk = last_probe_ok.map(|v| v.to_string());
        sqlx::query(
            "INSERT INTO channels (id, name, type, base_url, api_key, models, status, priority, \
             weight, config, model_mapping, timeout_secs, identity_revision, created_at, updated_at, \
             last_probe_ok) \
             VALUES (?, ?, 'openai', 'http://127.0.0.1:1', 'sk-x', '[]', ?, 1, 1, '{}', '{}', 60, 0, ?, ?, ?)",
        )
        .bind(id)
        .bind(id)
        .bind(status)
        .bind(&now)
        .bind(&now)
        .bind(lpk)
        .execute(pool)
        .await
        .unwrap();
    }

    // 场景:两个启用渠道(一个健康、一个探测失败)+ 一个禁用渠道。
    ins_channel(&pool, "c-healthy", 1, Some(1)).await;
    ins_channel(&pool, "c-down", 1, Some(0)).await;
    ins_channel(&pool, "c-disabled", 0, Some(1)).await;

    // 一个启用账号、一个禁用账号。
    sqlx::query(
        "INSERT INTO auth_accounts (id, provider, label, account_id, disabled, status, payload_json, created_at, updated_at) \
         VALUES ('a-active', 'openai', 'a', 'act', 0, 'active', '{}', ?, ?)",
    )
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO auth_accounts (id, provider, label, account_id, disabled, status, payload_json, created_at, updated_at) \
         VALUES ('a-disabled', 'openai', 'a', 'act2', 1, 'active', '{}', ?, ?)",
    )
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .unwrap();

    let dash = repo.get_dashboard_stats().await.unwrap();
    // 渠道:分母=2 个启用渠道;分子=仅健康渠道 1 个(c-down 探测失败、c-disabled 禁用都不计)。
    assert_eq!(dash.total_channels, 2, "禁用渠道不进分母");
    assert_eq!(dash.active_channels, 1, "探测失败渠道不计入分子");
    // 账号:分母=未禁用的 1 个;分子=未禁用且 active 的 1 个。
    assert_eq!(dash.total_auth_accounts, 1, "禁用账号不进分母");
    assert_eq!(dash.active_auth_accounts, 1);

    pool.close().await;
}
