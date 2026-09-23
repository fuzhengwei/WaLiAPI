//! Native protocol directions for the three matrix diagonal cells.

use super::direction::CodecDirection;
use super::error::{DecodeError, FeatureKind, PrepareError, UnsupportedFeatures};
use super::ports::{DecodedResponse, NonStreamDecoder, StreamDecoder};
use super::report::{ConversionContext, Usage};
use super::sse;
use super::types::{CodecId, Protocol};
use serde_json::Value;

/// A typed identity strategy. Three separate static values are registered so
/// each strategy always self-reports its concrete protocol pair.
pub struct IdentityDirection {
    protocol: Protocol,
}

impl IdentityDirection {
    pub const fn new(protocol: Protocol) -> Self {
        Self { protocol }
    }
}

/// 转发 Responses 历史输入前，修正旧版协议转换产生的非法条目：
/// - `function_call.id` 必须带 `fc_` 前缀；上游 tool_call id 只属于 `call_id`。
/// - `reasoning.content` 输入必须为空；旧明文推理迁移到 `summary`。
/// - `store=false` 时移除没有 `encrypted_content` 的本地 `rs_*` id，避免上游查找
///   从未持久化的 reasoning 条目。
///
/// 这些条目已经写入客户端旧会话，因此需要在 identity 回放路径兼容处理。
fn normalize_responses_input_items(request: &mut Value) {
    let store_is_disabled = request.get("store").and_then(Value::as_bool) == Some(false);
    let Some(items) = request.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items.iter_mut() {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let current = item.get("id").and_then(Value::as_str).unwrap_or("");
                if current.starts_with("fc_") {
                    continue;
                }
                let normalized = match current.strip_prefix("call_") {
                    // `call_xxx` → `fc_xxx`：确定且可重复，同一轮重试不会换 id。
                    Some(rest) if !rest.is_empty() => format!("fc_{rest}"),
                    _ => format!("fc_{}", uuid::Uuid::new_v4().simple()),
                };
                if let Some(object) = item.as_object_mut() {
                    object.insert("id".to_owned(), Value::String(normalized));
                }
            }
            Some("reasoning") => {
                let Some(object) = item.as_object_mut() else {
                    continue;
                };

                // store=false 时，只有带 encrypted_content 的 reasoning 才能跨请求
                // 回放。Chat/Messages 转换产生的 rs_* 只是本地条目 id，官方上游并未
                // 持久化；保留它会在修正 content 后继续报 "Item ... not found"。
                let has_encrypted_content = object
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .map(|content| !content.is_empty())
                    .unwrap_or(false);
                let has_legacy_content = object
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|content| !content.is_empty())
                    .unwrap_or(false);
                if !has_encrypted_content && (store_is_disabled || has_legacy_content) {
                    object.remove("id");
                }

                let Some(content) = object.get("content").and_then(Value::as_array) else {
                    continue;
                };
                if content.is_empty() {
                    continue;
                }

                // 旧版 Chat→Responses 把明文推理放在 reasoning.content 中；
                // 官方 Responses 输入侧要求该数组长度为 0。迁移到受支持的 summary，
                // 既让旧会话可恢复，也避免丢掉可读推理内容。
                let legacy_summary = content
                    .iter()
                    .filter(|part| {
                        part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                    })
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .map(|text| serde_json::json!({"type": "summary_text", "text": text}))
                    .collect::<Vec<_>>();
                object.remove("content");

                let summary_is_empty = object
                    .get("summary")
                    .and_then(Value::as_array)
                    .map(|summary| summary.is_empty())
                    .unwrap_or(true);
                if summary_is_empty && !legacy_summary.is_empty() {
                    object.insert("summary".to_owned(), Value::Array(legacy_summary));
                }
            }
            _ => {}
        }
    }
}

impl CodecDirection for IdentityDirection {
    fn id(&self) -> CodecId {
        CodecId::Native
    }

    fn downstream(&self) -> Protocol {
        self.protocol
    }

    fn upstream(&self) -> Protocol {
        self.protocol
    }

