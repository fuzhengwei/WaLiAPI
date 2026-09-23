use super::super::error::{FeatureKind, UnsupportedFeatures};
use super::super::report::ConversionContext;
use super::super::request;
use serde_json::{Map, Value};

const CHAT_TOP_LEVEL: &[&str] = &[
    "model",
    "messages",
    "max_tokens",
    "max_completion_tokens",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "reasoning_effort",
    "verbosity",
    "metadata",
    "store",
    "temperature",
    "top_p",
    "stop",
    "parallel_tool_calls",
    "n",
    "response_format",
];

/// 有 Responses 等价字段：原样透传（与 Messages→Responses 的映射保持一致）。
const CHAT_PASSTHROUGH_FIELDS: &[&str] = &["temperature", "top_p", "stop", "parallel_tool_calls"];

/// 无 Responses 等价物、且丢弃不会改变本次答复语义的 Chat 字段。
///
/// 这些是 Chat Completions 特有的采样 / 计费旋钮，透传会被上游以 400 拒掉，
/// 而让整段请求失败不划算：opencode、Cline、ChatBox 等客户端默认就会带
/// `temperature` / `presence_penalty` 之类的参数。丢弃会记入 `normalized`
/// 审计上下文，不静默吞掉。`n` 例外：`n > 1` 会真正减少下游拿到的候选数，
/// 因此单独 fail-closed（见字段校验）。
const CHAT_DROPPED_FIELDS: &[&str] = &[
    "presence_penalty",
    "frequency_penalty",
    "seed",
    "user",
    "logit_bias",
    "service_tier",
];

