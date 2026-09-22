use serde_json::Value;

use super::assembler::ResponsesSseAssembler;
use super::convert::convert_openai_sse_to_responses;
use super::events::create_synthetic_completed_events;
use super::state::StreamState;

fn extract_event_types(events: &[String]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| {
            // Each event string is like: "event: response.output_item.added\ndata: ...\n\n"
            let first_line = e.lines().next()?;
            if first_line.starts_with("event: ") {
                Some(first_line.trim_start_matches("event: ").trim().to_string())
            } else {
                None
            }
        })
        .collect()
}

fn extract_event_data(event: &str) -> Value {
    let data_line = event
        .lines()
        .find(|l| l.starts_with("data: "))
        .unwrap()
        .trim_start_matches("data: ")
        .trim();
    serde_json::from_str(data_line).unwrap()
}

/// 63 raw upstream fragments captured from `handle_responses_stream` via
/// `WALIAPI_DEBUG_SSE` instrumentation (deepseek-v4-flash / OpenCode-GO
/// channel, 2026-08-08). Every SSE record is split across multiple TCP
/// chunks — often mid-JSON, with the `\n\n` terminator landing in a fragment
/// that starts mid-record. This is the real-world fragmentation that used to
/// drop tool names / call ids / argument fragments.
const REAL_FRAGMENTS: &[&str] = &[
    "data: {\"id\":\"adba4265-1f45-4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"role\":\"assistant\",\"content\":null,\"reasoning_content\":\"\"}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk",
    "\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\"I\",\"reasoning_content\":null}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk",
    "\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\"'ll\",\"reasoning_content\":null}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f4",
    "5-4b6f-a564-ef2ca7a6e353\",",
    "\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\" read\",\"reasoning_content\":null}}],\"usage\":null}\n\ndata: {\"id\":\"adba4265-1f45-4b6f-a564-ef2ca7a6e353\",\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\" the\",\"reasoning_content\":null}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f4",
    "5-4b6f-a564-ef2ca7a6e353\",\"",
    "object\":\"chat.completion.chu",
    "nk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\" file\",\"reasoning_content\":null}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk",
    "\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\" at\",\"reasoning_content\":null}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f4",
    "5-4b6f-a564-ef2ca7a6e353\",",
    "\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\" that\",\"reasoning_content\":null}}],\"usage\":null}\n\ndata: {\"id\":\"adba4265-1f45-4b6f-a564-ef2ca7a6e353\",\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\" path\",\"reasoning_content\":null}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f4",
    "5-4b6f-a564-ef2ca7a6e353\",\"",
    "object\":\"chat.completion.chu",
    "nk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"content\":\".\",\"reasoning_content\":null}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"o",
    "bject\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_00_ET_qpwrSuOGqdNVOyDYESq94260\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f4",
    "5-4b6f-a564-ef2ca7a6e353\",",
    "\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"\"}}]}}],\"usage\":null}\n\ndata: {\"id\":\"adba4265-1f45-4b6f-a564-ef2ca7a6e353\",\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"path\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45",
    "-",
    "4b6f-a564-ef2ca7a6e353\",\"",
    "object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\": \"}}]}}],\"usage\":null}\n\ndata: {\"id\":\"adba4265-1f45-4b6f-a564-ef2ca7a6e353\",\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f4",
    "5-4b6f-a564-ef2ca7a6e353\",\"",
    "object\":\"chat.completion.chu",
    "nk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"/\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"tmp\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45",
    "-",
    "4b6f-a564-ef2ca7a6e353\",\"",
    "object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"/x\"}}]}}],\"usage\":null}\n\ndata: {\"id\":\"adba4265-1f45-4b6f-a564-ef2ca7a6e353\",\"object\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"ob",
    "ject\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":null,\"logprobs\":null,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]}}],\"usage\":null}\n\n",
    "data: {\"id\":\"adba4265-1f45-",
    "4b6f-a564-ef2ca7a6e353\",\"o",
    "bject\":\"chat.completion.chunk\",\"created\":1786166337,\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"logprobs\":null,\"delta\":{\"content\":\"\",\"reasoning_content\":null}}],\"usage\":{\"prompt_tokens\":348,\"completion_tokens\":53,\"total_tokens\":401,\"prompt_cache_hit_tokens\":256,\"prompt_cache_miss_tokens\":92,\"prompt_tokens_details\":{\"cached_tokens\":256},\"completion_tokens_details\":{\"reasoning_tokens\":0}}}\n\n",
    "data: [DONE]\n\n",
    "data: {\"choices\":[],\"cost\":\"0\"}\n\n",
];