    fn encode_request(
        &self,
        request: &Value,
        mapped_model: &str,
    ) -> Result<(Value, ConversionContext), PrepareError> {
        let mut encoded = request.clone();
        let object = encoded.as_object_mut().ok_or_else(|| {
            UnsupportedFeatures::single(
                FeatureKind::UnsupportedField,
                "/",
                "identity codec request must be an object",
            )
        })?;
        object.insert("model".to_owned(), Value::String(mapped_model.to_owned()));
        // Downstream non-stream requests usually omit `stream` (false is the
        // API default).  Some upstreams (e.g. anthropic proxies) stream by
        // default when the field is absent, which desyncs the native non-stream
        // facade into an "undecodable body" 502.  Pin the contract explicitly
        // for every protocol: `stream: false` is semantically identical to
        // omitting it on providers that respect the field, and forces
        // default-streaming upstreams into non-stream mode.
        if !object.contains_key("stream") {
            object.insert("stream".to_owned(), Value::Bool(false));
        }
        if self.protocol == Protocol::Responses {
            normalize_responses_input_items(&mut encoded);
        }
        let request_id = request
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let stream = request
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok((
            encoded,
            ConversionContext::new(request_id, mapped_model, stream),
        ))
    }

    fn new_response_decoder(
        &self,
        _context: &ConversionContext,
    ) -> Box<dyn NonStreamDecoder + Send + Sync> {
        Box::new(IdentityNonStreamDecoder {
            protocol: self.protocol,
        })
    }

    fn new_stream_response_decoder(
        &self,
        _context: &ConversionContext,
    ) -> Box<dyn StreamDecoder + Send + Sync> {
        Box::new(IdentityStreamDecoder {
            protocol: self.protocol,
            pending: Vec::new(),
            usage: None,
            saw_input_usage: false,
            saw_output_usage: false,
            done: false,
        })
    }
}

struct IdentityNonStreamDecoder {
    protocol: Protocol,
}

impl NonStreamDecoder for IdentityNonStreamDecoder {
    fn decode(&self, body: &Value) -> Result<DecodedResponse, DecodeError> {
        Ok(DecodedResponse {
            body: body.clone(),
            usage: parse_usage(self.protocol, body),
        })
    }
}

struct IdentityStreamDecoder {
    protocol: Protocol,
    pending: Vec<u8>,
    usage: Option<Usage>,
    saw_input_usage: bool,
    saw_output_usage: bool,
    done: bool,
}

impl IdentityStreamDecoder {
    fn consume_record(&mut self, record: &[u8]) -> Result<(), DecodeError> {
        let payload = sse::parse_data_payload(record).map_err(DecodeError::from)?;
        if payload.is_empty() {
            return Ok(());
        }
        if payload == "[DONE]" {
            if self.done {
                return Err(DecodeError::new("/", "duplicate SSE [DONE] record"));
            }
            self.done = true;
            return Ok(());
        }
        let event: Value = serde_json::from_str(&payload).map_err(|error| {
            DecodeError::new("/", format!("upstream SSE JSON was invalid: {error}"))
        })?;
        self.merge_event_usage(&event);
        Ok(())
    }

    fn merge_event_usage(&mut self, event: &Value) {
        let usage = match self.protocol {
            Protocol::Chat => event.get("usage"),
            Protocol::Messages => event
                .get("usage")
                .or_else(|| event.pointer("/message/usage")),
            Protocol::Responses => event
                .get("usage")
                .or_else(|| event.pointer("/response/usage")),
            Protocol::Gemini => event
                .get("usageMetadata")
                .or_else(|| event.pointer("/response/usageMetadata")),
        };
        let Some(usage) = usage else { return };
        let merged = self.usage.get_or_insert_with(|| Usage {
            usage_unknown: true,
            ..Usage::default()
        });
        let (input_key, output_key) = match self.protocol {
            Protocol::Chat => ("prompt_tokens", "completion_tokens"),
            Protocol::Messages | Protocol::Responses => ("input_tokens", "output_tokens"),
            Protocol::Gemini => ("promptTokenCount", "candidatesTokenCount"),
        };
        if let Some(input) = usage.get(input_key).and_then(Value::as_u64) {
            merged.input_tokens = input;
            self.saw_input_usage = true;
        }
        if let Some(output) = usage.get(output_key).and_then(Value::as_u64) {
            merged.output_tokens = output;
            self.saw_output_usage = true;
        }
        if let Some(cache_creation) = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
        {
            merged.cache_creation_input_tokens = cache_creation;
        }
        if let Some(cache_read) = usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .or_else(|| {
                // DeepSeek-compatible upstreams report cache hits with their
                // own field names instead of OpenAI *_details or Anthropic
                // cache_read_input_tokens.
                usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64)
            })
        {
            merged.cache_read_input_tokens = cache_read;
        }
        merged.usage_unknown = !(self.saw_input_usage && self.saw_output_usage);
    }

    /// Once `[DONE]` has been forwarded, the downstream response is complete.
    /// Some OpenAI-compatible upstreams nevertheless flush further records.
    /// They cannot be delivered to the client, but a well-formed usage object
    /// can still improve the audit log. Parsing failures are deliberately
    /// ignored here: post-terminal bytes must not reverse a completed stream
    /// into a gateway failure.
    fn capture_trailing_usage(&mut self, record: &[u8]) {
        let Ok(payload) = sse::parse_data_payload(record) else {
            return;
        };
        let Ok(event) = serde_json::from_str::<Value>(&payload) else {
            return;
        };
        self.merge_event_usage(&event);
    }
}