/// Encode a Chat Completions request as a Responses request.  This deliberately
/// emits only the backend allow-list fields; callers must not get a silent
/// escape hatch for a field this account upstream cannot represent.
pub fn encode_chat_to_responses(
    body: &Value,
    model: &str,
) -> Result<(Value, ConversionContext), UnsupportedFeatures> {
    let object = body.as_object().ok_or_else(|| {
        UnsupportedFeatures::single(
            FeatureKind::UnsupportedField,
            "/",
            "Chat request must be an object",
        )
    })?;
    let mut rejected = Vec::new();
    let mut normalized = Vec::new();
    // Chat `response_format` → Responses `text.format`（映射后统一写进 text）。
    let mut text_control: Option<Value> = None;
    for (key, value) in object {
        if CHAT_DROPPED_FIELDS.contains(&key.as_str()) {
            normalized.push(format!("/{key}"));
        } else if !CHAT_TOP_LEVEL.contains(&key.as_str()) {
            request::reject(
                &mut rejected,
                FeatureKind::UnsupportedField,
                format!("/{key}"),
                format!("Chat field {key:?} has no Responses backend representation"),
            );
        } else if key == "reasoning_effort" && !value.is_string() {
            request::reject(
                &mut rejected,
                FeatureKind::UnsupportedField,
                "/reasoning_effort",
                "Chat reasoning_effort must be a string",
            );
        } else if key == "verbosity" && !value.is_string() {
            request::reject(
                &mut rejected,
                FeatureKind::UnsupportedField,
                "/verbosity",
                "Chat verbosity must be a string",
            );
        } else if key == "store" && value.as_bool() != Some(false) {
            request::reject(
                &mut rejected,
                FeatureKind::UnsupportedField,
                "/store",
                "store must be false when converting Chat to Responses",
            );
        } else if matches!(key.as_str(), "temperature" | "top_p") && !value.is_number() {
            request::reject(
                &mut rejected,
                FeatureKind::UnsupportedField,
                format!("/{key}"),
                format!("Chat {key} must be a number"),
            );
        } else if key == "parallel_tool_calls" && !value.is_boolean() {
            request::reject(
                &mut rejected,
                FeatureKind::UnsupportedField,
                "/parallel_tool_calls",
                "Chat parallel_tool_calls must be a boolean",
            );
        } else if key == "n" {
            if value.as_u64() != Some(1) {
                // Responses 只表达单个候选；0、负数、浮点、字符串和 null
                // 都不是可安全归一为默认值的 `n`。
                request::reject(
                    &mut rejected,
                    FeatureKind::UnsupportedField,
                    "/n",
                    "Chat n must be the positive integer 1 when converting to Responses",
                );
            } else {
                // n == 1 与默认同义：不透传，但留下审计痕迹。
                normalized.push("/n".to_owned());
            }
        } else if key == "response_format" {
            match response_format_to_text(value, "/response_format") {
                Ok(Some(text)) => text_control = Some(text),
                // `{"type":"text"}` 表示无结构约束，等价于不发送。
                Ok(None) => normalized.push("/response_format".to_owned()),
                Err(error) => rejected.extend(error.fields),
            }
        } else if key == "stop" {
            if !value.is_string() {
                if let Err(error) = request::require_string_array(value, "/stop", "stop") {
                    rejected.extend(error.fields);
                }
            }
        }
    }

    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                "/messages",
                "Chat request requires messages array",
            )
        })?;
    let mut input = Vec::new();
    let mut instruction_parts = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match chat_message_to_responses(message, &format!("/messages/{index}")) {
            Ok(ChatMessageParts {
                instructions,
                mut items,
            }) => {
                instruction_parts.extend(instructions);
                input.append(&mut items);
            }
            Err(error) => rejected.extend(error.fields),
        }
    }
    if let Some(tools) = object.get("tools") {
        match super::chat_tools::chat_tools_to_responses(tools, "/tools") {
            Ok(_) => {}
            Err(error) => rejected.extend(error.fields),
        }
    }
    if let Some(choice) = object.get("tool_choice") {
        if let Err(error) = super::chat_tools::chat_tool_choice_to_responses(choice, "/tool_choice")
        {
            rejected.extend(error.fields);
        }
    }
    request::finish(rejected)?;

    let mut response = Map::new();
    response.insert("model".to_owned(), Value::String(model.to_owned()));
    response.insert("input".to_owned(), Value::Array(input));
    if object.get("max_completion_tokens").is_some() {
        // The ChatGPT Codex backend currently rejects max_output_tokens on this
        // path. The model catalog may expose output capacity, but request
        // translation must not forward a completion cap.
        normalized.push("/max_completion_tokens".to_owned());
    }
    if object.get("max_tokens").is_some() {
        normalized.push("/max_tokens".to_owned());
    }
    response.insert(
        "stream".to_owned(),
        Value::Bool(
            object
                .get("stream")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
    );
    if object.get("stream_options").is_some() {
        // Chat-only streaming options such as include_usage are not part of the
        // Responses request. Codex Responses streams report usage in
        // response.completed, so dropping this is intentional and observable.
        normalized.push("/stream_options".to_owned());
    }
    if object.get("reasoning_effort").is_some() {
        // Codex backend-api does not accept the public Responses `reasoning`
        // request field on this account endpoint. Keep the Chat request
        // compatible by dropping the preference and letting the account/model
        // default apply.
        normalized.push("/reasoning_effort".to_owned());
    }
    if object.get("verbosity").is_some() {
        // Same story for the public Responses `text.verbosity` control: the
        // Codex backend allow-list is narrower than the public API.
        normalized.push("/verbosity".to_owned());
    }
    if object.get("store").is_some() {
        // store:false is the OpenAI default and has no remote side effect.
        // Responses API handles storage through its own mechanism, so the
        // Chat store field is safely normalized away.
        normalized.push("/store".to_owned());
    }
    if object.get("metadata").is_some() {
        // Client metadata is an annotation only.  The Codex account backend
        // does not accept the public Responses metadata field, so keep the
        // request usable by dropping it with an audit entry.
        normalized.push("/metadata".to_owned());
    }
    for field in CHAT_PASSTHROUGH_FIELDS {
        if let Some(value) = object.get(*field) {
            response.insert((*field).to_owned(), value.clone());
        }
    }
    if let Some(text) = text_control {
        // Responses 的结构化输出控制在 `text.format`（Chat 为顶层 `response_format`）。
        response.insert("text".to_owned(), text);
    }
    if !instruction_parts.is_empty() {
        response.insert(
            "instructions".to_owned(),
            Value::String(instruction_parts.join("\n")),
        );
    }
    if let Some(tools) = object.get("tools") {
        let tools = super::chat_tools::chat_tools_to_responses(tools, "/tools")?;
        if !tools.is_empty() {
            response.insert("tools".to_owned(), Value::Array(tools));
        }
    }
    if let Some(choice) = object.get("tool_choice") {
        if let Some(choice) =
            super::chat_tools::chat_tool_choice_to_responses(choice, "/tool_choice")?
        {
            response.insert("tool_choice".to_owned(), choice);
        }
    }
    let mut context = ConversionContext::new(
        format!("chatcmpl_{}", uuid::Uuid::new_v4().simple()),
        model,
        object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    context.normalized = normalized;
    Ok((Value::Object(response), context))
}

/// Chat `response_format` → Responses `text.format`。
///
/// Chat 把 json_schema 的细节嵌在 `json_schema` 子对象里，Responses 是平铺到
/// `text.format`；`{"type":"text"}` 表示无结构约束，等价于不发送（返回 `None`）。
/// Codex 账号与 Grok 上游都已实测接受 `text.format` 的两种形态。
fn response_format_to_text(
    value: &Value,
    pointer: &str,
) -> Result<Option<Value>, UnsupportedFeatures> {
    let Some(object) = value.as_object() else {
        return Err(UnsupportedFeatures::single(
            FeatureKind::StructuredOutput,
            pointer,
            "Chat response_format must be an object",
        ));
    };
    match object.get("type").and_then(Value::as_str) {
        Some("text") => Ok(None),
        Some("json_object") => Ok(Some(serde_json::json!({
            "format": {"type": "json_object"}
        }))),
        Some("json_schema") => {
            let Some(nested) = object.get("json_schema").and_then(Value::as_object) else {
                return Err(UnsupportedFeatures::single(
                    FeatureKind::StructuredOutput,
                    format!("{pointer}/json_schema"),
                    "json_schema response_format requires a json_schema object",
                ));
            };
            let name = nested
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::StructuredOutput,
                        format!("{pointer}/json_schema/name"),
                        "json_schema response_format requires a name",
                    )
                })?;
            let schema = nested
                .get("schema")
                .filter(|schema| schema.is_object())
                .ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::StructuredOutput,
                        format!("{pointer}/json_schema/schema"),
                        "json_schema response_format requires an object schema",
                    )
                })?;
            let mut format = serde_json::json!({
                "type": "json_schema",
                "name": name,
                "schema": schema,
            });
            if let Some(description) = nested.get("description") {
                format["description"] = description.clone();
            }
            if let Some(strict) = nested.get("strict") {
                format["strict"] = strict.clone();
            }
            Ok(Some(serde_json::json!({"format": format})))
        }
        other => Err(UnsupportedFeatures::single(
            FeatureKind::StructuredOutput,
            format!("{pointer}/type"),
            format!("unsupported Chat response_format type {other:?}"),
        )),
    }
}

