use super::*;
use crate::health_probe::ProbeOutcome;
use sqlx::sqlite::SqlitePoolOptions;
use std::{path::PathBuf, sync::Arc};

struct FileFixture {
    dir: PathBuf,
    pool: SqlitePool,
}

impl FileFixture {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("waliapi-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = Self::connect(&dir).await;
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO channels (id, name, type, base_url, api_key, created_at, updated_at, \
             probe_latency_ms) VALUES ('channel', 'fixture', 'openai', \
             'http://127.0.0.1:1', 'fixture', '2020-01-01T00:00:00.000Z', \
             '2020-01-01T00:00:00.000Z', 321)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("CREATE TABLE probe_write_counter (writes INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO probe_write_counter VALUES (0)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER count_probe_writes \
             AFTER UPDATE OF last_probe_ok, last_probe_at, updated_at ON channels \
             BEGIN UPDATE probe_write_counter SET writes = writes + 1; END",
        )
        .execute(&pool)
        .await
        .unwrap();
        Self { dir, pool }
    }

    async fn connect(dir: &std::path::Path) -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(crate::db::sqlite_connect_options(&dir.join("probe.db")))
            .await
            .unwrap();
        assert!(crate::db::enable_wal_best_effort(&pool).await, "WAL 应启用");
        pool
    }

    async fn writes(&self) -> i64 {
        sqlx::query_scalar("SELECT writes FROM probe_write_counter")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn reset_counter(&self) {
        sqlx::query("UPDATE probe_write_counter SET writes = 0")
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn health(&self) -> (Option<i64>, Option<String>, String, Option<i64>) {
        sqlx::query_as(
            "SELECT last_probe_ok, last_probe_at, updated_at, probe_latency_ms \
             FROM channels WHERE id = 'channel'",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn track_mode_writes(&self) {
        sqlx::query("CREATE TABLE mode_write_counter (writes INTEGER NOT NULL)")
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mode_write_counter VALUES (0)")
            .execute(&self.pool)
            .await
            .unwrap();
        for event in ["INSERT", "UPDATE"] {
            sqlx::query(&format!(
                "CREATE TRIGGER count_mode_{event} AFTER {event} ON channel_mode_health \
                 BEGIN UPDATE mode_write_counter SET writes = writes + 1; END"
            ))
            .execute(&self.pool)
            .await
            .unwrap();
        }
    }

    async fn mode_writes(&self) -> i64 {
        sqlx::query_scalar("SELECT writes FROM mode_write_counter")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn mode_health(
        &self,
        endpoint: &str,
        is_stream: bool,
    ) -> (i64, Option<String>, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT consecutive_failures, cooldown_until, last_failure_at, last_failure_reason \
             FROM channel_mode_health WHERE channel_id = 'channel' AND endpoint = ? AND is_stream = ?",
        )
        .bind(endpoint)
        .bind(i64::from(is_stream))
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn reopen(&mut self) {
        self.pool.close().await;
        self.pool = Self::connect(&self.dir).await;
    }

    async fn close(self) {
        self.pool.close().await;
    }
}

impl Drop for FileFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn duplicate_concurrent_successes_merge_into_one_persistent_update() {
    let fixture = FileFixture::new().await;
    let barrier = Arc::new(tokio::sync::Barrier::new(65));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..64 {
        let pool = fixture.pool.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            Repository::new(pool)
                .mark_probe_ok_if_needed("channel")
                .await
                .unwrap()
        });
    }
    barrier.wait().await;
    let mut updated = 0;
    while let Some(result) = tasks.join_next().await {
        updated += result.unwrap();
    }
    assert_eq!(updated, 1);
    assert_eq!(fixture.writes().await, 1);
    let health = fixture.health().await;
    assert_eq!(health.0, Some(1));
    assert_eq!(health.1.as_deref(), Some(health.2.as_str()));
    chrono::DateTime::parse_from_rfc3339(health.1.as_deref().unwrap()).unwrap();
    assert_eq!(health.3, Some(321), "被动成功不能伪造主动探测延迟");
    Repository::new(fixture.pool.clone())
        .mark_probe_ok("channel")
        .await;
    assert_eq!(fixture.writes().await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn recent_health_skips_writes_and_stale_health_refreshes_both_timestamps() {
    let fixture = FileFixture::new().await;
    let repo = Repository::new(fixture.pool.clone());
    sqlx::query(
        "UPDATE channels SET last_probe_ok = 1, \
         last_probe_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 seconds'), \
         updated_at = 'configuration-update', probe_latency_ms = 87 WHERE id = 'channel'",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture.reset_counter().await;
    let recent = fixture.health().await;
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 0);
    assert_eq!(fixture.health().await, recent);
    assert_eq!(fixture.writes().await, 0);

    sqlx::query(
        "UPDATE channels SET \
         last_probe_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-31 seconds') \
         WHERE id = 'channel'",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture.reset_counter().await;
    let stale = fixture.health().await;
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 1);
    let fresh = fixture.health().await;
    assert_ne!(fresh.1, stale.1);
    assert_eq!(fresh.1.as_deref(), Some(fresh.2.as_str()));
    assert_eq!(fresh.3, Some(87));
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 0);
    assert_eq!(fixture.writes().await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn fresh_probe_and_mode_success_do_not_acquire_a_write_lock() {
    let fixture = FileFixture::new().await;
    fixture.track_mode_writes().await;
    let repo = Repository::new(fixture.pool.clone());
    repo.mark_probe_ok("channel").await;
    repo.record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    fixture.reset_counter().await;
    let mut connections = Vec::new();
    for _ in 0..5 {
        let mut connection = fixture.pool.acquire().await.unwrap();
        // 合成测试直接报告锁冲突，避免以真实等待时间作脆弱断言。
        sqlx::query("PRAGMA busy_timeout = 0")
            .execute(&mut *connection)
            .await
            .unwrap();
        connections.push(connection);
    }
    let mut writer = connections.pop().unwrap();
    drop(connections);
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *writer)
        .await
        .unwrap();
    let blocked = sqlx::query("UPDATE channels SET last_probe_ok = 1 WHERE id = 'missing'")
        .execute(&fixture.pool)
        .await
        .unwrap_err();
    assert!(
        matches!(blocked, sqlx::Error::Database(ref error) if error.code().as_deref() == Some("5"))
    );
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 0);
    repo.record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    sqlx::query("ROLLBACK").execute(&mut *writer).await.unwrap();
    drop(writer);
    assert_eq!(fixture.writes().await, 0);
    assert_eq!(fixture.mode_writes().await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn first_concurrent_mode_successes_insert_one_row_and_one_actual_write() {
    let mut fixture = FileFixture::new().await;
    fixture.track_mode_writes().await;
    let barrier = Arc::new(tokio::sync::Barrier::new(65));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..64 {
        let pool = fixture.pool.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            Repository::new(pool)
                .record_channel_mode_success("channel", "chat_completions", false)
                .await
                .unwrap();
        });
    }
    barrier.wait().await;
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    assert_eq!(fixture.mode_writes().await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_mode_health")
            .fetch_one(&fixture.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        fixture.mode_health("chat_completions", false).await,
        (0, None, None, None)
    );
    fixture.reopen().await;
    Repository::new(fixture.pool.clone())
        .record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    assert_eq!(fixture.mode_writes().await, 1, "重启继续读取持久健康状态");
    assert_eq!(fixture.writes().await, 0, "模式健康不改渠道主动探测列");
    fixture.close().await;
}

#[tokio::test]
async fn mode_failure_recovers_immediately_and_new_failures_keep_transport_isolation() {
    let fixture = FileFixture::new().await;
    fixture.track_mode_writes().await;
    let repo = Repository::new(fixture.pool.clone());
    for (endpoint, stream) in [
        ("chat_completions", false),
        ("chat_completions", true),
        ("responses", false),
    ] {
        repo.record_channel_mode_success("channel", endpoint, stream)
            .await
            .unwrap();
    }
    assert_eq!(fixture.mode_writes().await, 3);
    let failure_at = now_iso();
    let cooldown_until = "2999-01-01T00:00:00.000Z";
    for _ in 0..2 {
        repo.record_channel_mode_failure(
            "channel",
            "chat_completions",
            false,
            &failure_at,
            cooldown_until,
            "fixture transport error",
        )
        .await
        .unwrap();
    }
    assert_eq!(fixture.mode_health("chat_completions", false).await.0, 2);
    assert_eq!(
        fixture
            .mode_health("chat_completions", false)
            .await
            .1
            .as_deref(),
        Some(cooldown_until)
    );
    assert!(repo
        .get_enabled_channels_for_mode("chat_completions", false, &now_iso())
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        repo.get_enabled_channels_for_mode("chat_completions", true, &now_iso())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repo.get_enabled_channels_for_mode("responses", false, &now_iso())
            .await
            .unwrap()
            .len(),
        1
    );
    repo.record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    assert_eq!(
        fixture.mode_health("chat_completions", false).await,
        (0, None, None, None)
    );
    assert_eq!(fixture.mode_writes().await, 6);
    repo.record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    assert_eq!(fixture.mode_writes().await, 6);
    for failures in 1..=2 {
        repo.record_channel_mode_failure(
            "channel",
            "chat_completions",
            false,
            &failure_at,
            cooldown_until,
            "fixture transport error",
        )
        .await
        .unwrap();
        let failed = fixture.mode_health("chat_completions", false).await;
        assert_eq!(failed.0, failures);
        assert_eq!(
            failed.1.as_deref(),
            (failures >= 2).then_some(cooldown_until)
        );
        assert_eq!(failed.2.as_deref(), Some(failure_at.as_str()));
        assert_eq!(failed.3.as_deref(), Some("fixture transport error"));
    }
    repo.record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    assert_eq!(fixture.mode_writes().await, 9);
    assert_eq!(
        fixture.mode_health("chat_completions", true).await,
        (0, None, None, None)
    );
    assert_eq!(
        fixture.mode_health("responses", false).await,
        (0, None, None, None)
    );
    assert_eq!(fixture.health().await.0, None);
    assert_eq!(fixture.writes().await, 0);
    fixture.close().await;
}

#[tokio::test]
async fn mode_success_clears_each_unhealthy_column_including_stale_metadata() {
    let fixture = FileFixture::new().await;
    fixture.track_mode_writes().await;
    let repo = Repository::new(fixture.pool.clone());
    repo.record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    for assignment in [
        "consecutive_failures = 1",
        "cooldown_until = '2999-01-01T00:00:00.000Z'",
        "last_failure_at = '2020-01-01T00:00:00.000Z'",
        "last_failure_reason = ''",
    ] {
        sqlx::query(&format!(
            "UPDATE channel_mode_health SET {assignment} \
             WHERE channel_id = 'channel' AND endpoint = 'chat_completions' AND is_stream = 0"
        ))
        .execute(&fixture.pool)
        .await
        .unwrap();
        let before = fixture.mode_writes().await;
        repo.record_channel_mode_success("channel", "chat_completions", false)
            .await
            .unwrap();
        assert_eq!(
            fixture.mode_health("chat_completions", false).await,
            (0, None, None, None)
        );
        assert_eq!(fixture.mode_writes().await, before + 1);
        repo.record_channel_mode_success("channel", "chat_completions", false)
            .await
            .unwrap();
        assert_eq!(fixture.mode_writes().await, before + 1);
    }
    fixture.close().await;
}

#[tokio::test]
async fn active_probes_and_recovery_are_not_hidden_by_passive_refresh() {
    let fixture = FileFixture::new().await;
    let repo = Repository::new(fixture.pool.clone());
    repo.mark_probe_ok("channel").await;
    fixture.reset_counter().await;
    let failed_at = now_iso();
    repo.record_channel_probe(
        "channel",
        "fixture",
        ProbeOutcome {
            ok: false,
            latency_ms: 991,
        },
        &failed_at,
    )
    .await
    .unwrap();
    assert_eq!(fixture.health().await.0, Some(0));
    assert_eq!(
        fixture.health().await.1.as_deref(),
        Some(failed_at.as_str())
    );
    assert_eq!(fixture.writes().await, 1);
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 1);
    assert_eq!(fixture.health().await.0, Some(1));
    assert_eq!(fixture.health().await.3, Some(991));
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 0);
    assert_eq!(fixture.writes().await, 2);
    let failed_log: (String, i64) = sqlx::query_as(
        "SELECT mode, status_code FROM request_logs WHERE channel_id = 'channel' AND is_probe = 1",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        failed_log,
        ("probe".into(), 502),
        "不修改既有主动探测审计语义"
    );

    let success_at = now_iso();
    repo.record_channel_probe(
        "channel",
        "fixture",
        ProbeOutcome {
            ok: true,
            latency_ms: 33,
        },
        &success_at,
    )
    .await
    .unwrap();
    let active = fixture.health().await;
    assert_eq!(active.0, Some(1));
    assert_eq!(active.1.as_deref(), Some(success_at.as_str()));
    assert_eq!(active.3, Some(33));
    assert_eq!(fixture.writes().await, 3, "新主动探测即使稳态也应立即写入");
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 0);
    assert_eq!(fixture.writes().await, 3);
    fixture.close().await;
}

#[tokio::test]
async fn unknown_invalid_or_future_health_is_refreshed_without_delay() {
    let fixture = FileFixture::new().await;
    let repo = Repository::new(fixture.pool.clone());
    for (ok, timestamp) in [
        (None, Some(now_iso())),
        (Some(1), None),
        (Some(1), Some("invalid timestamp".into())),
        (Some(1), Some("2999-01-01T00:00:00.000Z".into())),
    ] {
        sqlx::query(
            "UPDATE channels SET last_probe_ok = ?, last_probe_at = ? WHERE id = 'channel'",
        )
        .bind(ok)
        .bind(timestamp)
        .execute(&fixture.pool)
        .await
        .unwrap();
        fixture.reset_counter().await;
        assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 1);
        assert_eq!(fixture.writes().await, 1);
        let refreshed = fixture.health().await;
        assert_eq!(refreshed.0, Some(1));
        assert_eq!(refreshed.3, Some(321));
        chrono::DateTime::parse_from_rfc3339(refreshed.1.as_deref().unwrap()).unwrap();
    }
    // 历史时间戳带时区也按同一真实时间比较，而非字符串顺序。
    let offset_timestamp = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
    sqlx::query("UPDATE channels SET last_probe_at = ? WHERE id = 'channel'")
        .bind(offset_timestamp)
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.reset_counter().await;
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 0);
    assert_eq!(fixture.writes().await, 0);
    assert_eq!(repo.mark_probe_ok_if_needed("missing").await.unwrap(), 0);
    fixture.close().await;
}

