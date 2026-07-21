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
    let content = openai_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let finish_reason = openai_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("finish_reason"))
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

    serde_json::json!({
        "id": openai_resp.get("id").cloned().unwrap_or(Value::String(format!("resp_{}", uuid::Uuid::new_v4()))),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": model,
        "output": [{
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": content
            }],
            "status": "completed"
        }],
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
    // Pass through tools (convert Responses tool format to OpenAI function tool format)
    if let Some(tools) = body.get("tools") {
        if let Some(arr) = tools.as_array() {
            let openai_tools: Vec<Value> = arr.iter().filter_map(|t| {
                // Only convert function-type tools, skip built-in tools (web_search, file_search, etc.)
                if t.get("type").and_then(|ty| ty.as_str()) == Some("function") {
                    Some(t.clone())
                } else {
                    None
                }
            }).collect();
            if !openai_tools.is_empty() {
                openai_body["tools"] = Value::Array(openai_tools);
            }
        }
    }

    openai_body
}

/// Convert Responses API `input` array to OpenAI `messages` array.
fn convert_responses_input_to_messages(input: &Value) -> Value {
    let messages = if let Some(arr) = input.as_array() {
        let mut msgs = Vec::new();
        for item in arr {
            // Each item can be a message: {type: "message", role: "user", content: [{type: "input_text", text: "..."}]}
            if item.get("type").and_then(|t| t.as_str()) == Some("message") || item.get("role").is_some() {
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
            } else if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                // Simple string input or {type: "input_text", text: "..."}
                msgs.push(serde_json::json!({
                    "role": "user",
                    "content": text,
                }));
            } else if let Some(s) = item.as_str() {
                // Raw string input
                msgs.push(serde_json::json!({
                    "role": "user",
                    "content": s,
                }));
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
    let content = openai_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let finish_reason = openai_resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("finish_reason"))
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

    serde_json::json!({
        "id": openai_resp.get("id").cloned().unwrap_or(Value::String(format!("msg_{}", uuid::Uuid::new_v4().simple()))),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{
            "type": "text",
            "text": content
        }],
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

    openai_body
}

/// Convert Anthropic messages array to OpenAI messages array.
/// Anthropic content can be string or array of content blocks.
fn convert_anthropic_messages_to_openai(messages: &Value, system: Option<String>) -> Value {
    let mut msgs = Vec::new();

    // Prepend system message if present
    if let Some(sys) = system {
        msgs.push(serde_json::json!({"role": "system", "content": sys}));
    }

    if let Some(arr) = messages.as_array() {
        for msg in arr {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user").to_string();
            let content = if let Some(content_arr) = msg.get("content").and_then(|c| c.as_array()) {
                // Extract text from content blocks
                let texts: Vec<String> = content_arr.iter().filter_map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                }).collect();
                Value::String(texts.join(""))
            } else if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                Value::String(s.to_string())
            } else {
                msg.get("content").cloned().unwrap_or(Value::String(String::new()))
            };
            msgs.push(serde_json::json!({
                "role": role,
                "content": content,
            }));
        }
    }

    Value::Array(msgs)
}
