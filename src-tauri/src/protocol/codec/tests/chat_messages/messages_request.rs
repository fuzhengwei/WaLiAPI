use crate::protocol::codec::registry::CodecRegistry;
use serde_json::json;

use super::support::reject_features;

// ===========================================================================
// messages_to_chat_v1 — request encoding
// ===========================================================================

#[test]
fn messages_request_system_text_and_sampling() {
    let body = json!({
        "model": "m",
        "max_tokens": 64,
        "temperature": 0.5,
        "system": [{"type": "text", "text": "sys"}],
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
    });
    let prepared = CodecRegistry::messages_to_chat("up", &body).unwrap();
    let out = &prepared.encoded_request;
    assert_eq!(out["model"], "up");
    assert_eq!(out["max_tokens"], 64);
    assert_eq!(out["temperature"], 0.5);
    assert_eq!(
        out["messages"][0],
        json!({"role": "system", "content": "sys"})
    );
    assert_eq!(out["messages"][1]["content"], "hi");
}

#[test]
fn messages_request_preserves_empty_thinking_as_reasoning_content() {
    let body = json!({
        "model": "m",
        "thinking": {"type": "enabled", "budget_tokens": 1024},
        "messages": [{
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": ""},
                {"type": "tool_use", "id": "call_1", "name": "lookup", "input": {}}
            ]
        }]
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let assistant = &prepared.encoded_request["messages"][0];
    assert_eq!(assistant["reasoning_content"], "");
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
}

#[test]
fn messages_request_stream_options_are_allowed_and_force_usage() {
    let body = json!({
        "model": "m",
        "stream": true,
        "stream_options": {"include_usage": false, "custom": true},
        "messages": [{"role": "user", "content": "hi"}]
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let out = &prepared.encoded_request;
    assert_eq!(out["stream_options"]["include_usage"], true);
    assert_eq!(out["stream_options"]["custom"], true);
}

#[test]
fn messages_request_tools_choice_and_tool_results() {
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "assistant", "content": [{"type": "text", "text": "checking"}, {"type": "tool_use", "id": "call_1", "name": "weather", "input": {"city": "Paris"}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "sunny"}, {"type": "text", "text": "thanks"}]}
        ],
        "tools": [{"name": "weather", "description": "weather", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "any", "disable_parallel_tool_use": true}
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let out = &prepared.encoded_request;
    assert_eq!(out["parallel_tool_calls"], false);
    assert_eq!(out["tool_choice"], "required");
    assert_eq!(out["tools"][0]["function"]["name"], "weather");
    // No system message here, so messages[0] is the assistant with tool_calls.
    assert_eq!(out["messages"][0]["content"], "checking");
    assert_eq!(
        out["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    assert_eq!(out["messages"][1]["role"], "tool");
    assert_eq!(out["messages"][2]["content"], "thanks");
}

#[test]
fn messages_request_thinking_fail_open_and_builtin_tools_rejected() {
    // Fail-open: thinking is mapped to reasoning_effort, never rejected.
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "thinking": {"type": "enabled", "budget_tokens": 1024}
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let out = &prepared.encoded_request;
    // budget 1024 -> low (CPA ConvertBudgetToLevel).
    assert_eq!(out["reasoning_effort"], "low");

    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "tools": [{"type": "web_search", "name": "web"}]
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("builtin_tool")));
}

#[test]
fn messages_request_thinking_variants_map_reasoning_effort() {
    // CPA ConvertClaudeRequestToOpenAI semantics, exercised directly.
    // enabled + budget_tokens -> level
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "thinking": {"type": "enabled", "budget_tokens": 1024}
    });
    let out = &CodecRegistry::messages_to_chat("m", &body)
        .unwrap()
        .encoded_request;
    assert_eq!(out["reasoning_effort"], "low", "1024 -> low");

    // enabled without budget -> auto
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "thinking": {"type": "enabled"}
    });
    let out = &CodecRegistry::messages_to_chat("m", &body)
        .unwrap()
        .encoded_request;
    assert_eq!(out["reasoning_effort"], "auto");

    // adaptive + output_config.effort -> passthrough (lowercased)
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "MEDIUM"}
    });
    let out = &CodecRegistry::messages_to_chat("m", &body)
        .unwrap()
        .encoded_request;
    assert_eq!(
        out["reasoning_effort"], "medium",
        "effort lowercased passthrough"
    );

    // adaptive without effort -> xhigh
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "thinking": {"type": "adaptive"}
    });
    let out = &CodecRegistry::messages_to_chat("m", &body)
        .unwrap()
        .encoded_request;
    assert_eq!(out["reasoning_effort"], "xhigh");

    // disabled -> none
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "thinking": {"type": "disabled"}
    });
    let out = &CodecRegistry::messages_to_chat("m", &body)
        .unwrap()
        .encoded_request;
    assert_eq!(out["reasoning_effort"], "none");
}

