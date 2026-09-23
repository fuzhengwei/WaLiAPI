use super::super::error::{FeatureKind, UnsupportedFeatures};
use super::super::report::ConversionContext;
use super::super::request;
use super::message::convert_anthropic_message_to_chat;
use serde_json::{Map, Value};

/// Encode an Anthropic Messages request into an OpenAI Chat Completions
/// request.  `model` is the mapped upstream model decided by the caller.
pub fn encode_messages_to_chat(
    body: &Value,
    model: &str,
) -> Result<(Value, ConversionContext), UnsupportedFeatures> {
    let mut out = Vec::new();
    // Fail-open drops/transforms recorded as JSON pointers for the report.
    let mut normalized: Vec<String> = Vec::new();

    // Top-level fields we can map 1:1 between Messages and Chat.  `top_k` is
    // deliberately absent: OpenAI Chat has no top_k, so it cannot be preserved
    // and must be rejected rather than silently dropped.
    const SUPPORTED_TOP_LEVEL: &[&str] = &[
        "model",
        "messages",
        "max_tokens",
        "temperature",
        "top_p",
        "stop_sequences",
        "stream",
        "stream_options",
        "tools",
        "tool_choice",
        "system",
        "user",
    ];

    // Native Anthropic features with no Chat equivalent are rejected here,
    // before any upstream access.  Anything not in the supported whitelist is
    // rejected with a concrete JSON pointer (never silently dropped), matching
    // the chat_to_messages_v1 top-level scan.  Two classes are exceptions,
    // both fail-open by decision (T13):
    //   - `thinking`/`output_config` are *mapped* to `reasoning_effort` below
    //     (CLIProxyAPI semantics); they are never rejected.
    //   - `metadata` and `container`/`context_management`/
    //     `context_management_config` have no Chat equivalent and are dropped,
    //     recorded on the report's
    //     `normalized` list rather than rejected.
    if let Some(obj) = body.as_object() {
        for (key, value) in obj.iter() {
            if !SUPPORTED_TOP_LEVEL.contains(&key.as_str()) {
                match key.as_str() {
                    "thinking" | "output_config" => {
                        // Mapped to `reasoning_effort` in the assembly section.
                        let _ = value;
                    }
                    "metadata"
                    | "container"
                    | "context_management"
                    | "context_management_config"
                    // Anthropic 的 `safeguards`（分类器上下文声明）在 Chat 里没有
                    // 对应物。Claude Code 2.1.280 在特定条件下会带上它
                    // （`safeguards: [{type, classifier_context}]`），整段拒绝会让
                    // 用户直接无法对话，因此按 fail-open 丢弃并记录。
                    | "safeguards" => {
                        normalized.push(format!("/{key}"));
                        let _ = value;
                    }
                    other => {
                        request::reject(
                            &mut out,
                            FeatureKind::UnsupportedField,
                            format!("/{other}"),
                            format!("Messages field {other:?} has no Chat Completions equivalent"),
                        );
                        let _ = value;
                    }
                }
            }
        }
    }

    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            let mut rejections = out.clone();
            request::reject(
                &mut rejections,
                FeatureKind::UnknownRole,
                "/messages",
                "Messages request requires a messages array",
            );
            UnsupportedFeatures::new(rejections)
        })?;

    // system -> single system message (order preserved, annotations stripped).
    let mut system_text: Option<String> = None;
    if let Some(sys) = body.get("system") {
        match request::anthropic_system_to_chat(sys, "/system", &mut normalized) {
            Ok(text) => {
                if !text.is_empty() {
                    system_text = Some(text);
                }
            }
            Err(e) => out.extend(e.fields),
        }
    }

    let reasoning_effort = anthropic_thinking_to_reasoning_effort(body);
    let require_tool_reasoning_content = reasoning_effort.as_deref().is_some_and(|v| v != "none");

    let mut chat_messages: Vec<Value> = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        let mp = format!("/messages/{i}");
        match convert_anthropic_message_to_chat(
            msg,
            &mp,
            &mut normalized,
            require_tool_reasoning_content,
        ) {
            Ok(mut msgs) => chat_messages.append(&mut msgs),
            Err(e) => out.extend(e.fields),
        }
    }

    if !out.is_empty() {
        return Err(UnsupportedFeatures::new(out));
    }

    let mut chat = Map::new();
    chat.insert("model".to_string(), Value::String(model.to_string()));
    if let Some(sys) = system_text {
        chat.insert(
            "messages".to_string(),
            Value::Array(
                std::iter::once(serde_json::json!({"role": "system", "content": sys}))
                    .chain(chat_messages.into_iter())
                    .collect(),
            ),
        );
    } else {
        chat.insert("messages".to_string(), Value::Array(chat_messages));
    }
    chat.insert(
        "max_tokens".to_string(),
        body.get("max_tokens")
            .and_then(Value::as_u64)
            .map(Value::from)
            .unwrap_or(Value::from(4096u64)),
    );
    chat.insert("stream".to_string(), Value::Bool(stream));
    if let Some(t) = body.get("temperature") {
        chat.insert("temperature".to_string(), t.clone());
    }
    if let Some(t) = body.get("top_p") {
        chat.insert("top_p".to_string(), t.clone());
    }
    if let Some(stop) = body.get("stop_sequences") {
        request::require_string_array(stop, "/stop_sequences", "stop_sequences")?;
        chat.insert("stop".to_string(), stop.clone());
    }
    // tools
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mut chat_tools = Vec::new();
        for (i, tool) in tools.iter().enumerate() {
            let tp = format!("/tools/{i}");
            match convert_anthropic_tool_to_chat(tool, &tp) {
                Ok(t) => chat_tools.push(t),
                Err(e) => out.extend(e.fields),
            }
        }
        if !out.is_empty() {
            return Err(UnsupportedFeatures::new(out));
        }
        if !chat_tools.is_empty() {
            chat.insert("tools".to_string(), Value::Array(chat_tools));
        }
    }
    // tool_choice
    if let Some(tc) = body.get("tool_choice") {
        let v = anthropic_tool_choice_to_chat(tc, "/tool_choice")?;
        chat.insert("tool_choice".to_string(), v);
    }
    if body
        .pointer("/tool_choice/disable_parallel_tool_use")
        .and_then(Value::as_bool)
        == Some(true)
    {
        chat.insert("parallel_tool_calls".to_string(), Value::Bool(false));
    }
    if stream {
        let mut options = body
            .get("stream_options")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !options.is_object() {
            return Err(UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                "/stream_options",
                "stream_options must be an object",
            ));
        }
        options["include_usage"] = Value::Bool(true);
        chat.insert("stream_options".to_string(), options);
    }

    // thinking / output_config -> reasoning_effort (fail-open mapping, CPA
    // semantics).  Only present when the downstream asked for thinking; absent
    // thinking leaves `reasoning_effort` unset so the upstream applies its own
    // default.  The upstream (not us) adjudicates whether the model supports it.
    if let Some(effort) = reasoning_effort {
        chat.insert("reasoning_effort".to_string(), Value::String(effort));
    }

    let request_id = body
        .get("id")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4().simple()));

    let mut context = ConversionContext::new(request_id, model.to_string(), stream);
    context.normalized = normalized;
    Ok((Value::Object(chat), context))
}

