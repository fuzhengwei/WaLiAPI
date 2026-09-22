use crate::protocol::legacy::responses_decode::convert_responses_input_to_messages;
use crate::protocol::legacy::responses_to_openai;

#[test]
fn responses_input_reasoning_item_becomes_assistant_reasoning_content() {
    // A `reasoning` item must be forwarded to the upstream Chat request as
    // `reasoning_content` on the assistant message it precedes. Without this,
    // DeepSeek thinking models reject the 2nd+ turn with
    // "The `reasoning_content` in the thinking mode must be passed back."
    let input = serde_json::json!([
        {
            "type": "reasoning",
            "id": "rs_abc",
            "summary": [{"type": "summary_text", "text": "Let me think."}],
            "content": [{"type": "reasoning_text", "text": "chain of thought"}]
        },
        {
            "type": "message",
            "id": "msg_xyz",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "The answer is 42."}]
        }
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["content"], "The answer is 42.");
    assert_eq!(
        msgs[0]["reasoning_content"],
        "Let me think.chain of thought"
    );
}

#[test]
fn responses_input_reasoning_item_without_content_still_preserved() {
    // DeepSeek-compatible: even a reasoning item with only a summary must be
    // forwarded as reasoning_content (no crash, no drop).
    let input = serde_json::json!([
        {"type": "reasoning", "id": "rs_abc", "summary": [{"type": "summary_text", "text": "Let me think."}]},
        {"type": "message", "id": "msg_xyz", "role": "assistant", "content": [{"type": "output_text", "text": "Hi"}]}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs[0]["reasoning_content"], "Let me think.");
}

#[test]
fn responses_input_message_carries_reasoning_content_directly() {
    // Some clients attach reasoning_content directly to the message item.
    let input = serde_json::json!([
        {"type": "message", "id": "msg_xyz", "role": "assistant",
         "reasoning_content": "direct chain",
         "content": [{"type": "output_text", "text": "Hi"}]}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs[0]["reasoning_content"], "direct chain");
}

#[test]
fn responses_reasoning_round_trip_to_chat() {
    // Simulates the full Codex repro: upstream DeepSeek streams reasoning_content,
    // WaLiAPI emits a `reasoning` item in Responses API format, then the next
    // turn's input (echoing that reasoning item) converts back to Chat with
    // `reasoning_content` so DeepSeek accepts the request.
    use crate::protocol::responses::{
        convert_openai_sse_to_responses, create_synthetic_completed_events, StreamState,
    };

    let response_id = "resp_repro";
    let mut state = StreamState::default();

    // Upstream turn output: reasoning_content then content, then stop.
    let ev1 = convert_openai_sse_to_responses(
        r#"data: {"id":"c1","choices":[{"index":0,"delta":{"reasoning_content":"deepseek thought"},"finish_reason":null}]}"#,
        "deepseek-v4-flash",
        response_id,
        "",
        &mut state,
    );
    let ev2 = convert_openai_sse_to_responses(
        r#"data: {"id":"c1","choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":null}]}"#,
        "deepseek-v4-flash",
        response_id,
        "answer",
        &mut state,
    );
    let ev3 = convert_openai_sse_to_responses(
        r#"data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "deepseek-v4-flash",
        response_id,
        "answer",
        &mut state,
    );
    let ev4 = create_synthetic_completed_events(
        "deepseek-v4-flash",
        response_id,
        "answer",
        &state,
        10,
        5,
    );

    // The stream Codex receives must announce + complete the reasoning item.
    let stream: String = ev1.into_iter().chain(ev2).chain(ev3).chain(ev4).collect();
    assert!(stream.contains("\"type\":\"response.output_item.added\""));
    assert!(stream.contains("\"type\":\"reasoning\""));
    assert!(stream.contains("\"type\":\"reasoning_summary_text\""));
    assert!(stream.contains("deepseek thought"));
    assert!(stream.contains("\"type\":\"response.completed\""));

    // Next turn: Codex echoes reasoning + message items back in input.
    let next_input = serde_json::json!([
        {"type": "reasoning", "id": "rs_repro", "summary": [{"type": "summary_text", "text": "deepseek thought"}]},
        {"type": "message", "id": "msg_repro", "role": "assistant", "content": [{"type": "output_text", "text": "answer"}]},
        {"type": "message", "id": "msg_u2", "role": "user", "content": [{"type": "input_text", "text": "continue"}]}
    ]);
    let messages = convert_responses_input_to_messages(&next_input);
    let msgs = messages.as_array().unwrap();
    let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(assistant["reasoning_content"], "deepseek thought");
    assert_eq!(assistant["content"], "answer");
}

#[test]
fn responses_to_openai_drops_tool_choice_when_no_function_tools() {
    // GitHub issue #13: Codex sends `tool_choice: "auto"` even on plain
    // no-tool requests. Chat Completions rejects `tool_choice` without
    // `tools` ("When using `tool_choice`, `tools` must be set."), so the
    // conversion must strip it whenever no convertible function tools exist.
    let body = serde_json::json!({
        "model": "gpt-4",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "tool_choice": "auto",
        // Only non-function tools (e.g. web_search) — must NOT keep tool_choice.
        "tools": [{"type": "web_search"}]
    });
    let converted = responses_to_openai(&body).unwrap();
    assert!(
        converted.get("tool_choice").is_none(),
        "tool_choice must be dropped when no function tools convert"
    );
    assert!(
        converted.get("tools").is_none(),
        "non-function tools must not be forwarded"
    );
}

#[test]
fn responses_to_openai_keeps_tool_choice_only_with_function_tools() {
    // When the request does carry convertible function tools, tool_choice
    // passes through; the assistant tool-call message must use "" instead of
    // null content (some strict OpenAI-compatible services reject content:null).
    let body = serde_json::json!({
        "model": "gpt-4",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "list files"}]},
            {"type": "function_call", "call_id": "call_A", "name": "list", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call_A", "output": "a.txt"}
        ],
        "tools": [{"type": "function", "name": "list", "parameters": {"type": "object", "properties": {}}}],
        "tool_choice": {"type": "function", "name": "list"}
    });
    let converted = responses_to_openai(&body).unwrap();
    assert!(
        converted.get("tool_choice").is_some(),
        "tool_choice must be kept when function tools are present"
    );
    assert_eq!(converted["tool_choice"]["type"], "function");
    assert_eq!(converted["tool_choice"]["function"]["name"], "list");
    assert_eq!(converted["tools"].as_array().unwrap().len(), 1);

    let msgs = converted["messages"].as_array().unwrap();
    let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(assistant["tool_calls"][0]["id"], "call_A");
    assert_eq!(
        assistant["content"], "",
        "assistant tool-call message must use empty string, not null"
    );
}