#[test]
fn messages_request_safeguards_dropped_fail_open() {
    // Claude Code 2.1.280 在特定条件下会发送顶层 `safeguards`
    // （`[{type, classifier_context}]`）。Chat / Responses / Gemini 都没有对应物，
    // 整段拒绝会让会话直接无法继续，因此按 fail-open 丢弃并记录。
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "safeguards": [{"type": "classifier", "classifier_context": "ctx"}]
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    assert!(prepared.encoded_request.get("safeguards").is_none());
    assert!(prepared
        .report
        .normalized
        .iter()
        .any(|pointer| pointer.contains("safeguards")));
}

#[test]
fn messages_request_container_dropped_fail_open() {
    // container / context_management have no Chat equivalent; dropped and
    // recorded on the report, never rejected.
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "container": {"type": "super_container"},
        "context_management": {"turns": 4},
        "context_management_config": {"mode": "auto"}
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let out = &prepared.encoded_request;
    assert!(out.get("container").is_none());
    assert!(out.get("context_management").is_none());
    assert!(out.get("context_management_config").is_none());
    // The report surfaces the drop pointers.
    let report = &prepared.report;
    assert!(report.normalized.iter().any(|p| p.contains("container")));
    assert!(report
        .normalized
        .iter()
        .any(|p| p.contains("context_management")));
}

#[test]
fn messages_request_assistant_thinking_becomes_reasoning_content() {
    // An assistant message carrying a thinking block keeps its reasoning as
    // `reasoning_content` on the Chat message; redacted_thinking is dropped.
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "user", "content": "q"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "chain"},
                {"type": "redacted_thinking", "data": "sig"},
                {"type": "text", "text": "answer"}
            ]}
        ]
    });
    let out = &CodecRegistry::messages_to_chat("m", &body)
        .unwrap()
        .encoded_request;
    let assistant = &out["messages"][1];
    assert_eq!(assistant["reasoning_content"], "chain");
    assert_eq!(assistant["content"], "answer");
}

#[test]
fn messages_request_unknown_role_and_block_rejected() {
    let body = json!({
        "model": "m",
        "messages": [{"role": "bogus", "content": "x"}]
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("unknown_role")));

    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": [{"type": "document", "source": {}}]}]
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("unknown_block")));
}

#[test]
fn messages_request_mid_conversation_system_maps_to_chat_system_role() {
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "user", "content": "activate strict mode"},
            {"role": "system", "content": [{"type": "text", "text": "strict mode active", "cache_control": {"type": "ephemeral"}}]},
            {"role": "assistant", "content": "ack"}
        ]
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let messages = prepared.encoded_request["messages"].as_array().unwrap();
    assert_eq!(
        messages[0],
        json!({"role": "user", "content": "activate strict mode"})
    );
    assert_eq!(
        messages[1],
        json!({"role": "system", "content": "strict mode active"})
    );
    assert_eq!(messages[2], json!({"role": "assistant", "content": "ack"}));
}

#[test]
fn messages_request_strips_lossless_cache_controls() {
    let body = json!({
        "model": "m",
        "system": [{"type": "text", "text": "cached", "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": [{"type": "text", "text": "x", "cache_control": {"type": "ephemeral"}}]}]
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let out = &prepared.encoded_request;
    assert_eq!(out["messages"][0]["content"], "cached");
    assert!(out["messages"][0].get("cache_control").is_none());
}

#[test]
fn messages_request_rejects_invalid_tool_input() {
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "assistant", "content": [{"type": "tool_use", "id": "c", "name": "run", "input": [1, 2]}]}
        ]
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("invalid_tool_arguments")));
}