/// Map an Anthropic `thinking` config to an OpenAI `reasoning_effort` value,
/// following CLIProxyAPI's `ConvertClaudeRequestToOpenAI`.  Returns `None`
/// when the downstream did not ask for thinking (or asked for an unrecognized
/// type), in which case `reasoning_effort` is left unset.
pub(crate) fn anthropic_thinking_to_reasoning_effort(body: &Value) -> Option<String> {
    let thinking = body.get("thinking")?;
    if !thinking.is_object() {
        return None;
    }
    let ty = thinking.get("type").and_then(Value::as_str)?;
    match ty {
        "enabled" => {
            // budget_tokens present -> ConvertBudgetToLevel; absent -> auto.
            match thinking.get("budget_tokens").and_then(Value::as_i64) {
                Some(budget) => {
                    crate::protocol::thinking::budget_to_level(budget).map(String::from)
                }
                None => Some("auto".to_string()),
            }
        }
        "adaptive" | "auto" => {
            // Explicit output_config.effort passes through (lowercased); else xhigh.
            match body
                .get("output_config")
                .and_then(|oc| oc.get("effort"))
                .and_then(Value::as_str)
            {
                Some(effort) if !effort.trim().is_empty() => {
                    Some(effort.trim().to_ascii_lowercase())
                }
                _ => Some("xhigh".to_string()),
            }
        }
        "disabled" => Some("none".to_string()),
        _ => None,
    }
}

