//! SSE framing + the streaming commit-barrier pump.
//!
//! Protocol conversion is deliberately absent from this module.  A selected
//! [`PreparedCodec`](crate::protocol::codec::PreparedCodec) creates the decoder
//! before entering the pump, so this type only owns framing, commit state and
//! decoder driving.

use crate::core::stream_supervisor::{StreamSupervisor, StreamTransitionError};
use crate::protocol::codec::StreamDecoder;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum PumpError {
    Protocol(String),
    Supervisor(String),
}

impl From<StreamTransitionError> for PumpError {
    fn from(error: StreamTransitionError) -> Self {
        Self::Supervisor(format!("{error:?}"))
    }
}

impl PumpError {
    pub fn message(&self) -> &str {
        match self {
            Self::Protocol(message) | Self::Supervisor(message) => message,
        }
    }
}

pub fn record_end(input: &[u8]) -> Option<usize> {
    crate::protocol::codec::sse::record_end(input)
}

pub fn parse_data_payload(record: &[u8]) -> Result<String, String> {
    crate::protocol::codec::sse::parse_data_payload(record).map_err(|error| error.message)
}

/// 泵累积内容（正文/推理文本）上限（FIX-11）：超长生成停止拼接并落截断
/// 标记，防止故障/恶意上游把内存与日志体积放大到失控。下游转发不受影响
/// （转发的字节不经过累积），仅影响落库的 response_choices。
pub(crate) const MAX_ACCUMULATED_CONTENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const ACCUMULATION_TRUNCATION_MARKER: &str =
    "\n\n[... WaLiAPI: accumulated content truncated ...]";

/// Validate only enough framing to retain the pre-commit failover barrier.
/// Full protocol validation belongs to the decoder factory selected at prepare
/// time and therefore runs inside [`StreamPumpCore::new`].
pub fn validate_native_first_record(record: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(record);
    if text.contains("event:") {
        return Ok(());
    }
    let payload = parse_data_payload(record)?;
    if payload.is_empty() || payload == "[DONE]" {
        return Ok(());
    }
    serde_json::from_str::<Value>(&payload)
        .map(|_| ())
        .map_err(|error| format!("first SSE data frame is not valid JSON: {error}"))
}

/// 下行 SSE 事件是否为协议终止标记：Chat 的 `data: [DONE]`、Anthropic 的
/// `message_stop`、Responses 的 `response.completed`/`response.done`。
///
/// 终止事件到达即视为流在协议层完成——下游已拿到完整响应，不必再等上游
/// TCP EOF（部分上游发完数据后延迟关连接，EOF 可能迟迟不来，issue #57）。
/// 只识别成功终止：错误型事件（`error`/`response.failed`）仍交给 EOF/
/// 超时路径处理，避免把「上游报错后挂死」记成成功流。
pub(crate) fn sse_event_is_terminal(event: &str) -> bool {
    for line in event.lines() {
        let line = line.trim();
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            return true;
        }
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            if let Some(t) = v.get("type").and_then(Value::as_str) {
                if matches!(t, "message_stop" | "response.completed" | "response.done") {
                    return true;
                }
            }
        }
    }
    false
}

/// A protocol-agnostic pump. The decoder is mandatory: identity directions use
/// the same path as conversions, which prevents an executor-side "native"
/// branch from bypassing validation or usage collection.
pub struct StreamPumpCore {
    supervisor: StreamSupervisor,
    decoder: Box<dyn StreamDecoder + Send + Sync>,
    first_frame: Vec<u8>,
    first_done: bool,
    terminal_registered: bool,
    finished: bool,
    accumulated_content: String,
    accumulated_reasoning: String,
    /// role from the first delta that carries one (defaults to "assistant").
    response_role: Option<String>,
    /// finish_reason captured from the terminal SSE record.
    finish_reason: Option<String>,
    /// tool_calls accumulated from stream deltas, keyed by index.
    tool_calls_map: std::collections::BTreeMap<i64, serde_json::Value>,
    /// 累积内容触上限后置位：停止拼接正文/推理，落库时追加截断标记（FIX-11）。
    accumulation_truncated: bool,
}

