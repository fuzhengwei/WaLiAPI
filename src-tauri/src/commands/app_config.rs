use crate::db::repository::Repository;
use crate::AppState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

// ── 数据结构 ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub label: String,
    pub icon: String,
    pub description: String,
    pub config_path: String,
    pub config_format: String,
    pub available: bool,
    pub applied: bool,
    pub download_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub success: bool,
    pub message: String,
    /// Codex 专用：切回原账号后检测到 auth.json 仍处于 API Key 模式时的提示
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigContent {
    pub exists: bool,
    pub content: String,
    pub error: Option<String>,
}

// ── 应用定义 ──

struct AppDef {
    name: &'static str,
    label: &'static str,
    icon: &'static str,
    description: &'static str,
    config_format: &'static str,
    download_url: &'static str,
    config_dir_fn: fn() -> PathBuf,
    config_file: &'static str,
    check_installed_fn: fn(&PathBuf) -> bool,
}

fn home_dir() -> PathBuf {
    if let Ok(path) = std::env::var("WALIAPI_TARGET_HOME") {
        let path = path.trim();
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

const APPS: &[AppDef] = &[
    AppDef {
        name: "claude-code",
        label: "Claude Code",
        icon: "terminal",
        description: "Anthropic 的命令行 AI 编程助手，读取 ~/.claude/settings.json 中的 env 配置",
        config_format: "JSON (~/.claude/settings.json)",
        download_url: "https://docs.anthropic.com/en/docs/claude-code/overview",
        config_dir_fn: || home_dir().join(".claude"),
        config_file: "settings.json",
        check_installed_fn: |dir| dir.exists() || home_dir().join(".claude.json").exists(),
    },
    AppDef {
        name: "codex",
        label: "Codex CLI",
        icon: "code",
        description: "OpenAI Codex 命令行工具，读取 ~/.codex/auth.json 和 config.toml",
        config_format: "JSON + TOML (~/.codex/)",
        download_url: "https://github.com/openai/codex",
        config_dir_fn: || home_dir().join(".codex"),
        config_file: "config.toml",
        check_installed_fn: |dir| dir.exists(),
    },
    AppDef {
        name: "gemini-cli",
        label: "Gemini CLI",
        icon: "boxes",
        description: "Google Gemini 命令行工具，读取 ~/.gemini/.env 和 settings.json",
        config_format: "ENV + JSON (~/.gemini/)",
        download_url: "https://github.com/google-gemini/gemini-cli",
        config_dir_fn: || home_dir().join(".gemini"),
        config_file: ".env",
        check_installed_fn: |dir| dir.exists(),
    },
    AppDef {
        name: "claude-desktop",
        label: "Claude Desktop",
        icon: "sparkles",
        description: "Anthropic 桌面应用，读取 claude_desktop_config.json",
        config_format: "JSON (claude_desktop_config.json)",
        download_url: "https://claude.ai/download",
        config_dir_fn: || {
            #[cfg(target_os = "macos")]
            {
                home_dir().join("Library/Application Support/Claude")
            }
            #[cfg(target_os = "windows")]
            {
                home_dir().join("AppData/Roaming/Claude")
            }
            #[cfg(target_os = "linux")]
            {
                home_dir().join(".config/Claude")
            }
        },
        config_file: "claude_desktop_config.json",
        check_installed_fn: |dir| dir.exists(),
    },
    AppDef {
        name: "opencode",
        label: "OpenCode",
        icon: "wrench",
        description: "开源 AI 编程工具，读取 opencode.json 中的 provider 配置",
        config_format: "JSON (~/.config/opencode/opencode.json)",
        download_url: "https://opencode.ai",
        config_dir_fn: || home_dir().join(".config/opencode"),
        config_file: "opencode.json",
        check_installed_fn: |dir| dir.exists(),
    },
    AppDef {
        name: "openclaw",
        label: "OpenClaw",
        icon: "bot",
        description: "开源 Agent 框架，读取配置文件中的 provider 段",
        config_format: "JSON (~/.qclaw/)",
        download_url: "https://openclaw.ai",
        config_dir_fn: || home_dir().join(".qclaw"),
        config_file: "config.json",
        check_installed_fn: |dir| dir.exists(),
    },
    AppDef {
        name: "hermes",
        label: "Hermes Agent",
        icon: "code",
        description: "Hermes Agent 框架，读取配置文件中的 custom_providers 段",
        config_format: "TOML/JSON (Hermes config)",
        download_url: "https://github.com/openai/hermes",
        config_dir_fn: || home_dir().join(".hermes"),
        config_file: "config.json",
        check_installed_fn: |dir| dir.exists(),
    },
    AppDef {
        name: "walicode",
        label: "WaLiCode",
        icon: "code",
        description: "AI Coding Assistant，写入 ai_settings.json 中的 provider 和 apiKey 配置",
        config_format: "JSON (~/Library/Application Support/WaLiCode/ai_settings.json)",
        download_url: "https://walicode.xiaofuge.cn/",
        #[cfg(target_os = "macos")]
        config_dir_fn: || home_dir().join("Library/Application Support/WaLiCode"),
        #[cfg(target_os = "windows")]
        config_dir_fn: || home_dir().join("AppData/Roaming/WaLiCode"),
        #[cfg(target_os = "linux")]
        config_dir_fn: || home_dir().join(".config/walicode"),
        config_file: "ai_settings.json",
        check_installed_fn: |dir| dir.exists(),
    },
];

// ── 原子写入 ──

fn atomic_write(path: &PathBuf, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let tmp = path.with_extension(format!(
        "tmp.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::write(&tmp, data).map_err(|e| format!("写入临时文件失败: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("替换文件失败: {e}")
    })?;
    Ok(())
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("读取文件失败: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("解析 JSON 失败: {e}"))
}

fn write_json_file<T: Serialize>(path: &PathBuf, data: &T) -> Result<(), String> {
    let json = to_pretty_json(data).map_err(|e| format!("序列化 JSON 失败: {e}"))?;
    atomic_write(path, json.as_bytes())
}

/// 自定义 JSON pretty printer，不转义 non-ASCII 字符
fn to_pretty_json<T: Serialize>(data: &T) -> Result<String, String> {
    let value = serde_json::to_value(data).map_err(|e| format!("{e}"))?;
    let mut out = String::new();
    write_value(&mut out, &value, 0);
    Ok(out)
}

fn write_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, v: &serde_json::Value, depth: usize) {
    match v {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        serde_json::Value::Number(n) => out.push_str(&n.to_string()),
        serde_json::Value::String(s) => write_json_string(out, s),
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                out.push_str("[]");
            } else {
                out.push('[');
                for (i, item) in arr.iter().enumerate() {
                    out.push('\n');
                    write_indent(out, depth + 1);
                    write_value(out, item, depth + 1);
                    if i < arr.len() - 1 {
                        out.push(',');
                    }
                }
                out.push('\n');
                write_indent(out, depth);
                out.push(']');
            }
        }
        serde_json::Value::Object(obj) => {
            if obj.is_empty() {
                out.push_str("{}");
            } else {
                out.push('{');
                let len = obj.len();
                for (i, (k, val)) in obj.iter().enumerate() {
                    out.push('\n');
                    write_indent(out, depth + 1);
                    write_json_string(out, k);
                    out.push_str(": ");
                    write_value(out, val, depth + 1);
                    if i < len - 1 {
                        out.push(',');
                    }
                }
                out.push('\n');
                write_indent(out, depth);
                out.push('}');
            }
        }
    }
}

