use serde_json::Value;

/// Normalize a Responses API `tool_choice` to OpenAI Chat Completions shape.
///
/// Responses API accepts either a bare string ("auto" | "none" | "required") or
/// an object — `{"type": "auto"|"none"|"required"}` or
/// `{"type": "function", "name": "foo"}`. Chat Completions wants a bare string
/// or `{"type": "function", "function": {"name": "foo"}}`. Returns `None` when
/// the value cannot be represented (caller then drops tool_choice).
fn responses_tool_choice_to_chat(tc: &Value) -> Option<Value> {
    if let Some(s) = tc.as_str() {
        return Some(Value::String(s.to_string()));
    }
    let obj = tc.as_object()?;
    let ty = obj.get("type")?.as_str()?;
    match ty {
        "auto" | "none" | "required" => Some(Value::String(ty.to_string())),
        "function" => {
            let name = obj.get("name")?.as_str()?;
            if name.is_empty() {
                return None;
            }
            Some(serde_json::json!({
                "type": "function",
                "function": {"name": name}
            }))
        }
        _ => None,
    }
}

/// Convert Responses API request to OpenAI Chat Completions format.
pub fn responses_to_openai(
    body: &Value,
) -> Result<Value, crate::protocol::codec::UnsupportedFeatures> {
    const SUPPORTED_TOP_LEVEL: &[&str] = &[
        "model",
        "input",
        "instructions",
        "tools",
        "tool_choice",
        "max_output_tokens",
        "stream",
        "temperature",
        "top_p",
        // Codex Responses controls with no Chat representation: tolerated and
        // dropped (the Responses→Messages composition wrapper records them in
        // the ConversionReport).
        "parallel_tool_calls",
        "store",
        "include",
        "prompt_cache_key",
        "prompt_cache_options",
        "client_metadata",
        // `text` 控制输出格式与详细度：`format` 映射为 Chat 的
        // `response_format`，`verbosity` 没有 Chat 等价物（丢弃）。
        "text",
        // Mapped below: `reasoning.effort` → top-level `reasoning_effort`.
        "reasoning",
    ];
    let object = body.as_object().ok_or_else(|| {
        crate::protocol::codec::UnsupportedFeatures::single(
            crate::protocol::codec::FeatureKind::UnsupportedField,
            "/",
            "Responses request must be a JSON object",
        )
    })?;
    for key in object.keys() {
        if !SUPPORTED_TOP_LEVEL.contains(&key.as_str()) {
            return Err(crate::protocol::codec::UnsupportedFeatures::single(
                crate::protocol::codec::FeatureKind::UnsupportedField,
                format!("/{key}"),
                format!("Responses field {key:?} is not supported by Responses→Chat conversion"),
            ));
        }
    }
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();

    // Convert input array to messages array
    let messages = if let Some(input) = body.get("input") {
        convert_responses_input_to_messages(input)
    } else {
        Value::Array(vec![])
    };

    // max_output_tokens -> max_tokens
    let max_tokens = body
        .get("max_output_tokens")
        .and_then(|m| m.as_u64())
        .unwrap_or(4096);

    let stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

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

    // Responses `text.format` → Chat `response_format`：Codex CLI 的请求会带
    // `text`，JSON schema 输出要保住，`verbosity` 等无对应物的字段丢弃而非 400。
    if let Some(text) = body.get("text") {
        match text
            .pointer("/format/type")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "json_schema" => {
                let format = text.get("format").unwrap_or(&Value::Null);
                openai_body["response_format"] = serde_json::json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": format.get("name").and_then(Value::as_str).unwrap_or("response"),
                        "schema": format.get("schema").cloned().unwrap_or_else(|| serde_json::json!({})),
                        "strict": format.get("strict").cloned().unwrap_or(Value::Bool(false)),
                    }
                });
            }
            "json_object" => {
                openai_body["response_format"] = serde_json::json!({"type": "json_object"});
            }
            // "text" 或缺失：默认文本输出，无需任何字段。
            _ => {}
        }
    }
    // Convert Responses API tools to Chat Completions tools format.
    // Responses API uses flat format: { type: "function", name, parameters, description }
    // Chat Completions uses nested format: { type: "function", function: { name, parameters, description } }
    if let Some(tools) = body.get("tools") {
        if let Some(arr) = tools.as_array() {
            let openai_tools: Vec<Value> = arr
                .iter()
                .filter_map(|t| {
                    let tool_type = t.get("type").and_then(|ty| ty.as_str()).unwrap_or("");
                    match tool_type {
                        // Function tools: convert flat → nested
                        "function" => {
                            // Already in Chat Completions format (has "function" field) — pass through
                            if t.get("function").is_some() {
                                return Some(t.clone());
                            }
                            // Responses API flat format → convert to Chat Completions nested format.
                            // Chat Completions requires an object JSON schema.
                            let parameters = t.get("parameters").cloned().unwrap_or(Value::Null);
                            let parameters = if parameters.is_null() || !parameters.is_object() {
                                serde_json::json!({"type": "object", "properties": {}})
                            } else {
                                let mut params = parameters;
                                if params.get("type").is_none() {
                                    if let Some(obj) = params.as_object_mut() {
                                        obj.insert(
                                            "type".to_string(),
                                            Value::String("object".to_string()),
                                        );
                                    }
                                }
                                params
                            };
                            let func = serde_json::json!({
                                "name": t.get("name").cloned().unwrap_or(Value::Null),
                                "parameters": parameters,
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
                        _ => None,
                    }
                })
                .collect();
            if !openai_tools.is_empty() {
                openai_body["tools"] = Value::Array(openai_tools);
            }
        }
    }

    // Normalize tool_choice to Chat Completions shape, but ONLY when the
    // converted request actually carries function tools. `openai_body["tools"]`
    // is only set when the conversion produced a non-empty array, so its
    // presence is the exact gate. OpenAI Chat Completions rejects `tool_choice`
    // without `tools` ("When using `tool_choice`, `tools` must be set."), and
    // Codex sends `tool_choice: "auto"` even for plain no-tool requests —
    // passing it through unconditionally turns those into an upstream 400/502.
    if openai_body.get("tools").is_some() {
        if let Some(tc) = body.get("tool_choice") {
            if let Some(normalized) = responses_tool_choice_to_chat(tc) {
                openai_body["tool_choice"] = normalized;
            }
        }
    }

    // Map Responses `reasoning.effort` to Chat `reasoning_effort` so the
    // Chat→Messages leg (encode_chat_to_messages) can express it as Anthropic
    // thinking. A missing or malformed `reasoning` is tolerated fail-open.
    if let Some(effort) = body
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
    {
        openai_body["reasoning_effort"] = Value::String(effort.to_string());
    }

    // Pass through instructions as a system message if present
    if let Some(instructions) = body.get("instructions").and_then(|i| i.as_str()) {
        if !instructions.is_empty() {
            if let Some(msgs) = openai_body
                .get_mut("messages")
                .and_then(|m| m.as_array_mut())
            {
                msgs.insert(
                    0,
                    serde_json::json!({
                        "role": "system",
                        "content": instructions
                    }),
                );
            }
        }
    }

    Ok(openai_body)
}

/// Convert Responses API `input` array to OpenAI `messages` array.
/// Handles: message, function_call (assistant tool call), function_call_output (tool result)
pub(super) fn convert_responses_input_to_messages(input: &Value) -> Value {
    let messages = if let Some(arr) = input.as_array() {
        // First pass: collect all function_call call_ids and their matching outputs
        let mut call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut output_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Map from original (possibly empty) call_id → fallback call_id
        let mut call_id_fallback: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut fallback_counter = 0u32;
        // Whether a `function_call` item appears anywhere AFTER index `i`.
        // A `reasoning` item followed (possibly through an intermediate
        // assistant text message) by function_calls belongs to that
        // tool-calling turn: its text must ride on the assistant(tool_calls)
        // message emitted at flush time, NOT on the intermediate text message.
        // DeepSeek thinking mode rejects the follow-up otherwise with
        // "The reasoning_content in the thinking mode must be passed back."
        let function_call_after: Vec<bool> = {
            let mut v = vec![false; arr.len()];
            let mut seen = false;
            for (i, item) in arr.iter().enumerate().rev() {
                v[i] = seen;
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    seen = true;
                }
            }
            v
        };

        for item in arr {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "function_call" => {
                    let cid = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    if cid.is_empty() {
                        let fallback = format!("call_{}", fallback_counter);
                        fallback_counter += 1;
                        call_id_fallback.insert(cid.clone(), fallback.clone());
                        call_ids.insert(fallback);
                    } else {
                        call_ids.insert(cid);
                    }
                }
                "function_call_output" => {
                    let cid = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Use fallback if one was generated for the corresponding function_call
                    let effective_cid = call_id_fallback.get(&cid).cloned().unwrap_or(cid);
                    output_ids.insert(effective_cid);
                }
                _ => {}
            }
        }

        let mut msgs = Vec::new();
        // Reasoning from a preceding `reasoning` item is attached to the next
        // assistant message as `reasoning_content`. Without this, thinking-mode
        // providers (e.g. DeepSeek) reject multi-turn requests with
        // "The `reasoning_content` in the thinking mode must be passed back."
        let mut pending_reasoning: Option<String> = None;
        // Function_call items are buffered and flushed together as ONE assistant
        // message with a multi-element `tool_calls` array. Emitting a separate
        // assistant message per call breaks parallel tool use: DeepSeek rejects any
        // assistant message carrying tool_calls that isn't immediately followed by
        // tool messages for each of its call_ids.
        let mut pending_tool_calls: Vec<(String, String, String)> = Vec::new();
        // call_ids whose tool response is still awaited (their real output exists
        // later in the input). Regular messages are deferred until this empties so
        // they never sit between an assistant(tool_calls) and its tool messages.
        let mut awaiting: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Regular messages deferred while tool responses are pending.
        let mut deferred: Vec<Value> = Vec::new();

        // Flush buffered function_calls as one assistant message. For each call:
        // if a real output exists later in the input, mark it awaiting; otherwise
        // synthesize an empty tool response so upstream never sees an unanswered
        // tool_call_id.
        let flush_tool_calls =
            |msgs: &mut Vec<Value>,
             pending_tool_calls: &mut Vec<(String, String, String)>,
             awaiting: &mut std::collections::HashSet<String>,
             output_ids: &std::collections::HashSet<String>,
             pending_reasoning: &mut Option<String>| {
                if pending_tool_calls.is_empty() {
                    return;
                }
                // Never flush a new tool batch while an earlier assistant(tool_calls)
                // is still awaiting its tool replies. Doing so would emit a SECOND
                // assistant message between the first one and its tool messages,
                // which DeepSeek rejects ("assistant with tool_calls must be
                // followed by tool messages responding to each tool_call_id"). The
                // new calls stay buffered and flush together once awaiting drains.
                if !awaiting.is_empty() {
                    return;
                }
                let tool_calls: Vec<Value> = pending_tool_calls
                    .iter()
                    .map(|(cid, name, arguments)| {
                        serde_json::json!({
                            "id": cid,
                            "type": "function",
                            "function": {"name": name, "arguments": arguments}
                        })
                    })
                    .collect();
                let mut msg = serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": tool_calls,
                });
                if let Some(rc) = pending_reasoning.take() {
                    msg["reasoning_content"] = Value::String(rc);
                }
                msgs.push(msg);
                for (cid, _, _) in pending_tool_calls.iter() {
                    if output_ids.contains(cid) {
                        awaiting.insert(cid.clone());
                    } else {
                        msgs.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": cid,
                            "content": ""
                        }));
                    }
                }
                pending_tool_calls.clear();
            };

        for (idx, item) in arr.iter().enumerate() {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match item_type {
                // reasoning: thinking chain → attach as reasoning_content on the following assistant message
                "reasoning" => {
                    let mut text = String::new();
                    if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                        for block in summary {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                text.push_str(t);
                            }
                        }
                    }
                    if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                        for block in content {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                text.push_str(t);
                            }
                        }
                    }
                    if text.is_empty() {
                        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                            text = t.to_string();
                        }
                    }
                    if !text.is_empty() {
                        pending_reasoning = Some(text);
                    }
                }

                // function_call: assistant's tool call → buffer for the next merged assistant message
                "function_call" => {
                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let arguments = item.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                    let original_call_id = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Use fallback call_id if the original was empty
                    let call_id = call_id_fallback
                        .get(&original_call_id)
                        .cloned()
                        .unwrap_or(original_call_id);
                    pending_tool_calls.push((call_id, name.to_string(), arguments.to_string()));
                }

                // function_call_output: tool result → OpenAI tool message, then
                // release any deferred messages once every awaited output has landed
                "function_call_output" => {
                    flush_tool_calls(
                        &mut msgs,
                        &mut pending_tool_calls,
                        &mut awaiting,
                        &output_ids,
                        &mut pending_reasoning,
                    );
                    let original_call_id = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Use fallback call_id if one was generated for the corresponding function_call
                    let call_id = call_id_fallback
                        .get(&original_call_id)
                        .cloned()
                        .unwrap_or(original_call_id);
                    let output = item.get("output").and_then(|o| o.as_str()).unwrap_or("");
                    awaiting.remove(&call_id);
                    msgs.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                    // Tool responses are in; regular messages can be emitted again.
                    if awaiting.is_empty() {
                        msgs.append(&mut deferred);
                    }
                }

                // message: standard chat message
                "message" | _ if item.get("role").is_some() => {
                    flush_tool_calls(
                        &mut msgs,
                        &mut pending_tool_calls,
                        &mut awaiting,
                        &output_ids,
                        &mut pending_reasoning,
                    );
                    let role = item
                        .get("role")
                        .and_then(|r| r.as_str())
                        .unwrap_or("user")
                        .to_string();
                    // Map Roles that some providers don't recognize
                    // 'developer' is an OpenAI alias for 'system' (used by Codex/Responses API)
                    let role = match role.as_str() {
                        "developer" => "system".to_string(),
                        other => other.to_string(),
                    };
                    let content = if let Some(content_arr) =
                        item.get("content").and_then(|c| c.as_array())
                    {
                        // Preserve input images instead of silently dropping them. The
                        // downstream codec decides whether the source is representable.
                        let mut blocks = Vec::new();
                        let mut text_only = true;
                        for block in content_arr {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                blocks.push(serde_json::json!({
                                    "type": "text",
                                    "text": text,
                                }));
                            } else if block.get("type").and_then(Value::as_str)
                                == Some("input_image")
                            {
                                text_only = false;
                                let image_url =
                                    block.get("image_url").and_then(Value::as_str).unwrap_or("");
                                blocks.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {"url": image_url},
                                }));
                            }
                        }
                        if text_only {
                            Value::String(
                                blocks
                                    .iter()
                                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                                    .collect::<String>(),
                            )
                        } else {
                            Value::Array(blocks)
                        }
                    } else if let Some(text) = item.get("content").and_then(|c| c.as_str()) {
                        Value::String(text.to_string())
                    } else {
                        Value::String(String::new())
                    };
                    let mut msg = serde_json::json!({
                        "role": role,
                        "content": content,
                    });
                    // Attach reasoning_content (from a preceding `reasoning` item,
                    // or carried directly on this message) for assistant turns so
                    // thinking-mode providers accept the request.
                    if role == "assistant" {
                        let rc = item
                            .get("reasoning_content")
                            .and_then(|r| r.as_str())
                            .map(|s| s.to_string())
                            .or_else(|| {
                                // If this assistant text message is part of a
                                // tool-calling turn (function_calls follow it),
                                // keep the reasoning for the assistant(tool_calls)
                                // message emitted at flush — that is the message
                                // DeepSeek's thinking mode requires it on.
                                if function_call_after[idx] {
                                    None
                                } else {
                                    pending_reasoning.take()
                                }
                            });
                        if let Some(rc) = rc {
                            if !rc.is_empty() {
                                msg["reasoning_content"] = Value::String(rc);
                            }
                        }
                    } else {
                        // A reasoning item belongs to the assistant message that
                        // immediately follows it; drop it before any other role —
                        // unless a buffered tool call still needs to flush with it.
                        if pending_tool_calls.is_empty() && awaiting.is_empty() {
                            pending_reasoning = None;
                        }
                    }
                    // Defer regular messages while tool responses are pending so
                    // they never interrupt an assistant(tool_calls)→tool(...) run.
                    if awaiting.is_empty() {
                        msgs.push(msg);
                    } else {
                        deferred.push(msg);
                    }
                }

                // Simple text item
                _ if item.get("text").is_some() => {
                    flush_tool_calls(
                        &mut msgs,
                        &mut pending_tool_calls,
                        &mut awaiting,
                        &output_ids,
                        &mut pending_reasoning,
                    );
                    let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    let msg = serde_json::json!({
                        "role": "user",
                        "content": text,
                    });
                    if awaiting.is_empty() {
                        msgs.push(msg);
                    } else {
                        deferred.push(msg);
                    }
                }

                // Raw string input
                _ => {
                    if let Some(s) = item.as_str() {
                        flush_tool_calls(
                            &mut msgs,
                            &mut pending_tool_calls,
                            &mut awaiting,
                            &output_ids,
                            &mut pending_reasoning,
                        );
                        let msg = serde_json::json!({
                            "role": "user",
                            "content": s,
                        });
                        if awaiting.is_empty() {
                            msgs.push(msg);
                        } else {
                            deferred.push(msg);
                        }
                    }
                }
            }
        }
        // End of input: flush any buffered tool calls and remaining deferred messages.
        flush_tool_calls(
            &mut msgs,
            &mut pending_tool_calls,
            &mut awaiting,
            &output_ids,
            &mut pending_reasoning,
        );
        if awaiting.is_empty() {
            msgs.append(&mut deferred);
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
