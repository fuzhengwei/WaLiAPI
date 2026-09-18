//! Chat Completions 请求 → Gemini Vertex `GenerateContentRequest`（inner）。
//! 不可表示的能力必须在发出上游请求前拒绝，避免静默丢字段。

use super::super::error::{FeatureKind, UnsupportedFeatures};
use super::super::report::ConversionContext;
use super::super::request;
use serde_json::{json, Map, Value};

const SUPPORTED_TOP_LEVEL: &[&str] = &[
    "model",
    "messages",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "stream",
    "tools",
    "tool_choice",
    "response_format",
    "reasoning_effort",
    "store",
    "stream_options",
];

pub fn encode_chat_to_gemini(
    body: &Value,
    model: &str,
) -> Result<(Value, ConversionContext), UnsupportedFeatures> {
    let mut rejected = Vec::new();
    let mut normalized = Vec::new();
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            if !SUPPORTED_TOP_LEVEL.contains(&key.as_str()) {
                request::reject(
                    &mut rejected,
                    FeatureKind::UnsupportedField,
                    format!("/{key}"),
                    format!("Chat field {key:?} is not supported by chat_to_gemini_v1"),
                );
                continue;
            }
            match key.as_str() {
                "store" if value.as_bool() == Some(false) => {
                    normalized.push("/store".to_owned());
                }
                "store" => request::reject(
                    &mut rejected,
                    FeatureKind::UnsupportedField,
                    "/store",
                    "store must be false when converting Chat to Gemini",
                ),
                "stream_options" | "reasoning_effort" => {
                    normalized.push(format!("/{key}"));
                }
                _ => {}
            }
        }
    }

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::UnknownRole,
                "/messages",
                "Chat request requires messages array",
            )
        })?;

    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    let mut tool_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for (i, msg) in messages.iter().enumerate() {
        let pointer = format!("/messages/{i}");
        convert_message(
            msg,
            &pointer,
            &mut system_parts,
            &mut contents,
            &mut tool_names,
            &mut rejected,
        )?;
    }

    let mut generation = Map::new();
    if let Some(temp) = body.get("temperature") {
        generation.insert("temperature".into(), temp.clone());
    }
    if let Some(top_p) = body.get("top_p") {
        generation.insert("topP".into(), top_p.clone());
    }
    if let Some(max_tokens) = body
        .get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
    {
        generation.insert("maxOutputTokens".into(), max_tokens.clone());
    }

    let mut decls = Vec::new();
    if let Some(tools) = body.get("tools") {
        let Some(tools) = tools.as_array() else {
            return Err(UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                "/tools",
                "Chat tools must be an array",
            ));
        };
        for (index, tool) in tools.iter().enumerate() {
            let pointer = format!("/tools/{index}");
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                request::reject(
                    &mut rejected,
                    FeatureKind::BuiltinTool,
                    format!("{pointer}/type"),
                    "Gemini only supports function tools",
                );
                continue;
            }
            let Some(function) = tool.get("function") else {
                request::reject(
                    &mut rejected,
                    FeatureKind::MissingToolField,
                    format!("{pointer}/function"),
                    "function tool is missing function",
                );
                continue;
            };
            let Some(name) = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                request::reject(
                    &mut rejected,
                    FeatureKind::MissingToolField,
                    format!("{pointer}/function/name"),
                    "function tool is missing name",
                );
                continue;
            };
            let Some(parameters) = function.get("parameters") else {
                request::reject(
                    &mut rejected,
                    FeatureKind::InvalidToolArguments,
                    format!("{pointer}/function/parameters"),
                    "function tool is missing parameters",
                );
                continue;
            };
            if !parameters.is_object() {
                request::reject(
                    &mut rejected,
                    FeatureKind::InvalidToolArguments,
                    format!("{pointer}/function/parameters"),
                    "function tool parameters must be an object schema",
                );
                continue;
            }
            let mut decl = json!({ "name": name, "parameters": parameters });
            if let Some(desc) = function.get("description") {
                decl["description"] = desc.clone();
            }
            decls.push(decl);
        }
    }
    let has_tools = !decls.is_empty();

    let mut tool_config = None;
    if let Some(choice) = body.get("tool_choice") {
        match function_calling_config(choice) {
            Ok(config) => {
                let mode = config.get("mode").and_then(Value::as_str);
                if has_tools || mode == Some("NONE") {
                    tool_config = Some(json!({ "functionCallingConfig": config }));
                } else if mode != Some("AUTO") {
                    request::reject(
                        &mut rejected,
                        FeatureKind::UnsupportedField,
                        "/tool_choice",
                        "tool_choice other than auto/none requires tools",
                    );
                }
                normalized.push("/tool_choice".to_owned());
            }
            Err((kind, pointer, message)) => {
                request::reject(&mut rejected, kind, pointer, message);
            }
        }
    }

    if let Some(format) = body.get("response_format") {
        apply_response_format(format, &mut generation, &mut rejected, &mut normalized);
    }

    request::finish(rejected)?;

    let mut out = json!({ "model": model, "contents": contents });
    if !system_parts.is_empty() {
        out["systemInstruction"] = json!({
            "parts": system_parts.into_iter().map(|text| json!({"text": text})).collect::<Vec<_>>()
        });
    }
    if has_tools {
        out["tools"] = json!([{ "functionDeclarations": decls }]);
    }
    if let Some(config) = tool_config {
        out["toolConfig"] = config;
    }
    if !generation.is_empty() {
        out["generationConfig"] = Value::Object(generation);
    }

    let request_id = body
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()));
    let mut context = ConversionContext::new(request_id, model, stream);
    context.normalized = normalized;
    Ok((out, context))
}

