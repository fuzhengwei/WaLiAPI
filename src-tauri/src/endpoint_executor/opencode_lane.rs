//! OpenCode 免费车道适配（openai:opencode 预设专用）。
//!
//! 上游 `https://opencode.ai/inference/v1/*` 不是普通的 OpenAI 兼容端点：
//! 网关对每个请求做 **v2 客户端指纹门禁**，任一条不满足即 403 FreeTierError。
//! 本模块把 WaLiAPI 的通用转发请求就地整形为门禁接受的形状。
//! 事实来源：dsh-our-free-model `src/upstream.js`（v2 形状，2026-10-16/10-17 逐项实测），
//! 会话 id 编码参考 https://github.com/jasonxu114514/opencode2api。
//!
//! 门禁规则（每条都对应一次 403）：
//! 1. 请求头：`Authorization: Bearer public`（公开池化凭据）+ 固定 User-Agent +
//!    `x-opencode-client: cli` + `x-opencode-project: global`，且
//!    `x-opencode-session` / `x-opencode-session-id` / `x-session-affinity` /
//!    `x-session-id` 四个同值会话头缺一不可；**不得**携带 v1 专用的
//!    `x-opencode-request` 头。
//! 2. 会话 id：形如 `ses_<12hex><14base62>`，且 12 个 hex 必须是
//!    `~(ts_ms << 12 | counter)` 的降序编码（低 12 位落在 `0xfc0–0xfff`）。
//!    额度按会话统计——每个请求新铸会话会立刻耗尽免费额度（429）。
//! 3. 工具指纹：`read` 与 `shell` 两个工具必须**声明**，且 description 与
//!    parameters 必须与官方 v2.0.26 CLI 逐字一致（改写/精简/空 properties 均 403）；
//!    调用方同名但定义不同的工具改名为 `<name>_client[N]` 并同步历史与 tool_choice；
//!    **绝不下发 `tool_choice: 'none'`**。
//!
//! 已知边界：工具改名后的**流式**响应侧还原未做（流式调用改名工具时下游看到的
//! 仍是 `read_client` 这样的别名）；非流式响应已还原。调用方工具极少恰好用
//! 小写 `read`/`shell` 这两个名字（官方客户端用 `Read`/`Bash`，不冲突）。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use futures_util::StreamExt;

/// 门禁指纹的客户端 User-Agent：整串匹配，任何前缀/后缀都会失效。
pub const CLIENT_UA: &str = "opencode/latest/2.0.26/cli";

/// 渠道保留的会话池大小：轮换使用，避免单会话额度集中耗尽。
const SESSION_POOL_SIZE: usize = 8;

/// 发送名 → 调用方原名（工具改名映射，用于响应侧还原）。
pub type AliasMap = BTreeMap<String, String>;

// ---------------------------------------------------------------------------
// 会话 id 铸造
// ---------------------------------------------------------------------------

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static LAST_STAMP: Mutex<u128> = Mutex::new(0);
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 渲染 `bytes` 为恰好 `width` 个 base62 字符（大端，逐字节取模，非严格大数）。
fn base62_from(bytes: &[u8]) -> String {
    bytes.iter().map(|b| BASE62[(*b % 62) as usize] as char).collect()
}

/// 铸造一个门禁形状的规范会话 id：`~(ts_ms * 0x1000 + counter)` 的 48 位取反
/// 编码（低 12 位落在 `0xfff - counter`，counter 1–63 在窗口内），毫秒内计数器
/// 复位保证突发铸造不越界。
pub fn mint_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut last = LAST_STAMP.lock().expect("session stamp poisoned");
    if *last != ts {
        *last = ts;
        SEQ.store(0, Ordering::Relaxed);
    }
    let seq = (SEQ.fetch_add(1, Ordering::Relaxed) % 63) + 1;
    drop(last);
    // 48 位：ts_ms * 0x1000 + counter，再按位取反（掩到 48 位）。
    let value = !((ts << 12) | seq as u128) & 0xffff_ffff_ffff;
    let mut hex = String::with_capacity(12);
    for i in 0..6 {
        let byte = ((value >> (40 - 8 * i)) & 0xff) as u8;
        hex.push_str(&format!("{byte:02x}"));
    }
    let mut random = [0u8; 14];
    use rand::RngCore;
    rand::rng().fill_bytes(&mut random);
    format!("ses_{hex}{}", base62_from(&random))
}