/// 写入 JSON 字符串，只转义必要的控制字符，保留 non-ASCII 原文
fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c), // 非 ASCII 字符（中文等）直接保留
        }
    }
    out.push('"');
}

// ── 备份与恢复 ──

fn backup_path(config_path: &PathBuf) -> PathBuf {
    let mut name = config_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    name.push_str(".waliapi-backup");
    config_path.with_file_name(name)
}

fn backup_config(config_path: &PathBuf) -> Result<(), String> {
    if config_path.exists() {
        let content = fs::read(config_path).map_err(|e| format!("读取配置失败: {e}"))?;
        atomic_write(&backup_path(config_path), &content)?;
    }
    Ok(())
}

/// 标记文件：记录写入前配置不存在，恢复时应删除写入的配置
fn absent_marker_path(config_path: &PathBuf) -> PathBuf {
    let mut name = config_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    name.push_str(".waliapi-absent");
    config_path.with_file_name(name)
}

fn restore_config(config_path: &PathBuf) -> Result<(), String> {
    let absent_marker = absent_marker_path(config_path);
    if absent_marker.exists() {
        // 写入前配置本就不存在：恢复 = 删除写入的配置
        if config_path.exists() {
            fs::remove_file(config_path).map_err(|e| format!("删除配置失败: {e}"))?;
        }
        let _ = fs::remove_file(&absent_marker);
        return Ok(());
    }
    let backup = backup_path(config_path);
    if backup.exists() {
        let content = fs::read(&backup).map_err(|e| format!("读取备份失败: {e}"))?;
        atomic_write(config_path, &content)?;
        let _ = fs::remove_file(&backup);
        Ok(())
    } else {
        Err("没有找到备份文件".to_string())
    }
}

// ── 获取 WaLiAPI 网关信息 ──

async fn get_waliapi_url(state: &Arc<AppState>) -> String {
    if let Ok(public_url) = std::env::var("WALIAPI_PUBLIC_URL") {
        let public_url = public_url.trim().trim_end_matches('/');
        if !public_url.is_empty() {
            return public_url.to_string();
        }
    }
    let port = *state.server_port.read().await;
    format!("http://127.0.0.1:{}", port)
}

#[allow(dead_code)]
fn get_waliapi_key(state: &Arc<AppState>) -> Result<String, String> {
    let repo = Repository::new(state.db.pool.clone());
    let keys = tokio::task::block_in_place(|| {
        tauri::async_runtime::handle().block_on(async { repo.get_all_api_keys().await })
    })
    .map_err(|e| format!("获取 API Key 失败: {e}"))?;

    keys.into_iter()
        .find(|k| k.status == 1)
        .map(|k| k.key)
        .ok_or_else(|| "没有可用的 API Key，请先在「密钥」页创建".to_string())
}

// ── 各应用配置写入逻辑 ──

/// 先读取并验证，再创建备份并原子替换。这样格式错误或认证冲突不会留下新的
/// 备份/缺失标记，更不会触碰用户的原文件。
fn write_claude_code_transactional(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let settings_path = config_dir.join("settings.json");
    let original = if settings_path.exists() {
        Some(fs::read(&settings_path).map_err(|e| format!("读取配置失败: {e}"))?)
    } else {
        None
    };
    let mut settings: serde_json::Value = match &original {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|e| format!("解析 JSON 失败: {e}"))?,
        None => serde_json::json!({}),
    };

    apply_waliapi_claude_code_settings(&mut settings, waliapi_url, waliapi_key, model)?;
    let json = to_pretty_json(&settings).map_err(|e| format!("序列化 JSON 失败: {e}"))?;

    // 只有所有校验均通过后才接触恢复资料；已有备份永远代表首次应用前的原始字节。
    let backup = backup_path(&settings_path);
    let absent = absent_marker_path(&settings_path);
    let created_backup = original.is_some() && !backup.exists();
    let created_absent_marker = original.is_none() && !absent.exists();
    if created_backup {
        atomic_write(&backup, original.as_ref().expect("checked above"))?;
    }
    if created_absent_marker {
        if let Err(error) = atomic_write(&absent, b"") {
            if created_backup {
                let _ = fs::remove_file(&backup);
            }
            return Err(error);
        }
    }

    if let Err(error) = atomic_write(&settings_path, json.as_bytes()) {
        // 本次失败不能改变恢复资料的可见状态。
        if created_backup {
            let _ = fs::remove_file(&backup);
        }
        if created_absent_marker {
            let _ = fs::remove_file(&absent);
        }
        return Err(error);
    }
    Ok(())
}

/// Claude Code 对不在内置目录中的模型会按 200K 上下文处理。参考 Codex 的
/// GPT-5.6 目录，已知 gpt-5.6 系列在 Claude Code 网关场景应声明为 372K。
const CLAUDE_CODE_GPT_56_CONTEXT_TOKENS: &str = "372000";
const CLAUDE_CODE_GPT_56_AUTO_COMPACT_TOKENS: &str = "360000";
const WALIAPI_CLAUDE_SETTINGS_META: &str = "_waliapi_claude_code";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaudeCodeModelCompatibility {
    context_tokens: Option<&'static str>,
    auto_compact_tokens: Option<&'static str>,
    behaves_as: Option<&'static str>,
    source: &'static str,
    confidence: &'static str,
}

fn claude_code_model_compatibility(model: &str) -> ClaudeCodeModelCompatibility {
    // 渠道列表有时以 `gpt-5.6-luna[1m]` 标示上游变体；兼容声明仍使用已
    // 确认的 GPT-5.6 注册表项，未知模型绝不根据名称臆造窗口。
    let model = model.trim().to_ascii_lowercase();
    let model = model.split('[').next().unwrap_or(&model);
    if model.starts_with("gpt-5.6") {
        ClaudeCodeModelCompatibility {
            context_tokens: Some(CLAUDE_CODE_GPT_56_CONTEXT_TOKENS),
            auto_compact_tokens: Some(CLAUDE_CODE_GPT_56_AUTO_COMPACT_TOKENS),
            behaves_as: Some("claude-opus-4-8"),
            source: "verified-gpt-5.6-model-metadata",
            confidence: "verified",
        }
    } else {
        ClaudeCodeModelCompatibility {
            context_tokens: None,
            auto_compact_tokens: None,
            behaves_as: None,
            source: "none",
            confidence: "unknown",
        }
    }
}

