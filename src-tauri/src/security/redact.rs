use super::{RiskLevel, SecurityFinding, SecuritySettings};
use serde::{Deserialize, Serialize};

/// Redact sensitive tokens in a JSON value, returning a new JSON with secrets replaced.
/// Only applies when settings.redact_secrets is true AND mode is "redact".
pub fn redact_json(value: &serde_json::Value, settings: &SecuritySettings) -> serde_json::Value {
    if !settings.enabled || !settings.redact_secrets {
        return value.clone();
    }
    let mut cloned = value.clone();
    redact_value_in_place(&mut cloned);
    cloned
}

fn redact_value_in_place(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            *s = redact_string(s);
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value_in_place(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                // Also check if the key itself indicates a secret field
                let lower_key = k.to_ascii_lowercase();
                if is_secret_field(&lower_key) {
                    if let Some(s) = v.as_str() {
                        if s.len() > 4 {
                            *v = serde_json::Value::String(mask_string(s));
                        }
                    }
                }
                redact_value_in_place(v);
            }
        }
        _ => {}
    }
}

fn redact_string(s: &str) -> String {
    let mut result = s.to_string();
    // Redact API keys
    result = redact_pattern(&result, |t| {
        (t.starts_with("sk-") && t.len() >= 24)
            || (t.starts_with("sk-ant-") && t.len() >= 30)
            || (t.starts_with("ghp_") && t.len() >= 20)
            || (t.starts_with("gho_") && t.len() >= 20)
            || (t.starts_with("xoxb-") && t.len() >= 20)
            || (t.starts_with("AKIA") && t.len() >= 16)
            || (t.starts_with("AIza") && t.len() >= 20)
    });
    // Redact JWT
    result = redact_pattern(&result, |t| {
        t.starts_with("eyJ") && t.len() >= 30 && t.contains('.')
    });
    // Redact Bearer tokens
    let lower = result.to_ascii_lowercase();
    if lower.contains("bearer ") {
        // Replace "Bearer xxx" with "Bearer [REDACTED]"
        let mut out = String::new();
        let mut consumed = String::new();
        let chars: Vec<char> = result.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if i + 7 <= chars.len() && chars[i..i+7].iter().collect::<String>().to_ascii_lowercase() == "bearer " {
                out.push_str("Bearer ");
                // Skip until whitespace or end
                i += 7;
                let mut token_started = false;
                while i < chars.len() && !chars[i].is_whitespace() {
                    if !token_started {
                        // Keep first 2 chars
                        if i + 2 <= chars.len() {
                            out.push(chars[i]);
                            out.push(chars[i+1]);
                            i += 2;
                        }
                        token_started = true;
                    } else {
                        i += 1;
                    }
                }
                out.push_str("****");
                consumed.clear();
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        result = out;
    }
    // Redact private keys
    if result.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
        || result.contains("-----BEGIN RSA PRIVATE KEY-----")
        || result.contains("-----BEGIN PRIVATE KEY-----")
    {
        result = result
            .replace("-----BEGIN OPENSSH PRIVATE KEY-----", "-----BEGIN [REDACTED PRIVATE KEY]-----")
            .replace("-----BEGIN RSA PRIVATE KEY-----", "-----BEGIN [REDACTED PRIVATE KEY]-----")
            .replace("-----BEGIN PRIVATE KEY-----", "-----BEGIN [REDACTED PRIVATE KEY]-----");
        // Remove key body lines
        let mut out = String::new();
        let mut in_key = false;
        for line in result.lines() {
            if line.contains("[REDACTED PRIVATE KEY]") {
                in_key = true;
                out.push_str(line);
                out.push('\n');
                continue;
            }
            if in_key {
                if line.starts_with("-----END") {
                    out.push_str("-----END [REDACTED PRIVATE KEY]-----");
                    out.push('\n');
                    in_key = false;
                }
                // Skip key body
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        result = out.trim_end().to_string();
    }
    result
}

fn redact_pattern<F>(text: &str, matcher: F) -> String
where
    F: Fn(&str) -> bool,
{
    let mut result = String::with_capacity(text.len());
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' || ch == '.' || ch == ':' {
            current.push(ch);
        } else {
            if !current.is_empty() {
                if matcher(&current) {
                    result.push_str(&mask_string(&current));
                } else {
                    result.push_str(&current);
                }
                current.clear();
            }
            result.push(ch);
        }
    }
    if !current.is_empty() {
        if matcher(&current) {
            result.push_str(&mask_string(&current));
        } else {
            result.push_str(&current);
        }
    }
    result
}

fn mask_string(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let prefix: String = chars.iter().take(4).collect();
    let suffix: String = chars.iter().rev().take(4).copied().collect::<Vec<_>>().iter().rev().copied().collect();
    format!("{}****{}", prefix, suffix)
}

fn is_secret_field(key: &str) -> bool {
    matches!(
        key,
        "api_key" | "apikey" | "secret" | "secret_key" | "access_key"
            | "access_token" | "auth_token" | "token" | "password" | "passwd"
            | "authorization" | "cookie" | "session" | "sessionid" | "private_key"
            | "client_secret" | "aws_secret_access_key" | "secretkey"
    )
}
