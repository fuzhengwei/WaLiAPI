//! 安全审计模块（最小桩版本）
//! 第1-6节仅定义接口，保证 Proxy 全链路可编译运行；
//! 完整的风险扫描引擎将在第2-2节实现。

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Clean,
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::Clean => "clean",
            RiskLevel::Info => "info",
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
            RiskLevel::Critical => "critical",
        }
    }

    pub fn rank(&self) -> i32 {
        match self {
            RiskLevel::Clean => 0,
            RiskLevel::Info => 1,
            RiskLevel::Low => 2,
            RiskLevel::Medium => 3,
            RiskLevel::High => 4,
            RiskLevel::Critical => 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityAction {
    Allow,
    Warn,
    Redact,
    Confirm,
    Block,
}

impl SecurityAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecurityAction::Allow => "allow",
            SecurityAction::Warn => "warn",
            SecurityAction::Redact => "redact",
            SecurityAction::Confirm => "confirm",
            SecurityAction::Block => "block",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityFinding {
    pub phase: String,
    pub category: String,
    pub rule_id: String,
    pub severity: RiskLevel,
    pub title: String,
    pub description: String,
    pub location: String,
    pub evidence_masked: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityScanResult {
    pub risk_level: RiskLevel,
    pub risk_score: i32,
    pub action: SecurityAction,
    pub sanitized: bool,
    pub blocked_reason: Option<String>,
    pub summary: String,
    pub findings: Vec<SecurityFinding>,
}

impl Default for SecurityScanResult {
    fn default() -> Self {
        Self {
            risk_level: RiskLevel::Clean,
            risk_score: 0,
            action: SecurityAction::Allow,
            sanitized: false,
            blocked_reason: None,
            summary: "未发现明显风险".to_string(),
            findings: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecuritySettings {
    pub enabled: bool,
    pub mode: String,
    pub scan_request: bool,
    pub scan_response: bool,
    pub scan_unicode: bool,
    pub scan_tools: bool,
    pub scan_network: bool,
    pub redact_secrets: bool,
    pub block_on_critical: bool,
    pub max_scan_bytes: usize,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "audit".to_string(),
            scan_request: true,
            scan_response: false,
            scan_unicode: true,
            scan_tools: true,
            scan_network: true,
            redact_secrets: false,
            block_on_critical: false,
            max_scan_bytes: 1024 * 1024,
        }
    }
}

pub fn get_security_settings(_app: &AppHandle) -> SecuritySettings {
    SecuritySettings::default()
}

pub fn scan_request(_body: &serde_json::Value, _settings: &SecuritySettings) -> SecurityScanResult {
    SecurityScanResult::default()
}

pub fn scan_response(_body: &serde_json::Value, _settings: &SecuritySettings) -> SecurityScanResult {
    SecurityScanResult::default()
}

pub fn redact_request_body(body: &serde_json::Value, _settings: &SecuritySettings) -> (serde_json::Value, bool) {
    (body.clone(), false)
}