fn waliapi_managed_string_array(
    settings: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Vec<String> {
    settings
        .get(WALIAPI_CLAUDE_SETTINGS_META)
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn secret_fingerprint(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_legacy_waliapi_settings(root: &serde_json::Map<String, serde_json::Value>) -> bool {
    root.get("_waliapi").and_then(serde_json::Value::as_bool) == Some(true)
        && !root.contains_key(WALIAPI_CLAUDE_SETTINGS_META)
}

/// 将 WaLiAPI 所有的字段投影到 Claude Code settings。settings.json 同时属于
/// Claude Code 和用户，绝不能为了更新网关而整体替换 env 或 modelPicker。
fn apply_waliapi_claude_code_settings(
    settings: &mut serde_json::Value,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let root = settings.as_object_mut().ok_or_else(|| {
        "Claude Code settings.json 必须是 JSON 对象，已取消写入以保护原配置".to_string()
    })?;

    let previously_managed_env = waliapi_managed_string_array(root, "managedEnvKeys");
    let previously_managed_picker_models = waliapi_managed_string_array(root, "modelPickerModels");
    let previous_auth_fingerprint = root
        .get(WALIAPI_CLAUDE_SETTINGS_META)
        .and_then(|m| m.get("managedAuthFingerprint"))
        .and_then(|f| f.as_str())
        .map(ToOwned::to_owned);
    let legacy_settings = is_legacy_waliapi_settings(root);

    let env = root
        .entry("env".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            "Claude Code settings.json 的 env 必须是 JSON 对象，已取消写入以保护原配置".to_string()
        })?;

    // Claude Code 网关统一使用真实数据面密钥作为 Bearer token。旧版本写入的
    // API_KEY 仅在值等于本次选择的密钥、或已记录为受管字段时迁移；未知凭据必须
    // 阻止写入，避免覆盖用户自己的 Anthropic 配置。
    for key in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"] {
        if let Some(value) = env.get(key) {
            let is_target_key = value.as_str() == Some(waliapi_key);
            let is_managed = previously_managed_env.iter().any(|managed| managed == key);
            if !is_target_key
                && !is_managed
            {
                return Err(format!(
                    "Claude Code 已存在非 WaLiAPI 管理的 {key}；为保护现有凭证未写入。请先在 settings.json 中移除或手动选择一种认证方式"
                ));
            }
            if key == "ANTHROPIC_API_KEY" && is_managed && !is_target_key {
                let fingerprint_matches = previous_auth_fingerprint
                    .as_deref()
                    .zip(value.as_str())
                    .is_some_and(|(expected, actual)| secret_fingerprint(actual) == expected);
                if !fingerprint_matches {
                    return Err("Claude Code 已修改受管的 ANTHROPIC_API_KEY；为保护现有凭证未写入".to_string());
                }
            }
        }
    }

    // 仅替换本次网关需要的字段；其余用户环境变量（包括企业代理和自定义模型）原样保留。
    env.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        serde_json::Value::String(waliapi_url.trim().trim_end_matches('/').to_string()),
    );
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".to_string(),
        serde_json::Value::String(waliapi_key.to_string()),
    );
    env.remove("ANTHROPIC_API_KEY");
    // 受管模型环境变量不应覆盖 Claude Code 的 /model 持久化选择。仅删除由
    // WaLiAPI 以前写入的值或 legacy 配置中的值；用户自行设置的覆盖原样保留。
    if previously_managed_env.iter().any(|managed| managed == "ANTHROPIC_MODEL")
        || legacy_settings
    {
        env.remove("ANTHROPIC_MODEL");
    }

    let mut managed_env_keys = vec![
        "ANTHROPIC_BASE_URL".to_string(),
        "ANTHROPIC_AUTH_TOKEN".to_string(),
    ];
    let compatibility = claude_code_model_compatibility(model);
    if let (Some(max_context), Some(auto_compact)) = (
        compatibility.context_tokens,
        compatibility.auto_compact_tokens,
    ) {
        for (key, value) in [
            ("CLAUDE_CODE_MAX_CONTEXT_TOKENS", max_context),
            ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", auto_compact),
        ] {
            if !env.contains_key(key) {
                env.insert(
                    key.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
                managed_env_keys.push(key.to_string());
            }
        }
    }

    let mut managed_picker_models = Vec::new();
    if !model.trim().to_ascii_lowercase().starts_with("claude-")
        && compatibility.behaves_as.is_some()
    {
        let picker = root
            .entry("modelPicker".to_string())
            .or_insert_with(|| serde_json::json!({ "options": [] }))
            .as_object_mut()
            .ok_or_else(|| {
                "Claude Code settings.json 的 modelPicker 必须是 JSON 对象，已取消写入以保护原配置"
                    .to_string()
            })?;
        let options = picker
            .get_mut("options")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| "Claude Code settings.json 的 modelPicker.options 必须是数组，已取消写入以保护原配置".to_string())?;

        // 仅删除此前由 WaLiAPI 加入的行，随后为当前模型写入一条最小兼容行。
        options.retain(|option| {
            let option_model = option.get("model").and_then(serde_json::Value::as_str);
            !option_model.is_some_and(|option_model| {
                previously_managed_picker_models
                    .iter()
                    .any(|managed| managed == option_model)
            })
        });

        if !options.iter().any(|option| {
            option.get("model").and_then(serde_json::Value::as_str) == Some(model.trim())
        }) {
            // Claude Code 2.1.266 已正式支持该形状。behavesAs 仅用于客户端的
            // prompt/capability/effort 处理，请求中的 model 仍保持用户选择的值。
            options.push(serde_json::json!({
                "model": model.trim(),
                "label": model.trim(),
                "description": "由 WaLiAPI 网关提供",
                "behavesAs": compatibility.behaves_as.expect("checked above")
            }));
            managed_picker_models.push(model.trim().to_string());
        }
    }

    root.insert("model".to_string(), serde_json::Value::String(model.trim().to_string()));
    root.insert("_waliapi".to_string(), serde_json::json!(true));
    root.insert(
        WALIAPI_CLAUDE_SETTINGS_META.to_string(),
        serde_json::json!({
            "version": 3,
            "managedEnvKeys": managed_env_keys,
            "modelPickerModels": managed_picker_models,
            "managedTopLevelFields": ["model"],
            "managedAuthFingerprint": secret_fingerprint(waliapi_key),
            "modelCompatibility": {
                "model": model.trim(),
                "source": compatibility.source,
                "confidence": compatibility.confidence,
            },
        }),
    );
    Ok(())
}

