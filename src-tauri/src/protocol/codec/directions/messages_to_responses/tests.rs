use super::stream::ResponsesMessagesStream;
use super::{decode_response, encode_request};
use crate::protocol::codec::ports::StreamDecoder;
use crate::protocol::codec::report::ConversionContext;

#[test]
fn request_drops_safeguards_fail_open() {
    // Claude Code 2.1.280 的顶层 `safeguards`（分类器上下文声明）在 Responses 里
    // 没有对应物；整段拒绝会让会话无法继续，因此按 fail-open 丢弃并记录。
    let (out, context) = encode_request(
        &serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "safeguards": [{"type": "classifier", "classifier_context": "ctx"}]
        }),
        "m",
    )
    .unwrap();
    assert!(out.get("safeguards").is_none());
    assert!(context
        .normalized
        .iter()
        .any(|pointer| pointer.contains("safeguards")));
}

#[test]
fn request_rejects_non_string_stop_sequence_elements() {
    for value in [serde_json::json!(["END", 1]), serde_json::json!([null])] {
        let error = encode_request(
            &serde_json::json!({
                "messages": [{"role": "user", "content": "hi"}],
                "stop_sequences": value
            }),
            "m",
        )
        .unwrap_err();
        assert!(error
            .json_pointers
            .iter()
            .any(|pointer| pointer.starts_with("/stop_sequences")));
    }
}