/// Feed each raw fragment through the record-reassembly seam (the same logic
/// the handler uses), then convert each complete record. This reproduces the
/// real upstream fragmentation; without reassembly the tool-call announcement
/// record (carrying name + id) and most argument-delta records are dropped.
fn run_reassembled(fragments: &[&str]) -> Vec<String> {
    let mut state = StreamState::default();
    let mut events = Vec::new();
    let mut asm = ResponsesSseAssembler::new();
    for frag in fragments {
        for record in asm.push(frag.as_bytes()) {
            events.extend(convert_openai_sse_to_responses(
                &record,
                "deepseek-v4-flash",
                "resp_test",
                "",
                &mut state,
            ));
        }
    }
    for record in asm.flush() {
        events.extend(convert_openai_sse_to_responses(
            &record,
            "deepseek-v4-flash",
            "resp_test",
            "",
            &mut state,
        ));
    }
    events
}

#[test]
fn reassembled_tool_call_survives_real_fragmentation() {
    let events = run_reassembled(REAL_FRAGMENTS);
    let types = extract_event_types(&events);

    // The text message must come through intact.
    assert!(
        types.contains(&"response.output_item.added".to_string()),
        "expected a tool call item to be added"
    );

    // The function_call output_item.added must carry the real name + call_id.
    let added = events
        .iter()
        .filter(|e| {
            extract_event_types(&[(*e).clone()]) == vec!["response.output_item.added".to_string()]
        })
        .map(|e| extract_event_data(e))
        .find(|d| d["item"]["type"] == "function_call")
        .expect("a function_call output_item.added must be emitted");

    assert_eq!(
        added["item"]["name"], "read",
        "tool call name lost by fragmentation: got {}",
        added["item"]["name"]
    );
    assert_eq!(
        added["item"]["call_id"], "call_00_ET_qpwrSuOGqdNVOyDYESq94260",
        "tool call id lost by fragmentation: got {}",
        added["item"]["call_id"]
    );
    // 条目 id 与关联 id 是两个字段：`id` 必须是 `fc_` 前缀的条目 id
    // （官方上游回放历史时会校验），`call_id` 保留上游 tool_call id。
    let item_id = added["item"]["id"].as_str().unwrap_or_default();
    assert!(
        item_id.starts_with("fc_"),
        "function_call item id must be fc_-prefixed: got {item_id}"
    );
    assert_ne!(
        item_id,
        added["item"]["call_id"].as_str().unwrap_or_default(),
        "item id must not be the tool call's association id"
    );

    // The final function_call output_item.done must carry full arguments.
    let done = events
        .iter()
        .filter(|e| {
            extract_event_types(&[(*e).clone()]) == vec!["response.output_item.done".to_string()]
        })
        .map(|e| extract_event_data(e))
        .find(|d| d["item"]["type"] == "function_call")
        .expect("a function_call output_item.done must be emitted");

    assert_eq!(
        done["item"]["arguments"], "{\"path\": \"/tmp/x\"}",
        "tool call arguments truncated by fragmentation: got {}",
        done["item"]["arguments"]
    );
    assert_eq!(done["item"]["name"], "read");
    assert_eq!(
        done["item"]["call_id"], "call_00_ET_qpwrSuOGqdNVOyDYESq94260",
        "tool call id must not fall back to call_1"
    );
}

