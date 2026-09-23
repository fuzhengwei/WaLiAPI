//! Shared request-feature analysis helpers for the two directions.
//!
//! These are strict: an unknown or unrepresentable field is *rejected*, never
//! silently dropped.  Each rejection carries a stable error code and a concrete
//! JSON pointer.

use super::error::{FeatureKind, UnsupportedFeatures};
use serde_json::Value;

/// Maximum size (bytes) for a base64 image payload we will forward.  This is a
/// fail-closed safety bound; larger attachments are rejected rather than
/// truncated.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Collect a rejection into a `Vec`.
pub fn reject(
    out: &mut Vec<super::error::RejectedField>,
    kind: FeatureKind,
    pointer: impl Into<String>,
    message: impl Into<String>,
) {
    out.push(super::error::RejectedField {
        code: kind.code().to_string(),
        pointer: pointer.into(),
        message: message.into(),
    });
}

/// Convert the collected validation errors to an [`UnsupportedFeatures`]
/// (empty -> `Ok(())`).
pub fn finish(out: Vec<super::error::RejectedField>) -> Result<(), UnsupportedFeatures> {
    if out.is_empty() {
        Ok(())
    } else {
        Err(UnsupportedFeatures::new(out))
    }
}

/// Validate a protocol field whose wire shape is an array of strings.
///
/// Checking only the container is insufficient: forwarding a number/object in
/// a stop sequence makes the upstream reject the whole request much later.
pub fn require_string_array(
    value: &Value,
    pointer: &str,
    field: &str,
) -> Result<(), UnsupportedFeatures> {
    let Some(items) = value.as_array() else {
        return Err(UnsupportedFeatures::single(
            FeatureKind::UnsupportedField,
            pointer,
            format!("{field} must be an array of strings"),
        ));
    };
    for (index, item) in items.iter().enumerate() {
        if !item.is_string() {
            return Err(UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                format!("{pointer}/{index}"),
                format!("{field} array elements must be strings"),
            ));
        }
    }
    Ok(())
}

/// Validate a Chat Completions `tool_choice` field (OpenAI shape) and produce
/// the Anthropic-equivalent Json value.  Only explicitly mappable forms are
/// accepted:
///   - `"auto"`/`"none"` → string passthrough
///   - `"required"` → `{"type":"any"}`
///   - `{"type":"function","function":{"name":...}}` → `{"type":"tool","name":...}`
///
/// Anything else (including a named-function `"auto"` string, `"disable_parallel_tool_calls"`,
/// `parallel_tool_calls` interplay, or an unknown object) is rejected.
pub fn chat_tool_choice_to_anthropic(
    value: &Value,
    pointer: &str,
) -> Result<Option<Value>, UnsupportedFeatures> {
    match value {
        Value::String(s) => match s.as_str() {
            "auto" => Ok(Some(Value::String("auto".to_string()))),
            "none" => Ok(Some(Value::String("none".to_string()))),
            "required" => Ok(Some(serde_json::json!({"type": "any"}))),
            _ => Err(UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                pointer,
                format!("OpenAI tool_choice string {s:?} has no safe Anthropic equivalent"),
            )),
        },
        Value::Object(m) => {
            let ty = m.get("type").and_then(Value::as_str);
            match ty {
                Some("function") => {
                    let name = m
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            UnsupportedFeatures::single(
                                FeatureKind::MissingToolField,
                                format!("{pointer}/function/name"),
                                "tool_choice type=function requires a non-empty function.name",
                            )
                        })?;
                    // Reject any extra keys we cannot represent.
                    for (k, _) in m {
                        if k != "type" && k != "function" {
                            return Err(UnsupportedFeatures::single(
                                FeatureKind::UnsupportedField,
                                format!("{pointer}/{k}"),
                                format!("tool_choice key {k:?} has no safe Anthropic equivalent"),
                            ));
                        }
                    }
                    let f = m.get("function").cloned().unwrap_or(Value::Null);
                    if let Some(extra) = f.as_object().map(|o| {
                        o.keys()
                            .filter(|k| k.as_str() != "name")
                            .map(|k| format!("{pointer}/function/{k}"))
                            .collect::<Vec<_>>()
                    }) {
                        if let Some(first) = extra.first() {
                            return Err(UnsupportedFeatures::single(
                                FeatureKind::UnsupportedField,
                                first.clone(),
                                "tool_choice function only supports name",
                            ));
                        }
                    }
                    Ok(Some(serde_json::json!({"type": "tool", "name": name})))
                }
                Some(other) => Err(UnsupportedFeatures::single(
                    FeatureKind::UnsupportedField,
                    pointer,
                    format!("OpenAI tool_choice type {other:?} has no safe Anthropic equivalent"),
                )),
                None => Err(UnsupportedFeatures::single(
                    FeatureKind::UnsupportedField,
                    pointer,
                    "tool_choice object must have a 'type'",
                )),
            }
        }
        _ => Err(UnsupportedFeatures::single(
            FeatureKind::UnsupportedField,
            pointer,
            "unexpected tool_choice value",
        )),
    }
}

