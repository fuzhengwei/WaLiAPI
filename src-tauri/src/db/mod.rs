pub mod models;
pub mod repository;

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use tauri::{AppHandle, Manager};

/// 迁移前备份文件名前缀。备份形如 `waliapi.db.pre-upgrade-20260806-190400`，与数据库同目录。
const BACKUP_PREFIX: &str = "waliapi.db.pre-upgrade-";

/// 保留的最近备份份数（超出后删除最旧的）。
const BACKUP_KEEP: usize = 3;

/// 与 SQLx 原默认值一致；用于短写锁竞争，不替代请求预算或单实例部署。
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const WAL_AUTOCHECKPOINT_PAGES: u32 = 1_000;

/// 所有池连接使用同一配置，已有 DELETE 库在首次连接时转换为 WAL。
/// 转换要求没有其他实例占用数据库；保持原默认 FULL synchronous。
pub(crate) fn sqlite_connect_options(db_path: &Path) -> SqliteConnectOptions {
    // bundled 依赖固定了修复版本；同时防止构建环境意外链接旧系统库后启用 WAL。
    let version = unsafe { libsqlite3_sys::sqlite3_libversion_number() };
    assert!(version >= 3_051_003, "WAL requires SQLite 3.51.3 or newer");
    SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .pragma("wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES.to_string())
}

/// 不等待活跃读事务结束，也不强制截断 WAL；未回写的帧保留供后续检查点处理。
async fn checkpoint_wal(pool: &SqlitePool) -> Result<(i64, i64, i64), sqlx::Error> {
    sqlx::query_as("PRAGMA wal_checkpoint(PASSIVE)")
        .fetch_one(pool)
        .await
}

pub struct Database {
    pub pool: SqlitePool,
}

/// 修复旧版迁移记录的 checksum，使 v0.1.1 用户升级到 v0.1.3 时不会 VersionMismatch
///
/// v0.1.1 → v0.1.3 迁移文件变更：
/// - 005_add_response_choices.sql → 005_add_response_choices_and_seq.sql（内容变更）
/// - 007 文件可能被本地修改过（description 不匹配）
///
/// 此函数在 sqlx::migrate 之前运行，将旧 checksum 更新为当前文件的 checksum。
/// 仅更新已有记录，不会跳过任何迁移。
async fn fix_legacy_migration_checksums(pool: &SqlitePool) {
    use sha2::Digest;

    // 计算当前迁移文件的 SHA-384 checksum（与 sqlx 算法一致：对文件内容原始字节做 SHA-384）
    let migration_005 = include_str!("../../migrations/005_add_response_choices_and_seq.sql");
    let checksum_005: Vec<u8> = sha2::Sha384::digest(migration_005.as_bytes()).to_vec();

    let migration_007 = include_str!("../../migrations/007_fix_log_seq.sql");
    let checksum_007: Vec<u8> = sha2::Sha384::digest(migration_007.as_bytes()).to_vec();

    // 更新 version=5 和 version=7 的 checksum（BLOB 类型），使其匹配当前文件
    for (version, new_checksum) in [(5i64, checksum_005), (7i64, checksum_007)] {
        let result = sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = ? WHERE version = ? AND checksum != ?",
        )
        .bind(&new_checksum)
        .bind(version)
        .bind(&new_checksum)
        .execute(pool)
        .await;

        if let Ok(res) = result {
            if res.rows_affected() > 0 {
                log::warn!("已修复迁移版本 {} 的 checksum 以兼容 v0.1.3", version);
            }
        }
    }
}

/// 迁移集内的最大版本号（当前编译进二进制的迁移文件）。
fn migration_max_version() -> i64 {
    sqlx::migrate!("./migrations")
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(0)
}

/// 数据库当前迁移版本：`_sqlx_migrations` 中最大的已成功版本。
/// 表不存在或为空时返回 0（全新数据库）。
async fn current_db_version(pool: &SqlitePool) -> i64 {
    let max: Option<i64> =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(pool)
            .await
            .ok()
            .flatten();
    max.unwrap_or(0)
}

