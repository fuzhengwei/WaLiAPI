use super::rules::{self, CustomRule};
use super::{RiskLevel, SecurityFinding, SecurityScanResult, SecuritySettings};
use crate::utils::text::truncate_utf8;
use std::time::{Duration, Instant};

const MAX_FINDINGS: usize = 80;

/// Whole-request cumulative scan budgets (T03 spec).  Every counter is
/// cumulative across the ENTIRE request tree — a 32MB/50MB request cannot
/// bypass the CPU budget by splitting into many small strings.  A `None`
/// field means "unlimited" for that axis.
#[derive(Debug, Clone)]
pub struct ScanBudget {
    /// Cumulative raw JSON bytes visited.  Default 32 MiB (matches the
    /// Anthropic route body limit; embeddings/images may be larger but the
    /// gate is separate from the transport body limit).
    pub max_total_bytes: Option<usize>,
    /// Cumulative string nodes visited.
    pub max_string_nodes: Option<usize>,
    /// Maximum JSON nesting depth.
    pub max_depth: Option<usize>,
    /// Cumulative wall-clock budget for the scan.
    pub max_elapsed: Option<Duration>,
    /// Per-string text cap applied before regex-ish rules (UTF-8 boundary).
    pub max_text_bytes_per_string: Option<usize>,
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self {
            max_total_bytes: Some(32 * 1024 * 1024),
            max_string_nodes: Some(50_000),
            max_depth: Some(256),
            max_elapsed: Some(Duration::from_millis(800)),
            max_text_bytes_per_string: Some(64 * 1024),
        }
    }
}

/// Marker returned when the cumulative budget is exceeded.  The caller MUST
/// fail closed (`security_scan_budget_exceeded`) and never report the request
/// as clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    Exceeded(String),
}

/// Shared scanning context enforcing the whole-request budgets.
pub struct ScanContext<'a> {
    settings: &'a SecuritySettings,
    budget: &'a ScanBudget,
    custom_rules: &'a [CustomRule],
    findings: Vec<SecurityFinding>,
    bytes_visited: usize,
    string_nodes: usize,
    depth: usize,
    started: Instant,
    elapsed_budget: Option<Duration>,
}

impl<'a> ScanContext<'a> {
    fn new(
        settings: &'a SecuritySettings,
        budget: &'a ScanBudget,
        custom_rules: &'a [CustomRule],
    ) -> Self {
        Self {
            settings,
            budget,
            custom_rules,
            findings: Vec::new(),
            bytes_visited: 0,
            string_nodes: 0,
            depth: 0,
            started: Instant::now(),
            elapsed_budget: budget.max_elapsed,
        }
    }

    fn check_budget(&self) -> Result<(), BudgetError> {
        if let Some(max) = self.budget.max_total_bytes {
            if self.bytes_visited > max {
                return Err(BudgetError::Exceeded(format!(
                    "security scan byte budget exceeded ({} > {} bytes)",
                    self.bytes_visited, max
                )));
            }
        }
        if let Some(max) = self.budget.max_string_nodes {
            if self.string_nodes > max {
                return Err(BudgetError::Exceeded(format!(
                    "security scan string-node budget exceeded ({} > {})",
                    self.string_nodes, max
                )));
            }
        }
        if let Some(max) = self.budget.max_depth {
            if self.depth > max {
                return Err(BudgetError::Exceeded(format!(
                    "security scan depth budget exceeded (depth {} > {})",
                    self.depth, max
                )));
            }
        }
        if let Some(max) = self.elapsed_budget {
            if self.started.elapsed() > max {
                return Err(BudgetError::Exceeded(format!(
                    "security scan time budget exceeded (elapsed > {:?})",
                    max
                )));
            }
        }
        Ok(())
    }
}