/// 渠道会话池：每个渠道惰性铸造一池会话，请求间轮换。
/// 额度按会话统计，因此同一渠道的请求共享一小池稳定会话而不是每请求新铸。
fn next_session(channel_id: &str) -> String {
    static POOLS: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    static CURSOR: AtomicUsize = AtomicUsize::new(0);
    let mut pools = POOLS.lock().expect("session pool poisoned");
    let pool = pools.entry(channel_id.to_string()).or_insert_with(|| {
        (0..SESSION_POOL_SIZE).map(|_| mint_session_id()).collect()
    });
    let idx = CURSOR.fetch_add(1, Ordering::Relaxed) % pool.len();
    pool[idx].clone()
}

// ---------------------------------------------------------------------------
// 请求头
// ---------------------------------------------------------------------------

/// 门禁要求的完整请求头集合（`stream` 决定 accept）。
pub fn gateway_headers(channel_id: &str, stream: bool) -> Vec<(String, String)> {
    let session = next_session(channel_id);
    vec![
        ("authorization".to_string(), "Bearer public".to_string()),
        ("user-agent".to_string(), CLIENT_UA.to_string()),
        ("x-opencode-client".to_string(), "cli".to_string()),
        ("x-opencode-session".to_string(), session.clone()),
        ("x-opencode-session-id".to_string(), session.clone()),
        ("x-session-affinity".to_string(), session.clone()),
        ("x-session-id".to_string(), session),
        ("x-opencode-project".to_string(), "global".to_string()),
        (
            "accept".to_string(),
            if stream { "text/event-stream".to_string() } else { "*/*".to_string() },
        ),
    ]
}

/// 模型列表同步（`GET /models`）同样要过指纹门禁。
pub fn listing_headers() -> Vec<(String, String)> {
    let session = next_session("listing");
    vec![
        ("authorization".to_string(), "Bearer public".to_string()),
        ("user-agent".to_string(), CLIENT_UA.to_string()),
        ("x-opencode-client".to_string(), "cli".to_string()),
        ("x-opencode-session".to_string(), session.clone()),
        ("x-opencode-session-id".to_string(), session.clone()),
        ("x-session-affinity".to_string(), session.clone()),
        ("x-session-id".to_string(), session),
        ("x-opencode-project".to_string(), "global".to_string()),
        ("accept".to_string(), "application/json".to_string()),
    ]
}

// ---------------------------------------------------------------------------
// 工具指纹（请求侧）
// ---------------------------------------------------------------------------

/// 门禁要求的两份官方 v2.0.26 CLI 工具定义（逐字，不得改写措辞或精简 schema）。
fn fingerprint_tool(name: &str) -> Value {
    let function = if name == "read" {
        json!({
            "description": "Read the contents of a file or directory. Supports text files, images, and PDFs. Images and PDFs are presented directly to the model. Each text line is prefixed by its 1-based line number as <line>: <content>. The prefix is for reference and is not part of the file content. Directory entries are returned one per line. Use offset and limit to read large files or directories in sections. Prefer one larger read over many small slices, and use grep to find specific content in large files.",
            "name": "read",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File or directory to read" },
                    "offset": { "type": "integer", "minimum": 0, "description": "The line or directory entry to start reading from (1-based)" },
                    "limit": { "type": "integer", "minimum": 0, "description": "The maximum number of lines or directory entries to read (defaults to and capped at 2000)" },
                },
                "required": ["path"],
                "additionalProperties": false,
            },
        })
    } else {
        json!({
            "description": "Execute a shell command and return its output. Commands run on Linux using bash. Quote file paths containing spaces or special characters. Prefer dedicated tools over shell commands when possible. When output is large, the full result is saved to a file and a truncated preview is returned. Rely on automatic truncation unless filtering the output is more useful. Commands accept an optional timeout, background commands have no timeout by default. Background commands return immediately, and you will be notified when they complete.",
            "name": "shell",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command string to execute" },
                    "workdir": { "type": "string", "description": "Working directory to execute the command in. Defaults to the current working directory. When possible, avoid changing directories in the command and set the working directory here instead." },
                    "timeout": { "type": "integer", "minimum": 0, "description": "Timeout in milliseconds. Set to 0 to disable the timeout. Defaults to 120000 for foreground commands. Background commands have no timeout by default." },
                    "background": { "type": "boolean", "description": "Run the command in the background and return immediately (useful for dev servers and long-running builds). You do not need to use '&' at the end of the command when using this parameter. You will be notified when it completes. DO NOT poll for completion." },
                },
                "required": ["command"],
                "additionalProperties": false,
            },
        })
    };
    json!({ "type": "function", "function": function })
}

