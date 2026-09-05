use super::super::super::{
    error::{FeatureKind, UnsupportedFeatures},
    report::ConversionContext,
    request::MAX_IMAGE_BYTES,
};
use super::{bad, required};
use serde_json::{Map, Value};

pub fn encode_request(
    request: &Value,
    mapped_model: &str,
) -> Result<(Value, ConversionContext), UnsupportedFeatures> {
    let o = request.as_object().ok_or_else(|| {
        bad(
            FeatureKind::UnsupportedField,
            "/",
            "Messages request must be an object",
        )
    })?;
    let mut normalized = Vec::new();
    for k in o.keys() {
        match k.as_str() {
            "model" | "messages" | "system" | "tools" | "tool_choice" | "thinking"
            | "output_config" | "max_tokens" | "stream" | "temperature" | "top_p"
            | "stop_sequences" => {}
            "metadata" | "container" | "context_management" | "context_management_config" => {
                normalized.push(format!("/{k}"))
            }
            other => {
                return Err(bad(
                    FeatureKind::UnsupportedField,
                    format!("/{other}"),
                    "Messages field is not representable by Responses",
                ))
            }
        }
    }
    let mut input = Vec::new();
    if let Some(system) = o.get("system") {
        let texts = system_text(system, "/system")?;
        if !texts.is_empty() {
            input.push(serde_json::json!({"type":"message","role":"developer","content":texts.into_iter().map(|text|serde_json::json!({"type":"input_text","text":text})).collect::<Vec<_>>() }));
        }
    }
    let messages = o.get("messages").and_then(Value::as_array).ok_or_else(|| {
        bad(
            FeatureKind::UnknownRole,
            "/messages",
            "Messages request requires messages array",
        )
    })?;
    for (i, msg) in messages.iter().enumerate() {
        input.extend(message_input(msg, &format!("/messages/{i}"))?)
    }
    let mut out = Map::new();
    out.insert("model".into(), Value::String(mapped_model.into()));
    out.insert("input".into(), Value::Array(input));
    out.insert(
        "max_output_tokens".into(),
        o.get("max_tokens")
            .cloned()
            .unwrap_or_else(|| Value::from(4096)),
    );
    out.insert(
        "stream".into(),
        Value::Bool(o.get("stream").and_then(Value::as_bool).unwrap_or(false)),
    );
    for k in ["temperature", "top_p"] {
        if let Some(v) = o.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    if let Some(v) = o.get("stop_sequences") {
        out.insert("stop".into(), v.clone());
    }
    if let Some(v) = o.get("tools") {
        out.insert("tools".into(), tools(v, "/tools")?);
    }
    if let Some(v) = o.get("tool_choice") {
        let (choice, parallel) = tool_choice(v, "/tool_choice")?;
        out.insert("tool_choice".into(), choice);
        if !parallel {
            out.insert("parallel_tool_calls".into(), Value::Bool(false));
        }
    }
    if let Some(e) = thinking_effort(o) {
        out.insert("reasoning".into(), serde_json::json!({"effort":e}));
        normalized.push("/thinking".into());
    }
    let mut c = ConversionContext::new(
        format!("msg_{}", uuid::Uuid::new_v4().simple()),
        mapped_model,
        o.get("stream").and_then(Value::as_bool).unwrap_or(false),
    );
    c.normalized = normalized;
    Ok((Value::Object(out), c))
}
fn system_text(v: &Value, p: &str) -> Result<Vec<String>, UnsupportedFeatures> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if b.get("type").and_then(Value::as_str) != Some("text") {
                    return Err(bad(
                        FeatureKind::UnknownBlock,
                        format!("{p}/{i}/type"),
                        "system blocks must be text",
                    ));
                }
                b.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        bad(
                            FeatureKind::UnknownBlock,
                            format!("{p}/{i}/text"),
                            "system text is required",
                        )
                    })
            })
            .collect(),
        _ => Err(bad(
            FeatureKind::UnknownBlock,
            p,
            "system must be text or text-block array",
        )),
    }
}
fn message_input(msg: &Value, p: &str) -> Result<Vec<Value>, UnsupportedFeatures> {
    let role = msg
        .get("role")
        .and_then(Value::as_str)
        .filter(|r| matches!(*r, "user" | "assistant" | "system"))
        .ok_or_else(|| {
            bad(
                FeatureKind::UnknownRole,
                format!("{p}/role"),
                "role must be user, assistant, or system",
            )
        })?;
    let parts = match msg.get("content") {
        Some(Value::String(s)) => vec![serde_json::json!({"type":"text","text":s})],
        Some(Value::Array(a)) => a.clone(),
        _ => {
            return Err(bad(
                FeatureKind::UnknownBlock,
                format!("{p}/content"),
                "content must be text or array",
            ))
        }
    };
    let mut out = Vec::new();
    for (i, b) in parts.iter().enumerate() {
        let bp = format!("{p}/content/{i}");
        match b.get("type").and_then(Value::as_str) {
            Some("text") => out.push(serde_json::json!({
                "type":"message", "role":if role == "system" { "developer" } else { role },
                "content":[{"type":if role=="assistant"{"output_text"}else{"input_text"},"text":b.get("text").and_then(Value::as_str).unwrap_or("")}]
            })),
            Some("image") if role == "user" => out.push(serde_json::json!({
                "type":"message", "role":role, "content":[image_input(b, &bp)?]
            })),
            Some("tool_use") if role == "assistant" => {
                let id=required(b,"id",&bp)?; let name=required(b,"name",&bp)?;
                let input=b.get("input").ok_or_else(||bad(FeatureKind::MissingToolField,format!("{bp}/input"),"tool input is required"))?;
                if !input.is_object(){return Err(bad(FeatureKind::InvalidToolArguments,format!("{bp}/input"),"tool input must be an object"))}
                out.push(serde_json::json!({"type":"function_call","call_id":id,"name":name,"arguments":serde_json::to_string(input).map_err(|_|bad(FeatureKind::InvalidToolArguments,format!("{bp}/input"),"tool input cannot be serialized"))?}));
            }
            Some("tool_result") if role == "user" => {
                let id=required(b,"tool_use_id",&bp)?;
                out.push(serde_json::json!({"type":"function_call_output","call_id":id,"output":tool_result_output(b.get("content"), &format!("{bp}/content"))?}));
            }
            Some("thinking") if role == "assistant" => {
                let text = b.get("thinking").and_then(Value::as_str).ok_or_else(|| bad(FeatureKind::UnknownBlock, format!("{bp}/thinking"), "thinking text is required"))?;
                // Responses reasoning replay uses `content/reasoning_text`, but
                // some upstream Responses-compatible backends (reported by users
                // on GPT-5.6 routes) also require the legacy/presentation
                // `summary` field to be present on replayed reasoning items.
                // Emit both: `content` preserves the original reasoning text for
                // continuation, while `summary` satisfies stricter validators.
                out.push(serde_json::json!({
                    "type":"reasoning",
                    "content":[{"type":"reasoning_text","text":text}],
                    "summary":[{"type":"summary_text","text":text}]
                }));
            }
            Some("redacted_thinking") => {},
            Some(x)=>return Err(bad(FeatureKind::UnknownBlock,format!("{bp}/type"),format!("content type {x:?} has no direct mapping"))),
            None=>return Err(bad(FeatureKind::UnknownBlock,format!("{bp}/type"),"content block type is required")),
        }
    }
    Ok(out)
}