fn write_codex(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    use toml_edit::DocumentMut;

    // Codex 鉴权方式：experimental_bearer_token 作为 Bearer token 发给上游
    // 不写 auth.json 的 OPENAI_API_KEY，避免 Codex 拿它去 OpenAI 验证
    // （参照 cc-switch 的做法，只通过 experimental_bearer_token 传递 key）

    let config_path = config_dir.join("config.toml");
    let existing_text = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .map_err(|e| format!("Failed to read config.toml: {e}"))?
    } else {
        String::new()
    };

    let mut doc = existing_text
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse config.toml: {e}"))?;

    // Set model_provider and model at top level
    doc["model_provider"] = toml_edit::value("waliapi");
    doc["model"] = toml_edit::value(model);

    // Ensure [model_providers] table exists
    if doc.get("model_providers").is_none() {
        let mut table = toml_edit::Table::new();
        table.set_implicit(true);
        doc["model_providers"] = toml_edit::Item::Table(table);
    }

    // Insert/update [model_providers.waliapi] preserving other providers
    if let Some(providers) = doc["model_providers"].as_table_mut() {
        let waliapi_entry = providers.entry("waliapi");
        let provider_table =
            waliapi_entry.or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        if let Some(table) = provider_table.as_table_mut() {
            table["name"] = toml_edit::value("WaLiAPI Gateway");
            table["base_url"] =
                toml_edit::value(format!("{}/v1", waliapi_url.trim_end_matches('/')));
            table["wire_api"] = toml_edit::value("responses");
            table["experimental_bearer_token"] = toml_edit::value(waliapi_key);
            // Ensure requires_openai_auth is NOT set for waliapi provider
            // This prevents Codex from trying to validate the token with OpenAI
            table.remove("requires_openai_auth");
        }

        // If there's a legacy 'custom' provider with requires_openai_auth = true
        // and base_url pointing to a third-party (non-OpenAI) endpoint,
        // remove requires_openai_auth to prevent auth conflicts
        if let Some(custom_table) = providers.get_mut("custom") {
            if let Some(t) = custom_table.as_table_mut() {
                if t.contains_key("requires_openai_auth") {
                    t.remove("requires_openai_auth");
                }
            }
        }
    }

    atomic_write(&config_path, doc.to_string().as_bytes())?;
    Ok(())
}

/// 检测 Codex auth.json 是否卡在 API Key 鉴权模式。
///
/// 判定（任一命中即视为 API Key 模式）：
/// - `auth_mode == "apikey"`
/// - `OPENAI_API_KEY` 为非空字符串，且没有 ChatGPT 登录态（`tokens` 缺失或为 null）
///
/// 返回 `Some(原因)` 表示处于 API Key 模式；`None` 表示 ChatGPT 登录态正常，
/// 或文件不存在 / 无法解析（交给 Codex 自行处理）。
fn detect_codex_apikey_mode(config_dir: &PathBuf) -> Option<String> {
    let auth_path = config_dir.join("auth.json");
    let text = std::fs::read_to_string(&auth_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;

    let auth_mode = v.get("auth_mode").and_then(|x| x.as_str()).unwrap_or("");
    let api_key = v
        .get("OPENAI_API_KEY")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let has_tokens = v.get("tokens").map(|t| t.is_object()).unwrap_or(false);

    if auth_mode.eq_ignore_ascii_case("apikey") || (!api_key.is_empty() && !has_tokens) {
        let reason = if !api_key.is_empty() {
            "OPENAI_API_KEY 已设置"
        } else {
            "auth_mode = apikey"
        };
        Some(format!("auth.json 处于 API Key 模式（{reason}）"))
    } else {
        None
    }
}

/// 将 Codex auth.json 重置为 ChatGPT 登录模式。
///
/// 原 auth.json 备份为 `~/.codex/auth.json.waliapi-backup`；重置后用户需运行
/// `codex login` 重新完成 ChatGPT 授权（官方推荐的退出 API Key 模式流程）。
#[tauri::command]
pub async fn reset_codex_auth() -> Result<ApplyResult, String> {
    reset_codex_auth_in(&home_dir().join(".codex"))
}

fn reset_codex_auth_in(config_dir: &PathBuf) -> Result<ApplyResult, String> {
    let auth_path = config_dir.join("auth.json");

    if !auth_path.exists() {
        return Ok(ApplyResult {
            success: false,
            message: "auth.json 不存在，无需重置：直接运行 codex login 登录 ChatGPT 账号即可"
                .to_string(),
            auth_warning: None,
        });
    }

    let content = fs::read(&auth_path).map_err(|e| format!("读取 auth.json 失败: {e}"))?;
    atomic_write(
        &auth_path.with_file_name("auth.json.waliapi-backup"),
        &content,
    )?;

    let reset = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": serde_json::Value::Null,
        "tokens": serde_json::Value::Null,
    });
    let text = serde_json::to_string_pretty(&reset).map_err(|e| format!("序列化失败: {e}"))?;
    atomic_write(&auth_path, text.as_bytes())?;

    Ok(ApplyResult {
        success: true,
        message: "已重置 auth.json 为 ChatGPT 登录模式（原文件备份为 ~/.codex/auth.json.waliapi-backup）。请重启 Codex 并运行 codex login 完成登录".to_string(),
        auth_warning: None,
    })
}

fn write_gemini_cli(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let env_path = config_dir.join(".env");
    let env_content = format!(
        "# Generated by WaLiAPI\nGEMINI_API_KEY={}\nGEMINI_BASE_URL={}\nGEMINI_MODEL={}\n",
        waliapi_key, waliapi_url, model
    );
    atomic_write(&env_path, env_content.as_bytes())?;

    let settings_path = config_dir.join("settings.json");
    if !settings_path.exists() {
        write_json_file(&settings_path, &serde_json::json!({}))?;
    }
    Ok(())
}

