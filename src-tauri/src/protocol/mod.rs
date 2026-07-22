pub mod responses;
pub mod anthropic;

use serde_json::Value;

/// Extract API key from either `Authorization: Bearer xxx` or `x-api-key: xxx` header.
pub fn extract_api_key(headers: &axum::http::HeaderMap) -> Option<String> {
    // Try Authorization: Bearer xxx first
    if let Some(auth) = headers.get("authorization").and_then(|h| h.to_str().ok()) {
        if let Some(key) = auth.strip_prefix("Bearer ") {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    // Fall back to x-api-key
    if let Some(key) = headers.get("x-api-key").and_then(|h| h.to_str().ok()) {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Detect if a request is in Anthropic format by checking headers and body.
pub fn is_anthropic_request(headers: &axum::http::HeaderMap, body: &Value) -> bool {
    // Check for anthropic-version header
    if headers.contains_key("anthropic-version") {
        return true;
    }
    // Check for x-api-key without Authorization Bearer
    if headers.contains_key("x-api-key") && !headers.contains_key("authorization") {
        return true;
    }
    // Check body: Anthropic format uses "max_tokens" but not "messages" with OpenAI structure
    // Actually both use "messages", so rely on headers primarily.
    // As a fallback, check if body has "max_tokens" but not "model" (unlikely to help).
    // The header-based detection is the primary signal.
    let _ = body;
    false
}

/// Detect if a request targets the Responses API format.
pub fn is_responses_request(body: &Value) -> bool {
    // Responses API uses "input" instead of "messages"
    body.get("input").is_some() && body.get("messages").is_none()
}

/// Convert OpenAI Chat Completions response to Responses API format.
pub fn openai_to_responses(openai_resp: &Value, model: &str) -> Value {
    let choice = openai_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first());

    let message = choice.and_then(|ch| ch.get("message"));

    let content = message
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let finish_reason = choice
        .and_then(|ch| ch.get("finish_reason"))
        .and_then(|f| f.as_str())
        .unwrap_or("stop");

    let prompt_tokens = openai_resp
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let completion_tokens = openai_resp
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);

    // Build output array: message + function_call items
    let mut output = Vec::new();

    // Add function_call outputs for tool_calls
    if let Some(tool_calls) = message.and_then(|m| m.get("tool_calls")).and_then(|t| t.as_array()) {
        for tc in tool_calls {
            let name = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("");
            let arguments = tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("");
            let call_id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
            output.push(serde_json::json!({
                "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
                "status": "completed"
            }));
        }
    }

    // Add text message output (always include, even if empty when tool_calls present)
    if !content.is_empty() || output.is_empty() {
        output.push(serde_json::json!({
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": content
            }],
            "status": "completed"
        }));
    }

    serde_json::json!({
        "id": openai_resp.get("id").cloned().unwrap_or(Value::String(format!("resp_{}", uuid::Uuid::new_v4()))),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": model,
        "output": output,
        "usage": {
            "input_tokens": prompt_tokens,
            "output_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        },
        "status": "completed",
        "finish_reason": finish_reason
    })
}

/// Convert Responses API request to OpenAI Chat Completions format.
pub fn responses_to_openai(body: &Value) -> Value {
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    
    // Convert input array to messages array
    let messages = if let Some(input) = body.get("input") {
        convert_responses_input_to_messages(input)
    } else {
        Value::Array(vec![])
    };

    // max_output_tokens -> max_tokens
    let max_tokens = body.get("max_output_tokens").and_then(|m| m.as_u64()).unwrap_or(4096);

    let stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    let mut openai_body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": stream,
    });

    // Pass through temperature if present
    if let Some(temp) = body.get("temperature") {
        openai_body["temperature"] = temp.clone();
    }
    // Pass through top_p if present
    if let Some(top_p) = body.get("top_p") {
        openai_body["top_p"] = top_p.clone();
    }
    // Convert Responses API tools to Chat Completions tools format.
    // Responses API uses flat format: { type: "function", name, parameters, description }
    // Chat Completions uses nested format: { type: "function", function: { name, parameters, description } }
    if let Some(tools) = body.get("tools") {
        if let Some(arr) = tools.as_array() {
            let openai_tools: Vec<Value> = arr.iter().filter_map(|t| {
                let tool_type = t.get("type").and_then(|ty| ty.as_str()).unwrap_or("");
                match tool_type {
                    // Function tools: convert flat → nested
                    "function" => {
                        // Already in Chat Completions format (has "function" field) — pass through
                        if t.get("function").is_some() {
                            return Some(t.clone());
                        }
                        // Responses API flat format → convert to Chat Completions nested format
                        let func = serde_json::json!({
                            "name": t.get("name").cloned().unwrap_or(Value::Null),
                            "parameters": t.get("parameters").cloned().unwrap_or(Value::Null),
                        });
                        let mut func_obj = func;
                        if let Some(desc) = t.get("description") {
                            func_obj["description"] = desc.clone();
                        }
                        if let Some(strict) = t.get("strict") {
                            func_obj["strict"] = strict.clone();
                        }
                        Some(serde_json::json!({
                            "type": "function",
                            "function": func_obj
                        }))
                    }
                    // Built-in tools (web_search, file_search, computer_use, etc.) — skip
                    _ => None
                }
            }).collect();
            if !openai_tools.is_empty() {
                openai_body["tools"] = Value::Array(openai_tools);
            }
        }
    }

    // Pass through tool_choice (format is the same between Responses and Chat Completions)
    if let Some(tc) = body.get("tool_choice") {
        openai_body["tool_choice"] = tc.clone();
    }

    // Pass through instructions as a system message if present
    if let Some(instructions) = body.get("instructions").and_then(|i| i.as_str()) {
        if !instructions.is_empty() {
            if let Some(msgs) = openai_body.get_mut("messages").and_then(|m| m.as_array_mut()) {
                msgs.insert(0, serde_json::json!({
                    "role": "system",
                    "content": instructions
                }));
            }
        }
    }

    openai_body
}