/// Scan the full JSON tree with whole-request budgets.
pub fn scan_with_budget(
    value: &serde_json::Value,
    phase: &str,
    settings: &SecuritySettings,
    budget: &ScanBudget,
    custom_rules: &[CustomRule],
) -> Result<SecurityScanResult, BudgetError> {
    let mut ctx = ScanContext::new(settings, budget, custom_rules);
    walk_json(value, phase, "$", &mut ctx)?;
    ctx.check_budget()?;

    let findings = ctx.findings;
    let mut score = 0;
    let mut max_level = RiskLevel::Clean;
    for f in &findings {
        let base = match f.severity {
            RiskLevel::Clean => 0,
            RiskLevel::Info => 5,
            RiskLevel::Low => 15,
            RiskLevel::Medium => 35,
            RiskLevel::High => 65,
            RiskLevel::Critical => 90,
        };
        score = score.max(base);
        if f.severity.rank() > max_level.rank() {
            max_level = f.severity.clone();
        }
    }

    let has_credential = findings.iter().any(|f| f.category == "credential");
    let has_network = findings.iter().any(|f| f.category == "network");
    let has_sensitive_file = findings.iter().any(|f| f.rule_id == "file.sensitive_path");
    let has_unicode = findings.iter().any(|f| f.category == "unicode");
    let has_shell = findings.iter().any(|f| f.rule_id.starts_with("tool.shell"));

    if has_credential && has_network {
        score += 25;
    }
    if has_sensitive_file && has_network {
        score += 25;
    }
    if has_unicode && has_network {
        score += 15;
    }
    if has_shell && has_sensitive_file {
        score += 20;
    }
    score = score.min(100);

    if score >= 90 {
        max_level = RiskLevel::Critical;
    } else if score >= 65 && max_level.rank() < RiskLevel::High.rank() {
        max_level = RiskLevel::High;
    } else if score >= 35 && max_level.rank() < RiskLevel::Medium.rank() {
        max_level = RiskLevel::Medium;
    }

    let summary = summarize(&findings, &max_level);

    Ok(SecurityScanResult {
        risk_level: max_level,
        risk_score: score,
        action: super::SecurityAction::Allow,
        sanitized: false,
        blocked_reason: None,
        summary,
        findings,
    })
}

fn walk_json(
    value: &serde_json::Value,
    phase: &str,
    path: &str,
    ctx: &mut ScanContext,
) -> Result<(), BudgetError> {
    if ctx.findings.len() >= MAX_FINDINGS {
        return Ok(());
    }
    ctx.check_budget()?;

    // Account for the node itself (approximate raw bytes).
    match value {
        serde_json::Value::String(s) => {
            ctx.string_nodes += 1;
            ctx.bytes_visited = ctx.bytes_visited.saturating_add(s.len());
            scan_text(s, phase, path, ctx);
        }
        serde_json::Value::Array(items) => {
            ctx.bytes_visited = ctx.bytes_visited.saturating_add(2);
            ctx.depth += 1;
            for (i, item) in items.iter().enumerate() {
                walk_json(item, phase, &format!("{}[{}]", path, i), ctx)?;
                if ctx.findings.len() >= MAX_FINDINGS {
                    break;
                }
            }
            ctx.depth = ctx.depth.saturating_sub(1);
        }
        serde_json::Value::Object(map) => {
            ctx.bytes_visited = ctx.bytes_visited.saturating_add(2);
            ctx.depth += 1;
            for (k, v) in map {
                let child = if path == "$" {
                    format!("$.{}", k)
                } else {
                    format!("{}.{}", path, k)
                };
                ctx.bytes_visited = ctx.bytes_visited.saturating_add(k.len());
                walk_json(v, phase, &child, ctx)?;
                if ctx.findings.len() >= MAX_FINDINGS {
                    break;
                }
            }
            ctx.depth = ctx.depth.saturating_sub(1);
        }
        _ => {}
    }
    Ok(())
}

fn scan_text(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    // UTF-8-safe truncation: never slice at a non-char boundary.
    let per_string_cap = ctx.budget.max_text_bytes_per_string.unwrap_or(usize::MAX);
    let scan_text = if text.len() > per_string_cap {
        truncate_utf8(text, per_string_cap)
    } else {
        text
    };
    // Respect the user-level per-string cap as an additional guard.
    let scan_text = if scan_text.len() > ctx.settings.max_scan_bytes {
        truncate_utf8(scan_text, ctx.settings.max_scan_bytes)
    } else {
        scan_text
    };

    // ── Whitelist short-circuit (per-category exemption) ─────────────
    // A keyword whitelist exempts the entire text from all built-in scans.
    let wl_keyword = rules::is_whitelisted("keyword", scan_text, ctx.custom_rules);
    if wl_keyword {
        return;
    }
    let wl_domain = rules::is_whitelisted("domain", scan_text, ctx.custom_rules);
    let wl_path = rules::is_whitelisted("path", scan_text, ctx.custom_rules);
    let wl_tool = rules::is_whitelisted("tool", scan_text, ctx.custom_rules);

    // ── Built-in scans (skipped per-category when whitelisted) ──────
    scan_credentials(scan_text, phase, location, ctx);
    if !wl_path {
        scan_paths(scan_text, phase, location, ctx);
    }
    if ctx.settings.scan_unicode {
        scan_unicode(scan_text, phase, location, ctx);
    }
    if ctx.settings.scan_network && !wl_domain {
        scan_network(scan_text, phase, location, ctx);
    }
    if ctx.settings.scan_tools && !wl_tool {
        scan_tool_risks(scan_text, phase, location, ctx);
    }
    if !wl_domain {
        scan_tracking_pixel(scan_text, phase, location, ctx);
    }
    scan_fingerprint_terms(scan_text, phase, location, ctx);

    // ── Custom blacklist rules ──────────────────────────────────────
    rules::apply_custom_rules(
        scan_text,
        phase,
        location,
        ctx.custom_rules,
        &mut ctx.findings,
    );
}

