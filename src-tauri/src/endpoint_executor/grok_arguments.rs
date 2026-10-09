//! Grok 响应侧工具参数规范化。
//!
//! grok-4.7 会把整数参数写成浮点：`{"yield_time_ms":4000.0,"max_output_tokens":8000.0}`。
//! Codex CLI 按本地 schema 反序列化时直接失败：
//! `failed to parse function arguments: invalid type: floating point `4000.0`, expected usize`。
//! 模型侧的数值习惯网关改不了，但响应里的 JSON 可以修正：把"数值上等于整数"的
//! 浮点字面量写成整数。数值不变，对确实期望浮点的字段同样安全（`1` 与 `1.0` 等值）。
//!
//! 改写是失败安全的：任何解析失败、非目标事件、非完整 JSON 分片都原样透传，
//! 只作用于 Grok provider 的上游响应（流式与非流式两条路径）。

use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;

/// 超过该值后 `f64` 无法精确表示整数，保持原样以免改变数值。
const MAX_EXACT_INTEGER: f64 = 9_007_199_254_740_992.0; // 2^53

/// 把工具参数文本里的整数值浮点规范成整数。
///
/// 仅在语义发生变化时返回改写后的文本：数值全部已是整数时返回 `None`，
/// 调用方保持原始字节不改写（也避免无谓的键序变化）。
pub(crate) fn normalize_arguments(text: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let normalized = normalize_value(parsed.clone());
    if normalized == parsed {
        return None;
    }
    serde_json::to_string(&normalized).ok()
}

/// 递归规范化 JSON 里的整数值浮点。
fn normalize_value(value: Value) -> Value {
    match value {
        Value::Number(number) => normalize_number(number),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_value).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, normalize_value(value)))
                .collect(),
        ),
        other => other,
    }
}

fn normalize_number(number: serde_json::Number) -> Value {
    // 整数字面量原样保留；只有浮点表示才需要改写。
    if !number.is_f64() {
        return Value::Number(number);
    }
    let Some(float) = number.as_f64() else {
        return Value::Number(number);
    };
    if !float.is_finite() || float.fract() != 0.0 || float.abs() > MAX_EXACT_INTEGER {
        return Value::Number(number);
    }
    if float >= 0.0 {
        if float <= u64::MAX as f64 {
            return Value::Number(serde_json::Number::from(float as u64));
        }
    } else if float >= i64::MIN as f64 {
        return Value::Number(serde_json::Number::from(float as i64));
    }
    Value::Number(number)
}

/// 改写单条下游 SSE record；只处理承载工具参数的事件，其余原样返回。
pub(crate) fn rewrite_record(record: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(record) else {
        return record.to_vec();
    };
    let mut rewritten_text = String::with_capacity(text.len());
    let mut touched = false;
    for line in text.split_inclusive('\n') {
        if let Some(payload) = line.trim_end_matches(['\n', '\r']).strip_prefix("data:") {
            if let Some(rewritten) = rewrite_event_payload(payload.trim_start()) {
                rewritten_text.push_str("data: ");
                rewritten_text.push_str(&rewritten);
                // 保留原始行尾：上游可能是 CRLF，丢掉 `\r` 会把 record 终止符
                // 从 `\r\n\r\n` 变成 `\n\r\n`，让后续 `record_end` 认不出边界。
                if line.ends_with("\r\n") {
                    rewritten_text.push_str("\r\n");
                } else if line.ends_with('\n') {
                    rewritten_text.push('\n');
                }
                touched = true;
                continue;
            }
        }
        rewritten_text.push_str(line);
    }
    if touched {
        rewritten_text.into_bytes()
    } else {
        record.to_vec()
    }
}

/// 规范化单个 SSE 事件的 `data:` JSON；未改变时返回 `None`。
fn rewrite_event_payload(payload: &str) -> Option<String> {
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let mut event: Value = serde_json::from_str(payload).ok()?;
    let kind = event.get("type").and_then(Value::as_str)?;
    let touched = match kind {
        "response.function_call_arguments.delta" => rewrite_string_field(&mut event, "delta"),
        "response.function_call_arguments.done" => rewrite_string_field(&mut event, "arguments"),
        "response.output_item.added" | "response.output_item.done" => {
            rewrite_item_arguments(&mut event)
        }
        // 终止事件回显整份 response，其中 output[] 也带着同一批 function_call。
        "response.completed" | "response.done" => rewrite_completed_output(&mut event),
        _ => false,
    };
    if !touched {
        return None;
    }
    serde_json::to_string(&event).ok()
}

