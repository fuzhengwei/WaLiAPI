//! Chat / Messages / Responses → Gemini generateContent 协议编解码器。
//!
//! `gemini` 是内部协议标识；认证与 UI 品牌已迁移为 Antigravity，但线协议
//! 和数据库兼容性仍由本模块保持。

mod decode;
mod encode;
mod stream;

use super::chat;
use super::direction::CodecDirection;
use super::error::{DecodeError, FeatureKind, PrepareError, UnsupportedFeatures};
use super::messages;
use super::ports::{DecodedResponse, NonStreamDecoder, StreamDecoder};
use super::report::ConversionContext;
use super::request;
use super::types::{CodecId, Protocol};
use serde_json::Value;

pub use decode::{decode_gemini_to_chat, GeminiToChatDecoder};
pub use encode::encode_chat_to_gemini;
pub use stream::GeminiToChatStreamDecoder;

pub static CHAT_TO_GEMINI: ChatToGemini = ChatToGemini;
pub static MESSAGES_TO_GEMINI: MessagesToGemini = MessagesToGemini;
pub static RESPONSES_TO_GEMINI: ResponsesToGemini = ResponsesToGemini;

pub struct ChatToGemini;
impl CodecDirection for ChatToGemini {
    fn id(&self) -> CodecId {
        CodecId::ChatToGeminiV1
    }
    fn downstream(&self) -> Protocol {
        Protocol::Chat
    }
    fn upstream(&self) -> Protocol {
        Protocol::Gemini
    }
    fn encode_request(
        &self,
        request: &Value,
        model: &str,
    ) -> Result<(Value, ConversionContext), PrepareError> {
        encode_chat_to_gemini(request, model)
    }
    fn new_response_decoder(
        &self,
        context: &ConversionContext,
    ) -> Box<dyn NonStreamDecoder + Send + Sync> {
        GeminiToChatDecoder::boxed(context)
    }
    fn new_stream_response_decoder(
        &self,
        context: &ConversionContext,
    ) -> Box<dyn StreamDecoder + Send + Sync> {
        GeminiToChatStreamDecoder::boxed(context)
    }
}

pub struct MessagesToGemini;
impl CodecDirection for MessagesToGemini {
    fn id(&self) -> CodecId {
        CodecId::MessagesToGeminiV1
    }
    fn downstream(&self) -> Protocol {
        Protocol::Messages
    }
    fn upstream(&self) -> Protocol {
        Protocol::Gemini
    }
    fn encode_request(
        &self,
        request: &Value,
        model: &str,
    ) -> Result<(Value, ConversionContext), PrepareError> {
        let (chat, first_context) = messages::encode_messages_to_chat(request, model)?;
        let (encoded, mut context) = encode_chat_to_gemini(&chat, model)?;
        merge_conversion_context(&mut context, &first_context);
        Ok((encoded, context))
    }
    fn new_response_decoder(
        &self,
        context: &ConversionContext,
    ) -> Box<dyn NonStreamDecoder + Send + Sync> {
        Box::new(GeminiThenChatToMessages {
            inner: context.clone(),
        })
    }
    fn new_stream_response_decoder(
        &self,
        context: &ConversionContext,
    ) -> Box<dyn StreamDecoder + Send + Sync> {
        Box::new(PipeGeminiToChatThen {
            gemini: GeminiToChatStreamDecoder::boxed(context),
            next: chat::ChatStreamDecoder::boxed(context),
        })
    }
}

fn validate_response_message_item(
    item: &Value,
    pointer: &str,
    rejected: &mut Vec<super::error::RejectedField>,
) {
    if let Some(content) = item.get("content") {
        if let Some(blocks) = content.as_array() {
            for (block_index, block) in blocks.iter().enumerate() {
                let block_pointer = format!("{pointer}/content/{block_index}");
                match block.get("type").and_then(Value::as_str) {
                    Some("input_text") | Some("output_text") | Some("text") => {
                        if block.get("text").and_then(Value::as_str).is_none() {
                            rejected.push(super::error::RejectedField {
                                code: FeatureKind::UnknownBlock.code().to_owned(),
                                pointer: format!("{block_pointer}/text"),
                                message: "text content requires a string text field".to_owned(),
                            });
                        }
                    }
                    Some("input_image") => {
                        let image_url = block.get("image_url").and_then(Value::as_str);
                        match image_url {
                            Some(url) if url.starts_with("data:") => {}
                            Some(_) => rejected.push(super::error::RejectedField {
                                code: FeatureKind::Media.code().to_owned(),
                                pointer: format!("{block_pointer}/image_url"),
                                message: "Gemini only supports data URI input images".to_owned(),
                            }),
                            None => rejected.push(super::error::RejectedField {
                                code: FeatureKind::Media.code().to_owned(),
                                pointer: format!("{block_pointer}/image_url"),
                                message: "input_image requires an image_url string".to_owned(),
                            }),
                        }
                    }
                    Some("input_file") | Some("file") => {
                        rejected.push(super::error::RejectedField {
                            code: FeatureKind::Document.code().to_owned(),
                            pointer: format!("{block_pointer}/type"),
                            message: "Gemini Responses conversion does not support file input"
                                .to_owned(),
                        })
                    }
                    Some(other) => rejected.push(super::error::RejectedField {
                        code: FeatureKind::UnknownBlock.code().to_owned(),
                        pointer: format!("{block_pointer}/type"),
                        message: format!("unsupported Responses content block {other:?}"),
                    }),
                    None => rejected.push(super::error::RejectedField {
                        code: FeatureKind::UnknownBlock.code().to_owned(),
                        pointer: block_pointer,
                        message: "Responses content block is missing type".to_owned(),
                    }),
                }
            }
        } else if !content.is_string() {
            rejected.push(super::error::RejectedField {
                code: FeatureKind::UnknownBlock.code().to_owned(),
                pointer: format!("{pointer}/content"),
                message: "Responses message content must be a string or array".to_owned(),
            });
        }
    }
}

struct GeminiThenChatToMessages {
    inner: ConversionContext,
}

