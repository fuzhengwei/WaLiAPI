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
        let (chat, _) = messages::encode_messages_to_chat(request, model)?;
        encode_chat_to_gemini(&chat, model)
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
            Some("reasoning") => rejected.push(super::error::RejectedField {
                code: FeatureKind::Thinking.code().to_owned(),
                pointer: format!("{pointer}/type"),
                message: "Gemini Responses conversion cannot preserve reasoning items safely"
                    .to_owned(),
            }),
            None => rejected.push(super::error::RejectedField {
                code: FeatureKind::UnknownBlock.code().to_owned(),
                pointer: format!("{pointer}/type"),
                message: "Responses input item is missing type and role".to_owned(),
            }),
            Some(other) => rejected.push(super::error::RejectedField {
                code: FeatureKind::UnknownBlock.code().to_owned(),
                pointer: format!("{pointer}/type"),
                message: format!("unsupported Responses input item {other:?}"),
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
        validate_responses_for_gemini(request)?;
        let mut chat = crate::protocol::responses_to_openai(request)?;
        chat.as_object_mut()
            .ok_or_else(|| {
                UnsupportedFeatures::single(
                    FeatureKind::UnsupportedField,
                    "/",
                    "Responses to Chat encoder produced a non-object request",
                )
            })?
            .insert("model".to_owned(), Value::String(model.to_owned()));
        encode_chat_to_gemini(&chat, model)
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
    use super::*;
    use serde_json::json;

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
            encoded["contents"][2]["parts"][0]["functionResponse"]["name"],
            "lookup"
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

    #[test]
    fn responses_builtin_tools_are_rejected_before_encoding() {
        let req = json!({
            "model": "gemini-2.5-flash",
            "input": "hi",
            "tools": [{"type": "web_search_preview"}]
        });
        let err = RESPONSES_TO_GEMINI
            .encode_request(&req, "gemini-2.5-flash")
            .unwrap_err();
        assert!(err.json_pointers.iter().any(|p| p == "/tools/0/type"));
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