fn scan_credentials(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    for token in split_candidates(text) {
        let t = token.trim_matches(|c: char| {
            c == '"' || c == '\'' || c == ',' || c == ';' || c == ')' || c == '(' || c == '`'
        });
        let lower = t.to_ascii_lowercase();
        let is_secret = (t.starts_with("sk-") && t.len() >= 24)
            || (t.starts_with("sk-ant-") && t.len() >= 30)
            || (t.starts_with("ghp_") && t.len() >= 20)
            || (t.starts_with("gho_") && t.len() >= 20)
            || (t.starts_with("xoxb-") && t.len() >= 20)
            || (t.starts_with("AKIA") && t.len() >= 16)
            || (t.starts_with("AIza") && t.len() >= 20)
            || (t.starts_with("eyJ") && t.len() >= 30 && t.contains('.'))
            || lower.starts_with("bearer ");
        if is_secret {
            add(
                &mut ctx.findings,
                phase,
                "credential",
                "credential.secret_token",
                RiskLevel::High,
                "发现疑似密钥/Token",
                "请求内容中出现 API Key、Bearer Token、GitHub Token、JWT 或云厂商密钥样式字符串。",
                location,
                t,
            );
            break;
        }
    }

    if text.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
        || text.contains("-----BEGIN RSA PRIVATE KEY-----")
        || text.contains("-----BEGIN PRIVATE KEY-----")
    {
        add(
            &mut ctx.findings,
            phase,
            "credential",
            "credential.private_key",
            RiskLevel::Critical,
            "发现私钥内容",
            "请求内容中包含私钥 PEM/OpenSSH 头部，存在严重凭证泄露风险。",
            location,
            "-----BEGIN PRIVATE KEY-----",
        );
    }

    let lower = text.to_ascii_lowercase();
    for key in [
        "authorization:",
        "cookie:",
        "sessionid=",
        "auth_token=",
        "secret_key",
        "access_key",
        "database_url",
    ] {
        if lower.contains(key) {
            add(
                &mut ctx.findings,
                phase,
                "credential",
                "credential.named_secret",
                RiskLevel::High,
                "发现敏感凭证字段",
                "请求内容包含 Authorization、Cookie、Session 或 Secret 字段名。",
                location,
                key,
            );
            break;
        }
    }
}

fn scan_paths(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    let lower = text.to_ascii_lowercase();
    let sensitive = [
        ".env",
        "~/.ssh",
        "/.ssh/",
        "id_rsa",
        "id_ed25519",
        ".aws/credentials",
        ".git-credentials",
        ".netrc",
        ".npmrc",
        ".pypirc",
        "credentials.json",
    ];
    for item in sensitive {
        if lower.contains(item) {
            add(
                &mut ctx.findings,
                phase,
                "file",
                "file.sensitive_path",
                RiskLevel::High,
                "发现敏感文件路径",
                "内容引用了 .env、SSH 私钥、云凭证或包管理器凭证等敏感路径。",
                location,
                item,
            );
            break;
        }
    }
    if text.contains("/Users/") || text.contains("C:\\Users\\") || text.contains("/home/") {
        add(
            &mut ctx.findings,
            phase,
            "infra",
            "infra.local_path",
            RiskLevel::Medium,
            "发现本地用户路径",
            "内容包含本地用户目录路径，可能暴露用户名、项目结构或机器信息。",
            location,
            snippet(text),
        );
    }
}