/// Validate an Anthropic `system` value and reduce it to the Chat-compatible
/// form (system role message content).  Returns the joined text.
///
/// A `thinking` block inside the system array is dropped fail-open (recorded
/// on `normalized`); `cache_control` blocks remain rejected (PromptCache is a
/// hard error, not a silent drop).
pub fn anthropic_system_to_chat(
    value: &Value,
    pointer: &str,
    normalized: &mut Vec<String>,
) -> Result<String, UnsupportedFeatures> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Array(items) => {
            let mut texts = Vec::new();
            for (i, block) in items.iter().enumerate() {
                let bp = format!("{pointer}/{i}");
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        texts.push(
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        );
                        // cache_control on a text block is a lossless annotation; strip it.
                    }
                    Some("cache_control") => {
                        return Err(UnsupportedFeatures::single(
                            FeatureKind::PromptCache,
                            bp,
                            "system cache_control block is not representable in Chat",
                        ));
                    }
                    Some("thinking") => {
                        // Fail-open: reasoning instructions are dropped, not
                        // rejected.  Recorded for the report.
                        normalized.push(bp);
                    }
                    Some(other) => {
                        return Err(UnsupportedFeatures::single(
                            FeatureKind::UnknownBlock,
                            bp,
                            format!("unsupported system block type {other:?}"),
                        ))
                    }
                    None => {
                        return Err(UnsupportedFeatures::single(
                            FeatureKind::UnknownBlock,
                            bp,
                            "system block missing type",
                        ))
                    }
                }
            }
            Ok(texts.join(""))
        }
        _ => Err(UnsupportedFeatures::single(
            FeatureKind::UnknownBlock,
            pointer,
            "system must be a string or an array of text blocks",
        )),
    }
}

/// Validate and convert an Anthropic tool `input_schema` to an OpenAI
/// `parameters` JSON schema.  Anthropic allows missing `type`, so we normalize
/// to `{"type":"object", ...}` while preserving the rest of the schema.  A
/// non-object schema is rejected.
pub fn anthropic_schema_to_chat_parameters(
    input_schema: &Value,
    pointer: &str,
) -> Result<Value, UnsupportedFeatures> {
    let mut parameters = input_schema.clone();
    if !parameters.is_object() {
        return Err(UnsupportedFeatures::single(
            FeatureKind::InvalidToolArguments,
            pointer,
            "tool input_schema must be a JSON object",
        ));
    }
    if parameters.get("type").is_none() {
        parameters["type"] = Value::String("object".to_string());
    }
    Ok(parameters)
}

/// Validate a user image content block and produce the OpenAI `image_url`
/// value.  Supports both Anthropic base64 and URL sources; enforces media type,
/// data URL form and size.
pub fn anthropic_image_to_chat(block: &Value, pointer: &str) -> Result<Value, UnsupportedFeatures> {
    let source = block.get("source").ok_or_else(|| {
        UnsupportedFeatures::single(
            FeatureKind::Media,
            format!("{pointer}/source"),
            "image block missing source",
        )
    })?;
    match source.get("type").and_then(Value::as_str) {
        Some("url") => {
            let url = source.get("url").and_then(Value::as_str).ok_or_else(|| {
                UnsupportedFeatures::single(
                    FeatureKind::Media,
                    format!("{pointer}/source/url"),
                    "image URL source missing url",
                )
            })?;
            if !url.starts_with("http://")
                && !url.starts_with("https://")
                && !url.starts_with("data:")
            {
                return Err(UnsupportedFeatures::single(
                    FeatureKind::Media,
                    format!("{pointer}/source/url"),
                    "image url must be http(s) or a data URL",
                ));
            }
            Ok(serde_json::json!({"url": url}))
        }
        Some("base64") => {
            let media_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    UnsupportedFeatures::single(
                        FeatureKind::Media,
                        format!("{pointer}/source/media_type"),
                        "base64 image missing media_type",
                    )
                })?;
            if !media_type.starts_with("image/") {
                return Err(UnsupportedFeatures::single(
                    FeatureKind::Media,
                    format!("{pointer}/source/media_type"),
                    format!("unsupported image media type {media_type:?}"),
                ));
            }
            let data = source.get("data").and_then(Value::as_str).ok_or_else(|| {
                UnsupportedFeatures::single(
                    FeatureKind::Media,
                    format!("{pointer}/source/data"),
                    "base64 image missing data",
                )
            })?;
            if data.len() > MAX_IMAGE_BYTES {
                return Err(UnsupportedFeatures::single(
                    FeatureKind::Media,
                    format!("{pointer}/source/data"),
                    "base64 image exceeds the size limit",
                ));
            }
            Ok(serde_json::json!({"url": format!("data:{media_type};base64,{data}")}))
        }
        Some(other) => Err(UnsupportedFeatures::single(
            FeatureKind::UnsupportedField,
            format!("{pointer}/source/type"),
            format!("unsupported image source type {other:?}"),
        )),
        None => Err(UnsupportedFeatures::single(
            FeatureKind::Media,
            format!("{pointer}/source/type"),
            "image source missing type",
        )),
    }
}