/// 改写事件顶层的字符串字段（工具参数文本）。
fn rewrite_string_field(event: &mut Value, field: &str) -> bool {
    let Some(current) = event.get(field).and_then(Value::as_str) else {
        return false;
    };
    let Some(normalized) = normalize_arguments(current) else {
        return false;
    };
    let Some(object) = event.as_object_mut() else {
        return false;
    };
    object.insert(field.to_owned(), Value::String(normalized));
    true
}

/// 改写 `response.output_item.*` 里 function_call 条目的参数。
fn rewrite_item_arguments(event: &mut Value) -> bool {
    let Some(item) = event.get_mut("item").and_then(Value::as_object_mut) else {
        return false;
    };
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return false;
    }
    rewrite_string_field_in(item, "arguments")
}

/// 改写终止事件里回显的 `response.output[].arguments`。
fn rewrite_completed_output(event: &mut Value) -> bool {
    let Some(items) = event
        .get_mut("response")
        .and_then(Value::as_object_mut)
        .and_then(|response| response.get_mut("output"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    let mut touched = false;
    for item in items.iter_mut() {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        touched |= rewrite_string_field_in(object, "arguments");
    }
    touched
}

fn rewrite_string_field_in(object: &mut serde_json::Map<String, Value>, field: &str) -> bool {
    let Some(current) = object.get(field).and_then(Value::as_str) else {
        return false;
    };
    let Some(normalized) = normalize_arguments(current) else {
        return false;
    };
    object.insert(field.to_owned(), Value::String(normalized));
    true
}

/// 包装 Grok 上游字节流，按 SSE record 逐个改写。
///
/// 跨 chunk 的不完整 record 会留在缓冲区，直到记录边界到达；流结束仍未闭合的
/// 尾部按原样发出（不会丢字节）。
pub(crate) fn rewrite_stream(
    mut inner: futures_util::stream::BoxStream<'static, Result<Bytes, std::io::Error>>,
) -> futures_util::stream::BoxStream<'static, Result<Bytes, std::io::Error>> {
    async_stream::stream! {
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(item) = inner.next().await {
            match item {
                Ok(chunk) => {
                    buffer.extend_from_slice(&chunk);
                    // 与解码器同一条 FIX-11 防护：上游持续发无终止符字节时在这里
                    // 就停，不让本层缓冲成为无界累加点。
                    if crate::protocol::codec::sse::pending_exceeded(&buffer) {
                        yield Err(std::io::Error::other(
                            crate::protocol::codec::sse::pending_overflow_message(),
                        ));
                        return;
                    }
                    let mut out: Vec<u8> = Vec::new();
                    while let Some(end) = crate::protocol::codec::sse::record_end(&buffer) {
                        let record: Vec<u8> = buffer.drain(..end).collect();
                        out.extend_from_slice(&rewrite_record(&record));
                    }
                    if !out.is_empty() {
                        yield Ok(Bytes::from(out));
                    }
                }
                Err(error) => {
                    yield Err(error);
                    return;
                }
            }
        }
        if !buffer.is_empty() {
            yield Ok(Bytes::from(rewrite_record(&buffer)));
        }
    }
    .boxed()
}

/// 非流式：改写 `output[].arguments`（Responses 形状）。
pub(crate) fn rewrite_response_body(body: &mut Value) {
    let Some(items) = body.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items.iter_mut() {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        rewrite_string_field_in(object, "arguments");
    }
}

/// 该 attempt 的响应是否需要工具参数规范化（仅 Grok provider）。
pub(crate) fn needs_normalization(auth_provider: Option<&str>) -> bool {
    matches!(auth_provider, Some("grok"))
}

/// 便于测试：把流式事件序列按 record 边界跑一遍改写。
#[cfg(test)]
pub(crate) fn rewrite_records_for_test(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buffer = input.to_vec();
    while let Some(end) = crate::protocol::codec::sse::record_end(&buffer) {
        let record: Vec<u8> = buffer.drain(..end).collect();
        out.extend_from_slice(&rewrite_record(&record));
    }
    out.extend_from_slice(&buffer);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integer_valued_floats_become_integers() {
        let text = r#"{"cmd":"echo 1.5","yield_time_ms":4000.0,"max_output_tokens":8000.0}"#;
        let out = normalize_arguments(text).unwrap();
        assert!(!out.contains("4000.0") && !out.contains("8000.0"));
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["yield_time_ms"].as_u64(), Some(4000));
        assert_eq!(value["max_output_tokens"].as_u64(), Some(8000));
        assert_eq!(value["cmd"].as_str(), Some("echo 1.5"));
    }

    #[test]
    fn already_integer_arguments_are_left_untouched() {
        // 数值未变就返回 None：保持上游原字节，避免无谓的键序重排。
        assert!(
            normalize_arguments(r#"{"yield_time_ms":4000,"max_output_tokens":8000}"#).is_none()
        );
        assert!(normalize_arguments(r#"{"ratio":0.5}"#).is_none());
        assert!(normalize_arguments(r#"{"label":"hi"}"#).is_none());
    }

    #[test]
    fn fractional_and_large_values_are_preserved() {
        let text = r#"{"ratio":1.5,"big":1e300,"neg":-2.0}"#;
        let out = normalize_arguments(text).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["ratio"].as_f64(), Some(1.5));
        assert!(value["big"].is_f64());
        assert_eq!(value["neg"].as_i64(), Some(-2));
    }

    #[test]
    fn nested_structures_are_normalized() {
        let text = r#"{"a":[{"b":1.0}],"c":{"d":[2.0,3.5]}}"#;
        let out = normalize_arguments(text).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["a"][0]["b"].as_u64(), Some(1));
        assert_eq!(value["c"]["d"][0].as_u64(), Some(2));
        assert_eq!(value["c"]["d"][1].as_f64(), Some(3.5));
    }

    #[test]
    fn invalid_json_is_left_alone() {
        assert!(normalize_arguments("{\"a\":4000.0").is_none());
        assert!(normalize_arguments("not json").is_none());
    }

    #[test]
    fn arguments_delta_event_is_rewritten() {
        let record = concat!(
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"sequence_number\":44,",
            "\"delta\":\"{\\\"yield_time_ms\\\":4000.0}\",\"item_id\":\"fc_1\",\"output_index\":1}\n\n"
        );
        let out = String::from_utf8(rewrite_record(record.as_bytes())).unwrap();
        assert!(out.contains(r#"\"yield_time_ms\":4000}"#), "{out}");
        assert!(!out.contains("4000.0"));
        assert!(out.starts_with("event: response.function_call_arguments.delta\n"));
    }

    #[test]
    fn output_item_done_event_is_rewritten() {
        let record = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",",
            "\"name\":\"run_task\",\"arguments\":\"{\\\"max_output_tokens\\\":8000.0}\"}}\n\n"
        );
        let out = String::from_utf8(rewrite_record(record.as_bytes())).unwrap();
        assert!(out.contains(r#"\"max_output_tokens\":8000}"#), "{out}");
    }

    #[test]
    fn unrelated_events_pass_through_byte_identical() {
        let record = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"cost 4000.0\"}\n\n";
        assert_eq!(rewrite_record(record.as_bytes()), record.as_bytes());
    }

    #[test]
    fn integer_arguments_record_passes_through_byte_identical() {
        let record = concat!(
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",",
            "\"arguments\":\"{\\\"yield_time_ms\\\":4000,\\\"max_output_tokens\\\":8000}\"}\n\n"
        );
        assert_eq!(rewrite_record(record.as_bytes()), record.as_bytes());
    }

    #[test]
    fn partial_json_delta_passes_through() {
        let record = concat!(
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"yield_ti\"}\n\n"
        );
        assert_eq!(rewrite_record(record.as_bytes()), record.as_bytes());
    }

    #[test]
    fn records_split_across_chunks_are_reassembled() {
        let full = concat!(
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"arguments\":\"{\\\"yield_time_ms\\\":3000.0}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\"}\n\n"
        );
        let out = String::from_utf8(rewrite_records_for_test(full.as_bytes())).unwrap();
        assert!(out.contains(r#"\"yield_time_ms\":3000}"#), "{out}");
        assert!(out.contains("event: response.completed"));
    }

    #[test]
    fn real_codex_exec_command_arguments_are_fixed() {
        // 形状取自真实 Codex 会话：grok-4.7 把 usize 参数写成浮点，Codex 报
        // `invalid type: floating point 4000.0, expected usize`。
        let text = r#"{"cmd":"ls -la","workdir":"/tmp","yield_time_ms":4000.0,"max_output_tokens":10000.0,"sandbox_permissions":"use_default"}"#;
        let out = normalize_arguments(text).unwrap();
        assert!(!out.contains("4000.0") && !out.contains("10000.0"));
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["yield_time_ms"].as_u64(), Some(4000));
        assert_eq!(value["max_output_tokens"].as_u64(), Some(10000));
        assert_eq!(value["sandbox_permissions"].as_str(), Some("use_default"));
        assert_eq!(value["cmd"].as_str(), Some("ls -la"));
    }

    #[tokio::test]
    async fn stream_rewrite_reassembles_records_split_across_chunks() {
        // 上游把一条 record 拆成两个 chunk：改写必须等边界到达后再做。
        let chunks = vec![
            Ok(Bytes::from_static(
                b"event: response.function_call_arguments.done\ndata: {\"type\":\"response.function_call_arguments.done\",\"arguments\":\"{\\\"yield_time_ms\\\":4000.",
            )),
            Ok(Bytes::from_static(
                b"0}\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\"}\n\n",
            )),
        ];
        let source = futures_util::stream::iter(chunks).boxed();
        let out = rewrite_stream(source).collect::<Vec<_>>().await;
        let text: String = out
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.expect("chunk").to_vec()).expect("utf8"))
            .collect();
        assert!(text.contains(r#"\"yield_time_ms\":4000}"#), "{text}");
        assert!(!text.contains("4000.0"));
        assert!(text.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn stream_rewrite_forwards_errors_and_tail_bytes() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(
                b"data: {\"type\":\"response.created\"}\n\n",
            )),
            Ok(Bytes::from_static(
                b"data: {\"type\":\"response.in_progress\"}",
            )),
        ];
        let source = futures_util::stream::iter(chunks).boxed();
        let out = rewrite_stream(source).collect::<Vec<_>>().await;
        let text: String = out
            .into_iter()
            .map(|chunk| String::from_utf8(chunk.expect("chunk").to_vec()).expect("utf8"))
            .collect();
        assert!(text.contains("response.created"));
        // 未闭合的尾部字节原样保留，不丢数据。
        assert!(text.contains("response.in_progress"));
    }

    #[test]
    fn crlf_records_keep_their_terminator() {
        // 上游可能用 CRLF；改写后丢掉 `\r` 会让 `\r\n\r\n` 变成 `\n\r\n`，
        // record_end 再也认不出边界。
        let record = concat!(
            "event: response.function_call_arguments.done\r\n",
            "data: {\"type\":\"response.function_call_arguments.done\",",
            "\"arguments\":\"{\\\"yield_time_ms\\\":4000.0}\"}\r\n\r\n"
        );
        let out = rewrite_record(record.as_bytes());
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(text.contains(r#"\"yield_time_ms\":4000}"#), "{text}");
        assert!(
            text.ends_with("\r\n\r\n"),
            "record 终止符必须保留: {text:?}"
        );
        // 改写后的字节仍能被同一套切分器切成一条完整 record。
        assert_eq!(
            crate::protocol::codec::sse::record_end(&out),
            Some(out.len())
        );
    }

    #[test]
    fn completed_event_output_is_rewritten() {
        let record = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[",
            "{\"type\":\"function_call\",\"name\":\"run\",\"arguments\":\"{\\\"yield_time_ms\\\":3000.0}\"}",
            "]}}\n\n"
        );
        let out = String::from_utf8(rewrite_record(record.as_bytes())).unwrap();
        assert!(out.contains(r#"\"yield_time_ms\":3000}"#), "{out}");
    }

    #[test]
    fn non_stream_body_is_rewritten() {
        let mut body = json!({
            "output": [
                {"type": "reasoning", "summary": []},
                {"type": "function_call", "name": "run_task", "arguments": "{\"yield_time_ms\":1000.0}"}
            ]
        });
        rewrite_response_body(&mut body);
        assert_eq!(
            body["output"][1]["arguments"],
            Value::String("{\"yield_time_ms\":1000}".to_owned())
        );
    }

    #[test]
    fn only_grok_provider_needs_normalization() {
        assert!(needs_normalization(Some("grok")));
        assert!(!needs_normalization(Some("codex")));
        assert!(!needs_normalization(None));
    }
}