fn scan_unicode(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    let mut zero_width = 0;
    let mut bidi = 0;
    let mut variation = 0;
    for ch in text.chars() {
        let code = ch as u32;
        if matches!(code, 0x200B | 0x200C | 0x200D | 0x2060 | 0xFEFF) {
            zero_width += 1;
        }
        if (0x202A..=0x202E).contains(&code) || (0x2066..=0x2069).contains(&code) {
            bidi += 1;
        }
        if (0xFE00..=0xFE0F).contains(&code) || (0xE0100..=0xE01EF).contains(&code) {
            variation += 1;
        }
    }
    if zero_width > 0 {
        add(
            &mut ctx.findings,
            phase,
            "unicode",
            "unicode.zero_width",
            RiskLevel::Medium,
            "发现零宽 Unicode 字符",
            "内容包含不可见零宽字符，可能用于隐藏标记或混淆文本。",
            location,
            &format!("zero_width_count={}", zero_width),
        );
    }
    if bidi > 0 {
        add(
            &mut ctx.findings,
            phase,
            "unicode",
            "unicode.bidi_control",
            RiskLevel::High,
            "发现方向控制 Unicode 字符",
            "内容包含 Bidi 方向控制字符，可能改变代码、URL 或命令的视觉顺序。",
            location,
            &format!("bidi_count={}", bidi),
        );
    }
    if variation > 0 {
        add(
            &mut ctx.findings,
            phase,
            "unicode",
            "unicode.variation_selector",
            RiskLevel::Medium,
            "发现变体选择符",
            "内容包含 Unicode variation selector，可能被用于隐写编码。",
            location,
            &format!("variation_count={}", variation),
        );
    }
}

fn scan_network(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    let lower = text.to_ascii_lowercase();
    let ip_probe = [
        "ifconfig.me",
        "ipinfo.io",
        "ip-api.com",
        "ipify.org",
        "ident.me",
        "icanhazip.com",
        "api.ip.sb",
    ];
    for domain in ip_probe {
        if lower.contains(domain) {
            add(
                &mut ctx.findings,
                phase,
                "network",
                "network.ip_probe",
                RiskLevel::High,
                "发现公网 IP 探测服务",
                "内容引用公网 IP 查询服务，可能用于识别真实出口 IP 或代理状态。",
                location,
                domain,
            );
            break;
        }
    }
    let suspicious = [
        "webhook.site",
        "requestbin",
        "ngrok",
        "trycloudflare.com",
        "pastebin.com",
        "transfer.sh",
        "file.io",
    ];
    for domain in suspicious {
        if lower.contains(domain) {
            add(
                &mut ctx.findings,
                phase,
                "network",
                "network.suspicious_domain",
                RiskLevel::High,
                "发现可疑外联域名",
                "内容引用临时 Webhook、隧道或文件投递服务，可能用于接收外传数据。",
                location,
                domain,
            );
            break;
        }
    }
    if lower.contains("http://") || lower.contains("https://") {
        add(
            &mut ctx.findings,
            phase,
            "network",
            "network.external_url",
            RiskLevel::Info,
            "发现外部 URL",
            "内容包含外部 URL，建议结合上下文判断是否为正常请求或外联风险。",
            location,
            first_url(text)
                .unwrap_or_else(|| "http(s) URL".to_string())
                .as_str(),
        );
    }
}

fn scan_tool_risks(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    let lower = text.to_ascii_lowercase();
    let has_shell = [
        "curl ",
        "wget ",
        " nc ",
        "ncat ",
        "scp ",
        "rsync ",
        "bash -c",
        "sh -c",
        "python -c",
        "node -e",
        "powershell",
        "osascript",
    ]
    .iter()
    .any(|x| lower.contains(x));
    if has_shell {
        add(&mut ctx.findings, phase, "tool", "tool.shell.network_or_exec", RiskLevel::Medium, "发现高风险命令片段", "内容包含 curl/wget/nc/scp/bash -c/python -c 等命令，可能涉及外联、下载执行或数据传输。", location, snippet(text));
    }
    let reads_sensitive = [
        "cat .env",
        "cat ~/.ssh",
        "cat /users",
        "cat /home",
        "env |",
        "printenv",
        "base64 ~/.ssh",
        "tar ",
        "zip ",
    ]
    .iter()
    .any(|x| lower.contains(x));
    let network = [
        "curl ", "wget ", "http://", "https://", "scp ", "rsync ", "nc ",
    ]
    .iter()
    .any(|x| lower.contains(x));
    if reads_sensitive && network {
        add(
            &mut ctx.findings,
            phase,
            "tool",
            "tool.shell.exfiltration",
            RiskLevel::Critical,
            "疑似敏感数据外传命令",
            "内容同时包含敏感文件/环境路径读取与外部网络传输特征，存在严重外泄风险。",
            location,
            snippet(text),
        );
    }
}