/// Responses `function_call_output.output` is text-only.  Messages permits
/// structured tool-result content, so translate readable text blocks
/// explicitly and reject everything else instead of emitting an invalid
/// Responses block.
fn tool_result_output(
    content: Option<&Value>,
    pointer: &str,
) -> Result<Value, UnsupportedFeatures> {
    match content.unwrap_or(&Value::Null) {
        Value::Null => Ok(Value::String(String::new())),
        Value::String(text) => Ok(Value::String(text.clone())),
        Value::Array(blocks) => {
            let mut text = String::new();
            for (index, block) in blocks.iter().enumerate() {
                let p = format!("{pointer}/{index}");
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => text.push_str(
                        block.get("text").and_then(Value::as_str).ok_or_else(|| {
                            bad(
                                FeatureKind::UnknownBlock,
                                format!("{p}/text"),
                                "tool-result text is required",
                            )
                        })?,
                    ),
                    Some(other) => {
                        return Err(bad(
                            FeatureKind::UnknownBlock,
                            format!("{p}/type"),
                            format!(
                                "tool-result block {other:?} is not representable by Responses"
                            ),
                        ))
                    }
                    None => {
                        return Err(bad(
                            FeatureKind::UnknownBlock,
                            format!("{p}/type"),
                            "tool-result block type is required",
                        ))
                    }
                }
            }
            Ok(Value::String(text))
        }
        _ => Err(bad(
            FeatureKind::UnknownBlock,
            pointer,
            "tool-result content must be text or text blocks",
        )),
    }
}
fn image_input(v: &Value, p: &str) -> Result<Value, UnsupportedFeatures> {
    let source = v.get("source").ok_or_else(|| {
        bad(
            FeatureKind::Media,
            format!("{p}/source"),
            "image source is required",
        )
    })?;
    match source.get("type").and_then(Value::as_str) {
        Some("base64") => {
            let media = required(source, "media_type", &format!("{p}/source"))?;
            let data = required(source, "data", &format!("{p}/source"))?;
            if data.len() > MAX_IMAGE_BYTES {
                return Err(bad(
                    FeatureKind::Media,
                    format!("{p}/source/data"),
                    "image exceeds supported maximum",
                ));
            }
            Ok(
                serde_json::json!({"type":"input_image","image_url":format!("data:{media};base64,{data}")}),
            )
        }
        Some("url") => {
            let url = required(source, "url", &format!("{p}/source"))?;
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(bad(
                    FeatureKind::Media,
                    format!("{p}/source/url"),
                    "image URL must be http(s)",
                ));
            }
            Ok(serde_json::json!({"type":"input_image","image_url":url}))
        }
        _ => Err(bad(
            FeatureKind::Media,
            format!("{p}/source/type"),
            "unsupported image source",
        )),
    }
}
fn tools(v: &Value, p: &str) -> Result<Value, UnsupportedFeatures> {
    v.as_array()
        .ok_or_else(|| bad(FeatureKind::UnsupportedField, p, "tools must be an array"))?
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let q = format!("{p}/{i}");
            if matches!(t.get("type").and_then(Value::as_str),Some(x)if x!="custom") {
                return Err(bad(
                    FeatureKind::BuiltinTool,
                    format!("{q}/type"),
                    "built-in tool has no direct mapping",
                ));
            }
            let name = required(t, "name", &q)?;
            let schema = t.get("input_schema").ok_or_else(|| {
                bad(
                    FeatureKind::InvalidToolArguments,
                    format!("{q}/input_schema"),
                    "input_schema is required",
                )
            })?;
            if !schema.is_object() {
                return Err(bad(
                    FeatureKind::InvalidToolArguments,
                    format!("{q}/input_schema"),
                    "input_schema must be an object",
                ));
            }
            let mut o = serde_json::json!({"type":"function","name":name,"parameters":schema});
            if let Some(d) = t.get("description") {
                o["description"] = d.clone();
            }
            Ok(o)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}
