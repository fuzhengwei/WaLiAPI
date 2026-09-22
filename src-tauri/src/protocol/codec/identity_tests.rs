use super::*;

fn decoder() -> IdentityStreamDecoder {
    IdentityStreamDecoder {
        protocol: Protocol::Chat,
        pending: Vec::new(),
        usage: None,
        saw_input_usage: false,
        saw_output_usage: false,
        done: false,
    }
}

#[test]
fn ignores_empty_sse_records_after_done() {
    let mut decoder = decoder();
    assert_eq!(decoder.feed(b"data: [DONE]\n\n").unwrap().len(), 1);

    assert!(decoder.feed(b": keepalive\n\n").unwrap().is_empty());
    assert!(decoder.feed(b"event: ping\n\n").unwrap().is_empty());
    assert!(decoder.finish().is_ok());
}

#[test]
fn discards_any_trailing_record_after_done_but_preserves_its_usage() {
    let mut decoder = decoder();
    decoder.feed(b"data: [DONE]\n\n").unwrap();

    assert!(decoder
            .feed(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"extra\"}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3,\"total_tokens\":5}}\n\n"
            )
            .unwrap()
            .is_empty());
    assert_eq!(decoder.usage().unwrap().input_tokens, 2);
    assert_eq!(decoder.usage().unwrap().output_tokens, 3);
}

#[test]
fn discards_response_content_after_done_even_in_same_network_chunk() {
    let mut decoder = decoder();
    assert!(
        decoder
            .feed(b"data: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"extra\"}}]}\n\n")
            .unwrap()
            .len()
            == 1
    );
}

#[test]
fn native_chat_preserves_reasoning_text_in_assistant_history() {
    let request = serde_json::json!({
        "model": "client-model",
        "stream": true,
        "messages": [{
            "role": "assistant",
            "content": "answer",
            "reasoning_text": "private reasoning to continue the tool loop",
            "reasoning_content": "provider-compatible reasoning"
        }]
    });

    let (encoded, _) = CHAT_IDENTITY
        .encode_request(&request, "upstream-model")
        .expect("native Chat request must be encodable");

    assert_eq!(encoded["model"], "upstream-model");
    assert_eq!(
        encoded["messages"][0]["reasoning_text"],
        "private reasoning to continue the tool loop"
    );
    assert_eq!(
        encoded["messages"][0]["reasoning_content"],
        "provider-compatible reasoning"
    );
}

#[test]
fn native_messages_omitted_stream_is_pinned_false() {
    // A downstream non-stream Messages request omits `stream`; the identity
    // codec must pin `stream: false` so default-streaming upstreams (e.g.
    // anthropic proxies) do not return SSE into the non-stream facade.
    let request = serde_json::json!({
        "model": "client-model",
        "max_tokens": 256,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let (encoded, _) = MESSAGES_IDENTITY
        .encode_request(&request, "upstream-model")
        .expect("native Messages request must be encodable");

    assert_eq!(encoded["stream"], false);
    assert_eq!(encoded["model"], "upstream-model");
}

#[test]
fn native_explicit_stream_true_is_preserved() {
    let request = serde_json::json!({
        "model": "client-model",
        "stream": true,
        "messages": [{"role": "user", "content": "hi"}]
    });

    let (encoded, _) = MESSAGES_IDENTITY
        .encode_request(&request, "upstream-model")
        .expect("native Messages request must be encodable");

    assert_eq!(encoded["stream"], true);
}

/// issue #129：旧会话里 `function_call` 条目的 `id` 曾被打上游 Chat 的
/// tool_call id（`call_xxx`），官方 Responses 上游回放历史时会 400。
/// identity 转发前必须把它规范成 `fc_` 前缀，且不动 `call_id`。
#[test]
fn responses_identity_rewrites_legacy_function_call_item_ids() {
    let request = serde_json::json!({
        "model": "gpt-5-codex",
        "stream": true,
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
            {
                "type": "function_call",
                "id": "call_b4a5022d86db4e57b13da2dd",
                "call_id": "call_b4a5022d86db4e57b13da2dd",
                "name": "Read",
                "arguments": "{}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_b4a5022d86db4e57b13da2dd",
                "output": "ok"
            },
            {
                "type": "function_call",
                "id": "fc_already_ok",
                "call_id": "call_keepme",
                "name": "Read",
                "arguments": "{}"
            }
        ]
    });

    let (encoded, _) = RESPONSES_IDENTITY
        .encode_request(&request, "gpt-5-codex")
        .expect("native Responses request must be encodable");
    let items = encoded["input"].as_array().unwrap();

    // 非法条目 id：改写成 `fc_` 前缀，但保留可辨识的后缀。
    assert_eq!(
        items[1]["id"],
        serde_json::json!("fc_b4a5022d86db4e57b13da2dd")
    );
    // `call_id` 是工具调用与结果之间的关联，必须原样。
    assert_eq!(
        items[1]["call_id"],
        serde_json::json!("call_b4a5022d86db4e57b13da2dd")
    );
    // 工具结果条目只靠 call_id 关联，未被追加 id。
    assert_eq!(
        items[2]["call_id"],
        serde_json::json!("call_b4a5022d86db4e57b13da2dd")
    );
    assert!(items[2].get("id").is_none());
    // 已合规的条目 id 不动。
    assert_eq!(items[3]["id"], serde_json::json!("fc_already_ok"));
    // 非 function_call 条目不受影响。
    assert!(items[0].get("id").is_none());
}

/// 非 Responses 的 identity 方向不得触碰 input。
#[test]
fn chat_identity_leaves_responses_shaped_input_untouched() {
    let request = serde_json::json!({
        "model": "m",
        "input": [{"type": "function_call", "id": "call_x", "call_id": "call_x", "name": "n", "arguments": "{}"}]
    });
    let (encoded, _) = CHAT_IDENTITY
        .encode_request(&request, "m-t")
        .expect("identity chat request must be encodable");
    assert_eq!(encoded["input"][0]["id"], serde_json::json!("call_x"));
}