impl NonStreamDecoder for GeminiThenChatToMessages {
    fn decode(&self, body: &Value) -> Result<DecodedResponse, DecodeError> {
        let (chat, usage) =
            decode_gemini_to_chat(body, &self.inner).map_err(DecodeError::from_unsupported)?;
        let messages = chat::decode_chat_response_to_messages(&chat, &self.inner)
            .map_err(DecodeError::from_unsupported)?;
        Ok(DecodedResponse {
            body: messages,
            usage,
        })
    }
}

/// Gemini 3 要求随 functionCall part 原样回传模型给的 `thoughtSignature`，否则
/// 上游 400（`Function call is missing a thought_signature`）。三种下游协议都没有
/// 承载它的字段，而客户端必定回传 tool call id，故附在 id 尾部：
/// `call_<uuid>.<signature>`。
pub(super) fn tool_call_id_with_signature(id: &str, signature: Option<&str>) -> String {
    match signature.filter(|sig| looks_like_thought_signature(sig)) {
        Some(sig) => format!("{id}.{sig}"),
        None => id.to_owned(),
    }
}

/// [`tool_call_id_with_signature`] 的逆向：拆出原始 id 与签名。
pub(super) fn split_tool_call_id(id: &str) -> (&str, Option<&str>) {
    match id.split_once('.') {
        Some((base, sig)) if looks_like_thought_signature(sig) => (base, Some(sig)),
        _ => (id, None),
    }
}

/// 只把确实像签名的尾部当签名（base64 字符集、有足够长度），避免误伤
/// 自带 `.` 的第三方 tool call id。
fn looks_like_thought_signature(value: &str) -> bool {
    value.len() >= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))
}

