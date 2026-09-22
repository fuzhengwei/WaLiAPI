use super::super::super::{
    error::{DecodeError, FeatureKind, UnsupportedFeatures},
    ports::StreamDecoder,
    report::{ConversionContext, Usage},
    sse,
};
use super::decode::merge_usage;
use super::{required, unsupported};
use serde_json::Value;
use std::collections::BTreeMap;

// Streaming Messages -> Responses.  The implementation buffers only whole SSE
// records and derives item ids from the source block index, so arbitrary byte
// splits cannot change the output sequence.
pub(super) struct MessagesResponsesStream {
    pending: Vec<u8>,
    id: String,
    model: String,
    started: bool,
    terminal: bool,
    usage: Usage,
    blocks: BTreeMap<usize, Block>,
    stop: Option<String>,
}
#[derive(Default)]
struct Block {
    item_id: String,
    kind: String,
    id: String,
    name: String,
    text: String,
    args: String,
    stopped: bool,
}
impl MessagesResponsesStream {
    pub(super) fn new(c: &ConversionContext) -> Self {
        Self {
            pending: Vec::new(),
            id: c.request_id.clone(),
            model: c.upstream_model.clone(),
            started: false,
            terminal: false,
            usage: Usage {
                usage_unknown: true,
                ..Usage::default()
            },
            blocks: BTreeMap::new(),
            stop: None,
        }
    }
    fn frame(name: &str, value: Value) -> String {
        sse::event(name, value)
    }
    fn start(&mut self, out: &mut Vec<String>) {
        if !self.started {
            self.started = true;
            let r = serde_json::json!({"id":self.id,"object":"response","model":self.model,"status":"in_progress","output":[]});
            out.push(Self::frame(
                "response.created",
                serde_json::json!({"type":"response.created","response":r}),
            ));
            out.push(Self::frame("response.in_progress",serde_json::json!({"type":"response.in_progress","response":{"id":self.id,"model":self.model}})));
        }
    }
}
impl StreamDecoder for MessagesResponsesStream {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, DecodeError> {
        self.pending.extend_from_slice(bytes);
        if sse::pending_exceeded(&self.pending) {
            return Err(DecodeError::from(unsupported(
                FeatureKind::UnknownEvent,
                "/",
                sse::pending_overflow_message(),
            )));
        }
        let mut out = Vec::new();
        while let Some(end) = sse::record_end(&self.pending) {
            let rec: Vec<u8> = self.pending.drain(..end).collect();
            out.extend(self.record(&rec).map_err(DecodeError::from)?);
        }
        Ok(out)
    }
    fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
        if !self.pending.is_empty() {
            return Err(DecodeError::from(unsupported(
                FeatureKind::UnknownEvent,
                "/",
                "Messages SSE ended mid-record",
            )));
        }
        if !self.terminal {
            return Err(DecodeError::from(unsupported(
                FeatureKind::UnknownEvent,
                "/",
                "Messages SSE ended without message_stop",
            )));
        }
        Ok(Vec::new())
    }
    fn usage(&self) -> Option<Usage> {
        Some(self.usage)
    }
    fn saw_terminal(&self) -> bool {
        self.terminal
    }
}
impl MessagesResponsesStream {
    fn record(&mut self, record: &[u8]) -> Result<Vec<String>, UnsupportedFeatures> {
        let payload = sse::parse_data_payload(record)?;
        if payload.is_empty() || payload == "[DONE]" {
            return Ok(Vec::new());
        }
        let event: Value = serde_json::from_str(&payload).map_err(|_| {
            unsupported(
                FeatureKind::UnknownEvent,
                "/",
                "Messages SSE data is not JSON",
            )
        })?;
        if event.get("type").and_then(Value::as_str) == Some("codex.rate_limits") {
            return Ok(vec![String::from_utf8_lossy(record).into_owned()]);
        }
        let ty = event.get("type").and_then(Value::as_str).ok_or_else(|| {
            unsupported(
                FeatureKind::UnknownEvent,
                "/type",
                "Messages SSE frame type is required",
            )
        })?;
        let mut out = Vec::new();
        match ty {
            "message_start" => {
                if let Some(m) = event.get("message") {
                    self.id = m
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or(&self.id)
                        .to_string();
                    self.model = m
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or(&self.model)
                        .to_string();
                    if let Some(u) = m.get("usage") {
                        merge_usage(&mut self.usage, u);
                    }
                }
                self.start(&mut out)
            }
            "content_block_start" => {
                self.start(&mut out);
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let b = event.get("content_block").unwrap_or(&Value::Null);
                let kind = b.get("type").and_then(Value::as_str).unwrap_or("");
                // 条目 id 的前缀是 Responses 协议的一部分：`msg_` / `rs_` / `fc_`
                // （官方上游回放历史时校验）。历史实现统一用 `item_N` 会被 400。
                let item_id = match kind {
                    "text" => format!("msg_{index}"),
                    "thinking" => format!("rs_{index}"),
                    "tool_use" => format!("fc_{index}"),
                    _ => format!("item_{index}"),
                };
                let mut block = Block {
                    item_id: item_id.clone(),
                    kind: kind.into(),
                    ..Block::default()
                };
                match kind {
                    "text" => {
                        out.push(Self::frame("response.output_item.added",serde_json::json!({"type":"response.output_item.added","output_index":index,"item":{"id":item_id,"type":"message","role":"assistant","content":[]}})));
                        out.push(Self::frame("response.content_part.added", serde_json::json!({"type":"response.content_part.added","output_index":index,"content_index":0,"part":{"type":"output_text","text":""}})));
                    }
                    "thinking" => {
                        out.push(Self::frame("response.output_item.added",serde_json::json!({"type":"response.output_item.added","output_index":index,"item":{"id":item_id,"type":"reasoning","summary":[]}})));
                        out.push(Self::frame("response.reasoning_summary_part.added", serde_json::json!({"type":"response.reasoning_summary_part.added","output_index":index,"summary_index":0,"part":{"type":"reasoning_summary_text","text":""}})));
                    }
                    "tool_use" => {
                        let id = required(b, "id", "/content_block_start/content_block")?;
                        let name = required(b, "name", "/content_block_start/content_block")?;
                        block.id = id.to_string();
                        block.name = name.to_string();
                        // `id` 是 `fc_` 开头的条目 id，`call_id` 是与工具结果
                        // 关联的上游调用 id，两者不能混用。
                        out.push(Self::frame("response.output_item.added",serde_json::json!({"type":"response.output_item.added","output_index":index,"item":{"id":item_id,"type":"function_call","call_id":id,"name":name,"arguments":""}})))
                    }
                    _ => {
                        return Err(unsupported(
                            FeatureKind::UnknownBlock,
                            "/content_block_start/content_block/type",
                            "unknown Messages content block",
                        ))
                    }
                }
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        let b = self.blocks.get_mut(&index).ok_or_else(|| {
                            unsupported(
                                FeatureKind::UnknownEvent,
                                "/index",
                                "text delta references unknown block",
                            )
                        })?;
                        b.text.push_str(text);
                        out.push(Self::frame("response.output_text.delta",serde_json::json!({"type":"response.output_text.delta","output_index":index,"item_id":b.item_id,"delta":text})))
                    }
                    Some("thinking_delta") => {
                        let text = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        let b = self.blocks.get_mut(&index).ok_or_else(|| {
                            unsupported(
                                FeatureKind::UnknownEvent,
                                "/index",
                                "thinking delta references unknown block",
                            )
                        })?;
                        b.text.push_str(text);
                        out.push(Self::frame("response.reasoning_summary_text.delta",serde_json::json!({"type":"response.reasoning_summary_text.delta","output_index":index,"delta":text})))
                    }
                    Some("input_json_delta") => {
                        let text = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let b = self.blocks.get_mut(&index).ok_or_else(|| {
                            unsupported(
                                FeatureKind::UnknownEvent,
                                "/index",
                                "tool delta references unknown block",
                            )
                        })?;
                        b.args.push_str(text);
                        out.push(Self::frame("response.function_call_arguments.delta",serde_json::json!({"type":"response.function_call_arguments.delta","output_index":index,"item_id":b.item_id,"delta":text})))
                    }
                    Some("signature_delta") => {}
                    Some(other) => {
                        return Err(unsupported(
                            FeatureKind::UnknownEvent,
                            "/delta/type",
                            format!("unknown Messages delta {other:?}"),
                        ))
                    }
                    None => {
                        return Err(unsupported(
                            FeatureKind::UnknownEvent,
                            "/delta/type",
                            "Messages delta type is required",
                        ))
                    }
                }
            }
            "content_block_stop" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let b = self.blocks.get_mut(&index).ok_or_else(|| {
                    unsupported(
                        FeatureKind::UnknownEvent,
                        "/index",
                        "block stop references unknown block",
                    )
                })?;
                if b.stopped {
                    return Err(unsupported(
                        FeatureKind::UnknownEvent,
                        "/index",
                        "duplicate content block stop",
                    ));
                }
                b.stopped = true;
                if b.kind == "text" {
                    out.push(Self::frame("response.output_text.done", serde_json::json!({"type":"response.output_text.done","output_index":index,"content_index":0,"text":b.text})));
                    out.push(Self::frame("response.content_part.done", serde_json::json!({"type":"response.content_part.done","output_index":index,"content_index":0,"part":{"type":"output_text","text":b.text}})));
                } else if b.kind == "thinking" {
                    out.push(Self::frame("response.reasoning_summary_text.done", serde_json::json!({"type":"response.reasoning_summary_text.done","output_index":index,"summary_index":0,"text":b.text})));
                    out.push(Self::frame("response.reasoning_summary_part.done", serde_json::json!({"type":"response.reasoning_summary_part.done","output_index":index,"summary_index":0,"part":{"type":"reasoning_summary_text","text":b.text}})));
                } else if b.kind == "tool_use" {
                    let p: Value = serde_json::from_str(&b.args).map_err(|_| {
                        unsupported(
                            FeatureKind::InvalidToolArguments,
                            "/delta/partial_json",
                            "tool arguments are not valid JSON",
                        )
                    })?;
                    if !p.is_object() {
                        return Err(unsupported(
                            FeatureKind::InvalidToolArguments,
                            "/delta/partial_json",
                            "tool arguments must be an object",
                        ));
                    }
                    out.push(Self::frame("response.function_call_arguments.done",serde_json::json!({"type":"response.function_call_arguments.done","output_index":index,"item_id":b.item_id,"arguments":b.args})));
                }
                let item = match b.kind.as_str() {
                    "text" => {
                        serde_json::json!({"id":b.item_id,"type":"message","role":"assistant","content":[{"type":"output_text","text":b.text}]})
                    }
                    "thinking" => {
                        serde_json::json!({"id":b.item_id,"type":"reasoning","summary":[{"type":"summary_text","text":b.text}]})
                    }
                    "tool_use" => {
                        serde_json::json!({"id":b.item_id,"type":"function_call","call_id":b.id,"name":b.name,"arguments":b.args})
                    }
                    _ => unreachable!("validated at content_block_start"),
                };
                out.push(Self::frame("response.output_item.done",serde_json::json!({"type":"response.output_item.done","output_index":index,"item":item})))
            }
            "message_delta" => {
                if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(reason.into())
                }
                if let Some(u) = event.get("usage") {
                    merge_usage(&mut self.usage, u);
                }
            }
            "message_stop" => {
                if self.terminal {
                    return Err(unsupported(
                        FeatureKind::UnknownEvent,
                        "/type",
                        "duplicate message_stop",
                    ));
                }
                self.start(&mut out);
                let status = match self.stop.as_deref() {
                    Some("max_tokens" | "model_context_window_exceeded") => "incomplete",
                    Some("end_turn" | "stop_sequence" | "refusal" | "pause_turn" | "tool_use") => {
                        "completed"
                    }
                    Some(x) => {
                        return Err(unsupported(
                            FeatureKind::UnknownFinishReason,
                            "/delta/stop_reason",
                            format!("unknown stop reason {x:?}"),
                        ))
                    }
                    None => {
                        return Err(unsupported(
                            FeatureKind::UnknownFinishReason,
                            "/delta/stop_reason",
                            "message_stop without stop reason",
                        ))
                    }
                };
                let output = self.blocks.values().filter_map(|block| match block.kind.as_str() {
                    "text" => Some(serde_json::json!({"id":block.item_id,"type":"message","role":"assistant","content":[{"type":"output_text","text":block.text}]})),
                    "thinking" => Some(serde_json::json!({"id":block.item_id,"type":"reasoning","summary":[{"type":"summary_text","text":block.text}]})),
                    "tool_use" => Some(serde_json::json!({"id":block.item_id,"type":"function_call","call_id":block.id,"name":block.name,"arguments":block.args})),
                    _ => None,
                }).collect::<Vec<_>>();
                let mut response = serde_json::json!({"id":self.id,"object":"response","model":self.model,"status":status,"output":output,"usage":{"input_tokens":self.usage.input_tokens,"output_tokens":self.usage.output_tokens,"total_tokens":self.usage.input_tokens+self.usage.output_tokens}});
                if status == "incomplete" {
                    response["incomplete_details"] =
                        serde_json::json!({"reason":"max_output_tokens"});
                }
                out.push(Self::frame(
                    "response.completed",
                    serde_json::json!({"type":"response.completed","response":response}),
                ));
                out.push("data: [DONE]\n\n".into());
                self.terminal = true
            }
            _ => {
                return Err(unsupported(
                    FeatureKind::UnknownEvent,
                    "/type",
                    format!("unknown Messages SSE event {ty:?}"),
                ))
            }
        }
        Ok(out)
    }
}