#[test]
fn responses_to_openai_flattens_object_tool_choice_modes() {
    // Responses object forms {"type": "auto"} / {"type": "none"} must flatten
    // to the bare strings Chat Completions expects.
    for (input, expected) in [("auto", "auto"), ("none", "none"), ("required", "required")] {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "tool_choice": {"type": input},
            "tools": [{"type": "function", "name": "list", "parameters": {"type": "object", "properties": {}}}]
        });
        let converted = responses_to_openai(&body).unwrap();
        assert_eq!(
            converted["tool_choice"], expected,
            "object {{type:{input}}} must flatten to string"
        );
    }
}

#[test]
fn responses_to_drops_object_tool_choice_without_name() {
    // {"type":"function"} without a name has no Chat equivalent — drop it.
    let body = serde_json::json!({
        "model": "gpt-4",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "tool_choice": {"type": "function"},
        "tools": [{"type": "function", "name": "list", "parameters": {"type": "object", "properties": {}}}]
    });
    assert!(
        responses_to_openai(&body).is_ok(),
        "legacy malformed tool choice remains safely omitted"
    );
}

#[test]
fn responses_to_openai_tolerates_codex_controls_and_maps_reasoning_effort() {
    // codex 0.147.0 always sends these top-level controls alongside the
    // standard Responses fields. They have no Chat representation and must
    // be tolerated (dropped) rather than rejected, and `reasoning.effort`
    // must map to top-level `reasoning_effort` for the Chat→Messages leg.
    let body = serde_json::json!({
        "model": "gpt-4",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "parallel_tool_calls": false,
        "store": true,
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": "key",
        "prompt_cache_options": {"prompt_cache_key": "key"},
        "client_metadata": {"turn": "1"},
        "reasoning": {"effort": "high"}
    });
    let converted = responses_to_openai(&body).unwrap();
    // The controls are dropped from the Chat body...
    for key in [
        "parallel_tool_calls",
        "store",
        "include",
        "prompt_cache_key",
        "prompt_cache_options",
        "client_metadata",
    ] {
        assert!(
            converted.get(key).is_none(),
            "{key} must not leak into the Chat body"
        );
    }
    // ...and reasoning.effort is mapped to reasoning_effort.
    assert_eq!(converted["reasoning_effort"], "high");
    // Legacy max_tokens default is unchanged (4096) on this path.
    assert_eq!(converted["max_tokens"], 4096);
}

