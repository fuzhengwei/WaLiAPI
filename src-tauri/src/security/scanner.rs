use super::{RiskLevel, SecurityFinding, SecurityScanResult, SecuritySettings};

const MAX_FINDINGS: usize = 80;

pub fn scan_json(value: &serde_json::Value, phase: &str, settings: &SecuritySettings) -> SecurityScanResult {
    let mut findings = Vec::new();
    walk_json(value, phase, "$", settings, &mut findings);

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

    if has_credential && has_network { score += 25; }
    if has_sensitive_file && has_network { score += 25; }
    if has_unicode && has_network { score += 15; }
    if has_shell && has_sensitive_file { score += 20; }
    score = score.min(100);

    if score >= 90 { max_level = RiskLevel::Critical; }
    else if score >= 65 && max_level.rank() < RiskLevel::High.rank() { max_level = RiskLevel::High; }
    else if score >= 35 && max_level.rank() < RiskLevel::Medium.rank() { max_level = RiskLevel::Medium; }

    let summary = summarize(&findings, &max_level);

    SecurityScanResult {
        risk_level: max_level,
        risk_score: score,
        action: super::SecurityAction::Allow,
        sanitized: false,
        blocked_reason: None,
        summary,
        findings,
    }
}

fn walk_json(value: &serde_json::Value, phase: &str, path: &str, settings: &SecuritySettings, findings: &mut Vec<SecurityFinding>) {
    if findings.len() >= MAX_FINDINGS { return; }
    match value {
        serde_json::Value::String(s) => scan_text(s, phase, path, settings, findings),
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk_json(item, phase, &format!("{}[{}]", path, i), settings, findings);
                if findings.len() >= MAX_FINDINGS { break; }
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let child = if path == "$" { format!("$.{}", k) } else { format!("{}.{}", path, k) };
                walk_json(v, phase, &child, settings, findings);
                if findings.len() >= MAX_FINDINGS { break; }
            }
        }
        _ => {}
    }
}

fn scan_text(text: &str, phase: &str, location: &str, settings: &SecuritySettings, findings: &mut Vec<SecurityFinding>) {
    let scan_text = if text.len() > settings.max_scan_bytes { &text[..settings.max_scan_bytes] } else { text };

    scan_credentials(scan_text, phase, location, findings);
    scan_paths(scan_text, phase, location, findings);
    if settings.scan_unicode { scan_unicode(scan_text, phase, location, findings); }
    if settings.scan_network { scan_network(scan_text, phase, location, findings); }
    if settings.scan_tools { scan_tool_risks(scan_text, phase, location, findings); }
    scan_tracking_pixel(scan_text, phase, location, findings);
    scan_fingerprint_terms(scan_text, phase, location, findings);
}

fn scan_credentials(text: &str, phase: &str, location: &str, findings: &mut Vec<SecurityFinding>) {
    for token in split_candidates(text) {
        let t = token.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ';' || c == ')' || c == '(' || c == '`');
        let lower = t.to_ascii_lowercase();
        let is_secret =
            (t.starts_with("sk-") && t.len() >= 24) ||
            (t.starts_with("sk-ant-") && t.len() >= 30) ||
            (t.starts_with("ghp_") && t.len() >= 20) ||
            (t.starts_with("gho_") && t.len() >= 20) ||
            (t.starts_with("xoxb-") && t.len() >= 20) ||
            (t.starts_with("AKIA") && t.len() >= 16) ||
            (t.starts_with("AIza") && t.len() >= 20) ||
            (t.starts_with("eyJ") && t.len() >= 30 && t.contains('.')) ||
            lower.starts_with("bearer ");
        if is_secret {
            add(findings, phase, "credential", "credential.secret_token", RiskLevel::High, "发现疑似密钥/Token", "请求内容中出现 API Key、Bearer Token、GitHub Token、JWT 或云厂商密钥样式字符串。", location, t);
            break;
        }
    }

    if text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") || text.contains("-----BEGIN RSA PRIVATE KEY-----") || text.contains("-----BEGIN PRIVATE KEY-----") {
        add(findings, phase, "credential", "credential.private_key", RiskLevel::Critical, "发现私钥内容", "请求内容中包含私钥 PEM/OpenSSH 头部，存在严重凭证泄露风险。", location, "-----BEGIN PRIVATE KEY-----");
    }

    let lower = text.to_ascii_lowercase();
    for key in ["authorization:", "cookie:", "sessionid=", "auth_token=", "secret_key", "access_key", "database_url"] {
        if lower.contains(key) {
            add(findings, phase, "credential", "credential.named_secret", RiskLevel::High, "发现敏感凭证字段", "请求内容包含 Authorization、Cookie、Session 或 Secret 字段名。", location, key);
            break;
        }
    }
}