fn convert_message(
    msg: &Value,
    pointer: &str,
    system_parts: &mut Vec<String>,
    contents: &mut Vec<Value>,
    tool_names: &mut std::collections::HashMap<String, String>,
    rejected: &mut Vec<crate::protocol::codec::error::RejectedField>,
) -> Result<(), UnsupportedFeatures> {
    let role = msg.get("role").and_then(Value::as_str).ok_or_else(|| {
        UnsupportedFeatures::single(
            FeatureKind::UnknownRole,
            format!("{pointer}/role"),
            "Chat message missing role",
        )
    })?;

    match role {
        "system" | "developer" => {
            push_text_content(msg, pointer, system_parts, rejected)?;
            Ok(())
        }
        "user" => {
            let parts = content_parts(msg, pointer, rejected)?;
            if !parts.is_empty() {
                contents.push(json!({ "role": "user", "parts": parts }));
            }
            Ok(())
        }
        "assistant" => {
            let mut parts = content_parts(msg, pointer, rejected)?;
            if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                for (ci, call) in calls.iter().enumerate() {
                    let call_pointer = format!("{pointer}/tool_calls/{ci}");
                    if call.get("type").and_then(Value::as_str) != Some("function") {
                        return Err(UnsupportedFeatures::single(
                            FeatureKind::BuiltinTool,
                            format!("{call_pointer}/type"),
                            "only function tool calls are supported",
                        ));
                    }
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            UnsupportedFeatures::single(
                                FeatureKind::MissingToolField,
                                format!("{call_pointer}/id"),
                                "tool call missing id",
                            )
                        })?;
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            UnsupportedFeatures::single(
                                FeatureKind::MissingToolField,
                                format!("{call_pointer}/function/name"),
                                "tool call missing function name",
                            )
                        })?;
                    tool_names.insert(id.to_owned(), name.to_owned());
                    let args_raw = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            UnsupportedFeatures::single(
                                FeatureKind::InvalidToolArguments,
                                format!("{call_pointer}/function/arguments"),
                                "tool call arguments must be JSON object text",
                            )
                        })?;
                    let args: Value = serde_json::from_str(args_raw).map_err(|_| {
                        UnsupportedFeatures::single(
                            FeatureKind::InvalidToolArguments,
                            format!("{call_pointer}/function/arguments"),
                            "tool call arguments must be JSON object text",
                        )
                    })?;
                    if !args.is_object() {
                        return Err(UnsupportedFeatures::single(
                            FeatureKind::InvalidToolArguments,
                            format!("{call_pointer}/function/arguments"),
                            "tool call arguments must be a JSON object",
                        ));
                    }
                    parts.push(json!({ "functionCall": { "name": name, "args": args } }));
                }
            }
            if !parts.is_empty() {
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Ok(())
        }
        "tool" => {
            let tool_call_id = msg
                .get("tool_call_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::MissingToolField,
                        format!("{pointer}/tool_call_id"),
                        "tool result is missing tool_call_id",
                    )
                })?;
            let name = tool_names.get(tool_call_id).ok_or_else(|| {
                UnsupportedFeatures::single(
                    FeatureKind::MissingToolField,
                    format!("{pointer}/tool_call_id"),
                    "tool result does not match a previous assistant tool call",
                )
            })?;
            let response = match msg.get("content") {
                Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({ "result": s })),
                Some(Value::Null) | None => {
                    return Err(UnsupportedFeatures::single(
                        FeatureKind::InvalidToolArguments,
                        format!("{pointer}/content"),
                        "tool result requires content",
                    ));
                }
                Some(other) => other.clone(),
            };
            contents.push(json!({
                "role": "user",
                "parts": [{ "functionResponse": { "name": name, "response": response } }]
            }));
            Ok(())
        }
        other => Err(UnsupportedFeatures::single(
            FeatureKind::UnknownRole,
            format!("{pointer}/role"),
            format!("unsupported Chat role {other}"),
        )),
    }
}

fn push_text_content(
    msg: &Value,
    pointer: &str,
    out: &mut Vec<String>,
    rejected: &mut Vec<crate::protocol::codec::error::RejectedField>,
) -> Result<(), UnsupportedFeatures> {
    match msg.get("content") {
        Some(Value::String(s)) if !s.is_empty() => out.push(s.clone()),
        Some(Value::Array(blocks)) => {
            for (bi, block) in blocks.iter().enumerate() {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            if !t.is_empty() {
                                out.push(t.to_owned());
                            }
                        }
                    }
                    _ => request::reject(
                        rejected,
                        FeatureKind::UnknownBlock,
                        format!("{pointer}/content/{bi}"),
                        "system content block must be text",
                    ),
                }
            }
        }
        Some(Value::String(_)) | None => {}
        _ => request::reject(
            rejected,
            FeatureKind::UnknownBlock,
            format!("{pointer}/content"),
            "system content must be text",
        ),
    }
    Ok(())
}