#[test]
fn test_text_only_stream() {
    let mut state = StreamState::default();
    let response_id = "resp_test123";

    // Chunk 1: text delta
    let chunk1 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
    let events1 =
        convert_openai_sse_to_responses(chunk1, "gpt-4", response_id, "Hello", &mut state);
    let types1 = extract_event_types(&events1);
    assert_eq!(
        types1,
        vec![
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
        ]
    );

    // Verify output_index for output_item.added is 0
    let added_data = extract_event_data(&events1[0]);
    assert_eq!(added_data["output_index"], 0);
    assert_eq!(added_data["item"]["type"], "message");

    // Chunk 2: more text
    let chunk2 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}"#;
    let events2 =
        convert_openai_sse_to_responses(chunk2, "gpt-4", response_id, "Hello world", &mut state);
    let types2 = extract_event_types(&events2);
    assert_eq!(types2, vec!["response.output_text.delta"]);

    // Chunk 3: finish
    let chunk3 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
    let events3 =
        convert_openai_sse_to_responses(chunk3, "gpt-4", response_id, "Hello world", &mut state);
    let types3 = extract_event_types(&events3);
    assert_eq!(
        types3,
        vec![
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
        ]
    );

    // Verify output_index in done events
    let text_done_data = extract_event_data(&events3[0]);
    assert_eq!(text_done_data["output_index"], 0);
    assert_eq!(text_done_data["text"], "Hello world");
}

#[test]
fn test_tool_call_only_stream() {
    let mut state = StreamState::default();
    let response_id = "resp_test456";

    // Chunk 1: tool call start (id + name)
    let chunk1 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#;
    let events1 = convert_openai_sse_to_responses(chunk1, "gpt-4", response_id, "", &mut state);
    let types1 = extract_event_types(&events1);
    assert_eq!(types1, vec!["response.output_item.added"]);

    // Verify it's a function_call item
    let added_data = extract_event_data(&events1[0]);
    assert_eq!(added_data["item"]["type"], "function_call");
    assert_eq!(added_data["item"]["call_id"], "call_abc");
    assert_eq!(added_data["item"]["name"], "get_weather");
    assert_eq!(added_data["output_index"], 0);

    // Chunk 2: arguments delta
    let chunk2 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":\"SF\"}"}}]},"finish_reason":null}]}"#;
    let events2 = convert_openai_sse_to_responses(chunk2, "gpt-4", response_id, "", &mut state);
    let types2 = extract_event_types(&events2);
    assert_eq!(types2, vec!["response.function_call_arguments.delta"]);

    // Verify output_index in arguments delta
    let args_delta_data = extract_event_data(&events2[0]);
    assert_eq!(args_delta_data["output_index"], 0);
    assert_eq!(args_delta_data["delta"], "{\"city\":\"SF\"}");

    // Chunk 3: finish with tool_calls
    let chunk3 =
        r#"data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
    let events3 = convert_openai_sse_to_responses(chunk3, "gpt-4", response_id, "", &mut state);
    let types3 = extract_event_types(&events3);
    assert_eq!(
        types3,
        vec![
            "response.function_call_arguments.done",
            "response.output_item.done",
        ]
    );

    // Verify output_item.done has function_call type
    let item_done_data = extract_event_data(&events3[1]);
    assert_eq!(item_done_data["item"]["type"], "function_call");
    assert_eq!(item_done_data["item"]["arguments"], "{\"city\":\"SF\"}");

    // Chunk 4: stray trailing arguments delta AFTER .done — must be suppressed.
    let chunk4 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"EXTRA"}}]},"finish_reason":null}]}"#;
    let events4 = convert_openai_sse_to_responses(chunk4, "gpt-4", response_id, "", &mut state);
    assert!(
        extract_event_types(&events4).is_empty(),
        "no delta may be re-emitted after function_call_arguments.done: {:?}",
        events4
    );
    // The stray arguments must not leak into the accumulated result either.
    assert_eq!(
        state.tool_calls[&0].accumulated_arguments,
        "{\"city\":\"SF\"}"
    );
}