fn scan_paths(text: &str, phase: &str, location: &str, findings: &mut Vec<SecurityFinding>) {
    let lower = text.to_ascii_lowercase();
    let sensitive = [".env", "~/.ssh", "/.ssh/", "id_rsa", "id_ed25519", ".aws/credentials", ".git-credentials", ".netrc", ".npmrc", ".pypirc", "credentials.json"];
    for item in sensitive {
        if lower.contains(item) {
            add(findings, phase, "file", "file.sensitive_path", RiskLevel::High, "发现敏感文件路径", "内容引用了 .env、SSH 私钥、云凭证或包管理器凭证等敏感路径。", location, item);
            break;
        }
    }
    if text.contains("/Users/") || text.contains("C:\\Users\\") || text.contains("/home/") {
        add(findings, phase, "infra", "infra.local_path", RiskLevel::Medium, "发现本地用户路径", "内容包含本地用户目录路径，可能暴露用户名、项目结构或机器信息。", location, snippet(text));
    }
}

fn scan_unicode(text: &str, phase: &str, location: &str, findings: &mut Vec<SecurityFinding>) {
    let mut zero_width = 0;
    let mut bidi = 0;
    let mut variation = 0;
    for ch in text.chars() {
        let code = ch as u32;
        if matches!(code, 0x200B | 0x200C | 0x200D | 0x2060 | 0xFEFF) { zero_width += 1; }
        if (0x202A..=0x202E).contains(&code) || (0x2066..=0x2069).contains(&code) { bidi += 1; }
        if (0xFE00..=0xFE0F).contains(&code) || (0xE0100..=0xE01EF).contains(&code) { variation += 1; }
    }
    if zero_width > 0 {
        add(findings, phase, "unicode", "unicode.zero_width", RiskLevel::Medium, "发现零宽 Unicode 字符", "内容包含不可见零宽字符，可能用于隐藏标记或混淆文本。", location, &format!("zero_width_count={}", zero_width));
    }
    if bidi > 0 {
        add(findings, phase, "unicode", "unicode.bidi_control", RiskLevel::High, "发现方向控制 Unicode 字符", "内容包含 Bidi 方向控制字符，可能改变代码、URL 或命令的视觉顺序。", location, &format!("bidi_count={}", bidi));
    }
    if variation > 0 {
        add(findings, phase, "unicode", "unicode.variation_selector", RiskLevel::Medium, "发现变体选择符", "内容包含 Unicode variation selector，可能被用于隐写编码。", location, &format!("variation_count={}", variation));
    }
}

fn scan_network(text: &str, phase: &str, location: &str, findings: &mut Vec<SecurityFinding>) {
    let lower = text.to_ascii_lowercase();
    let ip_probe = ["ifconfig.me", "ipinfo.io", "ip-api.com", "ipify.org", "ident.me", "icanhazip.com", "api.ip.sb"];
    for domain in ip_probe {
        if lower.contains(domain) {
            add(findings, phase, "network", "network.ip_probe", RiskLevel::High, "发现公网 IP 探测服务", "内容引用公网 IP 查询服务，可能用于识别真实出口 IP 或代理状态。", location, domain);
            break;
        }
    }
    let suspicious = ["webhook.site", "requestbin", "ngrok", "trycloudflare.com", "pastebin.com", "transfer.sh", "file.io"];
    for domain in suspicious {
        if lower.contains(domain) {
            add(findings, phase, "network", "network.suspicious_domain", RiskLevel::High, "发现可疑外联域名", "内容引用临时 Webhook、隧道或文件投递服务，可能用于接收外传数据。", location, domain);
            break;
        }
    }
    if lower.contains("http://") || lower.contains("https://") {
        add(findings, phase, "network", "network.external_url", RiskLevel::Info, "发现外部 URL", "内容包含外部 URL，建议结合上下文判断是否为正常请求或外联风险。", location, first_url(text).unwrap_or_else(|| "http(s) URL".to_string()).as_str());
    }
}

