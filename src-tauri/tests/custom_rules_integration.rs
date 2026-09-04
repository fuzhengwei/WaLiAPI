use serde_json::json;
use sqlx::SqlitePool;
use waliapi_lib::security::{
    gate::{gate_original, DownstreamProtocol},
    rules::{CreateCustomRuleInput, CustomRuleRepository},
    SecurityAction, SecuritySettings,
};

/// In-memory SQLite with all migrations applied.
async fn fresh_db() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory db");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrate fresh db");
    pool
}

fn security_settings_block() -> SecuritySettings {
    SecuritySettings {
        enabled: true,
        mode: "block".to_string(),
        scan_request: true,
        scan_response: false,
        scan_unicode: true,
        scan_tools: true,
        scan_network: true,
        redact_secrets: false,
        block_on_critical: true,
        max_scan_bytes: 1024 * 1024,
    }
}

#[tokio::test]
async fn test_custom_blacklist_keyword_intercepts_and_blocks() {
    let pool = fresh_db().await;

    // 1. Create and enable a custom keyword blacklist rule
    CustomRuleRepository::create(
        &pool,
        &CreateCustomRuleInput {
            rule_type: "blacklist".to_string(),
            category: "keyword".to_string(),
            pattern: "ProjectCobra".to_string(),
            severity: Some("high".to_string()),
            action: Some("block".to_string()),
            description: Some("Confidential internal project codename".to_string()),
        },
    )
    .await
    .expect("create custom blacklist rule");

    let rules = CustomRuleRepository::get_enabled(&pool)
        .await
        .expect("get enabled rules");
    assert_eq!(rules.len(), 1);

    // 2. Scan a request containing the blacklisted keyword
    let body = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "user", "content": "Please explain the architecture of ProjectCobra in detail."}
        ]
    });

    let audited = gate_original(
        DownstreamProtocol::ChatCompletions,
        "/v1/chat/completions",
        body,
        None,
        "gpt-4o".to_string(),
        false,
        None,
        &security_settings_block(),
        None,
        rules,
    )
    .expect("gate executes");

    // 3. Verify custom finding and block action
    assert_eq!(audited.audit_result.action, SecurityAction::Block);
    assert!(audited
        .audit_result
        .findings
        .iter()
        .any(|f| f.rule_id == "custom.blacklist.keyword"
            && f.category == "custom"
            && f.evidence_masked.contains("ProjectCobra")));
}

#[tokio::test]
async fn test_custom_whitelist_domain_exempts_builtin_alarm() {
    let pool = fresh_db().await;

    let settings = security_settings_block();
    let probe_body = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "user", "content": "Check external IP via https://ifconfig.me/all"}
        ]
    });

    // 1. Without whitelist: builtin rule triggers on ifconfig.me
    let audited_without_wl = gate_original(
        DownstreamProtocol::ChatCompletions,
        "/v1/chat/completions",
        probe_body.clone(),
        None,
        "gpt-4o".to_string(),
        false,
        None,
        &settings,
        None,
        vec![],
    )
    .expect("gate executes");

    assert!(audited_without_wl
        .audit_result
        .findings
        .iter()
        .any(|f| f.rule_id == "network.ip_probe"));

    // 2. Insert custom whitelist for domain "ifconfig.me"
    CustomRuleRepository::create(
        &pool,
        &CreateCustomRuleInput {
            rule_type: "whitelist".to_string(),
            category: "domain".to_string(),
            pattern: "ifconfig.me".to_string(),
            severity: Some("low".to_string()),
            action: Some("allow".to_string()),
            description: Some("Exempt internal IP probe".to_string()),
        },
    )
    .await
    .expect("create whitelist rule");

    let rules = CustomRuleRepository::get_enabled(&pool)
        .await
        .expect("get enabled rules");

    // 3. With whitelist: domain check is short-circuited and exempt
    let audited_with_wl = gate_original(
        DownstreamProtocol::ChatCompletions,
        "/v1/chat/completions",
        probe_body,
        None,
        "gpt-4o".to_string(),
        false,
        None,
        &settings,
        None,
        rules,
    )
    .expect("gate executes");

    assert!(!audited_with_wl
        .audit_result
        .findings
        .iter()
        .any(|f| f.rule_id == "network.ip_probe"));
}

#[tokio::test]
async fn test_disabled_custom_rule_is_ignored() {
    let pool = fresh_db().await;

    // 1. Create a custom rule
    let rule = CustomRuleRepository::create(
        &pool,
        &CreateCustomRuleInput {
            rule_type: "blacklist".to_string(),
            category: "keyword".to_string(),
            pattern: "ProjectSecret".to_string(),
            severity: Some("high".to_string()),
            action: Some("block".to_string()),
            description: Some("Secret codename".to_string()),
        },
    )
    .await
    .expect("create rule");

    // 2. Disable the rule
    CustomRuleRepository::update_enabled(&pool, &rule.id, false)
        .await
        .expect("disable rule");

    // 3. Enabled list should not include the disabled rule
    let enabled_rules = CustomRuleRepository::get_enabled(&pool)
        .await
        .expect("get enabled rules");
    assert!(enabled_rules.is_empty());

    // 4. Gate run with enabled rules passes without violation
    let body = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "user", "content": "Tell me about ProjectSecret"}
        ]
    });

    let audited = gate_original(
        DownstreamProtocol::ChatCompletions,
        "/v1/chat/completions",
        body,
        None,
        "gpt-4o".to_string(),
        false,
        None,
        &security_settings_block(),
        None,
        enabled_rules,
    )
    .expect("gate executes");

    assert_eq!(audited.audit_result.action, SecurityAction::Allow);
    assert!(!audited
        .audit_result
        .findings
        .iter()
        .any(|f| f.category == "custom"));
}
