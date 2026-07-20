use crate::security::rules::{
    BuiltinRule, BuiltinRuleRepository, CustomRule, CustomRuleRepository,
    CreateCustomRuleInput, UpdateBuiltinRuleInput, seed_builtin_rules,
};
use crate::AppState;

#[tauri::command]
pub async fn get_builtin_security_rules(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<BuiltinRule>, String> {
    // Auto-seed if empty
    let rules = BuiltinRuleRepository::get_all(&state.db.pool)
        .await
        .map_err(|e| e.to_string())?;
    if rules.is_empty() {
        seed_builtin_rules(&state.db.pool)
            .await
            .map_err(|e| e.to_string())?;
        return BuiltinRuleRepository::get_all(&state.db.pool)
            .await
            .map_err(|e| e.to_string());
    }
    Ok(rules)
}

#[tauri::command]
pub async fn update_builtin_security_rule(
    id: String,
    input: UpdateBuiltinRuleInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    BuiltinRuleRepository::update(&state.db.pool, &id, &input)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_builtin_security_rule(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    BuiltinRuleRepository::delete(&state.db.pool, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reset_builtin_security_rules(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<BuiltinRule>, String> {
    BuiltinRuleRepository::reset_to_defaults(&state.db.pool)
        .await
        .map_err(|e| e.to_string())?;
    BuiltinRuleRepository::get_all(&state.db.pool)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_custom_security_rules(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<CustomRule>, String> {
    CustomRuleRepository::get_all(&state.db.pool)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_custom_security_rule(
    input: CreateCustomRuleInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<CustomRule, String> {
    CustomRuleRepository::create(&state.db.pool, &input)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn toggle_custom_security_rule(
    id: String,
    enabled: bool,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    CustomRuleRepository::update_enabled(&state.db.pool, &id, enabled)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_custom_security_rule(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    CustomRuleRepository::delete(&state.db.pool, &id)
        .await
        .map_err(|e| e.to_string())
}