/// 工具在 Chat Completions 线协议里的名字（`tool.function.name` 或裸 `tool.name`）。
fn tool_name_of(tool: &Value) -> &str {
    tool.get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .or_else(|| tool.get("name").and_then(Value::as_str))
        .unwrap_or("")
}

/// 工具是否与官方定义逐字一致（name + description + parameters）。
fn carries_fingerprint_definition(tool: &Value, name: &str) -> bool {
    let official = fingerprint_tool(name);
    let Some(function) = tool.get("function").and_then(Value::as_object) else {
        return false;
    };
    let Some(expected) = official.get("function").and_then(Value::as_object) else {
        return false;
    };
    for key in ["name", "description", "parameters"] {
        if function.get(key) != expected.get(key) {
            return false;
        }
    }
    true
}

/// 注入工具时附带的占位提示（压住模型对注入项的调用，不误伤调用方自己的工具）。
fn placeholder_note(names: &[&str]) -> String {
    let list = names.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(" or ");
    if names.len() > 1 {
        format!("Do not call the {list} tools; they are placeholders that do nothing. Use the other tools you have been given instead.")
    } else {
        format!("Do not call the {list} tool; it is a placeholder that does nothing. Use the other tools you have been given instead.")
    }
}

/// 把占位提示并入首条 system 消息；没有 system 消息时在最前插入一条。
fn note_injected_tools(body: &mut Value, note: &str) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(first) = messages.first_mut() {
        if first.get("role").and_then(Value::as_str) == Some("system") {
            if let Some(content) = first.get_mut("content") {
                if let Some(text) = content.as_str() {
                    *content = Value::String(format!("{text}\n\n{note}"));
                    return;
                }
            }
        }
    }
    let mut messages = std::mem::take(messages);
    messages.insert(0, json!({ "role": "system", "content": note }));
    body["messages"] = Value::Array(messages);
}

/// 把历史里的工具调用名同步改为别名（否则模型会把早前的原名调用读成注入项）。
/// `map` 是 发送名(别名) → 调用方原名，因此按值反查别名。
fn rename_history_tool_calls(messages: &mut [Value], map: &AliasMap) {
    for message in messages.iter_mut() {
        let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
            continue;
        };
        for call in calls.iter_mut() {
            let name = call
                .get_mut("function")
                .and_then(|f| f.get_mut("name"))
                .and_then(|v| v.as_str().map(str::to_string));
            if let Some(name) = name {
                if let Some((alias, _)) = map.iter().find(|(_, original)| *original == &name) {
                    call["function"]["name"] = Value::String(alias.clone());
                }
            }
        }
    }
}

/// 满足 v2 免费车道工具指纹门禁（就地改写请求体）。
///
/// 返回 改名映射（发送名 → 调用方原名），供响应侧还原；空映射表示无改名。
pub fn apply_fingerprint(body: &mut Value) -> AliasMap {
    let mut map = AliasMap::new();
    let taken: Vec<String> = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.iter().map(tool_name_of).filter(|n| !n.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();

    // 先改写调用方同名但定义不同的工具，再注入缺失的门禁定义。
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut present: Vec<String> = Vec::new();
    for tool in body.get("tools").and_then(Value::as_array).cloned().unwrap_or_default() {
        let name = tool_name_of(&tool).to_string();
        if name != "read" && name != "shell" {
            out.push(tool);
            continue;
        }
        if seen.contains(&name) {
            continue; // 同名重复声明：上游按重复拒，留第一个。
        }
        seen.push(name.clone());
        if carries_fingerprint_definition(&tool, &name) {
            present.push(name);
            out.push(tool);
            continue;
        }
        let mut alias = format!("{name}_client");
        let mut n = 2;
        while taken.contains(&alias) {
            alias = format!("{name}_client{n}");
            n += 1;
        }
        map.insert(alias.clone(), name);
        let mut renamed = tool;
        renamed["function"]["name"] = Value::String(alias);
        out.push(renamed);
    }
    let injected: Vec<&str> = ["read", "shell"]
        .into_iter()
        .filter(|name| !present.iter().any(|p| p == name))
        .collect();
    for name in &injected {
        out.push(fingerprint_tool(name));
    }
    if !out.is_empty() {
        body["tools"] = Value::Array(out);
    }

    // 改名同步到历史 tool_calls。
    if !map.is_empty() {
        if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
            rename_history_tool_calls(messages, &map);
        }
    }
    // 绝不下发 `tool_choice: 'none'`（v2 见到即 403）。
    let choice_none = matches!(body.get("tool_choice"), Some(Value::String(c)) if c == "none")
        || body
            .get("tool_choice")
            .and_then(Value::as_object)
            .map(|o| o.get("type").and_then(Value::as_str) == Some("none"))
            .unwrap_or(false);
    if choice_none {
        if let Some(object) = body.as_object_mut() {
            object.remove("tool_choice");
        }
    }
    if !injected.is_empty() {
        let note = placeholder_note(&injected);
        note_injected_tools(body, &note);
    }
    map
}