fn write_claude_desktop(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let config_path = config_dir.join("claude_desktop_config.json");
    let mut config: serde_json::Value = if config_path.exists() {
        read_json_file(&config_path).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if let Some(obj) = config.as_object_mut() {
        obj.insert(
            "apiKeyHelper".to_string(),
            serde_json::json!(format!("echo '{}'", waliapi_key)),
        );
        obj.insert("apiBaseUrl".to_string(), serde_json::json!(waliapi_url));
        obj.insert("defaultModel".to_string(), serde_json::json!(model));
        obj.insert("_waliapi".to_string(), serde_json::json!(true));
    }

    write_json_file(&config_path, &config)
}

fn write_opencode(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let config_path = config_dir.join("opencode.json");
    let mut config: serde_json::Value = if config_path.exists() {
        read_json_file(&config_path).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({"$schema": "https://opencode.ai/config.json"})
    };

    if let Some(obj) = config.as_object_mut() {
        let provider = serde_json::json!({
            "npm": "@ai-sdk/openai-compatible",
            "name": "WaLiAPI Gateway",
            "options": {
                "baseURL": format!("{}/v1", waliapi_url),
                "apiKey": waliapi_key
            },
            "models": {
                "waliapi-default": { "name": model }
            }
        });
        if let Some(providers) = obj.get_mut("provider").and_then(|v| v.as_object_mut()) {
            providers.insert("waliapi".to_string(), provider);
        } else {
            obj.insert(
                "provider".to_string(),
                serde_json::json!({"waliapi": provider}),
            );
        }
    }

    write_json_file(&config_path, &config)
}

fn write_openclaw(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let config_path = config_dir.join("config.json");
    let mut config: serde_json::Value = if config_path.exists() {
        read_json_file(&config_path).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if let Some(obj) = config.as_object_mut() {
        obj.insert(
            "baseUrl".to_string(),
            serde_json::json!(format!("{}/v1", waliapi_url)),
        );
        obj.insert("apiKey".to_string(), serde_json::json!(waliapi_key));
        obj.insert("model".to_string(), serde_json::json!(model));
        obj.insert("_waliapi".to_string(), serde_json::json!(true));
    }

    write_json_file(&config_path, &config)
}

fn write_hermes(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let config_path = config_dir.join("config.json");
    let mut config: serde_json::Value = if config_path.exists() {
        read_json_file(&config_path).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if let Some(obj) = config.as_object_mut() {
        if let Some(providers) = obj
            .get_mut("custom_providers")
            .and_then(|v| v.as_array_mut())
        {
            providers.retain(|p| p.get("id").and_then(|v| v.as_str()) != Some("waliapi"));
            let mut entry = serde_json::Map::new();
            entry.insert("id".to_string(), serde_json::json!("waliapi"));
            entry.insert("name".to_string(), serde_json::json!("WaLiAPI Gateway"));
            entry.insert(
                "base_url".to_string(),
                serde_json::json!(format!("{}/v1", waliapi_url)),
            );
            entry.insert("api_key".to_string(), serde_json::json!(waliapi_key));
            entry.insert("default_model".to_string(), serde_json::json!(model));
            providers.push(serde_json::Value::Object(entry));
        } else {
            let mut entry = serde_json::Map::new();
            entry.insert("id".to_string(), serde_json::json!("waliapi"));
            entry.insert("name".to_string(), serde_json::json!("WaLiAPI Gateway"));
            entry.insert(
                "base_url".to_string(),
                serde_json::json!(format!("{}/v1", waliapi_url)),
            );
            entry.insert("api_key".to_string(), serde_json::json!(waliapi_key));
            entry.insert("default_model".to_string(), serde_json::json!(model));
            obj.insert(
                "custom_providers".to_string(),
                serde_json::Value::Array(vec![serde_json::Value::Object(entry)]),
            );
        }
    }

    write_json_file(&config_path, &config)
}

fn write_walicode(
    config_dir: &PathBuf,
    waliapi_url: &str,
    waliapi_key: &str,
    model: &str,
) -> Result<(), String> {
    let base_url = format!("{}/v1", waliapi_url.trim_end_matches('/'));

    // WaLiCode 有两个可能的配置路径：
    //   1. 标准路径 ~/.config/walicode/ai_settings.json (settings_write_path 写入位置)
    //   2. 旧版路径 ~/Library/Application Support/WaLiCode/ai_settings.json (legacy)
    // WaLiCode 读取时优先查标准路径，fallback 到旧路径
    // 我们需要同时写入两个路径，确保不管走哪个都能读到

    let standard_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("walicode");
    let paths_to_write: Vec<PathBuf> = if *config_dir == standard_dir {
        vec![standard_dir.join("ai_settings.json")]
    } else {
        // 两个路径都写
        vec![
            standard_dir.join("ai_settings.json"),
            config_dir.join("ai_settings.json"),
        ]
    };

    // 读取已有配置：优先标准路径，其次旧路径
    let existing_config: serde_json::Value = paths_to_write
        .iter()
        .find_map(|p| {
            if p.exists() {
                read_json_file(p).ok()
            } else {
                None
            }
        })
        .unwrap_or_else(|| serde_json::json!({}));

    let mut config = existing_config;

    if let Some(obj) = config.as_object_mut() {
        // 在 customProviders 数组中查找或创建 waliapi provider
        let providers = obj
            .entry("customProviders".to_string())
            .or_insert_with(|| serde_json::json!([]));

        let mut found = false;
        if let Some(arr) = providers.as_array_mut() {
            for p in arr.iter_mut() {
                if p.get("id").and_then(|v| v.as_str()) == Some("waliapi") {
                    p["name"] = serde_json::json!("WaLiAPI");
                    p["apiKey"] = serde_json::json!(waliapi_key);
                    p["baseUrl"] = serde_json::json!(&base_url);
                    p["model"] = serde_json::json!(model);
                    p["apiFormat"] = serde_json::json!("openai");
                    p["enabled"] = serde_json::json!(true);
                    // 更新 customModels 列表
                    if let Some(cm) = p.get("customModels").and_then(|v| v.as_array()) {
                        if !cm.iter().any(|m| m.as_str() == Some(model)) {
                            if let Some(cm) =
                                p.get_mut("customModels").and_then(|v| v.as_array_mut())
                            {
                                cm.insert(0, serde_json::json!(model));
                            }
                        }
                    } else {
                        p["customModels"] = serde_json::json!([model]);
                    }
                    found = true;
                    break;
                }
            }

            if !found {
                arr.push(serde_json::json!({
                    "id": "waliapi",
                    "name": "WaLiAPI",
                    "apiKey": waliapi_key,
                    "baseUrl": base_url,
                    "model": model,
                    "customModels": [model],
                    "apiFormat": "openai",
                    "enabled": true
                }));
            }
        }

        // 激活 waliapi provider
        obj.insert(
            "activeCustomProviderId".to_string(),
            serde_json::json!("waliapi"),
        );
        // providerType 必须设为 custom，否则前端不会走 custom provider 分支
        obj.insert("providerType".to_string(), serde_json::json!("custom"));
        obj.insert("provider".to_string(), serde_json::json!("openai"));
        // 同步顶级字段（CLI resolve_effective_settings 的 fallback）
        obj.insert("apiKey".to_string(), serde_json::json!(waliapi_key));
        obj.insert("baseUrl".to_string(), serde_json::json!(&base_url));
        obj.insert("model".to_string(), serde_json::json!(model));
        obj.insert("_waliapi".to_string(), serde_json::json!(true));
    }

    // 写入所有目标路径
    let mut errors = Vec::new();
    for path in &paths_to_write {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = fs::create_dir_all(parent) {
                    errors.push(format!("创建目录 {} 失败: {}", parent.display(), e));
                    continue;
                }
            }
        }
        if let Err(e) = write_json_file(path, &config) {
            errors.push(format!("写入 {} 失败: {}", path.display(), e));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

// ── 检测是否已由 WaLiAPI 配置 ──

fn detect_applied(config_path: &PathBuf, app_name: &str) -> bool {
    if !config_path.exists() {
        return false;
    }
    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    match app_name {
        "claude-code" | "claude-desktop" | "openclaw" => {
            let v: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => return false,
            };
            if app_name == "claude-code" {
                let managed = v.get(WALIAPI_CLAUDE_SETTINGS_META)
                    .and_then(|m| m.get("version"))
                    .and_then(|n| n.as_u64())
                    .is_some_and(|version| version >= 3);
                let env = v.get("env").and_then(|e| e.as_object());
                managed && v.get("_waliapi").and_then(|x| x.as_bool()) == Some(true)
                    && env.and_then(|e| e.get("ANTHROPIC_AUTH_TOKEN")).and_then(|x| x.as_str()).is_some_and(|s| !s.is_empty())
                    && env.and_then(|e| e.get("ANTHROPIC_API_KEY")).is_none()
            } else {
                v.get("_waliapi").and_then(|v| v.as_bool()).unwrap_or(false)
            }
        }
        "codex" => content.contains("WaLiAPI") || content.contains("waliapi"),
        "gemini-cli" => content.contains("WaLiAPI"),
        "opencode" => {
            let v: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => return false,
            };
            v.pointer("/provider/waliapi").is_some()
        }
        "hermes" => {
            let v: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => return false,
            };
            v.get("custom_providers")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some("waliapi"))
                })
                .is_some()
        }
        "walicode" => {
            // 检查两个可能的路径：旧路径（config_path）和标准路径
            let standard_path = dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("walicode")
                .join("ai_settings.json");
            let check_path = if standard_path.exists() {
                &standard_path
            } else {
                config_path
            };
            if !check_path.exists() {
                return false;
            }
            let content = match fs::read_to_string(check_path) {
                Ok(c) => c,
                Err(_) => return false,
            };
            let v: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => return false,
            };
            // 检查 customProviders 中有 waliapi 且已激活
            let has_provider = v
                .get("customProviders")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some("waliapi"))
                })
                .is_some();
            let is_active =
                v.get("activeCustomProviderId").and_then(|v| v.as_str()) == Some("waliapi");
            has_provider && is_active
        }
        _ => false,
    }
}