#[test]
fn test_text_then_tool_call_stream() {
    let mut state = StreamState::default();
    let response_id = "resp_test789";

    // Chunk 1: text
    let chunk1 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"content":"Let me check"},"finish_reason":null}]}"#;
    let _ =
        convert_openai_sse_to_responses(chunk1, "gpt-4", response_id, "Let me check", &mut state);
    assert_eq!(state.text_output_index, 0);
    assert_eq!(state.next_output_index, 1);

    // Chunk 2: tool call start
    let chunk2 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_xyz","function":{"name":"search","arguments":""}}]},"finish_reason":null}]}"#;
    let events2 =
        convert_openai_sse_to_responses(chunk2, "gpt-4", response_id, "Let me check", &mut state);
    let types2 = extract_event_types(&events2);
    assert_eq!(types2, vec!["response.output_item.added"]);

    // Verify tool call gets output_index=1 (after text's index 0)
    let added_data = extract_event_data(&events2[0]);
    assert_eq!(added_data["output_index"], 1);
    assert_eq!(added_data["item"]["type"], "function_call");

    // Chunk 3: arguments
    let chunk3 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]},"finish_reason":null}]}"#;
    let events3 =
        convert_openai_sse_to_responses(chunk3, "gpt-4", response_id, "Let me check", &mut state);
    let types3 = extract_event_types(&events3);
    assert_eq!(types3, vec!["response.function_call_arguments.delta"]);

    // Chunk 4: finish
    let chunk4 =
        r#"data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
    let events4 =
        convert_openai_sse_to_responses(chunk4, "gpt-4", response_id, "Let me check", &mut state);
    let types4 = extract_event_types(&events4);
    assert_eq!(
        types4,
        vec![
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.function_call_arguments.done",
            "response.output_item.done",
        ]
    );

    // Verify text done uses index 0
    let text_done = extract_event_data(&events4[0]);
    assert_eq!(text_done["output_index"], 0);

    // function_call_arguments.done carries the required `name`
    let fc_args_done = extract_event_data(&events4[3]);
    assert_eq!(
        fc_args_done["type"],
        "response.function_call_arguments.done"
    );
    assert_eq!(fc_args_done["name"], "search");

    // Verify function_call done uses index 1
    let fc_item_done = extract_event_data(&events4[4]);
    assert_eq!(fc_item_done["output_index"], 1);
    assert_eq!(fc_item_done["item"]["type"], "function_call");
}

