use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub channel_type: String,
    pub base_url: String,
    pub api_key: String,
    pub models: String,
    pub status: i64,
    pub priority: i64,
    pub weight: i64,
    pub config: String,
    pub model_mapping: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_test_at: Option<String>,
    pub last_test_ok: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateChannelInput {
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub priority: Option<i64>,
    pub weight: Option<i64>,
    pub config: Option<serde_json::Value>,
    pub model_mapping: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateChannelInput {
    pub id: String,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub channel_type: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub models: Option<Vec<String>>,
    pub status: Option<i64>,
    pub priority: Option<i64>,
    pub weight: Option<i64>,
    pub config: Option<serde_json::Value>,
    pub model_mapping: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub key: String,
    pub status: i64,
    pub allowed_models: String,
    pub allowed_channels: String,
    pub quota_limit: i64,
    pub quota_used: i64,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateApiKeyInput {
    pub name: String,
    pub allowed_models: Option<Vec<String>>,
    pub allowed_channels: Option<Vec<String>>,
    pub quota_limit: Option<i64>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestLog {
    pub id: String,
    pub seq: Option<i64>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub model: String,
    pub upstream_model: Option<String>,
    pub mode: String,
    pub status_code: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub duration_ms: i64,
    pub error_message: Option<String>,
    pub is_stream: i64,
    pub is_retry: i64,
    pub created_at: String,
    pub request_body: Option<String>,
    pub risk_level: String,
    pub risk_score: i64,
    pub risk_summary: Option<String>,
    pub security_action: String,
    pub sanitized: i64,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardStats {
    pub today_requests: i64,
    pub today_total_tokens: i64,
    pub active_channels: i64,
    pub avg_latency_ms: f64,
    pub total_channels: i64,
    pub total_api_keys: i64,
    pub total_requests: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct LogStats {
    pub date: String,
    pub count: i64,
    pub total_tokens: i64,
}

pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}


#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestSecurityFinding {
    pub id: String,
    pub log_id: String,
    pub phase: String,
    pub category: String,
    pub rule_id: String,
    pub severity: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub evidence_masked: Option<String>,
    pub evidence_hash: Option<String>,
    pub action: Option<String>,
    pub created_at: String,
}