struct ChatMessageParts {
    instructions: Vec<String>,
    items: Vec<Value>,
}

fn chat_message_to_responses(
    message: &Value,
    pointer: &str,
) -> Result<ChatMessageParts, UnsupportedFeatures> {
    let message = message.as_object().ok_or_else(|| {
        UnsupportedFeatures::single(
            FeatureKind::UnknownRole,
            pointer,
            "Chat message must be an object",
        )
    })?;
    for key in message.keys() {
        if ![
            "role",
            "content",
            "reasoning_content",
            "tool_calls",
            "tool_call_id",
            "name",
        ]
        .contains(&key.as_str())
        {
            return Err(UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                format!("{pointer}/{key}"),
                "message field is not representable",
            ));
        }
    }
    let role = message.get("role").and_then(Value::as_str).ok_or_else(|| {
        UnsupportedFeatures::single(
            FeatureKind::UnknownRole,
            format!("{pointer}/role"),
            "message role is required",
        )
    })?;
    let content = chat_content_to_responses(
        message.get("content"),
        &format!("{pointer}/content"),
        role == "assistant",
    )?;
    match role {
        "system" | "developer" => {
            if message.get("tool_calls").is_some() {
                return Err(UnsupportedFeatures::single(
                    FeatureKind::UnsupportedField,
                    format!("{pointer}/tool_calls"),
                    "system/developer tool calls are invalid",
                ));
            }
            let text = content
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            Ok(ChatMessageParts {
                instructions: vec![text],
                items: Vec::new(),
            })
        }
        "user" => Ok(ChatMessageParts {
            instructions: Vec::new(),
            items: vec![serde_json::json!({"type":"message", "role":"user", "content": content})],
        }),
        "assistant" => {
            let mut items = Vec::new();
            if let Some(reasoning) = chat_reasoning_content_to_responses(
                message.get("reasoning_content"),
                &format!("{pointer}/reasoning_content"),
            )? {
                items.push(reasoning);
            }
            if !content.is_empty() {
                items.push(
                    serde_json::json!({"type":"message", "role":"assistant", "content": content}),
                );
            }
            if let Some(calls) = message.get("tool_calls") {
                let calls = calls.as_array().ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::MissingToolField,
                        format!("{pointer}/tool_calls"),
                        "tool_calls must be an array",
                    )
                })?;
                for (i, call) in calls.iter().enumerate() {
                    items.push(chat_tool_call_to_responses(
                        call,
                        &format!("{pointer}/tool_calls/{i}"),
                    )?);
                }
            }
            Ok(ChatMessageParts {
                instructions: Vec::new(),
                items,
            })
        }
        "tool" => {
            let call_id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::MissingToolField,
                        format!("{pointer}/tool_call_id"),
                        "tool message requires a tool_call_id",
                    )
                })?;
            let output = content
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            Ok(ChatMessageParts {
                instructions: Vec::new(),
                items: vec![
                    serde_json::json!({"type":"function_call_output", "call_id":call_id, "output":output}),
                ],
            })
        }
        _ => Err(UnsupportedFeatures::single(
            FeatureKind::UnknownRole,
            format!("{pointer}/role"),
            format!("Chat role {role:?} is not supported"),
        )),
    }
}

