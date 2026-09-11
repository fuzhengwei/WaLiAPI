pub mod models;
pub mod repository;

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tauri::{AppHandle, Manager};

/// 迁移前备份文件名前缀。备份形如 `waliapi.db.pre-upgrade-20260806-190400`，与数据库同目录。
const BACKUP_PREFIX: &str = "waliapi.db.pre-upgrade-";

/// 保留的最近备份份数（超出后删除最旧的）。
const BACKUP_KEEP: usize = 3;

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
/// 备份用 SQLite `VACUUM INTO` 生成一致快照（原子、对 live DB 安全），命名
/// `waliapi.db.pre-upgrade-<YYYYmmdd-HHMMSS>`，随后按 `BACKUP_KEEP` 清理旧备份。
/// 恢复为纯文件级：手动把备份文件复制回 `waliapi.db` 即可。
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
    // VACUUM INTO 目标已存在时行为未定义，先清除旧目标（同名同秒重复时防御）
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
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .expect("failed to connect to database");

        // 修复旧版迁移 checksum（v0.1.1 → v0.1.3 兼容）
        fix_legacy_migration_checksums(&pool).await;

        // 迁移前自动备份：schema 落后时先做文件级快照，再跑迁移。
        // 失败不阻断启动——迁移本身是增量、可逆的，备份是额外保险。
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
}
