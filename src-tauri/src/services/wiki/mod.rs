pub mod backup;
pub mod handlers;
pub mod ingest;
pub mod models;
pub mod project;
pub mod rebuild;
pub mod repository;
pub mod routes;

use crate::services::{Service, ServiceStatus};
use crate::AppState;
use async_trait::async_trait;
use axum::Router;
use std::sync::Arc;

pub struct WikiService;

#[async_trait]
impl Service for WikiService {
    fn id(&self) -> &'static str {
        "wiki"
    }

    fn name(&self) -> &'static str {
        "Wiki"
    }

    fn description(&self) -> &'static str {
        "LLM 增量 RAG：自动摄入文档 → 生成结构化 Wiki 页面 → 知识图谱 → 深度研究 → MCP 工具暴露"
    }

    async fn status(&self, state: &Arc<AppState>) -> ServiceStatus {
        let pool = &state.db.pool;
        let project_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wiki_projects WHERE status = 1")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        let page_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wiki_pages WHERE status = 'active'")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        let source_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM wiki_sources WHERE status = 'ingested'")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

        ServiceStatus {
            id: self.id().to_string(),
            name: self.name().to_string(),
            description: self.description().to_string(),
            enabled: true,
            running: true,
            stats: serde_json::json!({
                "projects": project_count,
                "pages": page_count,
                "sources": source_count,
            }),
        }
    }

    fn routes(&self, state: Arc<AppState>) -> Router<crate::server::router::SharedState> {
        routes::create_router(state)
    }
}