fn scan_tool_risks(text: &str, phase: &str, location: &str, findings: &mut Vec<SecurityFinding>) {
    let lower = text.to_ascii_lowercase();
    let has_shell = ["curl ", "wget ", " nc ", "ncat ", "scp ", "rsync ", "bash -c", "sh -c", "python -c", "node -e", "powershell", "osascript"].iter().any(|x| lower.contains(x));
    if has_shell {
        add(findings, phase, "tool", "tool.shell.network_or_exec", RiskLevel::Medium, "发现高风险命令片段", "内容包含 curl/wget/nc/scp/bash -c/python -c 等命令，可能涉及外联、下载执行或数据传输。", location, snippet(text));
    }
    let reads_sensitive = ["cat .env", "cat ~/.ssh", "cat /users", "cat /home", "env |", "printenv", "base64 ~/.ssh", "tar ", "zip "].iter().any(|x| lower.contains(x));
    let network = ["curl ", "wget ", "http://", "https://", "scp ", "rsync ", "nc "].iter().any(|x| lower.contains(x));
    if reads_sensitive && network {
        add(findings, phase, "tool", "tool.shell.exfiltration", RiskLevel::Critical, "疑似敏感数据外传命令", "内容同时包含敏感文件/环境变量读取与外部网络传输特征，存在严重外泄风险。", location, snippet(text));
    }
}

fn scan_tracking_pixel(text: &str, phase: &str, location: &str, findings: &mut Vec<SecurityFinding>) {
    let lower = text.to_ascii_lowercase();
    let has_img = lower.contains("<img") || lower.contains("![](") || lower.contains("![");
    let tracking = lower.contains("pixel") || lower.contains("track") || lower.contains("beacon") || lower.contains("open=") || lower.contains("analytics");
    let tiny = lower.contains("width=\"1\"") || lower.contains("height=\"1\"") || lower.contains("width='1'") || lower.contains("height='1'");
    if has_img && (tracking || tiny) && (lower.contains("http://") || lower.contains("https://")) {
        add(findings, phase, "network", "html.tracking_pixel", RiskLevel::High, "疑似追踪像素", "内容包含远程图片且具备 1x1、track、pixel、beacon 等追踪特征，可能暴露打开时间与真实 IP。", location, snippet(text));
    }
}

fn scan_fingerprint_terms(text: &str, phase: &str, location: &str, findings: &mut Vec<SecurityFinding>) {
    let lower = text.to_ascii_lowercase();
    let terms = ["timezone", "locale", "proxy", "vpn", "fingerprint", "risk control", "blacklist", "风控", "封禁", "代理检测", "时区", "指纹", "静默上报", "隐写"];
    let count = terms.iter().filter(|t| lower.contains(**t)).count();
    if count >= 2 {
        add(findings, phase, "prompt", "prompt.fingerprint_context", RiskLevel::Medium, "发现账号画像/风控相关上下文", "内容同时出现多个时区、代理、指纹、风控或隐写相关词，可能与账号画像或访问风险识别有关。", location, snippet(text));
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
    add(f, phase, category, rule_id, severity, title, description, location, evidence);
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
    if credential > 0 { parts.push(format!("{} 个凭证风险", credential)); }
    if file > 0 { parts.push(format!("{} 个敏感文件/路径风险", file)); }
    if tool > 0 { parts.push(format!("{} 个工具/命令风险", tool)); }
    if network > 0 { parts.push(format!("{} 个网络/追踪风险", network)); }
    if unicode > 0 { parts.push(format!("{} 个 Unicode 隐写/混淆风险", unicode)); }
    if parts.is_empty() { parts.push(format!("{} 个风险项", findings.len())); }
    format!("{:?}：发现{}。", level, parts.join("、"))
}

fn split_candidates(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || ['"', '\'', ',', ';', '(', ')', '[', ']', '{', '}', '<', '>'].contains(&c))
}

fn mask_evidence(e: &str) -> String {
    let s = e.replace('\n', " ");
    if s.len() <= 16 { return s; }
    let start: String = s.chars().take(8).collect();
    let end: String = s.chars().rev().take(4).collect::<String>().chars().rev().collect();
    format!("{}****{}", start, end)
}

fn snippet(text: &str) -> &str {
    let max = 160;
    if text.len() <= max { text } else { &text[..max] }
}

fn first_url(text: &str) -> Option<String> {
    for part in text.split_whitespace() {
        if part.starts_with("http://") || part.starts_with("https://") {
            return Some(part.trim_matches(|c: char| c == '"' || c == '\'' || c == ')' || c == ']').to_string());
        }
    }
    None
}
