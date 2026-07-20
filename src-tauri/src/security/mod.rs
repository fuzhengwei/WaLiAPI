use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub mod scanner;
pub mod redact;
pub mod rules;

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
            enabled: true,
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

pub fn get_security_settings(app: &AppHandle) -> SecuritySettings {
    let defaults = SecuritySettings::default();
    let Ok(store) = app.store("settings.json") else {
        return defaults;
    };

    SecuritySettings {
        enabled: store.get("security.enabled").and_then(|v| v.as_bool()).unwrap_or(defaults.enabled),
        mode: store.get("security.mode").and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or(defaults.mode),
        scan_request: store.get("security.scan_request").and_then(|v| v.as_bool()).unwrap_or(defaults.scan_request),
        scan_response: store.get("security.scan_response").and_then(|v| v.as_bool()).unwrap_or(defaults.scan_response),
        scan_unicode: store.get("security.scan_unicode").and_then(|v| v.as_bool()).unwrap_or(defaults.scan_unicode),
        scan_tools: store.get("security.scan_tools").and_then(|v| v.as_bool()).unwrap_or(defaults.scan_tools),
        scan_network: store.get("security.scan_network").and_then(|v| v.as_bool()).unwrap_or(defaults.scan_network),
        redact_secrets: store.get("security.redact_secrets").and_then(|v| v.as_bool()).unwrap_or(defaults.redact_secrets),
        block_on_critical: store.get("security.block_on_critical").and_then(|v| v.as_bool()).unwrap_or(defaults.block_on_critical),
        max_scan_bytes: store.get("security.max_scan_bytes").and_then(|v| v.as_u64()).unwrap_or(defaults.max_scan_bytes as u64) as usize,
    }
}

pub fn decide_action(result: &mut SecurityScanResult, settings: &SecuritySettings) {
    if !settings.enabled {
        result.action = SecurityAction::Allow;
        return;
    }

    let mode = settings.mode.as_str();
    result.action = match mode {
        "off" | "audit" => SecurityAction::Allow,
        "warn" => {
            if result.risk_level.rank() >= RiskLevel::Medium.rank() { SecurityAction::Warn } else { SecurityAction::Allow }
        }
        "redact" => {
            if result.risk_level.rank() >= RiskLevel::High.rank() { SecurityAction::Redact } else { SecurityAction::Allow }
        }
        "confirm" => {
            if result.risk_level.rank() >= RiskLevel::High.rank() { SecurityAction::Confirm } else { SecurityAction::Allow }
        }
        "block" => {
            if result.risk_level.rank() >= RiskLevel::High.rank() { SecurityAction::Block } else { SecurityAction::Allow }
        }
        _ => SecurityAction::Allow,
    };

    if settings.block_on_critical && result.risk_level == RiskLevel::Critical {
        result.action = SecurityAction::Block;
    }

    if matches!(result.action, SecurityAction::Block) {
        result.blocked_reason = Some(result.summary.clone());
    }
}

pub fn scan_request(body: &serde_json::Value, settings: &SecuritySettings) -> SecurityScanResult {
    if !settings.enabled || !settings.scan_request {
        return SecurityScanResult::default();
    }
    let mut result = scanner::scan_json(body, "request", settings);
    decide_action(&mut result, settings);
    result
}

/// Scan an upstream response for risks (sensitive info, tracking, etc.)
pub fn scan_response(body: &serde_json::Value, settings: &SecuritySettings) -> SecurityScanResult {
    if !settings.enabled || !settings.scan_response {
        return SecurityScanResult::default();
    }
    let mut result = scanner::scan_json(body, "response", settings);
    decide_action(&mut result, settings);
    result
}

/// Redact sensitive data from the request body before forwarding upstream.
/// Returns a new JSON value with secrets replaced.
pub fn redact_request_body(body: &serde_json::Value, settings: &SecuritySettings) -> (serde_json::Value, bool) {
    if !settings.enabled || !settings.redact_secrets {
        return (body.clone(), false);
    }
    let redacted = redact::redact_json(body, settings);
    let was_redacted = redacted != *body;
    (redacted, was_redacted)
}