impl StreamPumpCore {
    /// Feed the complete first record and any bytes read past it into the same
    /// fresh decoder before committing downstream. This makes an invalid first
    /// response (including an identity response) retryable without leaking raw
    /// bytes to the client.
    pub fn new(
        mut supervisor: StreamSupervisor,
        mut decoder: Box<dyn StreamDecoder + Send + Sync>,
        first_frame: Vec<u8>,
        carry: Vec<u8>,
    ) -> Result<Self, PumpError> {
        let mut output = Vec::new();
        let mut accumulated_content = String::new();
        let mut accumulated_reasoning = String::new();
        let mut response_role: Option<String> = None;
        let mut finish_reason: Option<String> = None;
        let mut tool_calls_map = std::collections::BTreeMap::<i64, serde_json::Value>::new();
        let mut terminal_registered = false;
        for bytes in [&first_frame[..], &carry[..]] {
            if bytes.is_empty() {
                continue;
            }
            let events = decoder.feed(bytes).map_err(|error| {
                PumpError::Protocol(format!("upstream stream could not be decoded: {error}"))
            })?;
            for event in &events {
                output.extend_from_slice(event.as_bytes());
                accumulate_from_sse_event(
                    event,
                    &mut accumulated_content,
                    &mut accumulated_reasoning,
                    &mut response_role,
                    &mut finish_reason,
                    &mut tool_calls_map,
                );
                terminal_registered |= sse_event_is_terminal(event);
            }
        }
        // 首帧/伴随帧里就可能含终止标记（极短响应），提前登记——输出扫描
        // 与解码器的上游终止状态双通道。
        terminal_registered |= decoder.saw_terminal();
        if terminal_registered {
            supervisor.register_terminal();
        }
        let accumulation_truncated = accumulated_content.len() >= MAX_ACCUMULATED_CONTENT_BYTES;
        Ok(Self {
            supervisor,
            decoder,
            first_frame: output,
            first_done: false,
            terminal_registered,
            finished: false,
            accumulated_content,
            accumulated_reasoning,
            response_role,
            finish_reason,
            tool_calls_map,
            accumulation_truncated,
        })
    }

    pub fn start(&mut self) -> Result<Vec<u8>, PumpError> {
        if self.first_done {
            return Ok(Vec::new());
        }
        self.supervisor.commit_downstream()?;
        self.supervisor.begin_streaming()?;
        self.first_done = true;
        Ok(std::mem::take(&mut self.first_frame))
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<u8>, PumpError> {
        let mut output = self.start()?;
        let events = self.decoder.feed(bytes).map_err(|error| {
            PumpError::Protocol(format!("upstream stream could not be decoded: {error}"))
        })?;
        for event in &events {
            output.extend_from_slice(event.as_bytes());
            self.accumulate_from_sse(event);
            if sse_event_is_terminal(event) {
                self.register_terminal_once();
            }
        }
        // 转换方向把终止事件推迟到 finish() 合成（输出里看不到），以解码器
        // 的上游终止状态为准（#57）。
        if self.decoder.saw_terminal() {
            self.register_terminal_once();
        }
        Ok(output)
    }

    /// Exactly-once 终止登记（push 检测与 finish 共用）。
    fn register_terminal_once(&mut self) {
        if !self.terminal_registered {
            self.terminal_registered = self.supervisor.register_terminal();
        }
    }

    /// 帧间空闲超时观测（FIX-08）：记录到流监督状态机供诊断，不改变泵状态。
    pub fn mark_idle_timeout(&mut self) {
        let _ = self
            .supervisor
            .on_timeout(crate::core::stream_supervisor::StreamTimeoutKind::StreamIdle);
    }

    pub fn finish(&mut self) -> Result<Vec<u8>, PumpError> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        let mut output = self.start()?;
        let events = self.decoder.finish().map_err(|error| {
            PumpError::Protocol(format!(
                "upstream stream ended with an incomplete decode: {error}"
            ))
        })?;
        for event in &events {
            output.extend_from_slice(event.as_bytes());
            self.accumulate_from_sse(event);
        }
        // Decoder correctness includes protocol terminal validation. The pump
        // owns only the supervisor's exactly-once terminal transition.
        self.register_terminal_once();
        Ok(output)
    }

    pub fn committed(&self) -> bool {
        self.supervisor.committed()
    }

    pub fn terminated(&self) -> bool {
        self.supervisor.terminal_emitted()
    }

    pub fn usage(&self) -> (i64, i64, i64, i64) {
        self.decoder.usage().map_or((0, 0, 0, 0), |usage| {
            let prompt = usage.input_tokens as i64;
            let completion = usage.output_tokens as i64;
            let cached = usage.cache_read_input_tokens as i64;
            (prompt, completion, prompt + completion, cached)
        })
    }

