use serde_json::Value;

/// Convert an OpenAI SSE chunk (Chat Completions stream) to Responses API SSE events.
/// 
/// OpenAI stream chunk format:
/// ```json
/// {"id":"...","choices":[{"delta":{"content":"hello"},"finish_reason":null}]}
/// ```
/// 
/// Responses API stream events:
/// - response.created
/// - response.output_text.delta (for content deltas)
/// - response.function_call_arguments.delta (for tool call argument deltas)
/// - response.completed
///
/// This function takes a raw SSE chunk text and returns a list of event strings to emit.
pub fn convert_openai_sse_to_responses(chunk_text: &str, model: &str, response_id: &str) -> Vec<String> {
    let mut events = Vec::new();

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

        // Extract delta content
        if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    // Content delta
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            let event = serde_json::json!({
                                "type": "response.output_text.delta",
                                "response_id": response_id,
                                "delta": content
                            });
                            events.push(format!("event: response.output_text.delta\ndata: {}\n\n", event));
                        }
                    }

                    // Tool calls delta — emit function_call events
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        for tc in tool_calls {
                            let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                            let tc_id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            let func = tc.get("function");
                            let name = func.and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("");
                            let arguments = func.and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("");

                            // If this delta has an id and name, it's the first chunk of a tool call
                            // → emit function_call event with name
                            if !tc_id.is_empty() && !name.is_empty() {
                                let event = serde_json::json!({
                                    "type": "response.function_call",
                                    "response_id": response_id,
                                    "call_id": tc_id,
                                    "name": name,
                                    "arguments": ""
                                });
                                events.push(format!("event: response.function_call\ndata: {}\n\n", event));
                            }

                            // If this delta has argument fragments, emit delta event
                            if !arguments.is_empty() {
                                let event = serde_json::json!({
                                    "type": "response.function_call_arguments.delta",
                                    "response_id": response_id,
                                    "item_id": format!("fc_{}", index),
                                    "delta": arguments
                                });
                                events.push(format!("event: response.function_call_arguments.delta\ndata: {}\n\n", event));
                            }
                        }
                    }
                }
                
                // Check for finish_reason
                if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    if !finish.is_empty() && finish != "null" {
                        let usage_prompt = json.get("usage").and_then(|u| u.get("prompt_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);
                        let usage_completion = json.get("usage").and_then(|u| u.get("completion_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);

                        let completed = serde_json::json!({
                            "type": "response.completed",
                            "response_id": response_id,
                            "model": model,
                            "status": "completed",
                            "usage": {
                                "input_tokens": usage_prompt,
                                "output_tokens": usage_completion,
                                "total_tokens": usage_prompt + usage_completion
                            }
                        });
                        events.push(format!("event: response.completed\ndata: {}\n\n", completed));
                    }
                }
            }
        }
    }

    // If the chunk contained [DONE], emit our own DONE marker
    if chunk_text.contains("[DONE]") {
        events.push("data: [DONE]\n\n".to_string());
    }

    events
}

/// Create the initial response.created event for Responses API stream.
pub fn create_response_created_event(model: &str, response_id: &str) -> String {
    let event = serde_json::json!({
        "type": "response.created",
        "response_id": response_id,
        "model": model,
        "status": "in_progress"
    });
    format!("event: response.created\ndata: {}\n\n", event)
}

/// Parse usage from OpenAI SSE chunk (reuses logic from handlers).
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