fn content_parts(
    msg: &Value,
    pointer: &str,
    rejected: &mut Vec<crate::protocol::codec::error::RejectedField>,
) -> Result<Vec<Value>, UnsupportedFeatures> {
    match msg.get("content") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => {
            if s.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![json!({ "text": s })])
            }
        }
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for (bi, item) in items.iter().enumerate() {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = item.get("text").and_then(Value::as_str) {
                            if !t.is_empty() {
                                parts.push(json!({ "text": t }));
                            }
                        }
                    }
                    Some("image_url") => {
                        let url = item
                            .pointer("/image_url/url")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        match parse_data_url(url) {
                            Some((mime, data)) => {
                                parts.push(json!({
                                    "inlineData": { "mimeType": mime, "data": data }
                                }));
                            }
                            None => request::reject(
                                rejected,
                                FeatureKind::Media,
                                format!("{pointer}/content/{bi}/image_url/url"),
                                "only data: URI images are supported",
                            ),
                        }
                    }
                    other => request::reject(
                        rejected,
                        FeatureKind::UnknownBlock,
                        format!("{pointer}/content/{bi}/type"),
                        format!("unsupported content block {other:?}"),
                    ),
                }
            }
            Ok(parts)
        }
        _ => Err(UnsupportedFeatures::single(
            FeatureKind::UnknownBlock,
            format!("{pointer}/content"),
            "content must be a string or array",
        )),
    }
}

fn function_calling_config(value: &Value) -> Result<Value, (FeatureKind, String, String)> {
    match value {
        Value::String(mode) => match mode.as_str() {
            "auto" => Ok(json!({ "mode": "AUTO" })),
            "none" => Ok(json!({ "mode": "NONE" })),
            "required" => Ok(json!({ "mode": "ANY" })),
            other => Err((
                FeatureKind::UnsupportedField,
                "/tool_choice".to_owned(),
                format!("unsupported tool_choice {other:?}"),
            )),
        },
        Value::Object(map) => {
            let ty = map.get("type").and_then(Value::as_str).unwrap_or("");
            match ty {
                "auto" => Ok(json!({ "mode": "AUTO" })),
                "none" => Ok(json!({ "mode": "NONE" })),
                "required" | "any" => Ok(json!({ "mode": "ANY" })),
                "function" => {
                    let name = map
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .or_else(|| map.get("name").and_then(Value::as_str))
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            (
                                FeatureKind::MissingToolField,
                                "/tool_choice".to_owned(),
                                "tool_choice type=function requires a function name".to_owned(),
                            )
                        })?;
                    Ok(json!({
                        "mode": "ANY",
                        "allowedFunctionNames": [name]
                    }))
                }
                other => Err((
                    FeatureKind::UnsupportedField,
                    "/tool_choice/type".to_owned(),
                    format!("unsupported tool_choice type {other:?}"),
                )),
            }
        }
        _ => Err((
            FeatureKind::UnsupportedField,
            "/tool_choice".to_owned(),
            "tool_choice must be a string or object".to_owned(),
        )),
    }
}

fn apply_response_format(
    value: &Value,
    generation: &mut Map<String, Value>,
    rejected: &mut Vec<crate::protocol::codec::error::RejectedField>,
    normalized: &mut Vec<String>,
) {
    let ty = value.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "text" => {
            normalized.push("/response_format".to_owned());
        }
        "json_object" => {
            generation.insert("responseMimeType".into(), json!("application/json"));
            normalized.push("/response_format".to_owned());
        }
        "json_schema" => {
            let schema = value
                .pointer("/json_schema/schema")
                .cloned()
                .or_else(|| value.get("schema").cloned());
            match schema {
                Some(Value::Object(schema)) => {
                    generation.insert("responseMimeType".into(), json!("application/json"));
                    generation.insert("responseSchema".into(), Value::Object(schema));
                    normalized.push("/response_format".to_owned());
                }
                Some(Value::String(_)) => request::reject(
                    rejected,
                    FeatureKind::StructuredOutput,
                    "/response_format/json_schema/schema",
                    "json_schema must be an inline object; remote schema URLs are not fetched",
                ),
                _ => request::reject(
                    rejected,
                    FeatureKind::StructuredOutput,
                    "/response_format/json_schema",
                    "json_schema requires an object schema",
                ),
            }
        }
        _ => request::reject(
            rejected,
            FeatureKind::StructuredOutput,
            "/response_format",
            format!("unsupported response_format type {ty:?}"),
        ),
    }
}

fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.contains(";base64") {
        return None;
    }
    let mime = meta.split(';').next()?.to_owned();
    if mime.is_empty() {
        return None;
    }
    Some((mime, data.to_owned()))
}