/// Convert Responses API `input` array to OpenAI `messages` array.
/// Handles: message, function_call (assistant tool call), function_call_output (tool result)
fn convert_responses_input_to_messages(input: &Value) -> Value {
    let messages = if let Some(arr) = input.as_array() {
        let mut msgs = Vec::new();
        for item in arr {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match item_type {
                // function_call: assistant's tool call → OpenAI assistant message with tool_calls
                "function_call" => {
                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let arguments = item.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                    let call_id = item.get("call_id").and_then(|c| c.as_str()).unwrap_or("");
                    msgs.push(serde_json::json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": arguments
                            }
                        }]
                    }));
                }

                // function_call_output: tool result → OpenAI tool message
                "function_call_output" => {
                    let call_id = item.get("call_id").and_then(|c| c.as_str()).unwrap_or("");
                    let output = item.get("output").and_then(|o| o.as_str()).unwrap_or("");
                    msgs.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }

                // message: standard chat message
                "message" | _ if item.get("role").is_some() => {
                    let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user").to_string();
                    let content = if let Some(content_arr) = item.get("content").and_then(|c| c.as_array()) {
                        // Extract text from content blocks
                        let texts: Vec<String> = content_arr.iter().filter_map(|block| {
                            // input_text, output_text, text
                            block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                        }).collect();
                        Value::String(texts.join(""))
                    } else if let Some(text) = item.get("content").and_then(|c| c.as_str()) {
                        Value::String(text.to_string())
                    } else {
                        Value::String(String::new())
                    };
                    msgs.push(serde_json::json!({
                        "role": role,
                        "content": content,
                    }));
                }

                // Simple text item
                _ if item.get("text").is_some() => {
                    let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    msgs.push(serde_json::json!({
                        "role": "user",
                        "content": text,
                    }));
                }

                // Raw string input
                _ => {
                    if let Some(s) = item.as_str() {
                        msgs.push(serde_json::json!({
                            "role": "user",
                            "content": s,
                        }));
                    }
                }
            }
        }
        msgs
    } else if let Some(s) = input.as_str() {
        // Simple string input
        vec![serde_json::json!({"role": "user", "content": s})]
    } else {
        vec![]
    };

    Value::Array(messages)
}