impl StreamDecoder for IdentityStreamDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, DecodeError> {
        self.pending.extend_from_slice(bytes);
        if sse::pending_exceeded(&self.pending) {
            return Err(DecodeError::new("/", sse::pending_overflow_message()));
        }
        let mut output = Vec::new();
        while let Some(end) = sse::record_end(&self.pending) {
            let record: Vec<u8> = self.pending.drain(..end).collect();
            // `[DONE]` is already on its way to the client, so no later record
            // can be usefully or safely delivered. Discard any non-compliant
            // trailing upstream event instead of reporting a 502 for a stream
            // that was successfully completed downstream.
            if self.done {
                self.capture_trailing_usage(&record);
                log::warn!("discarded an upstream SSE record after terminal [DONE]");
                continue;
            }
            self.consume_record(&record)?;
            output.push(
                String::from_utf8(record).map_err(|_| {
                    DecodeError::new("/", "upstream SSE record was not valid UTF-8")
                })?,
            );
        }
        if self.done && !self.pending.is_empty() {
            // Do not retain an unterminated trailing fragment indefinitely, or
            // turn a valid completed stream into `ended mid-record` at EOF.
            log::warn!(
                "discarded {} trailing upstream bytes after terminal [DONE]",
                self.pending.len()
            );
            self.pending.clear();
        }
        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
        if !self.pending.is_empty() {
            return Err(DecodeError::new("/", "upstream SSE ended mid-record"));
        }
        Ok(Vec::new())
    }

    fn usage(&self) -> Option<Usage> {
        self.usage
    }
}

/// Extract the common usage shapes without performing a second protocol parse.
pub(crate) fn parse_usage(protocol: Protocol, body: &Value) -> Option<Usage> {
    let usage = match protocol {
        Protocol::Gemini => body
            .get("usageMetadata")
            .or_else(|| body.pointer("/response/usageMetadata"))?,
        _ => body.get("usage")?,
    };
    let input = match protocol {
        Protocol::Chat => usage.get("prompt_tokens").and_then(Value::as_u64),
        Protocol::Messages | Protocol::Responses => {
            usage.get("input_tokens").and_then(Value::as_u64)
        }
        Protocol::Gemini => usage.get("promptTokenCount").and_then(Value::as_u64),
    };
    let output = match protocol {
        Protocol::Chat => usage.get("completion_tokens").and_then(Value::as_u64),
        Protocol::Messages | Protocol::Responses => {
            usage.get("output_tokens").and_then(Value::as_u64)
        }
        Protocol::Gemini => usage.get("candidatesTokenCount").and_then(Value::as_u64),
    };
    Some(Usage {
        input_tokens: input.unwrap_or_default(),
        output_tokens: output.unwrap_or_default(),
        cache_creation_input_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_read_input_tokens: usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64)
            })
            .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
            .unwrap_or_default(),
        usage_unknown: input.is_none() || output.is_none(),
    })
}

pub static CHAT_IDENTITY: IdentityDirection = IdentityDirection::new(Protocol::Chat);
pub static MESSAGES_IDENTITY: IdentityDirection = IdentityDirection::new(Protocol::Messages);
pub static RESPONSES_IDENTITY: IdentityDirection = IdentityDirection::new(Protocol::Responses);

#[cfg(test)]
#[path = "identity_tests.rs"]
mod identity_tests;
