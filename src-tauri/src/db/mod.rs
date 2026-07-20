pub mod models;
pub mod repository;

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tauri::{AppHandle, Manager};

pub struct Database {
    pub pool: SqlitePool,
}

impl Database {
    pub async fn new(app: &AppHandle) -> Self {
        let app_data_dir = app
            .path()
            .app_data_dir()
            .expect("failed to get app data dir");

        std::fs::create_dir_all(&app_data_dir).expect("failed to create app data dir");

        let db_path = app_data_dir.join("waliapi.db");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .expect("failed to connect to database");

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .ok();

        // Seed built-in security rules if table exists and is empty
        let _ = crate::security::rules::seed_builtin_rules(&pool).await;

        Self { pool }
    }
}