/// 生成本次备份路径：`waliapi.db.pre-upgrade-<YYYYmmdd-HHMMSS>`，与数据库同目录。
fn make_backup_path(db_path: &Path) -> PathBuf {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    db_path.with_file_name(format!("{BACKUP_PREFIX}{ts}"))
}

/// 删除同目录下过旧的迁移前备份，只保留最近 `BACKUP_KEEP` 份。
/// 按文件修改时间排序（相同则按名称），新的在前。只匹配 `BACKUP_PREFIX` 前缀文件。
fn prune_old_backups(db_path: &Path) -> Result<(), String> {
    let dir = db_path.parent().ok_or("数据库路径缺少父目录")?;
    let mut backups: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| format!("读取备份目录失败: {e}"))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy();
            if !name.starts_with(BACKUP_PREFIX) {
                return None;
            }
            let mtime = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((mtime, path))
        })
        .collect();

    backups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    for (_mtime, path) in backups.into_iter().skip(BACKUP_KEEP) {
        std::fs::remove_file(&path)
            .map_err(|e| format!("删除旧备份 {} 失败: {e}", path.display()))?;
    }
    Ok(())
}

/// 迁移前自动备份。仅当数据库已存在（有迁移记录）且 schema 版本低于当前迁移集时执行。
///
/// 备份用 SQLite `VACUUM INTO` 生成一致事务快照（包含 WAL 内已提交数据），命名
/// `waliapi.db.pre-upgrade-<YYYYmmdd-HHMMSS>`，随后按 `BACKUP_KEEP` 清理旧备份。
/// 恢复完整快照前必须停止全部实例并关闭连接，再隔离原库及其 -wal/-shm。
/// 中断的 VACUUM INTO 可能留下不完整目标文件，不应作为成功备份使用。
///
/// 无需备份时返回 `Ok(None)`。备份失败只记录错误，不阻断启动。
async fn backup_before_migration(
    pool: &SqlitePool,
    db_path: &Path,
) -> Result<Option<PathBuf>, String> {
    let db_max = current_db_version(pool).await;
    let migration_max = migration_max_version();

    if db_max == 0 || db_max >= migration_max {
        return Ok(None);
    }

    let backup_path = make_backup_path(db_path);
    // VACUUM INTO 拒绝非空目标，先清除旧目标（同名同秒重复时防御）
    if backup_path.exists() {
        std::fs::remove_file(&backup_path)
            .map_err(|e| format!("移除旧备份 {} 失败: {e}", backup_path.display()))?;
    }
    let dest = backup_path.to_string_lossy().replace('\'', "''");
    sqlx::query(&format!("VACUUM INTO '{dest}'"))
        .execute(pool)
        .await
        .map_err(|e| format!("创建备份失败: {e}"))?;

    log::info!(
        "迁移前已备份数据库 (schema {db_max} -> {migration_max}): {}",
        backup_path.display()
    );

    prune_old_backups(db_path)?;

    Ok(Some(backup_path))
}

impl Database {
    pub async fn new(app: &AppHandle) -> Self {
        let app_data_dir = app
            .path()
            .app_data_dir()
            .expect("failed to get app data dir");
        Self::new_with_path(&app_data_dir).await
    }