/// Convert OpenAI Chat Completions response to Anthropic Messages format.
pub fn openai_to_anthropic(openai_resp: &Value, model: &str) -> Value {
    let choice = openai_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first());

    let message = choice.and_then(|ch| ch.get("message"));

    let content_text = message
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let finish_reason = choice
        .and_then(|ch| ch.get("finish_reason"))
        .and_then(|f| f.as_str())
        .unwrap_or("stop");

    let stop_reason = match finish_reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    };

    let input_tokens = openai_resp
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    let output_tokens = openai_resp
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);

    // Build content array: text blocks + tool_use blocks
    let mut content_blocks = Vec::new();

    // Add text block if present
    if !content_text.is_empty() {
        content_blocks.push(serde_json::json!({
            "type": "text",
            "text": content_text
        }));
    }

    // Add tool_use blocks for tool_calls
    if let Some(tool_calls) = message.and_then(|m| m.get("tool_calls")).and_then(|t| t.as_array()) {
        for tc in tool_calls {
            let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
            let func = tc.get("function");
            let name = func.and_then(|f| f.get("name").and_then(|n| n.as_str())).unwrap_or("");
            let arguments_str = func.and_then(|f| f.get("arguments").and_then(|a| a.as_str())).unwrap_or("{}");
            let input: Value = serde_json::from_str(&arguments_str).unwrap_or(serde_json::json!({}));

            content_blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input
            }));
        }
    }

    // If no content blocks at all, add empty text
    if content_blocks.is_empty() {
        content_blocks.push(serde_json::json!({
            "type": "text",
            "text": ""
        }));
    }

    serde_json::json!({
        "id": openai_resp.get("id").cloned().unwrap_or(Value::String(format!("msg_{}", uuid::Uuid::new_v4().simple()))),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content_blocks,
        "stop_reason": stop_reason,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens
        }
    })
}

/// Convert Anthropic Messages request to OpenAI Chat Completions format.
pub fn anthropic_to_openai(body: &Value) -> Value {
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let messages = body.get("messages").cloned().unwrap_or(Value::Array(vec![]));
    let max_tokens = body.get("max_tokens").and_then(|m| m.as_u64()).unwrap_or(4096);
    let stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    // Extract system message and prepend it
    let system = body.get("system").and_then(|s| {
        if let Some(str_val) = s.as_str() {
            Some(str_val.to_string())
        } else if let Some(arr) = s.as_array() {
            // Anthropic system can be an array of content blocks
            let texts: Vec<String> = arr.iter().filter_map(|block| {
                block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
            }).collect();
            Some(texts.join(""))
        } else {
            None
        }
    });

    // Convert Anthropic message content (array format) to OpenAI string format
    let openai_messages = convert_anthropic_messages_to_openai(&messages, system);

    let mut openai_body = serde_json::json!({
        "model": model,
        "messages": openai_messages,
        "max_tokens": max_tokens,
        "stream": stream,
    });

    if let Some(temp) = body.get("temperature") {
        openai_body["temperature"] = temp.clone();
    }
    if let Some(top_p) = body.get("top_p") {
        openai_body["top_p"] = top_p.clone();
    }
    // Pass through top_k (OpenAI also supports this via some providers)
    if let Some(top_k) = body.get("top_k") {
        openai_body["top_k"] = top_k.clone();
    }
    // Pass through stop_sequences → stop
    if let Some(stop_seq) = body.get("stop_sequences") {
        openai_body["stop"] = stop_seq.clone();
    }
    // Note: thinking field is not forwarded to OpenAI API since most providers don't support it.
    // It's safely ignored rather than causing an error.

    // Convert Anthropic tools to OpenAI tools format
    // Anthropic: {"name": "xxx", "description": "xxx", "input_schema": {...}}
    // OpenAI: {"type": "function", "function": {"name": "xxx", "description": "xxx", "parameters": {...}}}
    // Also handles Anthropic built-in tools (web_search, computer_use, etc.) which are skipped.
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let openai_tools: Vec<Value> = tools.iter().filter_map(|tool| {
            // Get the tool type — Anthropic custom tools use "custom" or have no type field
            let tool_type = tool.get("type").and_then(|t| t.as_str()).unwrap_or("custom");
            match tool_type {
                // Standard function tools (type "custom" or no type)
                "custom" | "" => {
                    let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    if name.is_empty() { return None; }
                    let description = tool.get("description").and_then(|d| d.as_str()).unwrap_or("");
                    let parameters = tool.get("input_schema").cloned().unwrap_or(serde_json::json!({}));
                    Some(serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": parameters
                        }
                    }))
                }
                // Built-in tools (web_search_*, computer_*, bash_*, text_editor_*, etc.) — skip
                _ => None
            }
        }).collect();
        if !openai_tools.is_empty() {
            openai_body["tools"] = Value::Array(openai_tools);
        }
    }

    // Convert tool_choice
    // Anthropic: {"type": "auto"} or {"type": "any"} or {"type": "tool", "name": "xxx"}
    // OpenAI: "auto" or "required" or {"type": "function", "function": {"name": "xxx"}}
    if let Some(tc) = body.get("tool_choice") {
        if let Some(tc_type) = tc.get("type").and_then(|t| t.as_str()) {
            let openai_tc = match tc_type {
                "auto" => Value::String("auto".to_string()),
                "any" => Value::String("required".to_string()),
                "tool" => {
                    let name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    serde_json::json!({
                        "type": "function",
                        "function": {"name": name}
                    })
                }
                _ => Value::String("auto".to_string()),
            };
            openai_body["tool_choice"] = openai_tc;
        } else if let Some(s) = tc.as_str() {
            openai_body["tool_choice"] = Value::String(s.to_string());
        }
    }

    openai_body
}

