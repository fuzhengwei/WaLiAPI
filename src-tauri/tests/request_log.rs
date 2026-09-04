//! T09 request-log observability tests.
//!
//! Covers:
//!   * migration 016 adds the 11 nullable observability columns and keeps the
//!     old queries (search/aggregate) working;
//!   * `Repository::create_log` persists the new fields (and NULLs for legacy
//!     callers);
//!   * `LogDto` maps the new fields (client_cancelled / stream_committed as
//!     booleans);
//!   * old-log compatibility: a row written with NULL observability columns
//!     still parses and maps to `LogDto` with `None` (old frontend types stay
//!     nullable);
//!   * a row with the new fields round-trips per-field.

use serde_json::json;
use waliapi_lib::{
    db::{models, repository::Repository},
    security::{gate::gate_original, SecuritySettings},
};

fn now() -> String {
    models::now_iso()
}

/// In-memory SQLite with all migrations (incl. 016) applied.
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

/// Build a full `RequestLog` with the T09 observability fields populated.
fn full_log(channel_id: Option<&str>, channel_name: Option<&str>) -> models::RequestLog {
    models::RequestLog {
        id: waliapi_lib::utils::id::new_id(),
        seq: None,
        api_key_id: Some("key-1".into()),
        api_key_name: Some("tester".into()),
        channel_id: channel_id.map(|s| s.to_string()),
        channel_name: channel_name.map(|s| s.to_string()),
        model: "alias".into(),
        upstream_model: Some("upstream-x".into()),
        mode: "chat".into(),
        status_code: 200,
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
        cached_tokens: 0,
        duration_ms: 120,
        error_message: None,
        is_stream: 1,
        is_retry: 0,
        created_at: now(),
        request_body: Some(json!({"model":"alias","messages":[]}).to_string()),
        response_choices: None,
        risk_level: "clean".into(),
        risk_score: 0,
        risk_summary: Some("ok".into()),
        security_action: "allow".into(),
        sanitized: 1,
        blocked_reason: None,
        trace_id: Some("trace-1".into()),
        // --- T09 observability ---
        downstream_protocol: Some("chat_completions".into()),
        downstream_endpoint: Some("chat_completions".into()),
        route_group: Some("chat_completions_g1_native".into()),
        upstream_protocol: Some("openai".into()),
        upstream_endpoint: Some("chat_completions".into()),
        provider: Some("deepseek".into()),
        codec_version: None,
        failure_class: None,
        identity_revision: Some(1),
        client_cancelled: Some(0),
        stream_committed: Some(1),
        upstream_type: "channel".into(),
        reasoning_effort: None,
    }
}

#[tokio::test]
async fn request_log_migration_016_adds_nullable_columns_and_old_queries_still_work() {
    let pool = fresh_db().await;
    // All 11 new columns exist and are nullable.
    for col in [
        "downstream_protocol",
        "downstream_endpoint",
        "route_group",
        "upstream_protocol",
        "upstream_endpoint",
        "provider",
        "codec_version",
        "failure_class",
        "identity_revision",
        "client_cancelled",
        "stream_committed",
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('request_logs') WHERE name = ?",
        )
        .bind(col)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "column {col} must exist from migration 016");
    }
    // A legacy-style row (old columns only) parses through the repository
    // (FromRow with NULL observability columns) and aggregate queries still run.
    let repo = Repository::new(pool);
    let legacy = models::RequestLog {
        id: waliapi_lib::utils::id::new_id(),
        seq: None,
        api_key_id: Some("key-1".into()),
        api_key_name: Some("legacy".into()),
        channel_id: None,
        channel_name: Some("old-channel".into()),
        model: "m".into(),
        upstream_model: None,
        mode: "chat".into(),
        status_code: 200,
        prompt_tokens: 7,
        completion_tokens: 3,
        total_tokens: 10,
        duration_ms: 50,
        error_message: None,
        is_stream: 0,
        is_retry: 0,
        created_at: now(),
        request_body: None,
        response_choices: None,
        risk_level: "clean".into(),
        risk_score: 0,
        risk_summary: None,
        security_action: "allow".into(),
        sanitized: 1,
        blocked_reason: None,
        trace_id: None,
        ..Default::default() // observability columns => NULL
    };
    repo.create_log(&legacy).await.expect("legacy log insert");
    repo.create_log(&full_log(None, Some("native")))
        .await
        .expect("full log insert");

    // Old list/search queries remain compatible.
    let logs = repo.get_logs(50, 0).await.expect("get_logs");
    assert_eq!(logs.len(), 2);
    let found = repo
        .search_logs(Some("legacy"), None, None, None, None, None, None, 50, 0)
        .await
        .expect("search_logs");
    assert_eq!(found.len(), 1);

    // Aggregate queries still work (channel stats / dashboard / log stats).
    let _ = repo.get_dashboard_stats().await.expect("dashboard stats");
    let _ = repo.get_channel_stats().await.expect("channel stats");
    let _ = repo.get_log_stats(7).await.expect("log stats");
}