// ── Tauri Commands ──

#[tauri::command]
pub async fn get_app_configs(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<AppInfo>, String> {
    get_app_configs_impl(state.inner()).await
}

pub async fn get_app_configs_impl(_state: &Arc<AppState>) -> Result<Vec<AppInfo>, String> {
    let apps: Vec<AppInfo> = APPS
        .iter()
        .map(|app| {
            let config_dir = (app.config_dir_fn)();
            let config_path = config_dir.join(app.config_file);
            let available = (app.check_installed_fn)(&config_dir);
            let applied = detect_applied(&config_path, app.name);

            AppInfo {
                name: app.name.to_string(),
                label: app.label.to_string(),
                icon: app.icon.to_string(),
                description: app.description.to_string(),
                config_path: config_path.to_string_lossy().to_string(),
                config_format: app.config_format.to_string(),
                available,
                applied,
                download_url: app.download_url.to_string(),
            }
        })
        .collect();

    Ok(apps)
}

#[tauri::command]
pub async fn apply_app_config(
    app_name: String,
    api_key: String,
    model: String,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<ApplyResult, String> {
    apply_app_config_impl(&app_name, &api_key, &model, state.inner()).await
}

pub async fn apply_app_config_impl(
    app_name: &str,
    api_key: &str,
    model: &str,
    state: &Arc<AppState>,
) -> Result<ApplyResult, String> {
    let waliapi_url = get_waliapi_url(state).await;

    let app_def = APPS
        .iter()
        .find(|a| a.name == app_name)
        .ok_or_else(|| format!("不支持的应用: {app_name}"))?;

    let config_dir = (app_def.config_dir_fn)();
    let config_path = config_dir.join(app_def.config_file);

    // 仅在「未应用」状态下备份：避免重复写入时把已被修改的配置当成原始配置覆盖备份，
    // 否则「恢复原配置」会恢复成 waliapi 配置，永远切不回去。
    if app_name == "claude-code" {
        // Claude Code 在事务写入器中完成读取、校验、首次备份和原子替换；
        // 外层不能提前创建备份，否则冲突失败会误消费原始备份。
    } else if detect_applied(&config_path, app_name) {
        // 已处于网关配置状态，保留最初的备份，直接重写即可
    } else if config_path.exists() {
        let _ = backup_config(&config_path);
        let _ = fs::remove_file(absent_marker_path(&config_path));
    } else {
        // 原配置不存在：打标记，恢复时删除写入的配置
        let _ = atomic_write(&absent_marker_path(&config_path), b"");
        let _ = fs::remove_file(backup_path(&config_path));
    }

    let result = match app_name {
        "claude-code" => {
            write_claude_code_transactional(&config_dir, &waliapi_url, &api_key, &model)
        }
        "codex" => write_codex(&config_dir, &waliapi_url, &api_key, &model),
        "gemini-cli" => write_gemini_cli(&config_dir, &waliapi_url, &api_key, &model),
        "claude-desktop" => write_claude_desktop(&config_dir, &waliapi_url, &api_key, &model),
        "opencode" => write_opencode(&config_dir, &waliapi_url, &api_key, &model),
        "openclaw" => write_openclaw(&config_dir, &waliapi_url, &api_key, &model),
        "hermes" => write_hermes(&config_dir, &waliapi_url, &api_key, &model),
        "walicode" => write_walicode(&config_dir, &waliapi_url, &api_key, &model),
        _ => return Err(format!("不支持的应用: {app_name}")),
    };

    match result {
        Ok(()) => {
            let msg = if app_name == "claude-code" {
                format!(
                    "WaLiAPI 网关配置已写入 {}。Claude Code 的 API Key 网关不要求 Anthropic 账户登录或执行 /login，请重启 Claude Code 生效",
                    config_path.display()
                )
            } else if app_name == "walicode" {
                format!(
                    "配置已写入。请重启 WaLiCode 使配置生效（WaLiCode 会使用本地缓存覆盖旧配置）"
                )
            } else {
                format!("配置已写入 {}", config_path.display())
            };
            Ok(ApplyResult {
                success: true,
                message: msg,
                auth_warning: None,
            })
        }
        Err(e) => {
            if app_name != "claude-code" {
                let _ = restore_config(&config_path);
            }
            Ok(ApplyResult {
                success: false,
                message: e,
                auth_warning: None,
            })
        }
    }
}

#[tauri::command]
pub async fn clear_app_config(app_name: String) -> Result<ApplyResult, String> {
    clear_app_config_impl(&app_name).await
}

pub async fn clear_app_config_impl(app_name: &str) -> Result<ApplyResult, String> {
    let app_def = APPS
        .iter()
        .find(|a| a.name == app_name)
        .ok_or_else(|| format!("不支持的应用: {app_name}"))?;

    let config_dir = (app_def.config_dir_fn)();
    let config_path = config_dir.join(app_def.config_file);

    match restore_config(&config_path) {
        Ok(()) => {
            let mut auth_warning = None;
            let message = if app_name == "codex" {
                // auth.json 若被其他工具改成 API Key 模式，仅恢复 config.toml 无法回到 ChatGPT 账号
                auth_warning = detect_codex_apikey_mode(&config_dir);
                format!(
                    "已恢复 {} 的原始配置，重启 Codex 后将使用原账号（auth.json 未被改动）",
                    app_def.label
                )
            } else {
                format!("已恢复 {} 的原始配置", app_def.label)
            };
            Ok(ApplyResult {
                success: true,
                message,
                auth_warning,
            })
        }
        Err(e) => Ok(ApplyResult {
            success: false,
            message: format!("恢复失败: {e}"),
            auth_warning: None,
        }),
    }
}

#[tauri::command]
pub async fn get_app_config_content(app_name: String) -> Result<ConfigContent, String> {
    get_app_config_content_impl(&app_name).await
}

pub async fn get_app_config_content_impl(app_name: &str) -> Result<ConfigContent, String> {
    let app_def = APPS
        .iter()
        .find(|a| a.name == app_name)
        .ok_or_else(|| format!("不支持的应用: {app_name}"))?;

    let config_dir = (app_def.config_dir_fn)();
    let config_path = config_dir.join(app_def.config_file);

    // WaLiCode 特殊处理：优先读标准路径 ~/.config/walicode/ai_settings.json
    let config_path = if app_name == "walicode" {
        let standard_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("walicode")
            .join("ai_settings.json");
        if standard_path.exists() {
            standard_path
        } else {
            config_path
        }
    } else {
        config_path
    };

    if !config_path.exists() {
        return Ok(ConfigContent {
            exists: false,
            content: String::new(),
            error: None,
        });
    }

    match fs::read_to_string(&config_path) {
        Ok(content) => Ok(ConfigContent {
            exists: true,
            content,
            error: None,
        }),
        Err(e) => Ok(ConfigContent {
            exists: true,
            content: String::new(),
            error: Some(format!("读取失败: {e}")),
        }),
    }
}

#[tauri::command]
pub async fn open_config_folder(app_name: String) -> Result<(), String> {
    let config_dir = prepare_app_config_path_impl(&app_name).await?;

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&config_dir)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {e}"))?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&config_dir)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {e}"))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&config_dir)
            .spawn()
            .map_err(|e| format!("打开文件夹失败: {e}"))?;
    }

    Ok(())
}