fn chat_reasoning_content_to_responses(
    reasoning: Option<&Value>,
    pointer: &str,
) -> Result<Option<Value>, UnsupportedFeatures> {
    let Some(reasoning) = reasoning else {
        return Ok(None);
    };
    let text = match reasoning {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("text")
            .or_else(|| object.get("reasoning"))
            .or_else(|| object.get("thinking"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => {
            return Err(UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                pointer,
                "reasoning_content must be a string or text object",
            ))
        }
    };
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "type": "reasoning",
        "summary": [{"type": "summary_text", "text": text}]
    })))
}

fn chat_content_to_responses(
    content: Option<&Value>,
    pointer: &str,
    output: bool,
) -> Result<Vec<Value>, UnsupportedFeatures> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![serde_json::json!({
            "type": if output { "output_text" } else { "input_text" },
            "text": text,
        })]),
        Some(Value::Array(parts)) => parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let p = format!("{pointer}/{i}");
                let object = part.as_object().ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::UnknownBlock,
                        &p,
                        "content part must be an object",
                    )
                })?;
                match object.get("type").and_then(Value::as_str) {
                    Some("text") => object
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| {
                            serde_json::json!({
                                "type": if output { "output_text" } else { "input_text" },
                                "text": text,
                            })
                        })
                        .ok_or_else(|| {
                            UnsupportedFeatures::single(
                                FeatureKind::UnknownBlock,
                                format!("{p}/text"),
                                "text part requires text",
                            )
                        }),
                    Some("image_url") => {
                        if output {
                            return Err(UnsupportedFeatures::single(
                                FeatureKind::Media,
                                format!("{p}/type"),
                                "assistant image content is not representable",
                            ));
                        }
                        let url = object
                            .get("image_url")
                            .and_then(|image| image.get("url"))
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                UnsupportedFeatures::single(
                                    FeatureKind::Media,
                                    format!("{p}/image_url/url"),
                                    "image_url requires url",
                                )
                            })?;
                        if !(url.starts_with("https://")
                            || url.starts_with("http://")
                            || url.starts_with("data:image/"))
                        {
                            return Err(UnsupportedFeatures::single(
                                FeatureKind::Media,
                                format!("{p}/image_url/url"),
                                "image url must be http(s) or image data URL",
                            ));
                        }
                        if url.len() > request::MAX_IMAGE_BYTES * 2 {
                            return Err(UnsupportedFeatures::single(
                                FeatureKind::Media,
                                format!("{p}/image_url/url"),
                                "image exceeds maximum supported size",
                            ));
                        }
                        Ok(serde_json::json!({"type":"input_image", "image_url":url}))
                    }
                    Some(other) => Err(UnsupportedFeatures::single(
                        FeatureKind::UnknownBlock,
                        format!("{p}/type"),
                        format!("content type {other:?} is not representable"),
                    )),
                    None => Err(UnsupportedFeatures::single(
                        FeatureKind::UnknownBlock,
                        format!("{p}/type"),
                        "content part requires type",
                    )),
                }
            })
            .collect(),
        Some(_) => Err(UnsupportedFeatures::single(
            FeatureKind::UnknownBlock,
            pointer,
            "content must be a string, null, or array",
        )),
    }
}

fn chat_tool_call_to_responses(value: &Value, pointer: &str) -> Result<Value, UnsupportedFeatures> {
    let call_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::MissingToolField,
                format!("{pointer}/id"),
                "tool call requires id",
            )
        })?;
    if value.get("type").and_then(Value::as_str) != Some("function") {
        return Err(UnsupportedFeatures::single(
            FeatureKind::BuiltinTool,
            format!("{pointer}/type"),
            "only function tool calls are supported",
        ));
    }
    let name = value
        .pointer("/function/name")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::MissingToolField,
                format!("{pointer}/function/name"),
                "tool call requires function name",
            )
        })?;
    let arguments = value
        .pointer("/function/arguments")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::InvalidToolArguments,
                format!("{pointer}/function/arguments"),
                "tool arguments must be a JSON string",
            )
        })?;
    if serde_json::from_str::<Value>(arguments).is_err() {
        return Err(UnsupportedFeatures::single(
            FeatureKind::InvalidToolArguments,
            format!("{pointer}/function/arguments"),
            "tool arguments must contain valid JSON",
        ));
    }
    Ok(
        serde_json::json!({"type":"function_call", "call_id":call_id, "name":name, "arguments":arguments}),
    )
}
