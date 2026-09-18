use super::{CodecId, CodecRegistry, Protocol};
use serde_json::json;

#[test]
fn identity_replaces_only_the_mapped_model_and_reports_native() {
    let request = json!({
        "model": "caller-model",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": false,
        "metadata": {"trace": "preserve"}
    });
    let prepared =
        CodecRegistry::prepare_pair(Protocol::Chat, Protocol::Chat, "mapped-model", &request)
            .expect("identity codec must prepare");

    assert_eq!(prepared.codec.id(), CodecId::Native);
    assert!(prepared.codec.is_identity());
    assert_eq!(prepared.codec.label(), "native");
    assert_eq!(prepared.encoded_request["model"], "mapped-model");
    assert_eq!(prepared.encoded_request["messages"], request["messages"]);
    assert_eq!(prepared.encoded_request["metadata"], request["metadata"]);
    assert_eq!(prepared.report.codec_id, CodecId::Native);
}

#[test]
fn prepared_codec_clone_creates_independent_stream_decoders() {
    let request = json!({
        "model": "caller-model",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": true
    });
    let prepared =
        CodecRegistry::prepare_pair(Protocol::Chat, Protocol::Messages, "mapped-model", &request)
            .expect("chat to messages must prepare");
    let mut first = prepared.codec.new_stream_decoder();
    let mut second = prepared.codec.clone().new_stream_decoder();
    // Chat -> Messages receives Anthropic Messages SSE on the response path.
    let frame = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"mapped-model\",\"usage\":{\"input_tokens\":1}}}\n\n";

    assert_eq!(
        first.feed(frame).expect("first decoder"),
        second.feed(frame).expect("second decoder")
    );
    // `second` was independently initialized: it must accept a continuation
    // without observing or sharing `first`'s internal pending state.
    assert!(first.feed(b"data: {").expect("fragmented first").is_empty());
    assert!(second
        .feed(b"data: {")
        .expect("fragmented second")
        .is_empty());
}

#[test]
fn identity_non_stream_response_returns_usage_with_body() {
    let prepared = CodecRegistry::prepare_pair(
        Protocol::Chat,
        Protocol::Chat,
        "mapped-model",
        &json!({"model": "caller", "messages": []}),
    )
    .unwrap();
    let raw = json!({
        "id": "chatcmpl_1",
        "usage": {"prompt_tokens": 3, "completion_tokens": 5}
    });
    let decoded = prepared
        .codec
        .new_non_stream_decoder()
        .decode(&raw)
        .unwrap();
    assert_eq!(decoded.body, raw);
    let usage = decoded.usage.expect("usage parsed with body");
    assert_eq!((usage.input_tokens, usage.output_tokens), (3, 5));
    assert!(!usage.usage_unknown);
}

#[test]
fn identity_stream_merges_protocol_specific_usage_locations() {
    let messages = CodecRegistry::prepare_pair(
        Protocol::Messages,
        Protocol::Messages,
        "mapped-model",
        &json!({"model":"caller", "stream":true}),
    )
    .unwrap();
    let mut messages_decoder = messages.codec.new_stream_decoder();
    messages_decoder.feed(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\n").unwrap();
    messages_decoder.feed(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n").unwrap();
    let usage = messages_decoder.usage().unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (7, 3));
    assert!(!usage.usage_unknown);

    let responses = CodecRegistry::prepare_pair(
        Protocol::Responses,
        Protocol::Responses,
        "mapped-model",
        &json!({"model":"caller", "stream":true}),
    )
    .unwrap();
    let mut responses_decoder = responses.codec.new_stream_decoder();
    responses_decoder.feed(b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n").unwrap();
    let usage = responses_decoder.usage().unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (5, 2));
    assert!(!usage.usage_unknown);
}

#[test]
fn strategy_wires_response_decoder_in_the_inverse_direction() {
    let chat_request = json!({"model": "caller", "messages": [{"role": "user", "content": "hi"}]});
    let chat_to_messages = CodecRegistry::prepare_pair(
        Protocol::Chat,
        Protocol::Messages,
        "mapped-model",
        &chat_request,
    )
    .unwrap();
    let messages_response = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "mapped-model",
        "content": [{"type": "text", "text": "hello"}], "stop_reason": "end_turn",
        "usage": {"input_tokens": 2, "output_tokens": 1}
    });
    let decoded = chat_to_messages
        .codec
        .new_non_stream_decoder()
        .decode(&messages_response)
        .unwrap();
    assert_eq!(decoded.body["object"], "chat.completion");

    let messages_request = json!({"model": "caller", "max_tokens": 32, "messages": [{"role": "user", "content": "hi"}]});
    let messages_to_chat = CodecRegistry::prepare_pair(
        Protocol::Messages,
        Protocol::Chat,
        "mapped-model",
        &messages_request,
    )
    .unwrap();
    let chat_response = json!({
        "id": "chatcmpl_1", "model": "mapped-model",
        "choices": [{"message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 2, "completion_tokens": 1}
    });
    let decoded = messages_to_chat
        .codec
        .new_non_stream_decoder()
        .decode(&chat_response)
        .unwrap();
    assert_eq!(decoded.body["type"], "message");
}

#[test]
fn registry_has_one_preparable_strategy_for_each_protocol_pair() {
    let chat = json!({"model": "caller", "messages": [{"role": "user", "content": "hi"}]});
    let messages = json!({"model": "caller", "max_tokens": 32, "messages": [{"role": "user", "content": "hi"}]});
    let responses = json!({"model": "caller", "input": []});
    let cases = [
        (Protocol::Chat, Protocol::Chat, &chat, CodecId::Native),
        (
            Protocol::Chat,
            Protocol::Messages,
            &chat,
            CodecId::ChatToMessagesV1,
        ),
        (
            Protocol::Chat,
            Protocol::Responses,
            &chat,
            CodecId::ChatToResponsesV1,
        ),
        (
            Protocol::Messages,
            Protocol::Chat,
            &messages,
            CodecId::MessagesToChatV1,
        ),
        (
            Protocol::Messages,
            Protocol::Messages,
            &messages,
            CodecId::Native,
        ),
        (
            Protocol::Messages,
            Protocol::Responses,
            &messages,
            CodecId::MessagesToResponsesV2,
        ),
        (
            Protocol::Responses,
            Protocol::Chat,
            &responses,
            CodecId::ResponsesToChatV1,
        ),
        (
            Protocol::Responses,
            Protocol::Messages,
            &responses,
            CodecId::ResponsesToMessagesV2,
        ),
        (
            Protocol::Responses,
            Protocol::Responses,
            &responses,
            CodecId::Native,
        ),
        (
            Protocol::Chat,
            Protocol::Gemini,
            &chat,
            CodecId::ChatToGeminiV1,
        ),
        (
            Protocol::Messages,
            Protocol::Gemini,
            &messages,
            CodecId::MessagesToGeminiV1,
        ),
        (
            Protocol::Responses,
            Protocol::Gemini,
            &responses,
            CodecId::ResponsesToGeminiV1,
        ),
    ];

    for (downstream, upstream, request, expected_id) in cases {
        let prepared = CodecRegistry::prepare_pair(downstream, upstream, "mapped-model", request)
            .unwrap_or_else(|error| {
                panic!("{downstream:?} -> {upstream:?} did not prepare: {error}")
            });
        assert_eq!(prepared.codec.id(), expected_id);
        assert_eq!(prepared.codec.downstream(), downstream);
        assert_eq!(prepared.codec.upstream(), upstream);
        assert_eq!(prepared.report.codec_id, expected_id);
    }
}