#[test]
fn messages_request_rejects_unknown_top_level_fields() {
    // R4: unknown top-level Messages fields are rejected with a JSON pointer,
    // never silently dropped.  A whitelist mirrors chat_to_messages_v1.
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "unknown": true
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("unsupported_feature.field")));
    assert!(e.json_pointers.iter().any(|p| p == "/unknown"));
}

#[test]
fn messages_request_drops_metadata_annotation() {
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "metadata": {"user_id": "u1"}
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    assert!(prepared.encoded_request.get("metadata").is_none());
    assert!(prepared
        .report
        .normalized
        .contains(&"/metadata".to_string()));
}

#[test]
fn messages_request_rejects_non_array_stop_sequences() {
    // R12: a non-array stop_sequences must be rejected, not silently dropped.
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "stop_sequences": "END"
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("unsupported_feature.field")));
}

#[test]
fn messages_request_rejects_non_string_stop_sequence_elements() {
    for value in [json!(["END", 1]), json!([null]), json!({"END": true})] {
        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "u"}],
            "stop_sequences": value
        });
        let error = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
        assert!(error
            .json_pointers
            .iter()
            .any(|pointer| pointer.starts_with("/stop_sequences")));
    }
}

#[test]
fn messages_request_tool_choice_strings_are_mapped_not_passed_through() {
    // R9: bare Anthropic tool_choice strings map to Chat values; unknown
    // strings and a bare "tool" (which needs a name) are rejected.
    for (input, expected) in [("auto", "auto"), ("any", "required")] {
        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "u"}],
            "tool_choice": input
        });
        let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
        assert_eq!(prepared.encoded_request["tool_choice"], expected);
    }
    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "tool_choice": "tool"
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("missing_tool_field")));

    let body = json!({
        "model": "m",
        "messages": [{"role": "user", "content": "u"}],
        "tool_choice": "bogus"
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("unsupported_feature.field")));
}

#[test]
fn messages_request_tool_use_requires_input_not_fabricated() {
    // R8/R21: a tool_use without `input` is malformed and must be rejected; we
    // never fabricate `{}`.  An explicit `input: {}` is accepted.
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "assistant", "content": [{"type": "tool_use", "id": "c", "name": "run"}]}
        ]
    });
    let e = CodecRegistry::messages_to_chat("m", &body).unwrap_err();
    assert!(reject_features(&e)
        .iter()
        .any(|c| c.contains("missing_tool_field")));
    assert!(e.json_pointers.iter().any(|p| p.ends_with("/input")));

    let body = json!({
        "model": "m",
        "messages": [
            {"role": "assistant", "content": [{"type": "tool_use", "id": "c", "name": "run", "input": {}}]}
        ]
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    assert_eq!(
        prepared.encoded_request["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
}

#[test]
fn messages_request_tool_results_stay_adjacent_to_assistant() {
    // tool ordering: a user message mixing text-before-tool_result must keep the
    // tool message adjacent to the assistant tool_calls it answers.  Expected
    // order: assistant(tool_calls) -> tool -> user(text).
    let body = json!({
        "model": "m",
        "messages": [
            {"role": "assistant", "content": [{"type": "tool_use", "id": "call_1", "name": "w", "input": {}}]},
            {"role": "user", "content": [
                {"type": "text", "text": "before"},
                {"type": "tool_result", "tool_use_id": "call_1", "content": "result"}
            ]}
        ]
    });
    let prepared = CodecRegistry::messages_to_chat("m", &body).unwrap();
    let msgs = prepared.encoded_request["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["tool_calls"][0]["id"], "call_1");
    // tool message must immediately follow the assistant, ahead of the text.
    assert_eq!(msgs[1]["role"], "tool");
    assert_eq!(msgs[1]["tool_call_id"], "call_1");
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"], "before");
}