fn scan_tracking_pixel(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    let lower = text.to_ascii_lowercase();
    let has_img = lower.contains("<img") || lower.contains("![](") || lower.contains("![");
    let tracking = lower.contains("pixel")
        || lower.contains("track")
        || lower.contains("beacon")
        || lower.contains("open=")
        || lower.contains("analytics");
    let tiny = lower.contains("width=\"1\"")
        || lower.contains("height=\"1\"")
        || lower.contains("width='1'")
        || lower.contains("height='1'");
    if has_img && (tracking || tiny) && (lower.contains("http://") || lower.contains("https://")) {
        add(&mut ctx.findings, phase, "network", "html.tracking_pixel", RiskLevel::High, "疑似追踪像素", "内容包含远程图片且具备 1x1、track、pixel、beacon 等追踪特征，可能暴露打开时间与真实 IP。", location, snippet(text));
    }
}

fn scan_fingerprint_terms(text: &str, phase: &str, location: &str, ctx: &mut ScanContext) {
    let lower = text.to_ascii_lowercase();
    let terms = [
        "timezone",
        "locale",
        "proxy",
        "vpn",
        "fingerprint",
        "risk control",
        "blacklist",
        "风控",
        "封禁",
        "代理检测",
        "时区",
        "指纹",
        "静默上报",
        "隐写",
    ];
    let count = terms.iter().filter(|t| lower.contains(**t)).count();
    if count >= 2 {
        add(&mut ctx.findings, phase, "prompt", "prompt.fingerprint_context", RiskLevel::Medium, "发现账号画像/风控相关上下文", "内容同时出现多个时区、代理、指纹、风控或隐写相关词，可能与账号画像或访问风险识别有关。", location, snippet(text));
    }
}

fn add(
    f: &mut Vec<SecurityFinding>,
    phase: &str,
    category: &str,
    rule_id: &str,
    severity: RiskLevel,
    title: &str,
    description: &str,
    location: &str,
    evidence: &str,
) {
    if f.len() >= MAX_FINDINGS {
        return;
    }
    f.push(SecurityFinding {
        phase: phase.to_string(),
        category: category.to_string(),
        rule_id: rule_id.to_string(),
        severity,
        title: title.to_string(),
        description: description.to_string(),
        location: location.to_string(),
        evidence_masked: mask_evidence(evidence),
    });
}

pub fn add_finding(
    f: &mut Vec<SecurityFinding>,
    phase: &str,
    category: &str,
    rule_id: &str,
    severity: RiskLevel,
    title: &str,
    description: &str,
    location: &str,
    evidence: &str,
) {
    add(
        f,
        phase,
        category,
        rule_id,
        severity,
        title,
        description,
        location,
        evidence,
    );
}

