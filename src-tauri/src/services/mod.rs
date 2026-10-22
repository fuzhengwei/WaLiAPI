pub mod builtin_channels;
pub mod channel_test;
pub mod knowledge;
pub mod mcp;
pub mod software;
pub mod upstream_models;
pub mod wiki;

use crate::server::router::SharedState;
use crate::AppState;
use async_trait::async_trait;
use axum::Router;
use serde::Serialize;
use std::sync::Arc;

/// Service trait — all services implement this interface
#[async_trait]
pub trait Service: Send + Sync {
    /// Service unique id
    fn id(&self) -> &'static str;
    /// Display name
    fn name(&self) -> &'static str;
    /// Description
    fn description(&self) -> &'static str;
    /// Whether enabled
    fn enabled(&self) -> bool {
        true
    }
    /// Service status
    async fn status(&self, state: &Arc<AppState>) -> ServiceStatus;
    /// Register routes
    fn routes(&self, state: Arc<AppState>) -> Router<SharedState>;
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub running: bool,
    pub stats: serde_json::Value,
}

/// Service manager
pub struct ServiceRegistry {
    services: Vec<Box<dyn Service>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        let mut registry = Self { services: vec![] };
        registry.register(Box::new(knowledge::KnowledgeService));
        registry.register(Box::new(mcp::McpService));
        registry.register(Box::new(software::SoftwareService));
        registry.register(Box::new(wiki::WikiService));
        registry
    }

    pub fn register(&mut self, service: Box<dyn Service>) {
        self.services.push(service);
    }

    /// Merge all service routes into one Router
    pub fn merge_routes(&self, state: Arc<AppState>) -> Router<SharedState> {
        let mut router = Router::new();
        for service in &self.services {
            if service.enabled() {
                router = router.merge(service.routes(state.clone()));
            }
        }
        router
    }

    /// 合并指定服务（按 id）的路由。服务端点按鉴权域分组装配：
    /// knowledge/wiki 挂管理员 token、mcp 挂独立 MCP token，见 router::build_router。
    pub fn merge_routes_for(&self, ids: &[&str], state: Arc<AppState>) -> Router<SharedState> {
        let mut router = Router::new();
        for service in &self.services {
            if service.enabled() && ids.contains(&service.id()) {
                router = router.merge(service.routes(state.clone()));
            }
        }
        router
    }

    /// Get all service statuses
    pub async fn list_status(&self, state: &Arc<AppState>) -> Vec<ServiceStatus> {
        let mut result = vec![];
        for service in &self.services {
            result.push(service.status(state).await);
        }
        result
    }
}
