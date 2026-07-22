use serde_json::Value;
use std::collections::HashMap;

/// Convert an OpenAI SSE chunk (Chat Completions stream) to Anthropic Messages SSE events.
///
/// Anthropic stream events for text:
/// - event: message_start
/// - event: content_block_start (index=0, type="text")
/// - event: content_block_delta (index=0, type="text_delta")
/// - event: content_block_stop (index=0)
///
/// Anthropic stream events for tool_use:
/// - event: content_block_start (index=N, type="tool_use", id, name)
/// - event: content_block_delta (index=N, type="input_json_delta", partial_json)
/// - event: content_block_stop (index=N)
///
/// Final events:
/// - event: message_delta (stop_reason, usage)
/// - event: message_stop
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
                    // --- Text content delta ---
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            // Ensure text block is open at index 0
                            if !state.text_block_open {
                                // Close any open tool_use blocks before opening text? No — text comes first.
                                let block_start = serde_json::json!({
                                    "type": "content_block_start",
                                    "index": 0,
                                    "content_block": {"type": "text", "text": ""}
                                });
                                events.push(format!("event: content_block_start\ndata: {}\n\n", block_start));
                                state.text_block_open = true;
                                state.current_index = 0;
                            }

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

                    // --- Tool calls delta ---
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tool_calls {
                            let tc_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;

                            // Get or create tool call state
                            let tool_state = state.tool_calls.entry(tc_index).or_insert_with(|| ToolCallState {
                                block_index: 0,
                                id: String::new(),
                                name: String::new(),
                                arguments: String::new(),
                                block_started: false,
                                block_stopped: false,
                            });

                            // If this is the first delta for this tool call, capture id and name
                            if !tool_state.block_started {
                                // Close text block if open
                                if state.text_block_open && !state.text_block_stopped {
                                    let block_stop = serde_json::json!({
                                        "type": "content_block_stop",
                                        "index": 0
                                    });
                                    events.push(format!("event: content_block_stop\ndata: {}\n\n", block_stop));
                                    state.text_block_stopped = true;
                                }

                                // Assign next content block index
                                state.next_block_index += 1;
                                tool_state.block_index = state.next_block_index;

                                // Extract id and function name
                                tool_state.id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                                let func = tc.get("function");
                                tool_state.name = func
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                // Emit content_block_start for tool_use
                                let block_start = serde_json::json!({
                                    "type": "content_block_start",
                                    "index": tool_state.block_index,
                                    "content_block": {
                                        "type": "tool_use",
                                        "id": tool_state.id,
                                        "name": tool_state.name,
                                        "input": {}
                                    }
                                });
                                events.push(format!("event: content_block_start\ndata: {}\n\n", block_start));
                                tool_state.block_started = true;
                            }

                            // Accumulate arguments delta
                            if let Some(args_delta) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()) {
                                tool_state.arguments.push_str(args_delta);

                                // Emit input_json_delta
                                let block_delta = serde_json::json!({
                                    "type": "content_block_delta",
                                    "index": tool_state.block_index,
                                    "delta": {
                                        "type": "input_json_delta",
                                        "partial_json": args_delta
                                    }
                                });
                                events.push(format!("event: content_block_delta\ndata: {}\n\n", block_delta));
                            }
                        }
                    }

                    // --- Reasoning content (thinking) ---
                    // Some OpenAI-compatible providers return delta.reasoning_content
                    if let Some(reasoning) = delta.get("reasoning_content").and_then(|r| r.as_str()) {
                        if !reasoning.is_empty() {
                            // Map to thinking block in Anthropic format
                            if !state.thinking_block_open {
                                let block_start = serde_json::json!({
                                    "type": "content_block_start",
                                    "index": state.next_block_index + 1,
                                    "content_block": {
                                        "type": "thinking",
                                        "thinking": ""
                                    }
                                });
                                events.push(format!("event: content_block_start\ndata: {}\n\n", block_start));
                                state.thinking_block_open = true;
                                state.thinking_block_index = state.next_block_index + 1;
                            }

                            let block_delta = serde_json::json!({
                                "type": "content_block_delta",
                                "index": state.thinking_block_index,
                                "delta": {
                                    "type": "thinking_delta",
                                    "thinking": reasoning
                                }
                            });
                            events.push(format!("event: content_block_delta\ndata: {}\n\n", block_delta));
                        }
                    }
                }

                // Check for finish_reason
                if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    if !finish.is_empty() && finish != "null" {
                        // Close text block if still open
                        if state.text_block_open && !state.text_block_stopped {
                            let block_stop = serde_json::json!({
                                "type": "content_block_stop",
                                "index": 0
                            });
                            events.push(format!("event: content_block_stop\ndata: {}\n\n", block_stop));
                            state.text_block_stopped = true;
                        }

                        // Close all tool_use blocks
                        for (_, ts) in &state.tool_calls {
                            if ts.block_started && !ts.block_stopped {
                                let block_stop = serde_json::json!({
                                    "type": "content_block_stop",
                                    "index": ts.block_index
                                });
                                events.push(format!("event: content_block_stop\ndata: {}\n\n", block_stop));
                            }
                        }

                        // Close thinking block if open
                        if state.thinking_block_open {
                            let block_stop = serde_json::json!({
                                "type": "content_block_stop",
                                "index": state.thinking_block_index
                            });
                            events.push(format!("event: content_block_stop\ndata: {}\n\n", block_stop));
                        }

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

/// State for a single tool call being streamed.
#[derive(Clone)]
struct ToolCallState {
    block_index: usize,
    id: String,
    name: String,
    arguments: String,
    block_started: bool,
    block_stopped: bool,
}

/// State for Anthropic stream conversion.
#[derive(Default)]
pub struct AnthropicStreamState {
    pub started: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    // Text block tracking
    text_block_open: bool,
    text_block_stopped: bool,
    // Tool call tracking: OpenAI tool_call index → state
    tool_calls: HashMap<usize, ToolCallState>,
    // Next content block index (0 = text, 1+ = tool_use)
    next_block_index: usize,
    current_index: usize,
    // Thinking block tracking
    thinking_block_open: bool,
    thinking_block_index: usize,
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