// ---------------------------------------------------------------------------
// 工具名还原（响应侧）
// ---------------------------------------------------------------------------

/// 在一个 JSON 值里递归还原所有 `tool_calls[].function.name` / delta 变体。
fn restore_names_in(value: &mut Value, map: &AliasMap) {
    match value {
        Value::Object(object) => {
            restore_names_in_object(object, map);
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                restore_names_in(item, map);
            }
        }
        _ => {}
    }
}

fn restore_names_in_object(object: &mut Map<String, Value>, map: &AliasMap) {
    // message.tool_calls[] / delta.tool_calls[] 共用这一形状。
    if let Some(calls) = object.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for call in calls.iter_mut() {
            if let Some(name) = call
                .get_mut("function")
                .and_then(|f| f.get_mut("name"))
                .and_then(|v| v.as_str().map(str::to_string))
            {
                if let Some(original) = map.get(&name) {
                    call["function"]["name"] = Value::String(original.clone());
                }
            }
        }
    }
    for (_key, child) in object.iter_mut() {
        restore_names_in(child, map);
    }
}

/// 非流式：还原响应体里的工具名。
pub fn restore_response_body(body: &mut Value, map: &AliasMap) {
    if map.is_empty() {
        return;
    }
    restore_names_in(body, map);
}

/// 流式：按 SSE record 逐个还原工具名（同 grok_arguments::rewrite_stream 的机制）。
pub fn rewrite_stream(
    mut inner: futures_util::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
    map: AliasMap,
) -> futures_util::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>> {
    async_stream::stream! {
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(item) = inner.next().await {
            match item {
                Ok(chunk) => {
                    buffer.extend_from_slice(&chunk);
                    if crate::protocol::codec::sse::pending_exceeded(&buffer) {
                        yield Err(std::io::Error::other(
                            crate::protocol::codec::sse::pending_overflow_message(),
                        ));
                        return;
                    }
                    let mut out: Vec<u8> = Vec::new();
                    while let Some(end) = crate::protocol::codec::sse::record_end(&buffer) {
                        let record: Vec<u8> = buffer.drain(..end).collect();
                        out.extend_from_slice(&rewrite_record(&record, &map));
                    }
                    if !out.is_empty() {
                        yield Ok(bytes::Bytes::from(out));
                    }
                }
                Err(error) => {
                    yield Err(error);
                    return;
                }
            }
        }
        if !buffer.is_empty() {
            yield Ok(bytes::Bytes::from(rewrite_record(&buffer, &map)));
        }
    }
    .boxed()
}

/// 还原单个 SSE record：只改写承载工具名的 `data:` 行，其余原样保留。
/// 按行处理并保留原始行尾（上游可能是 CRLF，丢掉 `\r` 会破坏 record 边界）。
fn rewrite_record(record: &[u8], map: &AliasMap) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(record) else {
        return record.to_vec();
    };
    let mut rewritten_text = String::with_capacity(text.len());
    let mut touched = false;
    for line in text.split_inclusive('\n') {
        if let Some(payload) = line.trim_end_matches(['\n', '\r']).strip_prefix("data:") {
            if let Some(rewritten) = restore_event_payload(payload.trim_start(), map) {
                rewritten_text.push_str("data: ");
                rewritten_text.push_str(&rewritten);
                if line.ends_with("\r\n") {
                    rewritten_text.push_str("\r\n");
                } else if line.ends_with('\n') {
                    rewritten_text.push('\n');
                }
                touched = true;
                continue;
            }
        }
        rewritten_text.push_str(line);
    }
    if touched {
        rewritten_text.into_bytes()
    } else {
        record.to_vec()
    }
}

