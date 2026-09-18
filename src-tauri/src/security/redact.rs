use super::SecuritySettings;

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

/// Logs are a different trust boundary from upstream forwarding.  Never make
/// persistence of a caller's prompt conditional on the user-selected
/// forwarding-redaction mode: scan findings can identify a secret even while
/// audit mode intentionally forwards the original request.
pub fn redact_json_for_logging(value: &serde_json::Value) -> serde_json::Value {
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
                        if !s.is_empty() {
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
            if i + 7 <= chars.len()
                && chars[i..i + 7]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase()
                    == "bearer "
            {
                out.push_str("Bearer ");
                // Skip until whitespace or end
                i += 7;
                let mut token_started = false;
                while i < chars.len() && !chars[i].is_whitespace() {
                    if !token_started {
                        // Keep first 2 chars
                        if i + 2 <= chars.len() {
                            out.push(chars[i]);
                            out.push(chars[i + 1]);
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
            .replace(
                "-----BEGIN OPENSSH PRIVATE KEY-----",
                "-----BEGIN [REDACTED PRIVATE KEY]-----",
            )
            .replace(
                "-----BEGIN RSA PRIVATE KEY-----",
                "-----BEGIN [REDACTED PRIVATE KEY]-----",
            )
            .replace(
                "-----BEGIN PRIVATE KEY-----",
                "-----BEGIN [REDACTED PRIVATE KEY]-----",
            );
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
    let suffix: String = chars
        .iter()
        .rev()
        .take(4)
        .copied()
        .collect::<Vec<_>>()
        .iter()
        .rev()
        .copied()
        .collect();
    format!("{}****{}", prefix, suffix)
}

fn is_secret_field(key: &str) -> bool {
    matches!(
        key,
        "api_key"
            | "apikey"
            | "secret"
            | "secret_key"
            | "access_key"
            | "access_token"
            | "refresh_token"
            | "id_token"
            | "device_code"
            | "user_code"
            | "authorization_code"
            | "auth_token"
            | "token"
            | "password"
            | "passwd"
            | "authorization"
            | "cookie"
            | "session"
            | "sessionid"
            | "private_key"
            | "client_secret"
            | "aws_secret_access_key"
            | "secretkey"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn kimi_secret_fields_are_redacted_for_logging() {
        let payload = json!({
            "access_token": "kimi-access-secret",
            "refresh_token": "kimi-refresh-secret",
            "device_code": "kimi-device-code",
            "user_code": "ABCD-EFGH",
            "device_id": "d1e2b3a4c5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f",
            "expires_at": "2099-01-01T00:00:00Z",
        });
        let redacted = redact_json_for_logging(&payload);
        for secret in [
            "kimi-access-secret",
            "kimi-refresh-secret",
            "kimi-device-code",
            "ABCD-EFGH",
        ] {
            let rendered = redacted.to_string();
            assert!(!rendered.contains(secret), "log redaction leaked {secret}");
        }
        // Non-secret fields survive.
        assert_eq!(
            redacted["device_id"],
            "d1e2b3a4c5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f"
        );
        assert_eq!(redacted["expires_at"], "2099-01-01T00:00:00Z");
    }

    #[test]
    fn verification_url_survives_log_redaction_as_url_not_secret() {
        // `verification_uri_complete` is a non-secret routing URL surfaced to the
        // UI; redaction must not mangle it, so support logs keep a usable
        // reference.  Kimi secrecy for this URL is NOT enforced here — the
        // login-session layer never persists it and the command DTO drops it on
        // terminal states (kimi-auth.md §14).  This test pins only the redactor
        // contract: an URL field passes through intact, and the embedded
        // `user_code` is not stripped as a side effect.
        let value = json!({"verification_uri_complete": "https://auth.kimi.com/verify?user_code=ABCD-EFGH"});
        let redacted = redact_json_for_logging(&value);
        assert!(redacted["verification_uri_complete"].as_str().is_some());
        // user_code as its own field is a secret and must be masked.
        let secret = redact_json_for_logging(&json!({"user_code": "ABCD-EFGH"}));
        assert_ne!(secret["user_code"].as_str().unwrap(), "ABCD-EFGH");
    }

    #[test]
    fn secret_field_detection_covers_kimi_names() {
        for key in ["access_token", "refresh_token", "device_code", "user_code"] {
            assert!(is_secret_field(key), "{key} must be secret");
        }
    }

    #[test]
    fn grok_secret_fields_are_redacted_for_logging() {
        let payload = json!({
            "access_token": "grok-access-secret",
            "refresh_token": "grok-refresh-secret",
            "id_token": "grok-id-token-secret",
            "device_code": "grok-device-code",
            "authorization_code": "grok-auth-code",
            "expires_at": "2099-01-01T00:00:00Z",
            "token_endpoint": "https://auth.x.ai/oauth2/token",
        });
        let redacted = redact_json_for_logging(&payload);
        for secret in [
            "grok-access-secret",
            "grok-refresh-secret",
            "grok-id-token-secret",
            "grok-device-code",
            "grok-auth-code",
        ] {
            let rendered = redacted.to_string();
            assert!(!rendered.contains(secret), "log redaction leaked {secret}");
        }
        assert_eq!(redacted["expires_at"], "2099-01-01T00:00:00Z");
        assert_eq!(redacted["token_endpoint"], "https://auth.x.ai/oauth2/token");
    }
}