#[test]
fn responses_input_reasoning_stays_on_tool_calls_message() {
    // Real Codex echo order (captured from the local gateway log):
    // reasoning → assistant text → function_call → function_call_output.
    // DeepSeek thinking mode requires `reasoning_content` on the
    // assistant(tool_calls) message; consuming it on the intermediate
    // text message makes the follow-up fail with
    // "The reasoning_content in the thinking mode must be passed back
    // to the API."
    let input = serde_json::json!([
        {"type": "reasoning", "id": "rs_a", "summary": [{"type": "summary_text", "text": "Let me check the file."}]},
        {"type": "message", "id": "msg_a", "role": "assistant", "content": [{"type": "output_text", "text": "Let me look at that."}]},
        {"type": "function_call", "id": "fc_1", "call_id": "call_A", "name": "read", "arguments": "{\"path\":\"/tmp/x\"}"},
        {"type": "function_call_output", "call_id": "call_A", "output": "file contents"},
        {"type": "message", "id": "msg_u", "role": "user", "content": [{"type": "input_text", "text": "continue"}]}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();

    // The tool-calling assistant message MUST carry the reasoning.
    let tool_call_msg = msgs
        .iter()
        .find(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
        .unwrap();
    assert_eq!(tool_call_msg["reasoning_content"], "Let me check the file.");
    assert_eq!(tool_call_msg["tool_calls"][0]["id"], "call_A");
    assert_eq!(msgs[0]["role"], "assistant");

    // The intermediate assistant text message must NOT have consumed it.
    let text_msg = msgs
        .iter()
        .find(|m| m["role"] == "assistant" && m.get("tool_calls").is_none())
        .unwrap();
    assert_eq!(text_msg["content"], "Let me look at that.");
    assert!(
        text_msg.get("reasoning_content").is_none()
            || text_msg["reasoning_content"]
                .as_str()
                .unwrap_or("")
                .is_empty(),
        "reasoning must not be consumed by the intermediate text message"
    );

    // Tool output still follows the tool_calls assistant message.
    assert_eq!(
        msgs[2],
        serde_json::json!({"role": "tool", "tool_call_id": "call_A", "content": "file contents"})
    );
}

#[test]
fn responses_input_reasoning_with_user_interleaved_before_output() {
    // Codex can interleave a user text message between function_call and
    // function_call_output. The reasoning must survive until the
    // assistant(tool_calls) flush even across that user message.
    let input = serde_json::json!([
        {"type": "reasoning", "id": "rs_a", "summary": [{"type": "summary_text", "text": "thinking"}]},
        {"type": "function_call", "id": "fc_1", "call_id": "call_A", "name": "shell", "arguments": "{}"},
        {"type": "message", "id": "msg_ok", "role": "user", "content": [{"type": "input_text", "text": "Approved command prefix saved"}]},
        {"type": "function_call_output", "call_id": "call_A", "output": "done"},
        {"type": "message", "id": "msg_next", "role": "user", "content": [{"type": "input_text", "text": "next"}]}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    let tool_call_msg = msgs
        .iter()
        .find(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
        .unwrap();
    assert_eq!(tool_call_msg["reasoning_content"], "thinking");
    assert_eq!(msgs[1]["role"], "tool");
}

#[test]
fn responses_input_parallel_function_calls_merge_into_one_assistant() {
    // Parallel function_calls must be merged into ONE assistant message with a
    // multi-element tool_calls array, immediately followed by their tool
    // messages. Splitting them into per-call assistant messages makes DeepSeek
    // reject the request ("assistant with tool_calls must be followed by tool
    // messages responding to each tool_call_id").
    let input = serde_json::json!([
        {"type": "function_call", "call_id": "call_A", "name": "read", "arguments": "{}"},
        {"type": "function_call", "call_id": "call_B", "name": "grep", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "call_A", "output": "file contents"},
        {"type": "function_call_output", "call_id": "call_B", "output": "matched lines"}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(msgs[0]["tool_calls"][0]["id"], "call_A");
    assert_eq!(msgs[0]["tool_calls"][1]["id"], "call_B");
    assert_eq!(
        msgs[1],
        serde_json::json!({"role": "tool", "tool_call_id": "call_A", "content": "file contents"})
    );
    assert_eq!(
        msgs[2],
        serde_json::json!({"role": "tool", "tool_call_id": "call_B", "content": "matched lines"})
    );
}

#[test]
fn responses_input_defers_text_message_until_tool_output() {
    // Codex interleaves a user text message ("Approved command prefix saved")
    // between function_call and function_call_output. That text must be
    // deferred until the tool output lands, so the assistant(tool_calls) is
    // immediately followed by its tool message.
    let input = serde_json::json!([
        {"type": "function_call", "call_id": "call_A", "name": "shell", "arguments": "{}"},
        {"type": "message", "id": "msg_ok", "role": "user", "content": [{"type": "input_text", "text": "Approved command prefix saved"}]},
        {"type": "function_call_output", "call_id": "call_A", "output": "done"},
        {"type": "message", "id": "msg_next", "role": "user", "content": [{"type": "input_text", "text": "next"}]}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["tool_calls"][0]["id"], "call_A");
    assert_eq!(
        msgs[1],
        serde_json::json!({"role": "tool", "tool_call_id": "call_A", "content": "done"})
    );
    assert_eq!(
        msgs[2],
        serde_json::json!({"role": "user", "content": "Approved command prefix saved"})
    );
    assert_eq!(
        msgs[3],
        serde_json::json!({"role": "user", "content": "next"})
    );
}

#[test]
fn responses_input_orphan_function_call_gets_empty_tool_response() {
    // A function_call with no matching output must still get a synthesized
    // empty tool message, otherwise the assistant(tool_calls) has no response.
    let input = serde_json::json!([
        {"type": "function_call", "call_id": "call_A", "name": "shell", "arguments": "{}"},
        {"type": "message", "id": "msg_next", "role": "user", "content": [{"type": "input_text", "text": "next"}]}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["tool_calls"][0]["id"], "call_A");
    assert_eq!(
        msgs[1],
        serde_json::json!({"role": "tool", "tool_call_id": "call_A", "content": ""})
    );
    assert_eq!(
        msgs[2],
        serde_json::json!({"role": "user", "content": "next"})
    );
}

#[test]
fn responses_input_reasoning_attaches_to_merged_tool_calls_message() {
    // When reasoning directly precedes parallel function_calls, the reasoning
    // content is attached to the merged assistant tool_calls message.
    let input = serde_json::json!([
        {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "think"}]},
        {"type": "function_call", "call_id": "call_A", "name": "read", "arguments": "{}"},
        {"type": "function_call", "call_id": "call_B", "name": "grep", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "call_A", "output": "a"},
        {"type": "function_call_output", "call_id": "call_B", "output": "b"}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["reasoning_content"], "think");
    assert_eq!(msgs[0]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(msgs[1]["role"], "tool");
    assert_eq!(msgs[2]["role"], "tool");
}

#[test]
fn responses_input_interleaved_call_never_leaves_orphan_assistant() {
    // A second function_call that arrives AFTER a user confirmation message
    // (still awaiting tool output for a prior call) must NOT be flushed as a
    // separate assistant message while the first one is still awaiting its
    // tool reply. Flushing it mid-await would emit
    // `assistant(tool_calls=[A]), assistant(tool_calls=[B]), tool_A, ...`
    // — the first assistant left without its tool message, which DeepSeek
    // rejects exactly like the original 502. Each assistant(tool_calls)
    // must be immediately followed by its own tool messages.
    let input = serde_json::json!([
        {"type": "function_call", "call_id": "call_A", "name": "shell", "arguments": "{}"},
        {"type": "message", "id": "msg_ok", "role": "user", "content": [{"type": "input_text", "text": "Approved command prefix saved"}]},
        {"type": "function_call", "call_id": "call_B", "name": "read", "arguments": "{}"},
        {"type": "function_call_output", "call_id": "call_A", "output": "done"},
        {"type": "function_call_output", "call_id": "call_B", "output": "file contents"}
    ]);
    let messages = convert_responses_input_to_messages(&input);
    let msgs = messages.as_array().unwrap();
    assert_eq!(msgs.len(), 5);
    // assistant(A) is immediately followed by tool_A — never by assistant(B).
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["tool_calls"][0]["id"], "call_A");
    assert_eq!(
        msgs[1],
        serde_json::json!({"role": "tool", "tool_call_id": "call_A", "content": "done"})
    );
    assert_eq!(
        msgs[2],
        serde_json::json!({"role": "user", "content": "Approved command prefix saved"})
    );
    // assistant(B) starts a fresh, valid turn: tool_B follows it directly.
    assert_eq!(msgs[3]["role"], "assistant");
    assert_eq!(msgs[3]["tool_calls"][0]["id"], "call_B");
    assert_eq!(
        msgs[4],
        serde_json::json!({"role": "tool", "tool_call_id": "call_B", "content": "file contents"})
    );
}

#[test]
fn responses_to_openai_accepts_text_control() {
    // Codex CLI 的部分请求会带 `text`（输出格式与详细度）。Chat 侧只有
    // `response_format` 能表达 `format`；`verbosity` 没有对应物，必须丢弃
    // 而不是让整个请求 400。
    let with_schema = serde_json::json!({
        "model": "gpt-4",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "text": {
            "format": {"type": "json_schema", "name": "out", "schema": {"type": "object"}, "strict": true},
            "verbosity": "medium"
        }
    });
    let converted = responses_to_openai(&with_schema).unwrap();
    assert_eq!(converted["response_format"]["type"], "json_schema");
    assert_eq!(converted["response_format"]["json_schema"]["name"], "out");
    assert_eq!(converted["response_format"]["json_schema"]["strict"], true);

    let verbosity_only = serde_json::json!({
        "model": "gpt-4",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "text": {"verbosity": "low"}
    });
    let converted = responses_to_openai(&verbosity_only).unwrap();
    assert!(converted.get("response_format").is_none());
}