    /// headless（waliapi-web）入口：显式指定数据目录。
    pub async fn new_with_path(app_data_dir: &Path) -> Self {
        std::fs::create_dir_all(app_data_dir).expect("failed to create app data dir");

        let db_path = app_data_dir.join("waliapi.db");

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(sqlite_connect_options(&db_path))
            .await
            .expect("failed to connect to database");

        // 修复旧版迁移 checksum（v0.1.1 → v0.1.3 兼容）
        fix_legacy_migration_checksums(&pool).await;

        // 迁移前自动备份：schema 落后时先做文件级快照，再跑迁移。
        // 失败继续启动沿用既有 best-effort 策略；不保证迁移普遍可逆。
        if let Err(e) = backup_before_migration(&pool, &db_path).await {
            log::error!("迁移前自动备份失败，继续启动: {e}");
        }

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("failed to run database migrations");

        // 补建升级前切片的关键词投影，不改动已有正文和向量。
        crate::services::knowledge::repository::KbRepository::new(pool.clone())
            .backfill_search_text()
            .await
            .expect("failed to backfill knowledge search index");

        // Seed built-in security rules if table exists and is empty
        let _ = crate::security::rules::seed_builtin_rules(&pool).await;

        // 启动写入后尽量回写；运行中由每个连接的自动检查点继续处理。
        // PASSIVE 未完成回写不影响已提交数据，检查点失败也不改变原启动异常语义。
        match checkpoint_wal(&pool).await {
            Ok((busy, wal_pages, checkpointed_pages)) => log::debug!(
                "SQLite WAL 检查点: busy={busy}, wal_pages={wal_pages}, checkpointed_pages={checkpointed_pages}"
            ),
            Err(error) => log::warn!("SQLite WAL 检查点失败，继续启动: {error}"),
        }

        Self { pool }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::time::{Duration, SystemTime};

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("waliapi-backup-{}", uuid::Uuid::new_v4()))
    }

    async fn test_pool(db_path: &Path) -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", db_path.display()))
            .await
            .expect("connect test db")
    }

    async fn wal_pool(db_path: &Path, max_connections: u32) -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(max_connections)
            .connect_with(sqlite_connect_options(db_path))
            .await
            .expect("connect WAL fixture")
    }

    /// 在数据库里模拟旧版迁移记录（版本 5）与一条业务数据。
    async fn seed_legacy_db(pool: &SqlitePool) {
        sqlx::query("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO items (name) VALUES ('pre-upgrade-data')")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE _sqlx_migrations (version INTEGER PRIMARY KEY, success INTEGER)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations (version, success) VALUES (5, 1)")
            .execute(pool)
            .await
            .unwrap();
    }

    #[test]
    fn make_backup_path_uses_pre_upgrade_prefix() {
        let db_path = Path::new("/tmp/waliapi/waliapi.db");
        let backup = make_backup_path(db_path);
        let name = backup.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with(BACKUP_PREFIX), "备份名应带前缀: {name}");
        let ts = &name[BACKUP_PREFIX.len()..];
        assert_eq!(ts.len(), 15, "时间戳应为 YYYYmmdd-HHMMSS: {ts}");
        assert_eq!(ts.as_bytes()[8], b'-', "时间戳第 9 位应为分隔符: {ts}");
        assert!(
            ts.chars()
                .enumerate()
                .filter(|(i, _)| *i != 8)
                .all(|(_, c)| c.is_ascii_digit()),
            "时间戳除分隔符外应全为数字: {ts}"
        );
        assert_eq!(backup.parent(), Some(Path::new("/tmp/waliapi")));
    }

    #[test]
    fn prune_keeps_latest_three_and_ignores_others() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();

        // 5 份按时间递增的备份（名称与 mtime 一致：090000 最旧 … 090004 最新）
        let names: Vec<String> = (0..5)
            .map(|i| format!("{BACKUP_PREFIX}20260806-09000{i}"))
            .collect();
        for (i, name) in names.iter().enumerate() {
            let p = dir.join(name);
            std::fs::write(&p, b"backup").unwrap();
            let f = std::fs::File::open(&p).unwrap();
            let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(i as u64);
            let _ = f.set_modified(ts);
        }
        // 无关文件不应被清理
        std::fs::write(dir.join("waliapi.db"), b"db").unwrap();
        std::fs::write(dir.join("config.toml.waliapi-backup"), b"cfg").unwrap();

        prune_old_backups(&dir.join("waliapi.db")).unwrap();

        let remaining: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(BACKUP_PREFIX))
            .collect();
        assert_eq!(remaining.len(), 3, "应只剩 3 份备份: {remaining:?}");
        for name in remaining.iter() {
            let newest = ["090002", "090003", "090004"];
            assert!(
                newest.iter().any(|s| name.ends_with(s)),
                "应保留最新的三份，但找到: {name}"
            );
        }
        assert!(dir.join("waliapi.db").exists(), "数据库文件不应被清理");
        assert!(
            dir.join("config.toml.waliapi-backup").exists(),
            "配置文件不应被清理"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn creates_backup_when_schema_is_behind() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("waliapi.db");
        let pool = test_pool(&db_path).await;
        seed_legacy_db(&pool).await;

        let backup = backup_before_migration(&pool, &db_path)
            .await
            .unwrap()
            .expect("schema 落后时应创建备份");
        assert!(backup.exists(), "备份文件应存在: {}", backup.display());

        // 备份内容一致：业务数据完整，迁移版本仍是旧版本 5
        let backup_pool = test_pool(&backup).await;
        let name: String = sqlx::query_scalar("SELECT name FROM items WHERE id = 1")
            .fetch_one(&backup_pool)
            .await
            .unwrap();
        assert_eq!(name, "pre-upgrade-data");
        let ver: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&backup_pool)
            .await
            .unwrap();
        assert_eq!(ver, 5, "备份应保留升级前的旧版本记录");

        // VACUUM INTO 是只读快照：备份后同一连接必须仍可写，迁移才能继续
        sqlx::query("INSERT INTO items (name) VALUES ('post-backup')")
            .execute(&pool)
            .await
            .expect("备份后连接应仍可写");

        pool.close().await;
        backup_pool.close().await;
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn no_backup_when_already_latest() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("waliapi.db");
        let pool = test_pool(&db_path).await;
        seed_legacy_db(&pool).await;
        // 把迁移记录改成当前最新版本
        let max = migration_max_version();
        sqlx::query("UPDATE _sqlx_migrations SET version = ? WHERE version = 5")
            .bind(max)
            .execute(&pool)
            .await
            .unwrap();

        let result = backup_before_migration(&pool, &db_path).await.unwrap();
        assert!(result.is_none(), "已是最新时不应备份");

        let backups: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(BACKUP_PREFIX))
            .collect();
        assert!(backups.is_empty(), "不应产生备份: {backups:?}");

        pool.close().await;
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn new_database_configures_wal_and_timeout_on_every_pool_connection() {
        let dir = temp_dir();
        let db = Database::new_with_path(&dir).await;
        // 同时持有五个连接，确保不是在同一连接上重复检查 PRAGMA。
        let mut connections = Vec::new();
        for _ in 0..5 {
            connections.push(db.pool.acquire().await.unwrap());
        }
        for connection in &mut connections {
            let sqlite_version: String = sqlx::query_scalar("SELECT sqlite_version()")
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            assert_eq!(sqlite_version, "3.51.3", "SQLx 使用已修复的实际 SQLite");
            let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            assert_eq!(journal, "wal");
            let timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            assert_eq!(timeout, 5_000);
            let pages: i64 = sqlx::query_scalar("PRAGMA wal_autocheckpoint")
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            assert_eq!(pages, 1_000);
            let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(&mut **connection)
                .await
                .unwrap();
            assert_eq!(synchronous, 2, "保持 FULL synchronous");
        }
        drop(connections);
        db.pool.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn startup_converts_legacy_delete_database_and_close_reopen_preserves_data() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("waliapi.db");
        let legacy = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Delete),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE legacy_items (name TEXT NOT NULL)")
            .execute(&legacy)
            .await
            .unwrap();
        sqlx::query("INSERT INTO legacy_items VALUES ('committed-before-WAL')")
            .execute(&legacy)
            .await
            .unwrap();
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&legacy)
            .await
            .unwrap();
        assert_eq!(journal, "delete");
        legacy.close().await;

        let db = Database::new_with_path(&dir).await;
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        sqlx::query("INSERT INTO legacy_items VALUES ('committed-after-WAL')")
            .execute(&db.pool)
            .await
            .unwrap();
        db.pool.close().await;

        let reopened = Database::new_with_path(&dir).await;
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM legacy_items ORDER BY rowid")
            .fetch_all(&reopened.pool)
            .await
            .unwrap();
        assert_eq!(names, ["committed-before-WAL", "committed-after-WAL"]);
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&reopened.pool)
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        reopened.pool.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wal_reader_keeps_snapshot_without_blocking_writer_or_passive_checkpoint() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let pool = wal_pool(&dir.join("waliapi.db"), 5).await;
        sqlx::query("CREATE TABLE status_fixture (status TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO status_fixture VALUES ('before')")
            .execute(&pool)
            .await
            .unwrap();
        checkpoint_wal(&pool).await.unwrap();

        let mut reader = pool.begin().await.unwrap();
        let before: String = sqlx::query_scalar("SELECT status FROM status_fixture")
            .fetch_one(&mut *reader)
            .await
            .unwrap();
        assert_eq!(before, "before");
        tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query("UPDATE status_fixture SET status = 'after'").execute(&pool),
        )
        .await
        .expect("WAL 读快照不应阻止另一个连接提交短写入")
        .unwrap();
        let snapshot: String = sqlx::query_scalar("SELECT status FROM status_fixture")
            .fetch_one(&mut *reader)
            .await
            .unwrap();
        assert_eq!(snapshot, "before", "原读快照仍然一致");
        let (_, pages, checkpointed) =
            tokio::time::timeout(Duration::from_secs(2), checkpoint_wal(&pool))
                .await
                .expect("PASSIVE 不等待长读结束")
                .unwrap();
        assert!(pages > checkpointed, "长读保留了尚不能回写的 WAL 帧");
        reader.rollback().await.unwrap();
        let (busy, pages, checkpointed) = checkpoint_wal(&pool).await.unwrap();
        assert_eq!(busy, 0);
        assert_eq!(pages, checkpointed);
        let committed: String = sqlx::query_scalar("SELECT status FROM status_fixture")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(committed, "after");
        pool.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn vacuum_backup_restores_committed_data_that_is_only_in_wal() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("waliapi.db");
        let pool = wal_pool(&db_path, 1).await;
        seed_legacy_db(&pool).await;
        checkpoint_wal(&pool).await.unwrap();
        // 单连接关闭自动检查点，证明新提交仍只位于 WAL，而不是主数据库文件。
        sqlx::query("PRAGMA wal_autocheckpoint=0")
            .execute(&pool)
            .await
            .unwrap();
        let main_before = std::fs::read(&db_path).unwrap();
        sqlx::query("INSERT INTO items (name) VALUES ('committed-only-in-WAL')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&db_path).unwrap(), main_before);
        assert!(std::fs::metadata(dir.join("waliapi.db-wal")).unwrap().len() > 32);

        let backup = backup_before_migration(&pool, &db_path)
            .await
            .unwrap()
            .unwrap();
        let restored_path = dir.join("restored.db");
        // VACUUM 成功后目标是独立完整快照；恢复到没有旧 sidecar 的新路径。
        std::fs::copy(&backup, &restored_path).unwrap();
        let restored = wal_pool(&restored_path, 1).await;
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM items ORDER BY id")
            .fetch_all(&restored)
            .await
            .unwrap();
        assert_eq!(names, ["pre-upgrade-data", "committed-only-in-WAL"]);
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&restored)
            .await
            .unwrap();
        assert_eq!(version, 5);
        sqlx::query("INSERT INTO items (name) VALUES ('source-still-writable')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        restored.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wal_backup_error_preserves_source_data_and_keeps_it_writable() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let pool = wal_pool(&dir.join("waliapi.db"), 1).await;
        seed_legacy_db(&pool).await;
        let invalid_target = dir.join("nonexistent-parent").join("waliapi.db");
        let error = backup_before_migration(&pool, &invalid_target)
            .await
            .unwrap_err();
        assert!(error.starts_with("创建备份失败:"));
        let original: String = sqlx::query_scalar("SELECT name FROM items WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(original, "pre-upgrade-data");
        sqlx::query("INSERT INTO items (name) VALUES ('continue-after-backup-error')")
            .execute(&pool)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        pool.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