/// Prepare and return the server-side configuration directory. Browser clients
/// cannot open a native file manager on a remote Linux host, so the Web bridge
/// returns this path while still preserving the desktop command's behavior.
pub async fn prepare_app_config_path_impl(app_name: &str) -> Result<String, String> {
    let app_def = APPS
        .iter()
        .find(|a| a.name == app_name)
        .ok_or_else(|| format!("不支持的应用: {app_name}"))?;

    let config_dir = (app_def.config_dir_fn)();

    // 如果目录不存在，尝试创建
    if !config_dir.exists() {
        fs::create_dir_all(&config_dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }

    // 如果配置文件不存在，先创建一个空文件
    let config_path = config_dir.join(app_def.config_file);
    if !config_path.exists() {
        atomic_write(&config_path, b"{}")?;
    }

    Ok(config_dir.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "waliapi-appcfg-{}-{}",
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn restore_with_backup_recovers_original_and_removes_backup() {
        let dir = temp_dir("backup");
        let config = dir.join("config.toml");
        fs::write(&config, b"original").unwrap();
        backup_config(&config).unwrap();
        // 模拟网关写入覆盖
        fs::write(&config, b"model_provider = \"waliapi\"").unwrap();

        restore_config(&config).unwrap();
        assert_eq!(fs::read(&config).unwrap(), b"original");
        assert!(!backup_path(&config).exists());
    }

    #[test]
    fn restore_with_absent_marker_deletes_written_config() {
        let dir = temp_dir("absent");
        let config = dir.join("config.toml");
        // 写入前配置不存在：打 absent 标记
        atomic_write(&absent_marker_path(&config), b"").unwrap();
        // 模拟网关写入
        fs::write(&config, b"model_provider = \"waliapi\"").unwrap();

        restore_config(&config).unwrap();
        assert!(!config.exists());
        assert!(!absent_marker_path(&config).exists());
    }

    #[test]
    fn restore_without_backup_or_marker_errors() {
        let dir = temp_dir("none");
        let config = dir.join("config.toml");
        assert!(restore_config(&config).is_err());
    }

    #[test]
    fn detect_apikey_mode_flags_key_without_tokens() {
        let dir = temp_dir("detect-key");
        fs::write(
            dir.join("auth.json"),
            r#"{"OPENAI_API_KEY": "sk-waliapi", "tokens": null}"#,
        )
        .unwrap();
        assert!(detect_codex_apikey_mode(&dir).is_some());
    }

    #[test]
    fn detect_apikey_mode_flags_auth_mode_field() {
        let dir = temp_dir("detect-mode");
        fs::write(
            dir.join("auth.json"),
            r#"{"auth_mode": "apikey", "OPENAI_API_KEY": null}"#,
        )
        .unwrap();
        assert!(detect_codex_apikey_mode(&dir).is_some());
    }

    #[test]
    fn detect_apikey_mode_ignores_chatgpt_login() {
        let dir = temp_dir("detect-chatgpt");
        fs::write(
            dir.join("auth.json"),
            r#"{"OPENAI_API_KEY": null, "tokens": {"id_token": "x", "access_token": "y"}, "last_refresh": "2026-01-01"}"#,
        )
        .unwrap();
        assert!(detect_codex_apikey_mode(&dir).is_none());
    }

    #[test]
    fn detect_apikey_mode_ignores_missing_or_invalid_auth_json() {
        let dir = temp_dir("detect-missing");
        assert!(detect_codex_apikey_mode(&dir).is_none());

        fs::write(dir.join("auth.json"), "not-json").unwrap();
        assert!(detect_codex_apikey_mode(&dir).is_none());
    }

    #[test]
    fn reset_codex_auth_backs_up_and_resets_to_chatgpt_mode() {
        let dir = temp_dir("reset");
        let auth_path = dir.join("auth.json");
        fs::write(&auth_path, r#"{"OPENAI_API_KEY": "sk-waliapi"}"#).unwrap();

        let result = reset_codex_auth_in(&dir).unwrap();
        assert!(result.success);

        let reset: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&auth_path).unwrap()).unwrap();
        assert_eq!(reset["auth_mode"], "chatgpt");
        assert!(reset["OPENAI_API_KEY"].is_null());

        let backup: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.join("auth.json.waliapi-backup")).unwrap(),
        )
        .unwrap();
        assert_eq!(backup["OPENAI_API_KEY"], "sk-waliapi");
    }

    #[test]
    fn reset_codex_auth_without_auth_json_fails_gracefully() {
        let dir = temp_dir("reset-missing");
        let result = reset_codex_auth_in(&dir).unwrap();
        assert!(!result.success);
        assert!(result.message.contains("codex login"));
    }

    #[test]
    fn claude_code_projection_preserves_user_fields_and_merges_env() {
        let mut settings = serde_json::json!({
            "permissions": {"allow": ["Bash"]},
            "env": {"CUSTOM_PROXY": "http://proxy", "ANTHROPIC_MODEL": "old"},
            "modelPicker": {"options": [{"model": "user-model", "label": "User model"}]}
        });

        apply_waliapi_claude_code_settings(
            &mut settings,
            "http://127.0.0.1:8777///",
            "sk-waliapi-test",
            "gpt-5.6-luna[1m]",
        )
        .unwrap();

        assert_eq!(settings["permissions"]["allow"][0], "Bash");
        assert_eq!(settings["env"]["CUSTOM_PROXY"], "http://proxy");
        assert_eq!(
            settings["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:8777"
        );
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-waliapi-test");
        assert!(settings["env"]["ANTHROPIC_API_KEY"].is_null());
        assert_eq!(settings["model"], "gpt-5.6-luna[1m]");
        assert_eq!(settings["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "372000");
        assert_eq!(settings["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"], "360000");
        assert_eq!(settings["modelPicker"]["options"][0]["model"], "user-model");
    }

    #[test]
    fn claude_code_projection_respects_user_context_and_known_claude_model() {
        let mut settings = serde_json::json!({
            "env": {
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "123456",
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "120000"
            },
            "modelPicker": {"options": [{"model": "user-model"}]}
        });

        apply_waliapi_claude_code_settings(
            &mut settings,
            "http://gateway/",
            "key",
            "claude-sonnet-4-6",
        )
        .unwrap();

        assert_eq!(settings["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "123456");
        assert_eq!(settings["env"]["CLAUDE_CODE_AUTO_COMPACT_WINDOW"], "120000");
        assert_eq!(
            settings["modelPicker"]["options"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn claude_code_unknown_model_does_not_guess_context_or_capabilities() {
        let mut settings = serde_json::json!({"env": {}});

        apply_waliapi_claude_code_settings(
            &mut settings,
            "http://gateway",
            "key",
            "vendor-private-model",
        )
        .unwrap();

        assert!(settings["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"].is_null());
        assert!(settings.get("modelPicker").is_none());
        assert_eq!(
            settings[WALIAPI_CLAUDE_SETTINGS_META]["modelCompatibility"]["confidence"],
            "unknown"
        );
    }

    #[test]
    fn claude_code_projection_rejects_unowned_auth_without_mutation() {
        let mut settings = serde_json::json!({
            "env": {"ANTHROPIC_AUTH_TOKEN": "user-secret", "CUSTOM": "keep"}
        });
        let original = settings.clone();

        let error = apply_waliapi_claude_code_settings(
            &mut settings,
            "http://gateway",
            "key",
            "custom-model",
        )
        .unwrap_err();

        assert!(error.contains("ANTHROPIC_AUTH_TOKEN"));
        assert_eq!(settings, original);
    }

    #[test]
    fn claude_code_legacy_api_key_is_migrated_only_when_matching_selected_key() {
        let mut settings = serde_json::json!({
            "_waliapi": true,
            "env": {"ANTHROPIC_API_KEY": "key"},
            "modelPicker": {"options": [{"model": "user-model"}]}
        });
        apply_waliapi_claude_code_settings(&mut settings, "http://gateway", "key", "model").unwrap();
        assert_eq!(settings["env"]["ANTHROPIC_AUTH_TOKEN"], "key");
        assert!(settings["env"]["ANTHROPIC_API_KEY"].is_null());
        assert_eq!(settings["model"], "model");
        assert_eq!(settings[WALIAPI_CLAUDE_SETTINGS_META]["version"], 3);
    }

    #[test]
    fn claude_code_legacy_unknown_api_key_is_rejected_without_mutation() {
        let mut settings = serde_json::json!({
            "_waliapi": true,
            "env": {"ANTHROPIC_API_KEY": "user-secret"}
        });
        let original = settings.clone();
        assert!(apply_waliapi_claude_code_settings(&mut settings, "http://gateway", "key", "model").is_err());
        assert_eq!(settings, original);
    }

    #[test]
    fn claude_code_user_rewrite_of_managed_api_key_is_rejected() {
        let mut settings = serde_json::json!({
            "env": {"ANTHROPIC_API_KEY": "user-edited"},
            WALIAPI_CLAUDE_SETTINGS_META: {
                "version": 3,
                "managedEnvKeys": ["ANTHROPIC_API_KEY"],
                "managedAuthFingerprint": secret_fingerprint("old-key")
            }
        });
        let original = settings.clone();
        assert!(apply_waliapi_claude_code_settings(&mut settings, "http://gateway", "new-key", "model").is_err());
        assert_eq!(settings, original);
    }

    #[test]
    fn claude_code_marker_alone_is_not_detected_as_applied() {
        let dir = temp_dir("claude-marker-only");
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"_waliapi":true}"#).unwrap();
        assert!(!detect_applied(&path, "claude-code"));
    }

    #[test]
    fn claude_code_transaction_preserves_original_backup_across_repeated_apply_and_restore() {
        let dir = temp_dir("claude-transaction");
        let path = dir.join("settings.json");
        let original = br#"{"env":{"CUSTOM":"keep"},"permissions":{"allow":["Bash"]}}"#;
        fs::write(&path, original).unwrap();

        write_claude_code_transactional(&dir, "http://gateway/", "key-1", "custom-model").unwrap();
        let backup = backup_path(&path);
        assert_eq!(fs::read(&backup).unwrap(), original);

        write_claude_code_transactional(&dir, "http://gateway/", "key-2", "claude-opus-4-6")
            .unwrap();
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap()["env"]
                ["ANTHROPIC_AUTH_TOKEN"],
            "key-2"
        );

        restore_config(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn claude_code_transaction_rejects_invalid_json_without_creating_backup() {
        let dir = temp_dir("claude-invalid");
        let path = dir.join("settings.json");
        fs::write(&path, b"not-json").unwrap();

        assert!(write_claude_code_transactional(&dir, "http://gateway", "key", "model").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not-json");
        assert!(!backup_path(&path).exists());
    }
}
