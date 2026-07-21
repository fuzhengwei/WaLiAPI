use serde_json::Value;

/// Convert an OpenAI SSE chunk (Chat Completions stream) to Anthropic Messages SSE events.
///
/// Anthropic stream events:
/// - event: message_start, data: {type: "message_start", message: {...}}
/// - event: content_block_start, data: {type: "content_block_start", index: 0, content_block: {type: "text", text: ""}}
/// - event: content_block_delta, data: {type: "content_block_delta", index: 0, delta: {type: "text_delta", text: "..."}}
/// - event: content_block_stop, data: {type: "content_block_stop", index: 0}
/// - event: message_delta, data: {type: "message_delta", delta: {stop_reason: "end_turn"}, usage: {output_tokens: N}}
/// - event: message_stop, data: {type: "message_stop"}
///
/// OpenAI stream chunk:
/// ```json
/// {"id":"...","choices":[{"delta":{"content":"hello"},"finish_reason":null}]}
/// ```
pub fn convert_openai_sse_to_anthropic(
    chunk_text: &str,
    model: &str,
    message_id: &str,
    state: &mut AnthropicStreamState,
) -> Vec<String> {
    let mut events = Vec::new();

    // Emit message_start on first chunk
    if !state.started {
        let msg_start = serde_json::json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "usage": {
                    "input_tokens": state.input_tokens,
                    "output_tokens": 0
                }
            }
        });
        events.push(format!("event: message_start\ndata: {}\n\n", msg_start));

        let block_start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        });
        events.push(format!("event: content_block_start\ndata: {}\n\n", block_start));

        state.started = true;
    }

    for line in chunk_text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data_str = trimmed.trim_start_matches("data:").trim();
        if data_str == "[DONE]" || data_str.is_empty() {
            continue;
        }

        let json: Value = match serde_json::from_str(data_str) {
            Ok(j) => j,
            Err(_) => continue,
        };

        if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    // Content delta
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            let block_delta = serde_json::json!({
                                "type": "content_block_delta",
                                "index": 0,
                                "delta": {
                                    "type": "text_delta",
                                    "text": content
                                }
                            });
                            events.push(format!("event: content_block_delta\ndata: {}\n\n", block_delta));
                            state.output_tokens += 1; // approximate
                        }
                    }
                }

                // Check for finish_reason
                if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    if !finish.is_empty() && finish != "null" {
                        // Close content block
                        let block_stop = serde_json::json!({
                            "type": "content_block_stop",
                            "index": 0
                        });
                        events.push(format!("event: content_block_stop\ndata: {}\n\n", block_stop));

                        let stop_reason = match finish {
                            "stop" => "end_turn",
                            "length" => "max_tokens",
                            "tool_calls" => "tool_use",
                            _ => "end_turn",
                        };

                        // Extract usage if present
                        let prompt_tokens = json.get("usage").and_then(|u| u.get("prompt_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);
                        let completion_tokens = json.get("usage").and_then(|u| u.get("completion_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);

                        let msg_delta = serde_json::json!({
                            "type": "message_delta",
                            "delta": {
                                "stop_reason": stop_reason,
                                "stop_sequence": null
                            },
                            "usage": {
                                "input_tokens": prompt_tokens,
                                "output_tokens": completion_tokens
                            }
                        });
                        events.push(format!("event: message_delta\ndata: {}\n\n", msg_delta));

                        let msg_stop = serde_json::json!({"type": "message_stop"});
                        events.push(format!("event: message_stop\ndata: {}\n\n", msg_stop));
                    }
                }
            }
        }
    }

    events
}

/// State for Anthropic stream conversion.
#[derive(Default)]
pub struct AnthropicStreamState {
    pub started: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Parse usage from OpenAI SSE chunk for Anthropic logging.
pub fn parse_usage_from_sse_chunk(text: &str) -> Option<(i64, i64, i64)> {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data_str = trimmed.trim_start_matches("data:").trim();
        if data_str == "[DONE]" || data_str.is_empty() {
            continue;
        }
        if let Ok(json) = serde_json::from_str::<Value>(data_str) {
            if let Some(usage) = json.get("usage") {
                let prompt = usage.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                let completion = usage.get("completion_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                let total = usage.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
                if total > 0 || prompt > 0 || completion > 0 {
                    return Some((prompt, completion, total));
                }
            }
        }
    }
    None
}