fn tool_choice(v: &Value, p: &str) -> Result<(Value, bool), UnsupportedFeatures> {
    let o = v.as_object().ok_or_else(|| {
        bad(
            FeatureKind::UnsupportedField,
            p,
            "tool_choice must be an object",
        )
    })?;
    let disable = o
        .get("disable_parallel_tool_use")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let r = match o.get("type").and_then(Value::as_str) {
        Some("auto") => Value::String("auto".into()),
        Some("any") => Value::String("required".into()),
        Some("none") => Value::String("none".into()),
        Some("tool") => serde_json::json!({"type":"function","name":required(v,"name",p)?}),
        _ => {
            return Err(bad(
                FeatureKind::UnsupportedField,
                format!("{p}/type"),
                "unsupported tool_choice",
            ))
        }
    };
    Ok((r, !disable))
}
fn thinking_effort(o: &Map<String, Value>) -> Option<&'static str> {
    let budget = o
        .get("thinking")
        .and_then(|thinking| thinking.get("budget_tokens"))
        .and_then(Value::as_i64)
        .or_else(|| {
            o.get("output_config")
                .and_then(|config| config.get("effort"))
                .and_then(Value::as_str)
                .map(|e| match e {
                    "low" => 1024,
                    "medium" => 8192,
                    "high" => 24576,
                    _ => 32768,
                })
        })?;
    crate::protocol::thinking::budget_to_level(budget)
}