/// Convert Anthropic messages array to OpenAI messages array.
/// Anthropic content can be string or array of content blocks.
/// Handles: text, tool_use (assistant), tool_result (user)
fn convert_anthropic_messages_to_openai(messages: &Value, system: Option<String>) -> Value {
    let mut msgs = Vec::new();

    // Prepend system message if present
    if let Some(sys) = system {
        msgs.push(serde_json::json!({"role": "system", "content": sys}));
    }

    if let Some(arr) = messages.as_array() {
        for msg in arr {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user").to_string();

            if let Some(content_arr) = msg.get("content").and_then(|c| c.as_array()) {
                // Complex content: may contain text, tool_use, or tool_result blocks
                let mut text_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                let mut tool_results: Vec<Value> = Vec::new();

                for block in content_arr {
                    match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "text" => {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        "tool_use" => {
                            // Anthropic tool_use → OpenAI assistant tool_calls
                            let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                            let arguments = serde_json::to_string(&input).unwrap_or_default();
                            tool_calls.push(serde_json::json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": arguments
                                }
                            }));
                        }
                        "thinking" => {
                            // Anthropic thinking block — extract text, prepend to message content
                            // OpenAI doesn't have a native thinking block, so we include it as text
                            if let Some(t) = block.get("thinking").and_then(|t| t.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        "image" => {
                            // Anthropic image block — skip for now (would need URL conversion)
                            // Future: convert to OpenAI image_url format
                        }
                        "tool_result" => {
                            // Anthropic tool_result → OpenAI tool message
                            let tool_use_id = block.get("tool_use_id").and_then(|t| t.as_str()).unwrap_or("");
                            let result_content = if let Some(rc) = block.get("content").and_then(|c| c.as_array()) {
                                // Extract text from result content blocks
                                let texts: Vec<String> = rc.iter().filter_map(|b| {
                                    b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                                }).collect();
                                texts.join("")
                            } else if let Some(s) = block.get("content").and_then(|c| c.as_str()) {
                                s.to_string()
                            } else {
                                String::new()
                            };
                            tool_results.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": result_content
                            }));
                        }
                        _ => {}
                    }
                }

                if !tool_calls.is_empty() {
                    // Assistant message with tool_calls
                    let content = if text_parts.is_empty() { Value::Null } else { Value::String(text_parts.join("")) };
                    msgs.push(serde_json::json!({
                        "role": role,
                        "content": content,
                        "tool_calls": tool_calls
                    }));
                } else if !tool_results.is_empty() {
                    // Tool result messages (may be multiple)
                    // If there's also text, add it as a preceding user message
                    if !text_parts.is_empty() {
                        msgs.push(serde_json::json!({
                            "role": role,
                            "content": text_parts.join("")
                        }));
                    }
                    for tr in tool_results {
                        msgs.push(tr);
                    }
                } else {
                    // Plain text message
                    msgs.push(serde_json::json!({
                        "role": role,
                        "content": text_parts.join("")
                    }));
                }
            } else if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                msgs.push(serde_json::json!({
                    "role": role,
                    "content": s.to_string(),
                }));
            } else {
                msgs.push(serde_json::json!({
                    "role": role,
                    "content": msg.get("content").cloned().unwrap_or(Value::String(String::new())),
                }));
            }
        }
    }

    Value::Array(msgs)
}