/// Antigravity/Code Assist 不识别 Responses 的 namespace 工具 marker。
///
/// 这类工具是调用方用于分组或路由的元数据，不是 Gemini
/// Responses 的工具类型里，`namespace`（分组 marker）与 `web_search` /
/// `file_search` / `computer_use` 等内置工具在 Gemini `functionDeclarations` 里
/// 没有等价物，保留会让上游 400（`Gemini only supports Responses function
/// tools`）。这里统一移除、只留 `function`，不把内置工具伪装成 function；
/// 指向被移除工具的 `tool_choice` 一并移除，避免悬空引用。
fn normalize_responses_for_gemini(body: &Value) -> (Value, Vec<String>) {
    let tools = body.get("tools").and_then(Value::as_array);
    let filtered_tools: Vec<Value> = tools
        .map(|items| {
            items
                .iter()
                .filter(|tool| tool.get("type").and_then(Value::as_str) == Some("function"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let removed_count = tools
        .map(|items| items.len().saturating_sub(filtered_tools.len()))
        .unwrap_or_default();

    let kept_names: std::collections::HashSet<&str> = filtered_tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    let droppable_tool_choice = body
        .get("tool_choice")
        .and_then(Value::as_object)
        .map(|choice| {
            let ty = choice.get("type").and_then(Value::as_str);
            match ty {
                // 不引用具体工具的选择器可以保留。
                Some("auto" | "none" | "required") => false,
                // 指向函数的 tool_choice 只在函数仍存在时保留。
                Some("function") => choice
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| !kept_names.contains(name))
                    .unwrap_or(true),
                // namespace 与被移除的内置工具：选择器一并移除。
                Some(_) => true,
                None => false,
            }
        })
        .unwrap_or(false);

    let mut normalized_fields = Vec::new();
    if let Some(tools) = tools {
        for (index, tool) in tools.iter().enumerate() {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                normalized_fields.push(format!("/tools/{index}"));
            }
        }
    }
    if droppable_tool_choice {
        normalized_fields.push("/tool_choice".to_owned());
    }
    if removed_count == 0 && !droppable_tool_choice {
        return (body.clone(), normalized_fields);
    }

    let mut normalized = body.clone();
    let Some(normalized_object) = normalized.as_object_mut() else {
        return (body.clone(), normalized_fields);
    };

    if filtered_tools.is_empty() {
        normalized_object.remove("tools");
    } else {
        normalized_object.insert("tools".to_owned(), Value::Array(filtered_tools));
    }
    if droppable_tool_choice {
        normalized_object.remove("tool_choice");
    }

    tracing::debug!(
        removed_tools = removed_count,
        dropped_tool_choice = droppable_tool_choice,
        "normalized unsupported Responses tools for Antigravity Gemini conversion"
    );
    (normalized, normalized_fields)
}

fn merge_conversion_context(target: &mut ConversionContext, first: &ConversionContext) {
    target.request_id = first.request_id.clone();
    target.stream = first.stream;
    let mut normalized = first.normalized.clone();
    normalized.append(&mut target.normalized);
    target.normalized = normalized;
}

fn responses_context(request: &Value, model: &str) -> ConversionContext {
    let request_id = request
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
    let mut context = ConversionContext::new(
        request_id,
        model,
        request
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    for field in [
        "parallel_tool_calls",
        "store",
        "include",
        "prompt_cache_key",
        "prompt_cache_options",
        "client_metadata",
    ] {
        if request.get(field).is_some() {
            context.normalized.push(format!("/{field}"));
        }
    }
    if request.pointer("/text/verbosity").is_some() {
        context.normalized.push("/text/verbosity".to_owned());
    }
    context
}

fn validate_responses_for_gemini(body: &Value) -> Result<(), UnsupportedFeatures> {
    let mut rejected = Vec::new();

    if let Some(tools) = body.get("tools") {
        let Some(tools) = tools.as_array() else {
            rejected.push(super::error::RejectedField {
                code: FeatureKind::UnsupportedField.code().to_owned(),
                pointer: "/tools".to_owned(),
                message: "Responses tools must be an array".to_owned(),
            });
            return Err(UnsupportedFeatures::new(rejected));
        };
        for (index, tool) in tools.iter().enumerate() {
            let pointer = format!("/tools/{index}");
            let ty = tool.get("type").and_then(Value::as_str);
            if ty != Some("function") {
                rejected.push(super::error::RejectedField {
                    code: FeatureKind::BuiltinTool.code().to_owned(),
                    pointer: format!("{pointer}/type"),
                    message: format!("Gemini only supports Responses function tools, found {ty:?}"),
                });
                continue;
            }
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            if name.is_none() {
                rejected.push(super::error::RejectedField {
                    code: FeatureKind::MissingToolField.code().to_owned(),
                    pointer: format!("{pointer}/name"),
                    message: "Responses function tool is missing name".to_owned(),
                });
            }
            if tool.get("parameters").is_none() {
                rejected.push(super::error::RejectedField {
                    code: FeatureKind::InvalidToolArguments.code().to_owned(),
                    pointer: format!("{pointer}/parameters"),
                    message: "Responses function tool is missing parameters".to_owned(),
                });
            }
        }
    }

    let Some(input) = body.get("input") else {
        return request::finish(rejected);
    };
    let Some(items) = input.as_array() else {
        if input.is_string() {
            return request::finish(rejected);
        }
        rejected.push(super::error::RejectedField {
            code: FeatureKind::UnknownBlock.code().to_owned(),
            pointer: "/input".to_owned(),
            message: "Gemini Responses conversion requires string or array input".to_owned(),
        });
        return Err(UnsupportedFeatures::new(rejected));
    };

    for (index, item) in items.iter().enumerate() {
        let pointer = format!("/input/{index}");
        let item_type = item.get("type").and_then(Value::as_str);
        if item_type.is_none() && item.get("role").is_some() {
            validate_response_message_item(item, &pointer, &mut rejected);
            continue;
        }
        match item_type {
            Some("message") => {
                validate_response_message_item(item, &pointer, &mut rejected);
            }
            Some("item") if item.get("role").is_some() => {
                validate_response_message_item(item, &pointer, &mut rejected);
            }
            Some("item") => rejected.push(super::error::RejectedField {
                code: FeatureKind::UnknownBlock.code().to_owned(),
                pointer: format!("{pointer}/role"),
                message: "Responses message item requires a role".to_owned(),
            }),
            Some("function_call") => {
                for (field, kind) in [
                    ("call_id", FeatureKind::MissingToolField),
                    ("name", FeatureKind::MissingToolField),
                ] {
                    if item
                        .get(field)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .is_none()
                    {
                        rejected.push(super::error::RejectedField {
                            code: kind.code().to_owned(),
                            pointer: format!("{pointer}/{field}"),
                            message: format!("function_call requires non-empty {field}"),
                        });
                    }
                }
                if item.get("arguments").and_then(Value::as_str).is_none() {
                    rejected.push(super::error::RejectedField {
                        code: FeatureKind::InvalidToolArguments.code().to_owned(),
                        pointer: format!("{pointer}/arguments"),
                        message: "function_call requires arguments JSON text".to_owned(),
                    });
                }
            }
            Some("function_call_output") => {
                if item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .is_none()
                {
                    rejected.push(super::error::RejectedField {
                        code: FeatureKind::MissingToolField.code().to_owned(),
                        pointer: format!("{pointer}/call_id"),
                        message: "function_call_output requires non-empty call_id".to_owned(),
                    });
                }
            }
            // 这些是 Responses 的已知内置调用/思考记录；Gemini 没有等价物，
            // 可以按既有 fail-open 策略丢弃。未知类型必须拒绝，避免转换器
            // 在 responses_decode.rs 的兜底分支里静默丢掉新协议对象。
            Some(
                "reasoning"
                | "web_search_call"
                | "file_search_call"
                | "computer_call"
                | "computer_call_output"
                | "mcp_call"
                | "mcp_list_tools"
                | "mcp_approval_request"
                | "mcp_approval_response"
                | "code_interpreter_call"
                | "local_shell_call"
                | "local_shell_call_output"
                | "custom_tool_call"
                | "custom_tool_call_output"
                | "image_generation_call"
                | "item_reference",
            ) => {}
            Some(other) => rejected.push(super::error::RejectedField {
                code: FeatureKind::UnknownBlock.code().to_owned(),
                pointer: format!("{pointer}/type"),
                message: format!("unsupported Responses input item type {other:?}"),
            }),
            None => rejected.push(super::error::RejectedField {
                code: FeatureKind::UnknownBlock.code().to_owned(),
                pointer: format!("{pointer}/type"),
                message: "Responses input item is missing type and role".to_owned(),
            }),
        }
    }

    request::finish(rejected)
}

pub struct ResponsesToGemini;
impl CodecDirection for ResponsesToGemini {
    fn id(&self) -> CodecId {
        CodecId::ResponsesToGeminiV1
    }
    fn downstream(&self) -> Protocol {
        Protocol::Responses
    }
    fn upstream(&self) -> Protocol {
        Protocol::Gemini
    }
    fn encode_request(
        &self,
        request: &Value,
        model: &str,
    ) -> Result<(Value, ConversionContext), PrepareError> {
        let (normalized, first_normalized) = normalize_responses_for_gemini(request);
        validate_responses_for_gemini(&normalized)?;
        let mut chat = crate::protocol::responses_to_openai(&normalized)?;
        chat.as_object_mut()
            .ok_or_else(|| {
                UnsupportedFeatures::single(
                    FeatureKind::UnsupportedField,
                    "/",
                    "Responses to Chat encoder produced a non-object request",
                )
            })?
            .insert("model".to_owned(), Value::String(model.to_owned()));
        let (encoded, chat_context) = encode_chat_to_gemini(&chat, model)?;
        let mut context = responses_context(request, model);
        context.normalized.extend(first_normalized);
        context.normalized.extend(chat_context.normalized);
        Ok((encoded, context))
    }
    fn new_response_decoder(
        &self,
        context: &ConversionContext,
    ) -> Box<dyn NonStreamDecoder + Send + Sync> {
        Box::new(GeminiThenChatToResponses {
            inner: context.clone(),
        })
    }
    fn new_stream_response_decoder(
        &self,
        context: &ConversionContext,
    ) -> Box<dyn StreamDecoder + Send + Sync> {
        Box::new(PipeGeminiToChatThen {
            gemini: GeminiToChatStreamDecoder::boxed(context),
            next: Box::new(super::responses_codec::ChatToResponsesStreamDecoder::new(
                context,
            )),
        })
    }
}

struct GeminiThenChatToResponses {
    inner: ConversionContext,
}

impl NonStreamDecoder for GeminiThenChatToResponses {
    fn decode(&self, body: &Value) -> Result<DecodedResponse, DecodeError> {
        let (chat, usage) =
            decode_gemini_to_chat(body, &self.inner).map_err(DecodeError::from_unsupported)?;
        if chat.pointer("/choices/0/message").is_none() {
            return Err(DecodeError::new(
                "/choices/0/message",
                "Chat response missing choices[0].message",
            ));
        }
        Ok(DecodedResponse {
            body: crate::protocol::openai_to_responses(&chat, &self.inner.upstream_model),
            usage,
        })
    }
}

struct PipeGeminiToChatThen {
    gemini: Box<dyn StreamDecoder + Send + Sync>,
    next: Box<dyn StreamDecoder + Send + Sync>,
}

impl StreamDecoder for PipeGeminiToChatThen {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, DecodeError> {
        let frames = self.gemini.feed(bytes)?;
        let mut out = Vec::new();
        for frame in frames {
            out.extend(self.next.feed(frame.as_bytes())?);
        }
        Ok(out)
    }
    fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
        let frames = self.gemini.finish()?;
        let mut out = Vec::new();
        for frame in frames {
            out.extend(self.next.feed(frame.as_bytes())?);
        }
        out.extend(self.next.finish()?);
        Ok(out)
    }
    fn usage(&self) -> Option<super::report::Usage> {
        self.gemini.usage().or_else(|| self.next.usage())
    }
    fn saw_terminal(&self) -> bool {
        self.next.saw_terminal() || self.gemini.saw_terminal()
    }
}

#[cfg(test)]
mod tests {
    use super::encode::GEMINI_SCHEMA_KEYS;
    use super::*;
    use serde_json::json;

    /// 递归校验：发给 Gemini 的 schema 不能出现白名单之外的键（上游会 400）。
    fn assert_only_gemini_schema_keys(schema: &Value) {
        let object = schema.as_object().expect("schema must be an object");
        for (key, value) in object {
            assert!(
                GEMINI_SCHEMA_KEYS.contains(&key.as_str()),
                "unexpected Gemini schema key: {key}"
            );
            match key.as_str() {
                "properties" => {
                    for sub in value.as_object().expect("properties").values() {
                        assert_only_gemini_schema_keys(sub);
                    }
                }
                "items" => assert_only_gemini_schema_keys(value),
                "anyOf" => {
                    for sub in value.as_array().expect("anyOf") {
                        assert_only_gemini_schema_keys(sub);
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn chat_text_round_trip() {
        let req = json!({
            "model": "gemini-2.5-flash",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"}
            ],
            "temperature": 0.2,
            "max_tokens": 16
        });
        let (encoded, ctx) = encode_chat_to_gemini(&req, "gemini-2.5-flash").unwrap();
        assert_eq!(encoded["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(encoded["contents"][0]["role"], "user");
        assert_eq!(encoded["generationConfig"]["maxOutputTokens"], 16);

        let gemini = json!({
            "response": {
                "candidates": [{
                    "content": { "parts": [{ "text": "hello" }] },
                    "finishReason": "STOP"
                }],
                "usageMetadata": { "promptTokenCount": 3, "candidatesTokenCount": 1 }
            }
        });
        let (chat, usage) = decode_gemini_to_chat(&gemini, &ctx).unwrap();
        assert_eq!(chat["choices"][0]["message"]["content"], "hello");
        assert_eq!(chat["choices"][0]["finish_reason"], "stop");
        assert_eq!(usage.unwrap().input_tokens, 3);
    }

    #[test]
    fn tools_map_to_function_call() {
        let req = json!({
            "messages": [
                {"role": "user", "content": "lookup"},
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "lookup", "arguments": "{\"q\":\"x\"}" }
                    }]
                },
                {"role": "tool", "tool_call_id": "c1", "content": "{\"ok\":true}"}
            ],
            "tools": [{
                "type": "function",
                "function": { "name": "lookup", "description": "d", "parameters": { "type": "object" } }
            }]
        });
        let (encoded, _) = encode_chat_to_gemini(&req, "m").unwrap();
        assert_eq!(
            encoded["contents"][1]["parts"][0]["functionCall"]["name"],
            "lookup"
        );
        assert_eq!(
            encoded["contents"][1]["parts"][0]["functionCall"]["id"],
            "c1"
        );
        assert_eq!(
            encoded["contents"][2]["parts"][0]["functionResponse"]["name"],
            "lookup"
        );
        assert_eq!(
            encoded["contents"][2]["parts"][0]["functionResponse"]["id"],
            "c1"
        );
    }

    #[test]
    fn responses_parallel_tool_calls_keep_distinct_ids_for_antigravity() {
        let request = json!({
            "model": "claude-sonnet-4.6",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "run both"}]},
                {"type": "function_call", "id": "fc_1", "call_id": "call_first", "name": "exec_command", "arguments": "{}"},
                {"type": "function_call", "id": "fc_2", "call_id": "call_second", "name": "exec_command", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_first", "output": "one"},
                {"type": "function_call_output", "call_id": "call_second", "output": "two"}
            ],
            "tools": [{"type": "function", "name": "exec_command", "parameters": {"type": "object"}}]
        });
        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&request, "claude-sonnet-4.6")
            .unwrap();
        assert_eq!(
            encoded["contents"][1]["parts"][0]["functionCall"]["id"],
            "call_first"
        );
        assert_eq!(
            encoded["contents"][1]["parts"][1]["functionCall"]["id"],
            "call_second"
        );
        assert_eq!(
            encoded["contents"][2]["parts"][0]["functionResponse"]["id"],
            "call_first"
        );
        assert_eq!(
            encoded["contents"][3]["parts"][0]["functionResponse"]["id"],
            "call_second"
        );
    }

    #[test]
    fn tool_choice_maps_to_function_calling_config() {
        let req = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": { "name": "lookup", "parameters": { "type": "object" } }
            }],
            "tool_choice": {"type": "function", "function": {"name": "lookup"}}
        });
        let (encoded, ctx) = encode_chat_to_gemini(&req, "m").unwrap();
        assert_eq!(
            encoded["toolConfig"]["functionCallingConfig"]["mode"],
            "ANY"
        );
        assert_eq!(
            encoded["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
            json!(["lookup"])
        );
        assert!(ctx.normalized.iter().any(|p| p == "/tool_choice"));
    }

    #[test]
    fn response_format_json_schema_is_inline_only() {
        let req = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "item",
                    "schema": {
                        "type": "object",
                        "properties": { "id": { "type": "string" } }
                    }
                }
            }
        });
        let (encoded, _) = encode_chat_to_gemini(&req, "m").unwrap();
        assert_eq!(
            encoded["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            encoded["generationConfig"]["responseSchema"]["properties"]["id"]["type"],
            "string"
        );

        let remote = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {
                "type": "json_schema",
                "json_schema": { "schema": "https://example.com/schema.json" }
            }
        });
        let err = encode_chat_to_gemini(&remote, "m").unwrap_err();
        assert!(err.json_pointers.iter().any(|p| p.contains("json_schema")));
    }

    #[test]
    fn response_schema_is_sanitized_for_gemini() {
        let req = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "item",
                    "schema": {
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"id": {"type": "string"}}
                    }
                }
            }
        });
        let (encoded, _) = encode_chat_to_gemini(&req, "m").unwrap();
        let schema = &encoded["generationConfig"]["responseSchema"];
        assert!(schema.get("$schema").is_none());
        assert!(schema.get("additionalProperties").is_none());
        assert_eq!(schema["properties"]["id"]["type"], json!("string"));
    }

    #[test]
    fn remote_image_is_rejected() {
        let req = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": { "url": "https://example.com/a.png" }
                }]
            }]
        });
        let err = encode_chat_to_gemini(&req, "m").unwrap_err();
        assert!(err.json_pointers.iter().any(|p| p.contains("image_url")));
    }

    #[test]
    fn empty_parts_are_dropped() {
        let req = json!({
            "messages": [
                {"role": "assistant", "content": ""},
                {"role": "user", "content": "hi"}
            ]
        });
        let (encoded, _) = encode_chat_to_gemini(&req, "m").unwrap();
        assert_eq!(encoded["contents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn safety_finish_reason_is_not_stop() {
        let ctx = ConversionContext::new("id", "m", false);
        let err = decode_gemini_to_chat(
            &json!({
                "candidates": [{
                    "content": { "parts": [{ "text": "nope" }] },
                    "finishReason": "SAFETY"
                }]
            }),
            &ctx,
        )
        .unwrap_err();
        assert!(err.message.contains("SAFETY"));
    }

    /// Claude Code 的工具 input_schema 是标准 JSON Schema，带 Gemini
    /// `Schema` 不认识的键；这些键必须被清洗掉，而不是原样发给上游。
    #[test]
    fn antigravity_tool_schema_drops_unsupported_json_schema_keywords() {
        let req = json!({
            "model": "gemini-3.8-flash-medium",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "name": "lookup",
                "description": "look up a value",
                "input_schema": {
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "additionalProperties": false,
                    "$defs": {"opt": {"type": ["string", "null"]}},
                    "properties": {
                        "q": {"type": "string", "examples": ["a"]},
                        "opt": {"$ref": "#/$defs/opt"}
                    },
                    "required": ["q"]
                }
            }]
        });
        let (encoded, _) = MESSAGES_TO_GEMINI
            .encode_request(&req, "gemini-3.8-flash-medium")
            .unwrap();
        let parameters = &encoded["tools"][0]["functionDeclarations"][0]["parameters"];

        assert!(parameters.get("$schema").is_none());
        assert!(parameters.get("additionalProperties").is_none());
        assert!(parameters.get("$defs").is_none());
        assert!(parameters["properties"]["q"].get("examples").is_none());
        assert_eq!(parameters["properties"]["q"]["type"], json!("string"));
        assert_eq!(parameters["required"], json!(["q"]));
        // `$ref` 内联后仍保留类型：draft 的 type 数组归一为单类型 + nullable。
        assert_eq!(parameters["properties"]["opt"]["type"], json!("string"));
        assert_eq!(parameters["properties"]["opt"]["nullable"], json!(true));
        assert_only_gemini_schema_keys(parameters);
    }

    /// 真实 Claude Code / MCP 风格的深层 schema 也不得漏到上游。
    #[test]
    fn deeply_nested_json_schema_is_fully_sanitized() {
        let req = json!({
            "model": "gemini-3.8-flash-medium",
            "max_tokens": 512,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "name": "mcp__demo__run",
                "description": "run a demo action",
                "input_schema": {
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "additionalProperties": false,
                    "$defs": {
                        "target": {
                            "type": "object",
                            "properties": {"path": {"type": "string", "format": "uri"}},
                            "required": ["path"],
                            "additionalProperties": false
                        }
                    },
                    "properties": {
                        "targets": {
                            "type": "array",
                            "items": {"$ref": "#/$defs/target"}
                        },
                        "mode": {"type": ["string", "null"], "enum": ["fast", null]},
                        "opts": {
                            "anyOf": [
                                {"type": "object", "properties": {"a": {"type": "integer", "exclusiveMinimum": 0}}},
                                {"type": "string", "pattern": "^[a-z]+$"}
                            ]
                        }
                    },
                    "required": ["targets"],
                    "oneOf": [{"required": ["mode"]}]
                }
            }]
        });
        let (encoded, _) = MESSAGES_TO_GEMINI
            .encode_request(&req, "gemini-3.8-flash-medium")
            .unwrap();
        let parameters = &encoded["tools"][0]["functionDeclarations"][0]["parameters"];

        assert_only_gemini_schema_keys(parameters);
        // `$ref` 内联保住了类型信息，而不是被删成空 schema。
        assert_eq!(
            parameters["properties"]["targets"]["items"]["properties"]["path"]["type"],
            json!("string")
        );
        assert_eq!(parameters["properties"]["mode"]["nullable"], json!(true));
    }

    /// Codex CLI 默认工具集里的 Responses 内置工具（`web_search` 等）在 Gemini
    /// 里没有等价物：应移除并保留其余 function 工具，而不是让整个请求 400。
    #[test]
    fn responses_builtin_tools_are_dropped_for_gemini() {
        let req = json!({
            "model": "gemini-3.8-flash-medium",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "tools": [
                {"type": "function", "name": "exec_command", "parameters": {"type": "object", "properties": {}}},
                {"type": "namespace", "name": "multi_agent_v1", "tools": []},
                {"type": "web_search"}
            ],
            "tool_choice": "auto"
        });
        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-3.8-flash-medium")
            .unwrap();
        let decls = encoded["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap();
        assert_eq!(decls.len(), 1, "只应保留 function 工具");
        assert_eq!(decls[0]["name"], json!("exec_command"));
        assert_eq!(
            encoded["toolConfig"]["functionCallingConfig"]["mode"],
            "AUTO"
        );
    }

    #[test]
    fn responses_to_gemini_preserves_request_context_and_normalized_fields() {
        let req = json!({
            "id": "resp_custom",
            "model": "m",
            "input": "hi",
            "stream": true,
            "parallel_tool_calls": false,
            "text": {"verbosity": "medium"}
        });
        let (encoded, context) = RESPONSES_TO_GEMINI.encode_request(&req, "m").unwrap();
        assert_eq!(context.request_id, "resp_custom");
        assert!(context.stream);
        assert_eq!(encoded["contents"][0]["parts"][0]["text"], "hi");
        assert!(context
            .normalized
            .contains(&"/parallel_tool_calls".to_string()));
        assert!(context.normalized.contains(&"/text/verbosity".to_string()));
    }

    #[test]
    fn responses_to_gemini_rejects_unknown_input_item_types() {
        let req = json!({
            "model": "m",
            "input": [{"type": "future_vendor_item", "payload": {"x": 1}}]
        });
        let error = RESPONSES_TO_GEMINI.encode_request(&req, "m").unwrap_err();
        assert!(error
            .features
            .contains(&"unsupported_feature.unknown_block".to_string()));
        assert!(error.json_pointers.contains(&"/input/0/type".to_string()));
    }

    #[test]
    fn messages_to_gemini_keeps_first_stage_normalized_context() {
        let req = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "safeguards": [{"type": "classifier", "classifier_context": "ctx"}]
        });
        let (_, context) = MESSAGES_TO_GEMINI.encode_request(&req, "m").unwrap();
        assert!(context.normalized.contains(&"/safeguards".to_string()));
    }

    /// Codex CLI 路径：Gemini 的 thoughtSignature 寄生在工具调用 id 上，
    /// Responses 协议里该 id 就是 `call_id`，回传时必须还能还原到
    /// functionCall part，否则 Gemini 3 会以缺签名 400。
    #[test]
    fn responses_call_id_carries_thought_signature_back_to_gemini() {
        const SIGNATURE: &str = "cmVzcG9uc2VzLXByb3RvY29sLXNpZ25hdHVyZS1sb25nIGVub3VnaA==";
        let call_id = format!("call_1.{SIGNATURE}");
        let req = json!({
            "model": "gemini-3.8-flash-medium",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "function_call", "id": "fc_1", "call_id": call_id, "name": "Read", "arguments": "{\"path\":\"/a\"}"},
                {"type": "function_call_output", "call_id": call_id, "output": "ok"}
            ],
            "tools": [{"type": "function", "name": "Read", "parameters": {"type": "object"}}]
        });
        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-3.8-flash-medium")
            .unwrap();

        let parts = encoded["contents"][1]["parts"]
            .as_array()
            .expect("助手消息应带 parts");
        assert_eq!(parts[0]["functionCall"]["name"], json!("Read"));
        assert_eq!(parts[0]["thoughtSignature"], json!(SIGNATURE));
    }

    /// Codex CLI 的真实请求形状（namespace / web_search 工具、text、reasoning、
    /// include、prompt_cache_key、历史里的 function_call/reasoning items）
    /// 必须能整体转到 Gemini，而不是被任一环节拒绝。
    #[test]
    fn codex_cli_responses_request_converts_to_gemini() {
        let req = json!({
            "model": "gemini-3.8-flash-medium",
            "instructions": "be brief",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                {
                    "type": "function_call",
                    "id": "call_1",
                    "call_id": "call_1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"ls\"}"
                },
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"},
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "thought"}]}
            ],
            "tools": [
                {"type": "function", "name": "exec_command", "parameters": {"type": "object", "properties": {}}},
                {"type": "namespace", "name": "multi_agent_v1", "tools": []},
                {"type": "web_search"}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "store": false,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "k",
            "text": {"verbosity": "medium"},
            "reasoning": {"effort": "high", "summary": "auto"},
            "stream": true
        });
        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-3.8-flash-medium")
            .expect("Codex CLI 形状的 Responses 请求必须能转到 Gemini");

        let decls = encoded["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], json!("exec_command"));
        assert_eq!(encoded["contents"][0]["role"], json!("user"));
        assert!(
            encoded.get("systemInstruction").is_some(),
            "instructions 应转成 systemInstruction"
        );
    }

    /// tool_choice 指向被移除的工具时不能留下悬空引用。
    #[test]
    fn responses_tool_choice_for_dropped_tool_is_removed() {
        let req = json!({
            "model": "m",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "tools": [
                {"type": "function", "name": "keep_me", "parameters": {"type": "object", "properties": {}}},
                {"type": "web_search"}
            ],
            "tool_choice": {"type": "web_search"}
        });
        let (encoded, _) = RESPONSES_TO_GEMINI.encode_request(&req, "m").unwrap();
        assert!(encoded.get("toolConfig").is_none());
        assert_eq!(
            encoded["tools"][0]["functionDeclarations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn responses_namespace_tools_are_ignored_for_antigravity() {
        let req = json!({
            "model": "gemini-2.5-flash",
            "input": "hi",
            "tools": [
                {"type": "namespace", "name": "shell", "tools": []},
                {
                    "type": "function",
                    "name": "lookup",
                    "description": "look up a value",
                    "parameters": {"type": "object", "properties": {}}
                }
            ],
            "tool_choice": {"type": "namespace", "name": "shell"}
        });

        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-2.5-flash")
            .unwrap();
        assert_eq!(
            encoded["tools"][0]["functionDeclarations"][0]["name"],
            "lookup"
        );
        assert!(encoded.get("toolConfig").is_none());
    }

    #[test]
    fn responses_only_namespace_tools_are_removed() {
        let req = json!({
            "model": "gemini-2.5-flash",
            "input": "hi",
            "tools": [{"type": "namespace", "name": "shell", "tools": []}],
            "tool_choice": {"type": "namespace", "name": "shell"}
        });

        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-2.5-flash")
            .unwrap();
        assert!(encoded.get("tools").is_none());
        assert!(encoded.get("toolConfig").is_none());
    }

    #[test]
    fn responses_namespace_tool_choice_is_removed_without_tools() {
        let req = json!({
            "model": "gemini-2.5-flash",
            "input": "hi",
            "tool_choice": {"type": "namespace", "name": "shell"}
        });

        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-2.5-flash")
            .unwrap();
        assert!(encoded.get("tools").is_none());
        assert!(encoded.get("toolConfig").is_none());
    }

    /// 内置工具（`web_search_preview` 等）在 Gemini 里没有等价物：移除后
    /// 请求仍可转发（Codex CLI 不会因此完全不可用），而不是整体 400。
    #[test]
    fn responses_builtin_tools_are_dropped_before_encoding() {
        let req = json!({
            "model": "gemini-2.5-flash",
            "input": "hi",
            "tools": [{"type": "web_search_preview"}]
        });
        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-2.5-flash")
            .unwrap();
        assert!(encoded.get("tools").is_none());

        // 同一请求里的 function 工具必须存活。
        let mixed = json!({
            "model": "gemini-2.5-flash",
            "input": "hi",
            "tools": [
                {"type": "function", "name": "lookup", "parameters": {"type": "object", "properties": {}}},
                {"type": "web_search_preview"}
            ]
        });
        let (encoded, _) = RESPONSES_TO_GEMINI
            .encode_request(&mixed, "gemini-2.5-flash")
            .unwrap();
        assert_eq!(
            encoded["tools"][0]["functionDeclarations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn responses_files_and_remote_images_are_rejected() {
        let file_req = json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_file", "file_id": "file_1"}]
            }]
        });
        let file_err = RESPONSES_TO_GEMINI
            .encode_request(&file_req, "m")
            .unwrap_err();
        assert!(file_err.json_pointers.iter().any(|p| p.ends_with("/type")));

        let image_req = json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": "https://example.com/image.png"
                }]
            }]
        });
        let image_err = RESPONSES_TO_GEMINI
            .encode_request(&image_req, "m")
            .unwrap_err();
        assert!(image_err
            .json_pointers
            .iter()
            .any(|p| p.ends_with("/image_url")));
    }

    #[test]
    fn responses_data_uri_images_are_preserved_for_gemini() {
        let req = json!({
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "what is this?"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
                ]
            }]
        });
        let (encoded, _) = RESPONSES_TO_GEMINI.encode_request(&req, "m").unwrap();
        assert_eq!(encoded["contents"][0]["parts"][0]["text"], "what is this?");
        assert_eq!(
            encoded["contents"][0]["parts"][1]["inlineData"]["mimeType"],
            "image/png"
        );
        assert_eq!(
            encoded["contents"][0]["parts"][1]["inlineData"]["data"],
            "AAAA"
        );
    }

    #[test]
    fn chat_tools_and_tool_results_fail_closed() {
        let builtin = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "web_search"}]
        });
        assert!(encode_chat_to_gemini(&builtin, "m").is_err());

        let missing_function = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function"}]
        });
        assert!(encode_chat_to_gemini(&missing_function, "m").is_err());

        let missing_result_id = json!({
            "messages": [
                {"role": "assistant", "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]},
                {"role": "tool", "content": "{}"}
            ]
        });
        assert!(encode_chat_to_gemini(&missing_result_id, "m").is_err());

        let unknown_result_id = json!({
            "messages": [
                {"role": "assistant", "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]},
                {"role": "tool", "tool_call_id": "call_2", "content": "{}"}
            ]
        });
        assert!(encode_chat_to_gemini(&unknown_result_id, "m").is_err());
    }

    #[test]
    fn stream_rejects_prompt_feedback_after_first_record() {
        let ctx = ConversionContext::new("id", "m", true);
        let mut decoder = GeminiToChatStreamDecoder::boxed(&ctx);
        decoder
            .feed(b"data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}}\n\n")
            .unwrap();
        let err = decoder
            .feed(b"data: {\"response\":{\"promptFeedback\":{\"blockReason\":\"SAFETY\",\"blockReasonMessage\":\"blocked\"}}}\n\n")
            .unwrap_err();
        assert!(err.message.contains("SAFETY"));
    }

    /// Gemini 3 的 thoughtSignature 必须经下游 tool call id 往返后回到 functionCall part。
    #[test]
    fn thought_signature_round_trips_through_tool_call_id() {
        const SIGNATURE: &str = "Q3VyaW91c2x5LWdlbmVyYXRlZC1zaWduYXR1cmUtZm9yLXJvdW5kLXRyaXA=";
        let gemini = json!({
            "candidates": [{
                "content": { "parts": [{
                    "functionCall": { "id": "call_original", "name": "Read", "args": {"file_path": "/tmp/a"} },
                    "thoughtSignature": SIGNATURE
                }]},
                "finishReason": "STOP"
            }]
        });
        let ctx = ConversionContext::new("id", "m", false);
        let (chat, _) = decode_gemini_to_chat(&gemini, &ctx).unwrap();
        let tool_call = chat["choices"][0]["message"]["tool_calls"][0].clone();
        let id = tool_call["id"].as_str().unwrap().to_owned();
        assert!(id.starts_with("call_original."));
        assert!(id.ends_with(SIGNATURE), "id 未携带签名: {id}");

        // 把同一条 tool call 原样回传，functionCall part 必须带回 thoughtSignature。
        let echo = json!({
            "messages": [
                {"role": "assistant", "tool_calls": [tool_call]},
                {"role": "tool", "tool_call_id": id, "content": "{}"}
            ],
            "tools": [{
                "type": "function",
                "function": {"name": "Read", "parameters": {"type": "object"}}
            }]
        });
        let (encoded, _) = encode_chat_to_gemini(&echo, "m").unwrap();
        let part = &encoded["contents"][0]["parts"][0];
        assert_eq!(part["thoughtSignature"], json!(SIGNATURE));
        assert_eq!(part["functionCall"]["id"], "call_original");
        assert_eq!(part["functionCall"]["name"], json!("Read"));
        assert_eq!(
            encoded["contents"][1]["parts"][0]["functionResponse"]["id"],
            "call_original"
        );
    }

    /// 流式响应同样要把签名带在 tool call id 上。
    #[test]
    fn stream_tool_call_carries_thought_signature() {
        const SIGNATURE: &str =
            "c3RyZWFtaW5nLXNpZ25hdHVyZS1sb25nLWVub3VnaC10by1iZS1hLXNpZ25hdHVyZQ==";
        let ctx = ConversionContext::new("chatcmpl-1", "gemini-3.8-flash-medium", true);
        let mut decoder = GeminiToChatStreamDecoder::boxed(&ctx);
        let chunk = json!({
            "response": {
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{
                            "functionCall": {"id": "call_stream", "name": "Read", "args": {"file_path": "/a"}},
                            "thoughtSignature": SIGNATURE
                        }]
                    },
                    "finishReason": "STOP"
                }]
            }
        });
        let frame = format!("data: {chunk}\n\n");
        let chunks = decoder.feed(frame.as_bytes()).unwrap();
        let joined = chunks.join("");
        assert!(joined.contains("call_stream."));
        assert!(joined.contains(SIGNATURE), "流式 tool call id 未携带签名");
        assert!(joined.contains("\"finish_reason\":\"tool_calls\""));
    }

    /// Claude Code 实走路径：Gemini 响应 → Messages 的 tool_use → 客户端回传 → Gemini 请求。
    #[test]
    fn thought_signature_survives_the_messages_round_trip() {
        const SIGNATURE: &str =
            "bWVzc2FnZXMtcHJvdG9jb2wtc2lnbmF0dXJlLWxvbmctZW5vdWdoLWZvci10ZXN0cyE=";
        let gemini = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {"name": "Read", "args": {"file_path": "/tmp/a"}},
                        "thoughtSignature": SIGNATURE
                    }]
                },
                "finishReason": "STOP"
            }]
        });
        let ctx = ConversionContext::new("id", "gemini-3.8-flash-medium", false);
        let (chat, _) = decode_gemini_to_chat(&gemini, &ctx).unwrap();
        let messages = super::chat::decode_chat_response_to_messages(&chat, &ctx).unwrap();
        let tool_use = messages["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["type"] == json!("tool_use"))
            .expect("Messages 响应应含 tool_use")
            .clone();
        let id = tool_use["id"].as_str().unwrap().to_owned();
        assert!(id.ends_with(SIGNATURE), "tool_use.id 未携带签名: {id}");

        let echo = json!({
            "model": "gemini-3.8-flash-medium",
            "max_tokens": 256,
            "messages": [
                {"role": "assistant", "content": [tool_use]},
                {"role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": id, "content": "ok"
                }]}
            ],
            "tools": [{
                "name": "Read",
                "description": "read a file",
                "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}}}
            }]
        });
        let (encoded, _) = MESSAGES_TO_GEMINI
            .encode_request(&echo, "gemini-3.8-flash-medium")
            .unwrap();
        let part = &encoded["contents"][0]["parts"][0];
        assert_eq!(part["thoughtSignature"], json!(SIGNATURE));
        assert_eq!(part["functionCall"]["name"], json!("Read"));
    }

    /// Claude Code 的安全分类器请求会带 stop_sequences（非流式），必须能转给 Gemini。
    #[test]
    fn anthropic_stop_sequences_map_to_gemini_stop_sequences() {
        let req = json!({
            "model": "gemini-3.8-flash-medium",
            "max_tokens": 64,
            "stop_sequences": ["</block>"],
            "messages": [{"role": "user", "content": "hi"}]
        });
        let (encoded, _) = MESSAGES_TO_GEMINI
            .encode_request(&req, "gemini-3.8-flash-medium")
            .unwrap();
        assert_eq!(
            encoded["generationConfig"]["stopSequences"],
            json!(["</block>"])
        );
    }

    #[test]
    fn chat_stop_maps_to_stop_sequences_and_respects_the_limit() {
        let single = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "stop": "END"
        });
        let (encoded, _) = encode_chat_to_gemini(&single, "m").unwrap();
        assert_eq!(encoded["generationConfig"]["stopSequences"], json!(["END"]));

        // Gemini 只接受最多 5 个序列，超出时必须报错而不是静默截断。
        let many = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "stop": ["1", "2", "3", "4", "5", "6"]
        });
        let err = encode_chat_to_gemini(&many, "m").unwrap_err();
        assert!(err.json_pointers.iter().any(|p| p == "/stop"));

        // 空数组表示“不限制”，不产生 stopSequences。
        let empty = json!({
            "messages": [{"role": "user", "content": "hi"}],
            "stop": []
        });
        let (encoded, _) = encode_chat_to_gemini(&empty, "m").unwrap();
        assert!(encoded
            .get("generationConfig")
            .and_then(|config| config.get("stopSequences"))
            .is_none());
    }

    /// 只有看起来确实像签名的尾部才会被当作签名，自带 `.` 的 id 不受影响。
    #[test]
    fn only_signature_shaped_suffixes_are_treated_as_signatures() {
        let base = "call_0123456789abcdef0123456789abcdef";
        let signature = "A".repeat(40);
        assert_eq!(
            tool_call_id_with_signature(base, Some(&signature)),
            format!("{base}.{signature}")
        );
        assert_eq!(
            split_tool_call_id(&format!("{base}.{signature}")),
            (base, Some(signature.as_str()))
        );
        assert_eq!(tool_call_id_with_signature(base, None), base);
        assert_eq!(split_tool_call_id("call_1.foo"), ("call_1.foo", None));
        assert_eq!(split_tool_call_id(base), (base, None));
    }

    #[test]
    fn stream_emits_reasoning_and_tool_calls() {
        let ctx = ConversionContext::new("chatcmpl-1", "gemini-2.5-flash", true);
        let mut decoder = GeminiToChatStreamDecoder::boxed(&ctx);
        let frame = concat!(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[",
            "{\"thought\":true,\"text\":\"think\"},",
            "{\"functionCall\":{\"name\":\"lookup\",\"args\":{\"q\":\"x\"}}}",
            "]},\"finishReason\":\"STOP\"}]}}\n\n"
        );
        let chunks = decoder.feed(frame.as_bytes()).unwrap();
        let joined = chunks.join("");
        assert!(joined.contains("reasoning_content"));
        assert!(joined.contains("think"));
        assert!(joined.contains("lookup"));
        assert!(joined.contains("\"finish_reason\":\"tool_calls\""));
        let rest = decoder.finish().unwrap();
        assert!(rest.is_empty() || rest.iter().any(|s| s.contains("[DONE]")));
    }
}
