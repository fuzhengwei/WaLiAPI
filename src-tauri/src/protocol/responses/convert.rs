use serde_json::Value;

use super::state::{next_seq, StreamState, ToolCallState};

/// Convert an OpenAI SSE chunk (Chat Completions stream) to Responses API SSE events.
///
/// This function is called repeatedly for each upstream SSE chunk and must be stateful.
/// The `state` parameter tracks all output items (text + tool calls) across calls.
///
/// # Event chains emitted
///
/// ## Text content
/// ```text
/// response.output_item.added (type=message)
/// response.content_part.added (type=output_text)
/// response.output_text.delta (per chunk)
/// response.output_text.done (at finish)
/// response.content_part.done
/// response.output_item.done
/// ```
///
/// ## Function call (tool_calls)
/// ```text
/// response.output_item.added (type=function_call)
/// response.function_call_arguments.delta (per chunk)
/// response.function_call_arguments.done
/// response.output_item.done
/// ```
///
/// ## Final events (emitted by `create_synthetic_completed_events`)
/// ```text
/// response.completed
/// data: [DONE]
/// ```
pub fn convert_openai_sse_to_responses(
    chunk_text: &str,
    _model: &str,
    response_id: &str,
    accumulated_content: &str,
    state: &mut StreamState,
) -> Vec<String> {
    let mut events = Vec::new();
    let msg_id = format!(
        "msg_{}",
        response_id.strip_prefix("resp_").unwrap_or(response_id)
    );
    let reasoning_id = format!(
        "rs_{}",
        response_id.strip_prefix("resp_").unwrap_or(response_id)
    );

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
                    // Reasoning content delta (DeepSeek R1, OpenAI o1/o3, etc.)
                    if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str())
                    {
                        if !reasoning.is_empty() {
                            // Announce the reasoning item (output_item.added) and its
                            // summary part BEFORE the first delta. Clients only persist
                            // items they saw "added" — without this the reasoning never
                            // enters the conversation and thinking-mode providers reject
                            // the next turn.
                            if !state.reasoning_item_added {
                                let reasoning_output_index = state.next_output_index;
                                state.reasoning_output_index = reasoning_output_index;
                                let seq = next_seq(state);
                                let item = serde_json::json!({
                                    "id": reasoning_id,
                                    "type": "reasoning",
                                    "status": "in_progress",
                                    "summary": [],
                                    "content": []
                                });
                                let item_event = serde_json::json!({
                                    "type": "response.output_item.added",
                                    "output_index": reasoning_output_index,
                                    "item": item,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.output_item.added\ndata: {}\n\n",
                                    item_event
                                ));

                                let seq = next_seq(state);
                                let part = serde_json::json!({
                                    "type": "reasoning_summary_text",
                                    "text": ""
                                });
                                let part_event = serde_json::json!({
                                    "type": "response.reasoning_summary_part.added",
                                    "item_id": reasoning_id,
                                    "output_index": reasoning_output_index,
                                    "summary_index": 0,
                                    "part": part,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.reasoning_summary_part.added\ndata: {}\n\n",
                                    part_event
                                ));

                                state.reasoning_item_added = true;
                                state.reasoning_part_added = true;
                                state.next_output_index += 1;
                            }

                            state.accumulated_reasoning.push_str(reasoning);
                            let reasoning_output_index = state.reasoning_output_index;
                            let seq = next_seq(state);
                            let event = serde_json::json!({
                                "type": "response.reasoning_summary_text.delta",
                                "item_id": reasoning_id,
                                "output_index": reasoning_output_index,
                                "summary_index": 0,
                                "delta": reasoning,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.reasoning_summary_text.delta\ndata: {}\n\n",
                                event
                            ));
                        }
                    }

                    // Content delta (text)
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            // Emit output_item.added + content_part.added before first text delta
                            if !state.text_item_added {
                                let text_output_index = state.next_output_index;
                                state.text_output_index = text_output_index;
                                let seq = next_seq(state);
                                let item = serde_json::json!({
                                    "id": msg_id,
                                    "type": "message",
                                    "status": "in_progress",
                                    "role": "assistant",
                                    "content": []
                                });
                                let item_event = serde_json::json!({
                                    "type": "response.output_item.added",
                                    "output_index": text_output_index,
                                    "item": item,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.output_item.added\ndata: {}\n\n",
                                    item_event
                                ));

                                let seq = next_seq(state);
                                let part = serde_json::json!({
                                    "type": "output_text",
                                    "text": "",
                                    "annotations": []
                                });
                                let part_event = serde_json::json!({
                                    "type": "response.content_part.added",
                                    "item_id": msg_id,
                                    "output_index": text_output_index,
                                    "content_index": 0,
                                    "part": part,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.content_part.added\ndata: {}\n\n",
                                    part_event
                                ));

                                state.text_item_added = true;
                                state.text_part_added = true;
                                state.next_output_index += 1;
                            }

                            let text_output_index = state.text_output_index;
                            let seq = next_seq(state);

                            let event = serde_json::json!({
                                "type": "response.output_text.delta",
                                "item_id": msg_id,
                                "output_index": text_output_index,
                                "content_index": 0,
                                "delta": content,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.output_text.delta\ndata: {}\n\n",
                                event
                            ));
                        }
                    }

                    // Tool calls delta
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                        state.has_tool_calls = true;

                        for tc in tool_calls {
                            let tc_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                            let tc_id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
                            let func = tc.get("function");
                            let name = func
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            let arguments = func
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                                .unwrap_or("");

                            // Initialize tool call state if this is the first time we see it
                            if !state.tool_calls.contains_key(&tc_index) {
                                let output_index = state.next_output_index;
                                // 条目 id 必须带 `fc_` 前缀；上游 Chat 的 tool_call id
                                // 是关联 id，只能进 `call_id`（写进 `id` 会让官方上游
                                // 在回放旧会话时报 "Expected an ID that begins with 'fc'"）。
                                let item_id = format!("fc_{}", uuid::Uuid::new_v4().simple());

                                state.tool_calls.insert(
                                    tc_index,
                                    ToolCallState {
                                        output_index,
                                        call_id: tc_id.to_string(),
                                        name: name.to_string(),
                                        item_id: item_id.clone(),
                                        accumulated_arguments: String::new(),
                                        item_added_sent: false,
                                        arguments_done_sent: false,
                                        output_item_done_sent: false,
                                    },
                                );
                                state.next_output_index += 1;
                            }

                            let tc_state = state.tool_calls.get_mut(&tc_index).unwrap();

                            // Always update call_id and name if they were empty and we now have values
                            // (upstream may send id in a later chunk than the first one)
                            if tc_state.call_id.is_empty() && !tc_id.is_empty() {
                                tc_state.call_id = tc_id.to_string();
                            }
                            if tc_state.name.is_empty() && !name.is_empty() {
                                tc_state.name = name.to_string();
                            }

                            // Emit output_item.added for function_call if not yet sent
                            if !tc_state.item_added_sent {
                                // If we have a call_id and name, emit the added event
                                let effective_name = if tc_state.name.is_empty() {
                                    name.to_string()
                                } else {
                                    tc_state.name.clone()
                                };
                                let effective_call_id = if tc_state.call_id.is_empty() {
                                    tc_id.to_string()
                                } else {
                                    tc_state.call_id.clone()
                                };

                                // Update stored values if they were empty before
                                if tc_state.call_id.is_empty() && !effective_call_id.is_empty() {
                                    tc_state.call_id = effective_call_id.clone();
                                }
                                if tc_state.name.is_empty() && !effective_name.is_empty() {
                                    tc_state.name = effective_name.clone();
                                }

                                let fc_item = serde_json::json!({
                                    "id": tc_state.item_id,
                                    "type": "function_call",
                                    "status": "in_progress",
                                    "call_id": tc_state.call_id,
                                    "name": tc_state.name,
                                    "arguments": ""
                                });
                                // Increment seq before borrowing tc_state
                                state.sequence_number += 1;
                                let seq = state.sequence_number;
                                let added_event = serde_json::json!({
                                    "type": "response.output_item.added",
                                    "output_index": tc_state.output_index,
                                    "item": fc_item,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.output_item.added\ndata: {}\n\n",
                                    added_event
                                ));
                                tc_state.item_added_sent = true;
                            }

                            // Emit arguments delta if we have arguments content — but never
                            // after the .done has already been sent (some upstreams re-send
                            // or deliver a trailing chunk after finish_reason).
                            if !arguments.is_empty() && !tc_state.arguments_done_sent {
                                tc_state.accumulated_arguments.push_str(arguments);

                                state.sequence_number += 1;
                                let seq = state.sequence_number;
                                let delta_event = serde_json::json!({
                                    "type": "response.function_call_arguments.delta",
                                    "item_id": tc_state.item_id,
                                    "output_index": tc_state.output_index,
                                    "delta": arguments,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.function_call_arguments.delta\ndata: {}\n\n",
                                    delta_event
                                ));
                            }
                        }
                    }
                }

                // Check for finish_reason
                if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    if !finish.is_empty() && finish != "null" {
                        // Close reasoning item if it was opened and not yet closed
                        if state.reasoning_item_added && !state.reasoning_item_done {
                            let reasoning_output_index = state.reasoning_output_index;
                            let seq = next_seq(state);
                            let text_done = serde_json::json!({
                                "type": "response.reasoning_summary_text.done",
                                "item_id": reasoning_id,
                                "output_index": reasoning_output_index,
                                "summary_index": 0,
                                "text": state.accumulated_reasoning,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.reasoning_summary_text.done\ndata: {}\n\n",
                                text_done
                            ));

                            let seq = next_seq(state);
                            let part = serde_json::json!({
                                "type": "reasoning_summary_text",
                                "text": state.accumulated_reasoning
                            });
                            let part_done = serde_json::json!({
                                "type": "response.reasoning_summary_part.done",
                                "item_id": reasoning_id,
                                "output_index": reasoning_output_index,
                                "summary_index": 0,
                                "part": part,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.reasoning_summary_part.done\ndata: {}\n\n",
                                part_done
                            ));

                            let seq = next_seq(state);
                            let completed_item = serde_json::json!({
                                "id": reasoning_id,
                                "type": "reasoning",
                                "status": "completed",
                                "summary": [{
                                    "type": "summary_text",
                                    "text": state.accumulated_reasoning
                                }],
                                "content": []
                            });
                            let item_done = serde_json::json!({
                                "type": "response.output_item.done",
                                "output_index": reasoning_output_index,
                                "item": completed_item,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.output_item.done\ndata: {}\n\n",
                                item_done
                            ));

                            state.reasoning_part_done = true;
                            state.reasoning_item_done = true;
                        }

                        // Close text item if it was opened and not yet closed
                        if state.text_item_added && !state.text_item_done {
                            let text_output_index = state.text_output_index;
                            let seq = next_seq(state);
                            let text_done = serde_json::json!({
                                "type": "response.output_text.done",
                                "item_id": msg_id,
                                "output_index": text_output_index,
                                "content_index": 0,
                                "text": accumulated_content,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.output_text.done\ndata: {}\n\n",
                                text_done
                            ));

                            let seq = next_seq(state);
                            let part = serde_json::json!({
                                "type": "output_text",
                                "text": accumulated_content,
                                "annotations": []
                            });
                            let part_done = serde_json::json!({
                                "type": "response.content_part.done",
                                "item_id": msg_id,
                                "output_index": text_output_index,
                                "content_index": 0,
                                "part": part,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.content_part.done\ndata: {}\n\n",
                                part_done
                            ));

                            let seq = next_seq(state);
                            let completed_item = serde_json::json!({
                                "id": msg_id,
                                "type": "message",
                                "status": "completed",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": accumulated_content,
                                    "annotations": []
                                }]
                            });
                            let item_done = serde_json::json!({
                                "type": "response.output_item.done",
                                "output_index": text_output_index,
                                "item": completed_item,
                                "sequence_number": seq
                            });
                            events.push(format!(
                                "event: response.output_item.done\ndata: {}\n\n",
                                item_done
                            ));

                            state.text_item_done = true;
                        }

                        // Ensure all tool calls have non-empty call_id before closing them.
                        // Some upstreams never send a tool_call id in streaming chunks.
                        for (_, tc_state) in state.tool_calls.iter_mut() {
                            if tc_state.call_id.is_empty() {
                                tc_state.call_id = format!("call_{}", tc_state.output_index);
                            }
                        }

                        // Close all tool call items
                        // Collect tool call data first to avoid double mutable borrow of state
                        let tool_calls_data: Vec<(
                            u64,
                            String,
                            String,
                            String,
                            String,
                            bool,
                            bool,
                            bool,
                        )> = state
                            .tool_calls
                            .iter()
                            .map(|(_, tc)| {
                                (
                                    tc.output_index as u64,
                                    tc.item_id.clone(),
                                    tc.call_id.clone(),
                                    tc.name.clone(),
                                    tc.accumulated_arguments.clone(),
                                    tc.item_added_sent,
                                    tc.arguments_done_sent,
                                    tc.output_item_done_sent,
                                )
                            })
                            .collect();

                        for (
                            output_index,
                            item_id,
                            call_id,
                            name,
                            accumulated_args,
                            _item_added,
                            arguments_done,
                            output_item_done,
                        ) in &tool_calls_data
                        {
                            if !arguments_done {
                                let seq = next_seq(state);
                                let args_done = serde_json::json!({
                                    "type": "response.function_call_arguments.done",
                                    "item_id": item_id,
                                    "output_index": output_index,
                                    "name": name,
                                    "arguments": accumulated_args,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.function_call_arguments.done\ndata: {}\n\n",
                                    args_done
                                ));
                            }

                            if !output_item_done {
                                let seq = next_seq(state);
                                let fc_completed = serde_json::json!({
                                    "id": item_id,
                                    "type": "function_call",
                                    "status": "completed",
                                    "call_id": call_id,
                                    "name": name,
                                    "arguments": accumulated_args
                                });
                                let item_done = serde_json::json!({
                                    "type": "response.output_item.done",
                                    "output_index": output_index,
                                    "item": fc_completed,
                                    "sequence_number": seq
                                });
                                events.push(format!(
                                    "event: response.output_item.done\ndata: {}\n\n",
                                    item_done
                                ));
                            }
                        }

                        // Mark tool calls as done
                        for (_, tc_state) in state.tool_calls.iter_mut() {
                            tc_state.arguments_done_sent = true;
                            tc_state.output_item_done_sent = true;
                        }

                        // Note: response.completed is NOT sent here. It's sent after the stream ends,
                        // so we can include usage from the final usage chunk (which comes after finish_reason).
                    }
                }
            }
        }
    }

    events
}
