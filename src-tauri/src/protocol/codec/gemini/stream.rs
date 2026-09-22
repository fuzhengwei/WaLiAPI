//! Gemini SSE → Chat Completions SSE。
//! 每条记录都检查语义阻断，不能让 promptFeedback 错误被正常 stop 掩盖。

use super::super::error::DecodeError;
use super::super::ports::StreamDecoder;
use super::super::report::{ConversionContext, Usage};
use super::super::sse;
use serde_json::{json, Value};

pub struct GeminiToChatStreamDecoder {
    context: ConversionContext,
    pending: Vec<u8>,
    started: bool,
    ended: bool,
    saw_tool_call: bool,
    next_tool_index: usize,
    usage: Usage,
}

impl GeminiToChatStreamDecoder {
    pub fn boxed(context: &ConversionContext) -> Box<dyn StreamDecoder + Send + Sync> {
        Box::new(Self {
            context: context.clone(),
            pending: Vec::new(),
            started: false,
            ended: false,
            saw_tool_call: false,
            next_tool_index: 0,
            usage: Usage {
                usage_unknown: true,
                ..Usage::default()
            },
        })
    }

    fn emit_role(&mut self) -> String {
        sse::data_frame(json!({
            "id": self.context.request_id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": self.context.upstream_model,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant" },
                "finish_reason": null
            }]
        }))
    }

    fn emit_text(&self, text: &str) -> String {
        sse::data_frame(json!({
            "id": self.context.request_id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": self.context.upstream_model,
            "choices": [{
                "index": 0,
                "delta": { "content": text },
                "finish_reason": null
            }]
        }))
    }

    fn emit_reasoning(&self, text: &str) -> String {
        sse::data_frame(json!({
            "id": self.context.request_id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": self.context.upstream_model,
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": text },
                "finish_reason": null
            }]
        }))
    }

    fn emit_tool_call(&mut self, name: &str, args: &Value, signature: Option<&str>) -> String {
        let index = self.next_tool_index;
        self.next_tool_index += 1;
        self.saw_tool_call = true;
        let arguments = serde_json::to_string(args).unwrap_or_else(|_| "{}".into());
        // Gemini 3 的 functionCall 带 thoughtSignature，必须随 id 带回上游。
        let id = super::tool_call_id_with_signature(
            &format!("call_{}", uuid::Uuid::new_v4().simple()),
            signature,
        );
        sse::data_frame(json!({
            "id": self.context.request_id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": self.context.upstream_model,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments }
                    }]
                },
                "finish_reason": null
            }]
        }))
    }

    fn emit_stop(&self, reason: &str) -> String {
        sse::data_frame(json!({
            "id": self.context.request_id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": self.context.upstream_model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": reason
            }]
        }))
    }
}

impl StreamDecoder for GeminiToChatStreamDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, DecodeError> {
        self.pending.extend_from_slice(bytes);
        if sse::pending_exceeded(&self.pending) {
            return Err(DecodeError::new("/", sse::pending_overflow_message()));
        }
        let mut out = Vec::new();
        loop {
            let Some(len) = sse::record_end(&self.pending) else {
                break;
            };
            let record: Vec<u8> = self.pending.drain(..len).collect();
            let payload =
                sse::parse_data_payload(&record).map_err(|e| DecodeError::from_unsupported(e))?;
            if payload.is_empty() || payload == "[DONE]" {
                if payload == "[DONE]" {
                    self.ended = true;
                }
                continue;
            }
            let event: Value = serde_json::from_str(&payload).map_err(|error| {
                DecodeError::new("/", format!("Gemini SSE JSON was invalid: {error}"))
            })?;
            let vertex = event.get("response").unwrap_or(&event);
            if let Some(feedback) = vertex.get("promptFeedback") {
                let reason = feedback
                    .get("blockReason")
                    .and_then(Value::as_str)
                    .filter(|reason| !reason.is_empty() && *reason != "BLOCK_REASON_UNSPECIFIED");
                if let Some(reason) = reason {
                    let message = feedback
                        .get("blockReasonMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("prompt blocked");
                    return Err(DecodeError::new(
                        "/promptFeedback/blockReason",
                        format!("Gemini promptFeedback {reason}: {message}"),
                    ));
                }
            }
            if let Some(meta) = vertex.get("usageMetadata") {
                if let Some(p) = meta.get("promptTokenCount").and_then(Value::as_u64) {
                    self.usage.input_tokens = p;
                    self.usage.usage_unknown = false;
                }
                if let Some(c) = meta.get("candidatesTokenCount").and_then(Value::as_u64) {
                    self.usage.output_tokens = c;
                    self.usage.usage_unknown = false;
                }
            }
            let candidate = vertex
                .get("candidates")
                .and_then(Value::as_array)
                .and_then(|a| a.first());
            let Some(candidate) = candidate else {
                continue;
            };
            if !self.started {
                self.started = true;
                out.push(self.emit_role());
            }
            if let Some(parts) = candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
            {
                for part in parts {
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                out.push(self.emit_reasoning(text));
                            }
                        }
                        continue;
                    }
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            out.push(self.emit_text(text));
                        }
                    }
                    if let Some(call) = part.get("functionCall") {
                        let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                        let args = call.get("args").cloned().unwrap_or(json!({}));
                        let signature = part.get("thoughtSignature").and_then(Value::as_str);
                        out.push(self.emit_tool_call(name, &args, signature));
                    }
                }
            }
            if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                let mapped = match reason {
                    "MAX_TOKENS" => "length",
                    "STOP" | "FINISH_REASON_UNSPECIFIED" | "OTHER" => {
                        if self.saw_tool_call {
                            "tool_calls"
                        } else {
                            "stop"
                        }
                    }
                    other => {
                        return Err(DecodeError::new(
                            "/candidates/0/finishReason",
                            format!("unsupported Gemini finishReason {other}"),
                        ))
                    }
                };
                out.push(self.emit_stop(mapped));
                out.push("data: [DONE]\n\n".to_owned());
                self.ended = true;
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
        if !self.pending.is_empty() {
            return Err(DecodeError::new("/", "upstream SSE ended mid-record"));
        }
        if self.ended {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        if !self.started {
            out.push(self.emit_role());
        }
        out.push(self.emit_stop("stop"));
        out.push("data: [DONE]\n\n".to_owned());
        self.ended = true;
        Ok(out)
    }

    fn usage(&self) -> Option<Usage> {
        Some(self.usage)
    }

    fn saw_terminal(&self) -> bool {
        self.ended
    }
}