/// 还原一个 `data:` JSON 帧里的工具名；未改变时返回 `None`（保持原字节）。
fn restore_event_payload(payload: &str, map: &AliasMap) -> Option<String> {
    if map.is_empty() || payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let mut event: Value = serde_json::from_str(payload).ok()?;
    let snapshot = event.clone();
    restore_names_in(&mut event, map);
    if event == snapshot {
        return None;
    }
    serde_json::to_string(&event).ok()
}

// ---------------------------------------------------------------------------
// 上游流式 → 非流式聚合
// ---------------------------------------------------------------------------

/// 把上游强制流式返回的 SSE 全量字节聚合成单个 `chat.completion` JSON。
///
/// 背景（2026-10-22 实测）：v2 免费车道门禁要求请求**必须** `stream:true`
/// 且携带指纹工具，否则一律 403 FreeTierError —— 即使下游要的是非流式。
/// 因此非流式请求以上游流式完成，响应在这里回聚合成标准非流式体：
/// - 顶层 `id`/`model` 取首帧，`usage` 取末个非空帧；
/// - `choices` 按 `index` 合并，`finish_reason` 取末个非空；
/// - `delta.content`/`reasoning*` 逐帧拼接；`tool_calls` 按 `index` 组装
///   （`id`/`name` 首帧生效，`arguments` 逐帧拼接）。
///
/// 解析不出任何完整帧时返回 `None`（交由调用方按解码失败处理）。
pub fn aggregate_stream_body(bytes: &[u8]) -> Option<Value> {
    let mut rest = bytes;
    let mut base: Option<Value> = None;
    while !rest.is_empty() {
        let end = crate::protocol::codec::sse::record_end(rest)?;
        let record = &rest[..end];
        rest = &rest[end..];
        let payload = match crate::endpoint_executor::sse::parse_data_payload(record) {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        let payload = payload.trim();
        if payload.is_empty() {
            continue;
        }
        if payload == "[DONE]" {
            break;
        }
        let Ok(frame) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        let base = base.get_or_insert_with(|| json!({
            "object": "chat.completion",
            "created": 0,
            "choices": [],
        }));
        merge_stream_frame(base, frame);
    }
    base
}

/// 把一个上游 chunk 合并进聚合体（就地）。
fn merge_stream_frame(base: &mut Value, frame: Value) {
    let Some(base_obj) = base.as_object_mut() else {
        return;
    };
    for key in ["id", "model", "system_fingerprint", "service_tier"] {
        if let Some(v) = frame.get(key) {
            base_obj.entry(key).or_insert(v.clone());
        }
    }
    if let Some(created) = frame.get("created").and_then(Value::as_i64) {
        if created > 0 {
            base_obj.insert("created".into(), Value::from(created));
        }
    }
    if let Some(usage) = frame.get("usage") {
        if usage.as_object().is_some_and(|u| !u.is_empty()) {
            base_obj.insert("usage".into(), usage.clone());
        }
    }
    let Some(choices) = frame.get("choices").and_then(Value::as_array) else {
        return;
    };
    let Some(base_choices) = base_obj.get_mut("choices").and_then(Value::as_array_mut) else {
        return;
    };
    for choice in choices {
        let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        while base_choices.len() <= index {
            base_choices.push(json!({ "index": base_choices.len(), "message": {}, "finish_reason": Value::Null }));
        }
        merge_stream_choice(&mut base_choices[index], choice);
    }
}

/// 把一个上游 choice 合并进聚合体的对应 choice。
fn merge_stream_choice(base_choice: &mut Value, choice: &Value) {
    let Some(obj) = base_choice.as_object_mut() else {
        return;
    };
    if let Some(finish) = choice.get("finish_reason") {
        if !finish.is_null() {
            obj.insert("finish_reason".into(), finish.clone());
        }
    }
    // 兼容个别网关把完整 message 放在末帧的写法。
    let delta = choice.get("delta").or_else(|| choice.get("message"));
    let Some(delta) = delta else {
        return;
    };
    let Some(message) = obj.get_mut("message").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(role) = delta.get("role").and_then(Value::as_str) {
        message
            .entry("role".to_string())
            .or_insert(Value::String(role.to_string()));
    }
    append_stream_text(message, "content", delta.get("content"));
    append_stream_text(message, "reasoning_content", delta.get("reasoning_content"));
    append_stream_text(message, "reasoning", delta.get("reasoning"));
    let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
        return;
    };
    let base_calls = message
        .entry("tool_calls".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(base_calls) = base_calls.as_array_mut() else {
        return;
    };
    for call in tool_calls {
        let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        while base_calls.len() <= index {
            base_calls.push(json!({
                "id": "",
                "type": "function",
                "function": { "name": "", "arguments": "" },
            }));
        }
        merge_stream_tool_call(&mut base_calls[index], call);
    }
}

/// 拼接一段流式文本（null/非字符串帧忽略）。
fn append_stream_text(message: &mut serde_json::Map<String, Value>, key: &str, value: Option<&Value>) {
    let Some(text) = value.and_then(Value::as_str) else {
        return;
    };
    match message.get_mut(key) {
        Some(Value::String(existing)) => existing.push_str(text),
        _ => {
            message.insert(key.to_string(), Value::String(text.to_string()));
        }
    }
}

/// 组装一个流式 tool_call（`id`/`name` 首帧生效，`arguments` 拼接）。
fn merge_stream_tool_call(base_call: &mut Value, call: &Value) {
    let Some(obj) = base_call.as_object_mut() else {
        return;
    };
    if let Some(id) = call.get("id").and_then(Value::as_str) {
        if obj.get("id").and_then(Value::as_str).unwrap_or("").is_empty() {
            obj.insert("id".into(), Value::String(id.to_string()));
        }
    }
    if let Some(kind) = call.get("type").and_then(Value::as_str) {
        obj.insert("type".into(), Value::String(kind.to_string()));
    }
    let Some(function) = call.get("function").and_then(Value::as_object) else {
        return;
    };
    let base_function = obj
        .entry("function".to_string())
        .or_insert_with(|| json!({ "name": "", "arguments": "" }));
    let Some(base_function) = base_function.as_object_mut() else {
        return;
    };
    if let Some(name) = function.get("name").and_then(Value::as_str) {
        if base_function.get("name").and_then(Value::as_str).unwrap_or("").is_empty() {
            base_function.insert("name".into(), Value::String(name.to_string()));
        }
    }
    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
        match base_function.get_mut("arguments") {
            Some(Value::String(existing)) => existing.push_str(arguments),
            _ => {
                base_function.insert("arguments".into(), Value::String(arguments.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn minted_session_matches_gate_shape() {
        let id = mint_session_id();
        assert!(id.starts_with("ses_"), "{id}");
        assert_eq!(id.len(), 4 + 12 + 14, "{id}");
        let hex = &id[4..16];
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
        // 低 12 位窗口：第 10 位是 c–f。
        let tenth = hex.as_bytes()[9] as char;
        assert!(('c'..='f').contains(&tenth), "{id}");
        let tail = &id[16..];
        assert!(tail.chars().all(|c| BASE62.iter().any(|b| *b as char == c)), "{id}");
    }

    #[test]
    fn session_pool_rotates_stable_ids() {
        let a = next_session("pool-test-channel");
        let ids: std::collections::HashSet<String> =
            (0..SESSION_POOL_SIZE * 2).map(|_| next_session("pool-test-channel")).collect();
        assert_eq!(ids.len(), SESSION_POOL_SIZE, "池应稳定为 {SESSION_POOL_SIZE} 个会话");
        assert!(ids.contains(&a));
    }

    #[test]
    fn fingerprint_tools_are_injected_and_none_is_stripped() {
        let mut body = json!({
            "model": "deepseek-v4.1-flash-free",
            "messages": [{ "role": "user", "content": "hi" }],
            "tool_choice": "none",
        });
        let map = apply_fingerprint(&mut body);
        assert!(map.is_empty());
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"read") && names.contains(&"shell"));
        assert!(body.get("tool_choice").is_none());
        let first = body["messages"][0]["content"].as_str().unwrap();
        assert!(first.contains("placeholders"));
    }

    #[test]
    fn conflicting_caller_tool_is_renamed_and_history_synced() {
        let mut body = json!({
            "messages": [
                { "role": "user", "content": "ls" },
                { "role": "assistant", "tool_calls": [
                    { "id": "c1", "type": "function",
                      "function": { "name": "read", "arguments": "{\"path\":\"/\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "c1", "content": "..." }
            ],
            "tools": [
                { "type": "function", "function": {
                    "name": "read", "description": "caller's own read",
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } } } } }
            ],
        });
        let map = apply_fingerprint(&mut body);
        assert_eq!(map.len(), 1);
        let (alias, original) = map.iter().next().unwrap();
        assert_eq!(original, "read");
        let tools = body["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&alias.as_str()) && names.contains(&"read") && names.contains(&"shell"));
        assert_eq!(tools.len(), 3);
        // 占位提示插在最前，assistant 消息移到 messages[2]。
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["name"].as_str(),
            Some(alias.as_str())
        );
    }

    #[test]
    fn verbatim_definitions_are_recognized_without_reinjection() {
        let mut body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [fingerprint_tool("read"), fingerprint_tool("shell")],
        });
        // 先跑一遍注入得到官方形状，再跑一遍应不再注入/改名。
        apply_fingerprint(&mut body);
        let snapshot = body.clone();
        let map = apply_fingerprint(&mut body);
        assert!(map.is_empty());
        assert_eq!(body["tools"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["content"], snapshot["messages"][0]["content"]);
    }

    #[test]
    fn non_stream_response_restores_tool_names() {
        let mut map = AliasMap::new();
        map.insert("read_client".to_string(), "read".to_string());
        let mut body = json!({
            "choices": [{ "message": { "role": "assistant", "tool_calls": [
                { "id": "c1", "type": "function",
                  "function": { "name": "read_client", "arguments": "{}" } }
            ]}}]
        });
        restore_response_body(&mut body, &map);
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"].as_str(),
            Some("read")
        );
    }

    #[test]
    fn stream_records_restore_tool_names() {
        let mut map = AliasMap::new();
        map.insert("shell_client".to_string(), "shell".to_string());
        let input = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"shell_client\"}}]}}]}\n\n\
                     data: [DONE]\n\n";
        let mut buffer = input.to_vec();
        let mut out: Vec<u8> = Vec::new();
        while let Some(end) = crate::protocol::codec::sse::record_end(&buffer) {
            let record: Vec<u8> = buffer.drain(..end).collect();
            out.extend_from_slice(&rewrite_record(&record, &map));
        }
        out.extend_from_slice(&buffer);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"name\":\"shell\""), "{text}");
        assert!(text.contains("[DONE]"), "{text}");
    }

    #[test]
    fn non_json_records_pass_through_untouched() {
        let record = b": keep-alive\n\n";
        let out = rewrite_record(record, &AliasMap::new());
        assert_eq!(&out, record);
    }

    #[test]
    fn stream_body_aggregates_into_completion() {
        let sse = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1792628468,\"model\":\"deepseek-v4.1-flash-free\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"delta\":{\"role\":\"assistant\",\"content\":\"he\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1792628468,\"model\":\"deepseek-v4.1-flash-free\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"delta\":{\"content\":\"llo\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1792628468,\"model\":\"deepseek-v4.1-flash-free\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
        );
        let body = aggregate_stream_body(sse.as_bytes()).expect("aggregates");
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["id"], "chatcmpl-1");
        assert_eq!(body["model"], "deepseek-v4.1-flash-free");
        assert_eq!(body["created"], 1792628468);
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert_eq!(body["choices"][0]["message"]["content"], "hello");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["usage"]["total_tokens"], 5);
    }

    #[test]
    fn stream_body_aggregates_tool_calls_by_index() {
        let sse = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"123\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"456\"}}]}},{\"index\":1,\"delta\":{}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let body = aggregate_stream_body(sse.as_bytes()).expect("aggregates");
        let call = &body["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(call["id"], "c1");
        assert_eq!(call["function"]["name"], "read");
        assert_eq!(call["function"]["arguments"], "123456");
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(body["choices"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn stream_body_without_frames_returns_none() {
        assert!(aggregate_stream_body(b"").is_none());
        assert!(aggregate_stream_body(b": keep-alive\n\n").is_none());
    }
}