#[test]
fn test_reasoning_item_full_lifecycle() {
    // A reasoning_content delta must announce a `reasoning` item with
    // output_item.added BEFORE any delta, and close it with
    // output_item.done. Without the item lifecycle, Codex never persists a
    // reasoning item, so the next turn omits reasoning_content and
    // DeepSeek rejects it ("must be passed back to the API").
    let mut state = StreamState::default();
    let response_id = "resp_rs123";

    // Chunk 1: reasoning delta
    let chunk1 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"reasoning_content":"Let me"},"finish_reason":null}]}"#;
    let events1 =
        convert_openai_sse_to_responses(chunk1, "deepseek-v4-flash", response_id, "", &mut state);
    let types1 = extract_event_types(&events1);
    assert_eq!(
        types1,
        vec![
            "response.output_item.added",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_text.delta",
        ]
    );

    // reasoning item announced with type=reasoning
    let added = extract_event_data(&events1[0]);
    assert_eq!(added["item"]["type"], "reasoning");
    assert_eq!(added["item"]["id"], "rs_rs123");
    assert_eq!(added["output_index"], 0);

    // summary part added before deltas
    let part_added = extract_event_data(&events1[1]);
    assert_eq!(part_added["part"]["type"], "reasoning_summary_text");

    // delta carries the reasoning text on item rs_
    let delta = extract_event_data(&events1[2]);
    assert_eq!(delta["delta"], "Let me");
    assert_eq!(delta["item_id"], "rs_rs123");

    // Chunk 2: more reasoning
    let chunk2 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"reasoning_content":" think."},"finish_reason":null}]}"#;
    let events2 =
        convert_openai_sse_to_responses(chunk2, "deepseek-v4-flash", response_id, "", &mut state);
    let types2 = extract_event_types(&events2);
    assert_eq!(types2, vec!["response.reasoning_summary_text.delta"]);

    // Chunk 3: content text
    let chunk3 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"content":"Answer"},"finish_reason":null}]}"#;
    let events3 = convert_openai_sse_to_responses(
        chunk3,
        "deepseek-v4-flash",
        response_id,
        "Answer",
        &mut state,
    );
    let types3 = extract_event_types(&events3);
    assert_eq!(
        types3,
        vec![
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
        ]
    );
    // text item gets output_index 1 (reasoning took 0)
    let text_added = extract_event_data(&events3[0]);
    assert_eq!(text_added["output_index"], 1);
    assert_eq!(text_added["item"]["type"], "message");

    // Chunk 4: finish
    let chunk4 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
    let events4 = convert_openai_sse_to_responses(
        chunk4,
        "deepseek-v4-flash",
        response_id,
        "Answer",
        &mut state,
    );
    let types4 = extract_event_types(&events4);
    assert_eq!(
        types4,
        vec![
            "response.reasoning_summary_text.done",
            "response.reasoning_summary_part.done",
            "response.output_item.done",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
        ]
    );

    // reasoning_summary_text.done carries the full accumulated text
    let rs_text_done = extract_event_data(&events4[0]);
    assert_eq!(rs_text_done["type"], "response.reasoning_summary_text.done");
    assert_eq!(rs_text_done["text"], "Let me think.");
    assert_eq!(rs_text_done["summary_index"], 0);
    assert_eq!(rs_text_done["item_id"], "rs_rs123");

    // reasoning item closed with the full text in the summary
    let rs_done = extract_event_data(&events4[2]);
    assert_eq!(rs_done["item"]["type"], "reasoning");
    assert_eq!(rs_done["item"]["status"], "completed");
    assert_eq!(rs_done["item"]["summary"][0]["text"], "Let me think.");
    assert_eq!(rs_done["output_index"], 0);

    // response.completed output array must contain the reasoning item too
    let synthetic = create_synthetic_completed_events(
        "deepseek-v4-flash",
        response_id,
        "Answer",
        &state,
        10,
        5,
    );
    let completed_event = synthetic
        .iter()
        .find(|e| {
            e.lines()
                .next()
                .map(|l| l == "event: response.completed")
                .unwrap_or(false)
        })
        .unwrap();
    let completed = extract_event_data(completed_event);
    let output = completed["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 2);
    assert_eq!(output[0]["type"], "reasoning");
    assert_eq!(output[0]["summary"][0]["text"], "Let me think.");
    assert_eq!(output[1]["type"], "message");
}

#[test]
fn test_synthetic_completed_with_tool_calls() {
    let mut state = StreamState::default();
    let response_id = "resp_test_syn";

    // Simulate: tool call only, no finish_reason in stream
    let chunk1 = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"test","arguments":"{\"x\":1}"}}]},"finish_reason":null}]}"#;
    let _ = convert_openai_sse_to_responses(chunk1, "gpt-4", response_id, "", &mut state);

    // Stream ends without finish_reason — call synthetic completed
    let synth = create_synthetic_completed_events("gpt-4", response_id, "", &state, 10, 20);
    let synth_types = extract_event_types(&synth);
    assert_eq!(
        synth_types,
        vec![
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ]
    );

    // function_call_arguments.done now carries the required `name`
    let args_done = extract_event_data(&synth[0]);
    assert_eq!(args_done["type"], "response.function_call_arguments.done");
    assert_eq!(args_done["name"], "test");

    // Verify response.completed has function_call in output
    let completed_data = extract_event_data(&synth[2]);
    assert_eq!(
        completed_data["response"]["output"][0]["type"],
        "function_call"
    );
    assert_eq!(completed_data["response"]["usage"]["input_tokens"], 10);
    assert_eq!(completed_data["response"]["usage"]["output_tokens"], 20);
    // usage now carries the official details sub-objects
    assert_eq!(
        completed_data["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
        0
    );
    assert_eq!(
        completed_data["response"]["usage"]["input_tokens_details"]["cached_tokens"],
        0
    );
    assert_eq!(completed_data["response"]["usage"]["total_tokens"], 30);
}