    pub fn accumulated_content(&self) -> &str {
        &self.accumulated_content
    }

    /// Accumulate content/reasoning/tool_calls from a downstream SSE event string.
    /// 正文/推理触上限后停止拼接（FIX-11）；终止事件检测在 push 外层独立
    /// 进行、不受累积上限影响，tool_calls 仍累积以保证落账结构完整。
    fn accumulate_from_sse(&mut self, event: &str) {
        if !self.accumulation_truncated
            && self.accumulated_content.len() >= MAX_ACCUMULATED_CONTENT_BYTES
        {
            self.accumulated_content
                .push_str(ACCUMULATION_TRUNCATION_MARKER);
            self.accumulation_truncated = true;
        }
        if self.accumulation_truncated {
            return;
        }
        accumulate_from_sse_event(
            event,
            &mut self.accumulated_content,
            &mut self.accumulated_reasoning,
            &mut self.response_role,
            &mut self.finish_reason,
            &mut self.tool_calls_map,
        );
    }

    /// Build a `response_choices` JSON string suitable for the audit log,
    /// consuming accumulated stream content.  Returns `None` when nothing
    /// was accumulated (empty stream or error).
    pub fn build_response_choices(&self) -> Option<String> {
        let has_content = !self.accumulated_content.is_empty()
            || !self.accumulated_reasoning.is_empty()
            || !self.tool_calls_map.is_empty();
        if !has_content {
            return None;
        }
        let mut message = serde_json::json!({
            "role": self.response_role.as_deref().unwrap_or("assistant"),
        });
        if !self.accumulated_content.is_empty() {
            message["content"] = serde_json::json!(&self.accumulated_content);
        }
        if !self.accumulated_reasoning.is_empty() {
            message["reasoning_content"] = serde_json::json!(&self.accumulated_reasoning);
        }
        if !self.tool_calls_map.is_empty() {
            let tcs: Vec<serde_json::Value> = self.tool_calls_map.values().cloned().collect();
            message["tool_calls"] = serde_json::json!(tcs);
        }
        let choices = vec![serde_json::json!({
            "index": 0,
            "message": message,
            "finish_reason": self.finish_reason.as_deref().unwrap_or("stop"),
        })];
        Some(serde_json::to_string(&choices).unwrap_or_default())
    }

    #[allow(dead_code)]
    pub fn abort(&mut self, reason: impl Into<String>) -> Result<(), PumpError> {
        self.supervisor.abort(reason).map_err(PumpError::from)
    }

    #[allow(dead_code)]
    pub fn client_cancel(&mut self) -> Result<(), PumpError> {
        self.supervisor.client_cancel().map_err(PumpError::from)
    }
}

