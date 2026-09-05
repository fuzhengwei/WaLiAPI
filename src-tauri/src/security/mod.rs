use serde::{Deserialize, Serialize};

pub mod features;
pub mod gate;
pub mod redact;
pub mod rules;
pub mod scanner;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            scan_request: false,
            scan_response: false,
            scan_unicode: false,
            scan_tools: false,
            scan_network: false,
            redact_secrets: false,
            block_on_critical: false,
            max_scan_bytes: 1024 * 1024,
        }
    }
}

pub fn get_security_settings(settings: &crate::settings_store::SettingsStore) -> SecuritySettings {
    let defaults = SecuritySettings::default();

    SecuritySettings {
        enabled: settings.get_bool("security.enabled", defaults.enabled),
        mode: {
            let value = settings.get_str("security.mode", &defaults.mode);
            if value.is_empty() {
                defaults.mode
            } else {
                value
            }
        },
        scan_request: settings.get_bool("security.scan_request", defaults.scan_request),
        scan_response: settings.get_bool("security.scan_response", defaults.scan_response),
        scan_unicode: settings.get_bool("security.scan_unicode", defaults.scan_unicode),
        scan_tools: settings.get_bool("security.scan_tools", defaults.scan_tools),
        scan_network: settings.get_bool("security.scan_network", defaults.scan_network),
        redact_secrets: settings.get_bool("security.redact_secrets", defaults.redact_secrets),
        block_on_critical: settings
            .get_bool("security.block_on_critical", defaults.block_on_critical),
        max_scan_bytes: settings.get_u64("security.max_scan_bytes", defaults.max_scan_bytes as u64)
            as usize,
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
            if result.risk_level.rank() >= RiskLevel::Medium.rank() {
                SecurityAction::Warn
            } else {
                SecurityAction::Allow
            }
        }
        "redact" => {
            if result.risk_level.rank() >= RiskLevel::High.rank() {
                SecurityAction::Redact
            } else {
                SecurityAction::Allow
            }
        }
        "confirm" => {
            if result.risk_level.rank() >= RiskLevel::High.rank() {
                SecurityAction::Confirm
            } else {
                SecurityAction::Allow
            }
        }
        "block" => {
            if result.risk_level.rank() >= RiskLevel::High.rank() {
                SecurityAction::Block
            } else {
                SecurityAction::Allow
            }
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

pub fn scan_request(
    body: &serde_json::Value,
    settings: &SecuritySettings,
    custom_rules: &[rules::CustomRule],
) -> SecurityScanResult {
    if !settings.enabled || !settings.scan_request {
        return SecurityScanResult::default();
    }
    let mut result = scanner::scan_with_budget(
        body,
        "request",
        settings,
        &scanner::ScanBudget::default(),
        custom_rules,
    )
    .unwrap_or_else(|err| {
        // Over-budget must fail closed as a high-risk block, never clean.
        let mut blocked = SecurityScanResult::default();
        blocked.risk_level = RiskLevel::Critical;
        blocked.risk_score = 100;
        blocked.action = SecurityAction::Block;
        blocked.blocked_reason = Some(blocked.summary.clone());
        blocked.summary = match err {
            scanner::BudgetError::Exceeded(msg) => msg,
        };
        blocked.blocked_reason = Some(blocked.summary.clone());
        blocked.findings.push(SecurityFinding {
            phase: "request".to_string(),
            category: "budget".to_string(),
            rule_id: "budget.scan_exceeded".to_string(),
            severity: RiskLevel::Critical,
            title: "安全扫描预算超限".to_string(),
            description: "整个请求的扫描预算被超过，请求被 fail-closed 拒绝。".to_string(),
            location: "$".to_string(),
            evidence_masked: "budget exceeded".to_string(),
        });
        blocked
    });
    decide_action(&mut result, settings);
    result
}

/// Scan an upstream response for risks (sensitive info, tracking, etc.)
/// Uses a response-side budget with a looser default (responses may be large).
pub fn scan_response(
    body: &serde_json::Value,
    settings: &SecuritySettings,
    custom_rules: &[rules::CustomRule],
) -> SecurityScanResult {
    if !settings.enabled || !settings.scan_response {
        return SecurityScanResult::default();
    }
    let budget = scanner::ScanBudget {
        max_total_bytes: Some(64 * 1024 * 1024),
        max_string_nodes: Some(100_000),
        max_depth: Some(256),
        max_elapsed: Some(std::time::Duration::from_millis(800)),
        max_text_bytes_per_string: Some(64 * 1024),
    };
    let mut result = scanner::scan_with_budget(body, "response", settings, &budget, custom_rules)
        .unwrap_or_else(|err| match err {
            scanner::BudgetError::Exceeded(msg) => {
                let mut blocked = SecurityScanResult::default();
                blocked.risk_level = RiskLevel::Critical;
                blocked.risk_score = 100;
                blocked.action = SecurityAction::Block;
                blocked.summary = msg;
                blocked.blocked_reason = Some(blocked.summary.clone());
                blocked
            }
        });
    decide_action(&mut result, settings);
    result
}

/// Redact sensitive data from the request body before forwarding upstream.
/// Returns a new JSON value with secrets replaced.
pub fn redact_request_body(
    body: &serde_json::Value,
    settings: &SecuritySettings,
) -> (serde_json::Value, bool) {
    if !settings.enabled || !settings.redact_secrets {
        return (body.clone(), false);
    }
    let redacted = redact::redact_json(body, settings);
    let was_redacted = redacted != *body;
    (redacted, was_redacted)
}