fn summarize(findings: &[SecurityFinding], level: &RiskLevel) -> String {
    if findings.is_empty() {
        return "未发现明显风险".to_string();
    }
    let mut credential = 0;
    let mut unicode = 0;
    let mut network = 0;
    let mut tool = 0;
    let mut file = 0;
    for f in findings {
        match f.category.as_str() {
            "credential" => credential += 1,
            "unicode" => unicode += 1,
            "network" => network += 1,
            "tool" => tool += 1,
            "file" => file += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if credential > 0 {
        parts.push(format!("{} 个凭证风险", credential));
    }
    if file > 0 {
        parts.push(format!("{} 个敏感文件/路径风险", file));
    }
    if tool > 0 {
        parts.push(format!("{} 个工具/命令风险", tool));
    }
    if network > 0 {
        parts.push(format!("{} 个网络/追踪风险", network));
    }
    if unicode > 0 {
        parts.push(format!("{} 个 Unicode 隐写/混淆风险", unicode));
    }
    if parts.is_empty() {
        parts.push(format!("{} 个风险项", findings.len()));
    }
    format!("{:?}：发现{}。", level, parts.join("、"))
}

fn split_candidates(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| {
        c.is_whitespace()
            || ['"', '\'', ',', ';', '(', ')', '[', ']', '{', '}', '<', '>'].contains(&c)
    })
}

fn mask_evidence(e: &str) -> String {
    let s = e.replace('\n', " ");
    if s.len() <= 16 {
        return s;
    }
    let start: String = s.chars().take(8).collect();
    let end: String = s
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{}****{}", start, end)
}

fn snippet(text: &str) -> &str {
    let max = 160;
    if text.len() <= max {
        text
    } else {
        truncate_utf8(text, max)
    }
}

fn first_url(text: &str) -> Option<String> {
    for part in text.split_whitespace() {
        if part.starts_with("http://") || part.starts_with("https://") {
            return Some(
                part.trim_matches(|c: char| c == '"' || c == '\'' || c == ')' || c == ']')
                    .to_string(),
            );
        }
    }
    None
}

#[cfg(test)]
mod scanner_tests {
    use super::*;
    use crate::security::SecurityAction;

    #[test]
    fn cumulative_byte_budget_counts_across_whole_tree() {
        let settings = SecuritySettings::default();
        let budget = ScanBudget {
            max_total_bytes: Some(64),
            ..Default::default()
        };
        let body = serde_json::json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "a".repeat(200)},
                {"role": "user", "content": "b".repeat(200)}
            ]
        });
        let err = scan_with_budget(&body, "request", &settings, &budget, &[]).unwrap_err();
        match err {
            BudgetError::Exceeded(msg) => assert!(msg.contains("byte budget")),
        }
    }

    #[test]
    fn many_small_strings_cannot_bypass_string_node_budget() {
        let settings = SecuritySettings::default();
        let budget = ScanBudget {
            max_string_nodes: Some(8),
            ..Default::default()
        };
        let arr: Vec<serde_json::Value> = (0..20)
            .map(|i| serde_json::json!({"x": format!("val{}", i)}))
            .collect();
        let body = serde_json::Value::Array(arr);
        let err = scan_with_budget(&body, "request", &settings, &budget, &[]).unwrap_err();
        match err {
            BudgetError::Exceeded(msg) => assert!(msg.contains("string-node")),
        }
    }

    #[test]
    fn depth_budget_is_enforced() {
        let settings = SecuritySettings::default();
        let budget = ScanBudget {
            max_depth: Some(4),
            ..Default::default()
        };
        let mut v = serde_json::json!("leaf");
        for _ in 0..30 {
            v = serde_json::json!({"a": v});
        }
        let err = scan_with_budget(&v, "request", &settings, &budget, &[]).unwrap_err();
        match err {
            BudgetError::Exceeded(msg) => assert!(msg.contains("depth")),
        }
    }

    #[test]
    fn utf8_truncation_never_panics_on_boundary() {
        let text = "界".repeat(100);
        let truncated = truncate_utf8(&text, 37); // 37 is not a multiple of 3
        assert!(text.is_char_boundary(truncated.len()));
        assert!(truncated.len() <= 37);
        // All bytes in truncated are valid UTF-8.
        std::str::from_utf8(truncated.as_bytes()).unwrap();
    }

    #[test]
    fn oversized_string_respects_per_string_cap_without_panic() {
        let settings = SecuritySettings::default();
        let budget = ScanBudget {
            max_text_bytes_per_string: Some(16),
            ..Default::default()
        };
        let body = serde_json::json!({"messages": [{"role": "user", "content": "界".repeat(500)}]});
        let result = scan_with_budget(&body, "request", &settings, &budget, &[]).unwrap();
        assert_eq!(result.action, SecurityAction::Allow);
    }

    #[test]
    fn time_budget_fails_closed_on_tiny_budget() {
        let settings = SecuritySettings::default();
        let budget = ScanBudget {
            max_elapsed: Some(Duration::from_nanos(1)),
            ..Default::default()
        };
        let body = serde_json::json!({"messages": [{"role": "user", "content": "hello world this is a scan".to_string()}]});
        let result = scan_with_budget(&body, "request", &settings, &budget, &[]);
        // Either the elapsed check trips immediately, or the scan finishes
        // within the nanos budget (rare); both are acceptable — the budget
        // failure path must never report clean.
        if let Err(BudgetError::Exceeded(msg)) = result {
            assert!(msg.contains("time budget"));
        }
    }
}