#[test]
fn request_preserves_tool_result_id() {
    let(out,_)=encode_request(&serde_json::json!({"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"ok"}]}]}),"m").unwrap();
    assert_eq!(out["input"][0]["call_id"], "call_1");
}

#[test]
fn request_maps_in_band_system_message_to_developer() {
    let (out, _) = encode_request(
        &serde_json::json!({"messages":[
            {"role":"user","content":"before"},
            {"role":"system","content":[{"type":"text","text":"hook context","cache_control":{"type":"ephemeral"}}]},
            {"role":"user","content":"after"}
        ]}),
        "m",
    )
    .unwrap();

    let input = out["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[1]["role"], "developer");
    assert_eq!(input[1]["content"][0]["type"], "input_text");
    assert_eq!(input[1]["content"][0]["text"], "hook context");
}

#[test]
fn request_preserves_interleaved_message_content_order_and_rejects_non_text_tool_output() {
    let (out, _) = encode_request(
        &serde_json::json!({"messages":[{
            "role":"assistant", "content":[
                {"type":"text", "text":"before"},
                {"type":"tool_use", "id":"call_1", "name":"lookup", "input":{}},
                {"type":"text", "text":"after"}
            ]
        }]}),
        "m",
    )
    .unwrap();
    assert_eq!(
        out["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["message", "function_call", "message"]
    );

    let err = encode_request(&serde_json::json!({"messages":[{
        "role":"user", "content":[{"type":"tool_result", "tool_use_id":"call_1", "content":[{"type":"image"}]}]
    }]}), "m").unwrap_err();
    assert!(err
        .features
        .iter()
        .any(|feature| feature == "unsupported_feature.unknown_block"));
}

#[test]
fn request_replays_thinking_as_reasoning_text_content() {
    let (out, _) = encode_request(
        &serde_json::json!({"messages":[{
            "role":"assistant",
            "content":[{"type":"thinking","thinking":"replay me"}]
        }]}),
        "m",
    )
    .unwrap();
    assert_eq!(out["input"][0]["type"], "reasoning");
    assert_eq!(out["input"][0]["content"][0]["type"], "reasoning_text");
    assert_eq!(out["input"][0]["content"][0]["text"], "replay me");

    let context = ConversionContext::new("msg_1", "m", false);
    let decoded = decode_response(
        &serde_json::json!({
            "status":"completed",
            "output":[{"type":"reasoning","content":[{"type":"reasoning_text","text":"replay me"}]}],
            "usage":{"input_tokens":1,"output_tokens":1}
        }),
        &context,
    )
    .unwrap();
    assert_eq!(decoded.body["content"][0]["type"], "thinking");
    assert_eq!(decoded.body["content"][0]["thinking"], "replay me");
}

#[test]
fn response_preserves_function_id() {
    let c = ConversionContext::new("x", "m", false);
    let d=decode_response(&serde_json::json!({"status":"completed","output":[{"type":"function_call","call_id":"call_1","name":"weather","arguments":"{}"}],"usage":{"input_tokens":1,"output_tokens":2}}),&c).unwrap();
    assert_eq!(d.body["content"][0]["id"], "call_1");
}

#[test]
fn stream_is_split_invariant_and_closes_message_once() {
    let source = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
        "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"你好\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\"}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("msg_1", "m", true);
    let mut expected = None;
    for split in 0..=source.len() {
        let mut decoder = ResponsesMessagesStream::new(&context);
        let mut events = decoder.feed(&source.as_bytes()[..split]).unwrap();
        events.extend(decoder.feed(&source.as_bytes()[split..]).unwrap());
        events.extend(decoder.finish().unwrap());
        if let Some(expected) = &expected {
            assert_eq!(&events, expected);
        } else {
            expected = Some(events);
        }
    }
    let events = expected.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.contains("message_stop"))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.contains("message_start"))
            .count(),
        1
    );
}

#[test]
fn stream_accepts_terminal_text_without_numeric_output_index() {
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hello\"}\n\n",
        "data: {\"type\":\"response.output_text.done\",\"text\":\"hello\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":\"0\",\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("msg_1", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    assert!(output.contains("hello"));
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_accepts_string_index_on_second_output_item_done() {
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"answer\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":1,\"delta\":\"thought\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":\"1\",\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":2}}}\n\n"
    );
    let context = ConversionContext::new("msg_1", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    assert!(output.contains("\"index\":1"));
    assert!(output.contains("thought"));
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_resolves_omitted_index_on_unique_item_completion() {
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":1,\"delta\":\"thought\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("msg_1", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    assert!(output.contains("\"index\":1"));
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_infers_delta_index_when_reasoning_precedes_message() {
    // A reasoning item at index 0 and a message at index 1: a text delta
    // that omits `output_index` must resolve to the open message block, not
    // default to 0 and mis-target the reasoning block.
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"delta\":\"think\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.done\",\"output_index\":0,\"text\":\"think\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
        "data: {\"type\":\"response.output_text.done\",\"text\":\"hello\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("msg_1", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    assert!(
        output.contains("hello"),
        "text must reach the message item:\n{output}"
    );
    assert!(
        output.contains("\"index\":1"),
        "text delta must target item 1:\n{output}"
    );
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_infers_part_lifecycle_index_when_omitted() {
    // A `content_part.done` frame that omits `output_index` after the item
    // was identified by an earlier lifecycle frame must resolve to the open
    // message item instead of failing closed.
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.content_part.done\",\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"hi\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("msg_1", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    assert!(
        output.contains("hi"),
        "part text must reach the message item:\n{output}"
    );
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_accepts_complete_responses_text_reasoning_and_tool_lifecycles() {
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hello\"}\n\n",
        "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"text\":\"hello\"}\n\n",
        "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"hello\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_part.added\",\"output_index\":1,\"summary_index\":0,\"part\":{\"type\":\"reasoning_summary_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":1,\"delta\":\"think\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.done\",\"output_index\":1,\"text\":\"think\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_part.done\",\"output_index\":1,\"summary_index\":0,\"part\":{\"type\":\"reasoning_summary_text\",\"text\":\"think\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":2,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"lookup\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":2,\"delta\":\"{}\"}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":2,\"arguments\":\"{}\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":2,\"item\":{\"type\":\"function_call\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("r", "m", true);
    let mut expected = None;
    for split in 0..=source.len() {
        let mut decoder = ResponsesMessagesStream::new(&context);
        let mut events = decoder.feed(&source.as_bytes()[..split]).unwrap();
        events.extend(decoder.feed(&source.as_bytes()[split..]).unwrap());
        events.extend(decoder.finish().unwrap());
        assert_eq!(decoder.usage().unwrap().output_tokens, 1);
        if let Some(previous) = &expected {
            assert_eq!(&events, previous);
        } else {
            expected = Some(events);
        }
    }
    let output = expected.unwrap().join("");
    assert!(output.contains("hello") && output.contains("think") && output.contains("call_1"));
}

#[test]
fn stream_accepts_deepseek_raw_cot_reasoning_content_parts() {
    // DeepSeek opts out of reasoning summaries and streams raw
    // chain-of-thought through the reasoning item's `content` array:
    // `content_part.*` with `part.type = "reasoning_text"` plus
    // `reasoning_text.delta/done`, indexed by `content_index`.  This
    // used to fail closed on `content_part.done` because the decoder
    // hard-assumed every `content_part` belonged to a message item.
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs\",\"type\":\"reasoning\",\"status\":\"in_progress\",\"content\":[]}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"reasoning_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"let me\"}\n\n",
        "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\" think\"}\n\n",
        "data: {\"type\":\"response.reasoning_text.done\",\"output_index\":0,\"content_index\":0,\"text\":\"let me think\"}\n\n",
        "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"reasoning_text\",\"text\":\"let me think\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs\",\"type\":\"reasoning\",\"content\":[{\"type\":\"reasoning_text\",\"text\":\"let me think\"}],\"status\":\"completed\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"id\":\"m1\",\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":1,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":1,\"delta\":\"answer\"}\n\n",
        "data: {\"type\":\"response.output_text.done\",\"output_index\":1,\"text\":\"answer\"}\n\n",
        "data: {\"type\":\"response.content_part.done\",\"output_index\":1,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"answer\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"id\":\"m1\",\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("msg_1", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    // reasoning_text must land in an Anthropic thinking block, not text.
    assert!(
        output.contains("\"thinking\":\"let me\"")
            && output.contains("\"thinking\":\" think\"")
            && output.contains("content_block_start")
            && output.contains("\"thinking\":\"\"") // empty thinking start block
            && output.contains("\"type\":\"thinking_delta\"")
            && output.contains("content_block_stop"),
        "raw CoT must stream as thinking:\n{output}"
    );
    // The message item still streams as text at a distinct index.
    assert!(
        output.contains("\"text\":\"answer\"") && output.contains("\"type\":\"text_delta\""),
        "message item must stream as text:\n{output}"
    );
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_maps_refusal_part_to_refusal_stop() {
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"refusal\",\"refusal\":\"\"}}\n\n",
        "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"refusal\",\"refusal\":\"I cannot help\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("r", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    // refusal text reaches the user as a text block.
    assert!(
        output.contains("\"text\":\"I cannot help\"") && output.contains("\"type\":\"text_delta\""),
        "refusal text must stream as text:\n{output}"
    );
    // and the terminal stop reason carries the refusal semantic.
    assert!(
        output.contains("\"stop_reason\":\"refusal\""),
        "refusal must terminate with stop_reason refusal:\n{output}"
    );
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_accepts_official_summary_text_part_type() {
    // The canonical summary part type is `summary_text` (not
    // `reasoning_summary_text`); both must be accepted.
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_part.added\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"delta\":\"think\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.done\",\"output_index\":0,\"text\":\"think\"}\n\n",
        "data: {\"type\":\"response.reasoning_summary_part.done\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"think\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("r", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    assert!(
        output.contains("\"thinking\":\"think\"") && output.contains("\"type\":\"thinking_delta\""),
        "summary must stream as thinking:\n{output}"
    );
    assert!(output.contains("message_stop"));
}

#[test]
fn stream_standalone_incomplete_is_max_tokens_not_failure() {
    // DeepSeek terminates truncated streams with a standalone
    // `response.incomplete` event.  That is a normal completion with a
    // `max_tokens` stop reason, not an upstream failure.
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"partial\"}\n\n",
        "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\",\"text\":\"partial\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\"}}\n\n",
        "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"r\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
    );
    let context = ConversionContext::new("r", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let mut events = decoder.feed(source.as_bytes()).unwrap();
    events.extend(decoder.finish().unwrap());
    let output = events.concat();
    assert!(
        output.contains("\"stop_reason\":\"max_tokens\"") && output.contains("message_stop"),
        "standalone incomplete must terminate with max_tokens:\n{output}"
    );
}

#[test]
fn stream_standalone_failed_remains_error() {
    let source = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"model\":\"m\"}}\n\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"r\",\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"boom\"}}}\n\n"
    );
    let context = ConversionContext::new("r", "m", true);
    let mut decoder = ResponsesMessagesStream::new(&context);
    let err = decoder.feed(source.as_bytes()).unwrap_err();
    assert!(
        err.to_string().contains("upstream reported failure"),
        "failed must surface as an upstream error, got: {err}"
    );
}