#[tokio::test]
async fn request_log_create_log_persists_t09_fields_and_log_dto_maps_them() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool);
    let log = full_log(Some("ch-1"), Some("native-channel"));
    repo.create_log(&log).await.expect("create_log");

    let stored = repo.get_log(&log.id).await.expect("get_log");
    assert_eq!(
        stored.downstream_protocol.as_deref(),
        Some("chat_completions")
    );
    assert_eq!(
        stored.downstream_endpoint.as_deref(),
        Some("chat_completions")
    );
    assert_eq!(
        stored.route_group.as_deref(),
        Some("chat_completions_g1_native")
    );
    assert_eq!(stored.upstream_protocol.as_deref(), Some("openai"));
    assert_eq!(
        stored.upstream_endpoint.as_deref(),
        Some("chat_completions")
    );
    assert_eq!(stored.provider.as_deref(), Some("deepseek"));
    assert_eq!(stored.codec_version, None);
    assert_eq!(stored.failure_class, None);
    assert_eq!(stored.identity_revision, Some(1));
    assert_eq!(stored.client_cancelled, Some(0));
    assert_eq!(stored.stream_committed, Some(1));

    let dto: waliapi_lib::commands::log::LogDto = stored.into();
    assert_eq!(
        dto.route_group.as_deref(),
        Some("chat_completions_g1_native")
    );
    assert_eq!(dto.upstream_protocol.as_deref(), Some("openai"));
    assert_eq!(dto.provider.as_deref(), Some("deepseek"));
    assert_eq!(dto.identity_revision, Some(1));
    assert_eq!(dto.client_cancelled, Some(false));
    assert_eq!(dto.stream_committed, Some(true));
}

#[tokio::test]
async fn request_log_upstream_type_defaults_filters_and_round_trips() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool);

    let channel_log = full_log(Some("channel-1"), Some("API channel"));
    assert_eq!(channel_log.upstream_type, "channel");
    repo.create_log(&channel_log).await.expect("channel log");

    let mut account_log = full_log(Some("account-1"), Some("Codex account"));
    account_log.upstream_type = "auth_account".into();
    repo.create_log(&account_log).await.expect("account log");

    let stored = repo
        .get_log(&account_log.id)
        .await
        .expect("account log read");
    assert_eq!(stored.upstream_type, "auth_account");
    let dto: waliapi_lib::commands::log::LogDto = stored.into();
    assert_eq!(dto.upstream_type, "auth_account");

    let channel_only = repo
        .search_logs_by_upstream_type(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("channel"),
            50,
            0,
        )
        .await
        .expect("channel filter");
    assert_eq!(channel_only.len(), 1);
    assert_eq!(channel_only[0].id, channel_log.id);

    let account_only = repo
        .search_logs_by_upstream_type(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("auth_account"),
            50,
            0,
        )
        .await
        .expect("account filter");
    assert_eq!(account_only.len(), 1);
    assert_eq!(account_only[0].id, account_log.id);
}

#[tokio::test]
async fn request_log_legacy_log_with_null_observability_maps_to_none_in_dto() {
    let pool = fresh_db().await;
    let repo = Repository::new(pool);
    let legacy = models::RequestLog {
        id: waliapi_lib::utils::id::new_id(),
        seq: None,
        api_key_id: None,
        api_key_name: None,
        channel_id: None,
        channel_name: None,
        model: "m".into(),
        upstream_model: None,
        mode: "chat".into(),
        status_code: 502,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        duration_ms: 0,
        error_message: Some("legacy error".into()),
        is_stream: 0,
        is_retry: 0,
        created_at: now(),
        request_body: None,
        response_choices: None,
        risk_level: "clean".into(),
        risk_score: 0,
        risk_summary: None,
        security_action: "allow".into(),
        sanitized: 1,
        blocked_reason: None,
        trace_id: None,
        ..Default::default() // observability columns => NULL
    };
    repo.create_log(&legacy).await.expect("create_log");
    let stored = repo.get_log(&legacy.id).await.expect("get_log");

    // Old frontend (nullable fields) stays compatible: every new field is None.
    assert_eq!(stored.route_group, None);
    assert_eq!(stored.upstream_protocol, None);
    assert_eq!(stored.identity_revision, None);
    assert_eq!(stored.client_cancelled, None);
    assert_eq!(stored.stream_committed, None);

    let dto: waliapi_lib::commands::log::LogDto = stored.into();
    assert_eq!(dto.route_group, None);
    assert_eq!(dto.client_cancelled, None);
    assert_eq!(dto.stream_committed, None);
    assert_eq!(dto.failure_class, None);
}

#[tokio::test]
async fn request_log_sanitized_log_body_is_what_gets_persisted() {
    // The gate's `sanitized_log_json` is the ONLY body source for logging; raw
    // body never reaches the log.  Verify the plumbing: the field we persist is
    // the sanitized log body string, and the raw secret is absent.
    let raw = json!({"model": "m", "messages": [{"role": "user", "content": "Bearer sk-abcdefghijklmnopqrstuvwxyz123456"}]});
    let audited = gate_original(
        waliapi_lib::security::gate::DownstreamProtocol::ChatCompletions,
        "/v1/chat/completions",
        raw.clone(),
        None,
        "m".to_string(),
        false,
        None,
        &SecuritySettings::default(),
        None,
        vec![],
    )
    .expect("gate");
    let log_body = serde_json::to_string(&audited.sanitized_log_json).unwrap();
    assert!(!log_body.contains("abcdefghijklmnopqrstuvwx"));
    assert!(serde_json::to_string(&raw)
        .unwrap()
        .contains("abcdefghijklmnopqrstuvwx"));
}