/// Convert an Anthropic tool to the Chat `tools` entry.
fn convert_anthropic_tool_to_chat(
    tool: &Value,
    pointer: &str,
) -> Result<Value, UnsupportedFeatures> {
    let ty = tool.get("type").and_then(Value::as_str).unwrap_or("custom");
    match ty {
        "custom" | "" => {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::MissingToolField,
                        format!("{pointer}/name"),
                        "tool is missing name",
                    )
                })?;
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let input_schema = tool.get("input_schema").ok_or_else(|| {
                UnsupportedFeatures::single(
                    FeatureKind::InvalidToolArguments,
                    format!("{pointer}/input_schema"),
                    format!("tool {name:?} is missing input_schema"),
                )
            })?;
            let parameters = request::anthropic_schema_to_chat_parameters(
                input_schema,
                &format!("{pointer}/input_schema"),
            )?;
            Ok(serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters
                }
            }))
        }
        _ => Err(UnsupportedFeatures::single(
            FeatureKind::BuiltinTool,
            format!("{pointer}/type"),
            format!("Anthropic built-in tool {ty:?} has no Chat equivalent"),
        )),
    }
}

/// Convert an Anthropic tool_choice to a Chat tool_choice.
fn anthropic_tool_choice_to_chat(tc: &Value, pointer: &str) -> Result<Value, UnsupportedFeatures> {
    if let Some(s) = tc.as_str() {
        // Anthropic accepts the bare strings auto/any; OpenAI only accepts
        // auto/none/required, so map (never pass through verbatim).
        return match s {
            "auto" => Ok(Value::String("auto".to_string())),
            "any" => Ok(Value::String("required".to_string())),
            // "tool" as a bare string carries no tool name; reject rather than
            // emit an empty-named Chat tool_choice.
            "tool" => Err(UnsupportedFeatures::single(
                FeatureKind::MissingToolField,
                format!("{pointer}/name"),
                "bare string tool_choice \"tool\" requires an explicit name (use the object form)",
            )),
            other => Err(UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                pointer,
                format!("unsupported tool_choice string {other:?}"),
            )),
        };
    }
    let ty = tc.get("type").and_then(Value::as_str).ok_or_else(|| {
        UnsupportedFeatures::single(
            FeatureKind::UnsupportedField,
            pointer,
            "tool_choice must have a type",
        )
    })?;
    match ty {
        "auto" => Ok(Value::String("auto".to_string())),
        "any" => Ok(Value::String("required".to_string())),
        "tool" => {
            let name = tc
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::MissingToolField,
                        format!("{pointer}/name"),
                        "tool_choice type=tool missing name",
                    )
                })?;
            Ok(serde_json::json!({
                "type": "function",
                "function": {"name": name}
            }))
        }
        other => Err(UnsupportedFeatures::single(
            FeatureKind::UnsupportedField,
            pointer,
            format!("unsupported tool_choice type {other:?}"),
        )),
    }
}