#[tokio::test]
async fn restart_reads_persistent_health_and_recovers_new_failure() {
    let mut fixture = FileFixture::new().await;
    Repository::new(fixture.pool.clone())
        .mark_probe_ok("channel")
        .await;
    let healthy = fixture.health().await;
    fixture.reopen().await;
    assert_eq!(
        Repository::new(fixture.pool.clone())
            .mark_probe_ok_if_needed("channel")
            .await
            .unwrap(),
        0
    );
    assert_eq!(fixture.health().await, healthy);
    assert_eq!(fixture.writes().await, 1);
    sqlx::query("UPDATE channels SET last_probe_ok = 0 WHERE id = 'channel'")
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.reset_counter().await;
    fixture.reopen().await;
    assert_eq!(
        Repository::new(fixture.pool.clone())
            .mark_probe_ok_if_needed("channel")
            .await
            .unwrap(),
        1
    );
    assert_eq!(fixture.health().await.0, Some(1));
    assert_eq!(fixture.writes().await, 1);
    fixture.close().await;
}

#[tokio::test]
async fn failed_sql_write_and_closed_pool_do_not_suppress_later_recovery() {
    let mut fixture = FileFixture::new().await;
    fixture.track_mode_writes().await;
    sqlx::query(
        "CREATE TRIGGER reject_probe_write BEFORE UPDATE ON channels \
         BEGIN SELECT RAISE(ABORT, 'fixture_probe_write_blocked'); END",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let error = Repository::new(fixture.pool.clone())
        .mark_probe_ok_if_needed("channel")
        .await
        .unwrap_err();
    assert!(matches!(error, sqlx::Error::Database(_)));
    assert_eq!(fixture.writes().await, 0);
    assert_eq!(fixture.health().await.0, None);
    sqlx::query(
        "CREATE TRIGGER reject_mode_write BEFORE INSERT ON channel_mode_health \
         BEGIN SELECT RAISE(ABORT, 'fixture_mode_write_blocked'); END",
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert!(matches!(
        Repository::new(fixture.pool.clone())
            .record_channel_mode_success("channel", "chat_completions", false)
            .await,
        Err(sqlx::Error::Database(_))
    ));
    assert_eq!(fixture.mode_writes().await, 0);
    sqlx::query("DROP TRIGGER reject_probe_write")
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER reject_mode_write")
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.pool.close().await;
    assert!(matches!(
        Repository::new(fixture.pool.clone())
            .mark_probe_ok_if_needed("channel")
            .await,
        Err(sqlx::Error::PoolClosed)
    ));
    assert!(matches!(
        Repository::new(fixture.pool.clone())
            .record_channel_mode_success("channel", "chat_completions", false)
            .await,
        Err(sqlx::Error::PoolClosed)
    ));
    fixture.reopen().await;
    assert_eq!(
        Repository::new(fixture.pool.clone())
            .mark_probe_ok_if_needed("channel")
            .await
            .unwrap(),
        1
    );
    assert_eq!(fixture.health().await.0, Some(1));
    assert_eq!(fixture.writes().await, 1);
    Repository::new(fixture.pool.clone())
        .record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    assert_eq!(fixture.mode_writes().await, 1);
    assert_eq!(
        fixture.mode_health("chat_completions", false).await,
        (0, None, None, None)
    );
    fixture.close().await;
}

#[tokio::test]
async fn cancelled_pool_wait_does_not_leave_a_detached_health_update() {
    let fixture = FileFixture::new().await;
    fixture.track_mode_writes().await;
    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(fixture.pool.acquire().await.unwrap());
    }
    let repo = Repository::new(fixture.pool.clone());
    let mut pending = Box::pin(repo.mark_probe_ok_if_needed("channel"));
    let mut pending_mode =
        Box::pin(repo.record_channel_mode_success("channel", "chat_completions", false));
    tokio::select! {
        biased;
        result = &mut pending => panic!("连接池应仍被占用: {result:?}"),
        result = &mut pending_mode => panic!("模式健康读取应仍在等连接: {result:?}"),
        _ = std::future::ready(()) => {},
    }
    drop(pending);
    drop(pending_mode);
    drop(held);
    tokio::task::yield_now().await;
    assert_eq!(fixture.writes().await, 0);
    assert_eq!(fixture.health().await.0, None);
    assert_eq!(fixture.mode_writes().await, 0);
    assert_eq!(repo.mark_probe_ok_if_needed("channel").await.unwrap(), 1);
    assert_eq!(fixture.writes().await, 1);
    repo.record_channel_mode_success("channel", "chat_completions", false)
        .await
        .unwrap();
    assert_eq!(fixture.mode_writes().await, 1);
    fixture.close().await;
}
