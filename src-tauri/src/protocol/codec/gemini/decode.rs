//! Gemini generateContent 响应 → Chat Completions JSON。
//! 对上游错误和缺失字段保持 fail-closed，不把异常响应伪装成成功。

use super::super::error::{DecodeError, FeatureKind, UnsupportedFeatures};
use super::super::ports::{DecodedResponse, NonStreamDecoder};
use super::super::report::{ConversionContext, Usage};
use serde_json::{json, Value};

pub struct GeminiToChatDecoder {
    context: ConversionContext,
}

impl GeminiToChatDecoder {
    pub fn boxed(context: &ConversionContext) -> Box<dyn NonStreamDecoder + Send + Sync> {
        Box::new(Self {
            context: context.clone(),
        })
    }
}

impl NonStreamDecoder for GeminiToChatDecoder {
    fn decode(&self, body: &Value) -> Result<DecodedResponse, DecodeError> {
        decode_gemini_to_chat(body, &self.context)
            .map(|(body, usage)| DecodedResponse { body, usage })
            .map_err(DecodeError::from_unsupported)
    }
}

pub fn decode_gemini_to_chat(
    body: &Value,
    context: &ConversionContext,
) -> Result<(Value, Option<Usage>), UnsupportedFeatures> {
    let vertex = body.get("response").unwrap_or(body);
    let candidate = vertex
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::UnknownEvent,
                "/candidates/0",
                "Gemini response missing candidates[0]",
            )
        })?;

    if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
        match reason {
            "STOP" | "MAX_TOKENS" | "FINISH_REASON_UNSPECIFIED" | "OTHER" => {}
            unknown => {
                return Err(UnsupportedFeatures::single(
                    FeatureKind::UnknownFinishReason,
                    "/candidates/0/finishReason",
                    format!("unsupported Gemini finishReason {unknown}"),
                ))
            }
        }
    }

    let parts = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut thinking = String::new();
    for part in &parts {
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                thinking.push_str(t);
            }
            continue;
        }
        if let Some(t) = part.get("text").and_then(Value::as_str) {
            text.push_str(t);
        }
        if let Some(call) = part.get("functionCall") {
            let name = call.get("name").and_then(Value::as_str).unwrap_or("");
            let args = call.get("args").cloned().unwrap_or(json!({}));
            let args_text = serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
            // Gemini 3 的 functionCall 带 thoughtSignature，必须随 id 带回上游。
            let base_id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
            let id = super::tool_call_id_with_signature(
                &base_id,
                part.get("thoughtSignature").and_then(Value::as_str),
            );
            tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": args_text }
            }));
        }
    }

    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls"
    } else {
        match candidate.get("finishReason").and_then(Value::as_str) {
            Some("MAX_TOKENS") => "length",
            _ => "stop",
        }
    };

    let mut message = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
    });
    if !thinking.is_empty() {
        message["reasoning_content"] = json!(thinking);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    let usage_meta = vertex
        .get("usageMetadata")
        .or_else(|| body.get("usageMetadata"));
    let prompt = usage_meta
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage_meta
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let usage_unknown = usage_meta.is_none();

    let out = json!({
        "id": context.request_id,
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": context.upstream_model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion,
        }
    });

    Ok((
        out,
        Some(Usage {
            input_tokens: prompt,
            output_tokens: completion,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: usage_meta
                .and_then(|u| u.get("cachedContentTokenCount"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            usage_unknown,
        }),
    ))
}