/// Parse a single SSE event string and accumulate content/reasoning/tool_calls.
/// Supports OpenAI Chat Completions, Anthropic Messages, and OpenAI Responses streaming formats.
fn accumulate_from_sse_event(
    event: &str,
    content: &mut String,
    reasoning: &mut String,
    role: &mut Option<String>,
    finish_reason: &mut Option<String>,
    tool_calls_map: &mut std::collections::BTreeMap<i64, serde_json::Value>,
) {
    // Each event may contain multiple `data:` lines.
    for line in event.lines() {
        let line = line.trim();
        if !line.starts_with("data:") {
            continue;
        }
        let payload = line[5..].trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // ── OpenAI Chat Completions streaming ──
        if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                // role (usually on the first delta)
                if let Some(r) = choice.pointer("/delta/role").and_then(|r| r.as_str()) {
                    if role.is_none() {
                        *role = Some(r.to_string());
                    }
                }
                // content
                if let Some(c) = choice.pointer("/delta/content").and_then(|c| c.as_str()) {
                    content.push_str(c);
                }
                // reasoning_content (DeepSeek / OpenAI o-series)
                if let Some(rc) = choice
                    .pointer("/delta/reasoning_content")
                    .and_then(|rc| rc.as_str())
                {
                    reasoning.push_str(rc);
                }
                // tool_calls
                if let Some(tcs) = choice
                    .pointer("/delta/tool_calls")
                    .and_then(|tc| tc.as_array())
                {
                    for tc in tcs {
                        let idx = tc.get("index").and_then(|i| i.as_i64()).unwrap_or(0);
                        let entry = tool_calls_map
                            .entry(idx)
                            .or_insert_with(|| serde_json::json!({"index": idx, "id": null, "type": "function", "function": {"name": "", "arguments": ""}}));
                        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                            entry["id"] = serde_json::json!(id);
                        }
                        if let Some(t) = tc.pointer("/function/name").and_then(|n| n.as_str()) {
                            entry["function"]["name"] = serde_json::json!(t);
                        }
                        if let Some(args) =
                            tc.pointer("/function/arguments").and_then(|a| a.as_str())
                        {
                            if let Some(existing) = entry
                                .pointer("/function/arguments")
                                .and_then(|a| a.as_str())
                            {
                                entry["function"]["arguments"] =
                                    serde_json::json!(format!("{existing}{args}"));
                            } else {
                                entry["function"]["arguments"] = serde_json::json!(args);
                            }
                        }
                    }
                }
                // finish_reason
                if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    *finish_reason = Some(fr.to_string());
                }
            }
            continue;
        }

        // ── Anthropic Messages / OpenAI Responses streaming ──
        // Both protocols stamp a `type` field on each event, so they must share
        // a single match.  (The Responses arms used to live in a second `if`
        // below this block, but the unconditional `continue` here made that
        // code unreachable — every Responses event fell into `_ => {}` and
        // nothing was ever accumulated for streaming Responses requests.)
        if let Some(et) = v.get("type").and_then(|t| t.as_str()) {
            match et {
                // ── Anthropic Messages ──
                "content_block_start" => {
                    if let Some(block) = v.get("content_block") {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            let idx = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            // Use a hash of the id as the key for tool calls
                            let key = idx
                                .bytes()
                                .fold(0i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64));
                            tool_calls_map.insert(key, serde_json::json!({
                                "index": key,
                                "id": idx,
                                "type": "function",
                                "function": {"name": block.get("name").and_then(|n| n.as_str()).unwrap_or(""), "arguments": ""}
                            }));
                        }
                    }
                }
                "content_block_delta" => {
                    if let Some(delta) = v.get("delta") {
                        if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                            if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                                content.push_str(t);
                            }
                        }
                        if delta.get("type").and_then(|t| t.as_str()) == Some("thinking_delta") {
                            if let Some(t) = delta.get("thinking").and_then(|t| t.as_str()) {
                                reasoning.push_str(t);
                            }
                        }
                        if delta.get("type").and_then(|t| t.as_str()) == Some("input_json_delta") {
                            if let Some(pj) = delta.get("partial_json").and_then(|j| j.as_str()) {
                                // Append to the most recent tool call's arguments
                                if let Some((&_k, _)) = tool_calls_map.last_key_value() {
                                    if let Some(entry) = tool_calls_map.get_mut(&_k) {
                                        let existing = entry
                                            .pointer("/function/arguments")
                                            .and_then(|a| a.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        entry["function"]["arguments"] =
                                            serde_json::json!(format!("{existing}{pj}"));
                                    }
                                }
                            }
                        }
                    }
                }
                "message_start" => {
                    if let Some(r) = v.pointer("/message/role").and_then(|r| r.as_str()) {
                        if role.is_none() {
                            *role = Some(r.to_string());
                        }
                    }
                }
                "message_delta" => {
                    if let Some(fr) = v.pointer("/delta/stop_reason").and_then(|s| s.as_str()) {
                        *finish_reason = Some(fr.to_string());
                    }
                }
                // ── OpenAI Responses API ──
                // Events like response.output_text.delta / response.completed
                "response.output_text.delta" | "response.text.delta" => {
                    if let Some(t) = v.get("delta").and_then(|d| d.as_str()) {
                        content.push_str(t);
                    }
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    if let Some(t) = v.get("delta").and_then(|d| d.as_str()) {
                        reasoning.push_str(t);
                    }
                }
                "response.function_call_arguments.delta" => {
                    // {item_id, output_index, delta} — key by item_id hash.
                    let item_id = v.get("item_id").and_then(|i| i.as_str()).unwrap_or("");
                    let key = item_id
                        .bytes()
                        .fold(0i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64));
                    if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                        let entry = tool_calls_map.entry(key).or_insert_with(|| {
                            serde_json::json!({"index": key, "id": item_id, "type": "function", "function": {"name": "", "arguments": ""}})
                        });
                        let existing = entry
                            .pointer("/function/arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("")
                            .to_string();
                        entry["function"]["arguments"] =
                            serde_json::json!(format!("{existing}{d}"));
                    }
                }
                "response.output_item.done" => {
                    // A completed function/custom-tool item carries the full
                    // name/arguments — authoritative over the deltas.
                    if let Some(item) = v.get("item") {
                        if matches!(
                            item.get("type").and_then(|t| t.as_str()),
                            Some("function_call" | "custom_tool_call")
                        ) {
                            let item_id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(|i| i.as_str())
                                .unwrap_or("");
                            let key = item_id
                                .bytes()
                                .fold(0i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64));
                            // Delta accumulation keyed by `item_id` (the "fc_…"
                            // id), while this event keys by call_id — merge onto
                            // the delta entry when both ids are present.
                            let delta_key = item
                                .get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .bytes()
                                .fold(0i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64));
                            tool_calls_map.remove(&delta_key);
                            tool_calls_map.insert(key, serde_json::json!({
                                "index": key,
                                "id": item_id,
                                "type": "function",
                                "function": {
                                    "name": item.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                                    "arguments": item
                                        .get("arguments")
                                        .or_else(|| item.get("input"))
                                        .and_then(|a| a.as_str())
                                        .unwrap_or(""),
                                }
                            }));
                        }
                    }
                }
                "response.completed" | "response.done" => {
                    // Try to extract from the final response object
                    if let Some(resp) = v.get("response") {
                        if let Some(output) = resp.get("output").and_then(|o| o.as_array()) {
                            for item in output {
                                if let Some(c) = item.get("content").and_then(|c| c.as_array()) {
                                    for part in c {
                                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                            if content.is_empty() {
                                                content.push_str(t);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::codec::{DecodeError, Usage};

    struct Decoder {
        usage: Option<Usage>,
    }

    impl StreamDecoder for Decoder {
        fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, DecodeError> {
            let text = String::from_utf8_lossy(bytes);
            if text.contains("bad") {
                return Err(DecodeError::new("/", "bad upstream event"));
            }
            if text.contains("usage") {
                self.usage = Some(Usage {
                    input_tokens: 2,
                    output_tokens: 3,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    usage_unknown: false,
                });
            }
            Ok(vec![format!("out:{text}")])
        }

        fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
            Ok(vec!["done".into()])
        }

        fn usage(&self) -> Option<Usage> {
            self.usage
        }
    }

    fn supervisor() -> StreamSupervisor {
        let mut supervisor = StreamSupervisor::new();
        supervisor.begin_connect().unwrap();
        supervisor.on_upstream_headers().unwrap();
        supervisor.on_first_frame_validated().unwrap();
        supervisor
    }

    #[test]
    fn first_record_and_carry_are_decoded_before_commit() {
        let mut pump = StreamPumpCore::new(
            supervisor(),
            Box::new(Decoder { usage: None }),
            b"first".to_vec(),
            b"carry".to_vec(),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(pump.start().unwrap()).unwrap(),
            "out:firstout:carry"
        );
        assert!(pump.committed());
    }

    #[test]
    fn decoder_usage_reaches_the_pump() {
        let mut pump = StreamPumpCore::new(
            supervisor(),
            Box::new(Decoder { usage: None }),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        pump.push(b"usage").unwrap();
        assert_eq!(pump.usage(), (2, 3, 5, 0));
        assert_eq!(pump.finish().unwrap(), b"done");
        assert!(pump.terminated());
    }

    /// #57：终止标记识别——三种下游协议的成功终止事件都算，错误事件不算。
    #[test]
    fn terminal_event_detection_covers_three_protocols() {
        // Chat
        assert!(sse_event_is_terminal("data: [DONE]\n\n"));
        // Anthropic（含 event: 行与 data JSON 两种形态）
        assert!(sse_event_is_terminal(
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        ));
        // Responses
        assert!(sse_event_is_terminal(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\"}}\n\n"
        ));
        assert!(sse_event_is_terminal(
            "data: {\"type\":\"response.done\"}\n\n"
        ));
        // 非终止：普通 delta / 错误事件（错误仍走 EOF/超时路径，不视为成功完成）
        assert!(!sse_event_is_terminal(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"
        ));
        assert!(!sse_event_is_terminal(
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\"}}\n\n"
        ));
        // JSON 带空格的宽松形态
        assert!(sse_event_is_terminal(
            "data: {\"type\": \"message_stop\"}\n\n"
        ));
    }

    /// #57：push 阶段检测到终止即登记 terminated()——不需要等 finish()。
    /// 用透传解码器（把输入原样作为事件输出）验证真实事件流。
    struct PassthroughDecoder;

    impl StreamDecoder for PassthroughDecoder {
        fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, DecodeError> {
            Ok(vec![String::from_utf8_lossy(bytes).to_string()])
        }
        fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
            Ok(vec![])
        }
        fn usage(&self) -> Option<Usage> {
            None
        }
    }

    #[test]
    fn push_registers_terminal_without_finish() {
        let mut pump = StreamPumpCore::new(
            supervisor(),
            Box::new(PassthroughDecoder),
            b"event: message_start\ndata: {\"type\":\"message_start\"}\n\n".to_vec(),
            Vec::new(),
        )
        .unwrap();
        pump.start().unwrap();
        assert!(!pump.terminated(), "message_start is not terminal");
        pump.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
            .unwrap();
        assert!(
            pump.terminated(),
            "message_stop during push must register terminal without finish()"
        );
        // finish 之后重复登记仍恰好一次（幂等）。
        pump.finish().unwrap();
        assert!(pump.terminated());
    }

    #[test]
    fn first_decoder_failure_stays_precommit() {
        let result = StreamPumpCore::new(
            supervisor(),
            Box::new(Decoder { usage: None }),
            b"bad".to_vec(),
            Vec::new(),
        );
        let error = match result {
            Ok(_) => panic!("bad first event must fail before commit"),
            Err(error) => error,
        };
        assert!(error.message().contains("bad upstream event"));
    }

    fn accumulate(
        event: &str,
    ) -> (
        String,
        String,
        Option<String>,
        Option<String>,
        std::collections::BTreeMap<i64, serde_json::Value>,
    ) {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut role = None;
        let mut finish_reason = None;
        let mut tool_calls = std::collections::BTreeMap::new();
        accumulate_from_sse_event(
            event,
            &mut content,
            &mut reasoning,
            &mut role,
            &mut finish_reason,
            &mut tool_calls,
        );
        (content, reasoning, role, finish_reason, tool_calls)
    }

    /// Regression: Responses API events carry a `type` field and used to be
    /// swallowed by the Anthropic branch's unconditional `continue`, leaving
    /// `response_choices` empty for every streaming Responses request.
    #[test]
    fn responses_api_output_text_deltas_are_accumulated() {
        let (mut content, ..) =
            accumulate("data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}");
        let (c2, reasoning, _, _, _) =
            accumulate("data: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}");
        content.push_str(&c2);
        assert_eq!(content, "Hello world");
        assert!(reasoning.is_empty());
    }

    #[test]
    fn responses_api_reasoning_deltas_are_accumulated() {
        let (_, reasoning, ..) = accumulate(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}",
        );
        assert_eq!(reasoning, "thinking");
    }

    #[test]
    fn responses_api_completed_extracts_text_fallback() {
        let (content, ..) = accumulate(
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"content\":[{\"type\":\"output_text\",\"text\":\"final answer\"}]}]}}",
        );
        assert_eq!(content, "final answer");
    }

    #[test]
    fn responses_api_function_call_output_item_is_accumulated() {
        let (.., tool_calls) = accumulate(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}}",
        );
        assert_eq!(tool_calls.len(), 1);
        let tc = tool_calls.values().next().unwrap();
        assert_eq!(
            tc.pointer("/function/name").and_then(|n| n.as_str()),
            Some("get_weather")
        );
        assert!(tc
            .pointer("/function/arguments")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .contains("Paris"));
    }

    #[test]
    fn responses_api_custom_tool_call_output_item_is_accumulated() {
        let (.., tool_calls) = accumulate(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ctc_1\",\"call_id\":\"call_1\",\"name\":\"exec\",\"input\":\"text(tools.exec_command({}))\"}}",
        );
        assert_eq!(tool_calls.len(), 1);
        let tool_call = tool_calls.values().next().unwrap();
        assert_eq!(
            tool_call.pointer("/function/name").and_then(|n| n.as_str()),
            Some("exec")
        );
        assert_eq!(
            tool_call
                .pointer("/function/arguments")
                .and_then(|a| a.as_str()),
            Some("text(tools.exec_command({}))")
        );
    }
}
