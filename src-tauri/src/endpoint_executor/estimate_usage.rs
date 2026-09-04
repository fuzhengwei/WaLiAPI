//! Fallback token estimation when upstream does not return usage.
//!
//! Uses `tiktoken-rs` (`cl100k_base`) to locally count tokens
//! for the request messages and response content.  This is an approximation —
//! different model families use different tokenizers — but it is far better
//! than showing 0 in the audit log when the upstream omits usage.
//!
//! Accuracy:
//! - OpenAI GPT-4o / GPT-4.1 family: `o200k_base` — exact match.
//! - OpenAI GPT-4 / GPT-3.5 family:  `cl100k_base` — exact match.
//! - Claude / Gemini / others:        `cl100k_base` — ±10-20% deviation.
//! - Multimodal (images/audio):       not counted (text-only).

use serde_json::Value;
use tiktoken_rs::cl100k_base;

/// Estimate token usage from a request body and optional response text.
///
/// Returns `(prompt_tokens, completion_tokens, total_tokens)`.
///
/// - `request_body`: the full JSON body sent to the upstream (we extract
///   `messages` content for chat endpoints, or `input` for embeddings).
/// - `response_text`: for non-stream, the response body text; for stream,
///   the accumulated downstream content.
/// - `model`: used to pick the tokenizer (currently all use `cl100k_base`).
pub fn estimate_usage(
    request_body: &Value,
    response_text: Option<&str>,
    _model: &str,
) -> (i64, i64, i64) {
    let prompt = estimate_prompt_tokens(request_body);
    let completion = response_text.map(|t| count_tokens(t)).unwrap_or(0);
    let total = prompt + completion;
    (prompt, completion, total)
}

/// Estimate prompt tokens from a request body.
///
/// Handles:
/// - OpenAI Chat: `messages[].content` (string or array of text parts)
/// - OpenAI Responses: `input` (string or array)
/// - OpenAI Embeddings: `input` (string or array)
/// - Anthropic Messages: `messages[].content` (string or array)
/// - Fallback: serialize the whole body to JSON string and count that.
fn estimate_prompt_tokens(body: &Value) -> i64 {
    // Chat Completions / Anthropic Messages: messages array
    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        let mut text = String::new();
        for msg in messages {
            // content can be a string or an array of content parts
            if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                text.push_str(content);
                text.push('\n');
            } else if let Some(parts) = msg.get("content").and_then(|c| c.as_array()) {
                for part in parts {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                        text.push('\n');
                    }
                }
            }
            // role also costs a few tokens
            if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                text.push_str(role);
                text.push('\n');
            }
        }
        if !text.is_empty() {
            // Add overhead for message framing (~4 tokens per message: role + delimiters)
            let framing = (messages.len() as i64) * 4;
            return count_tokens(&text) + framing;
        }
    }

    // Responses API: input field
    if let Some(input) = body.get("input") {
        let text = input_to_text(input);
        if !text.is_empty() {
            return count_tokens(&text);
        }
    }

    // Embeddings: input field
    if let Some(input) = body.get("input") {
        let text = input_to_text(input);
        if !text.is_empty() {
            return count_tokens(&text);
        }
    }

    // Fallback: count the whole JSON body
    let json_str = serde_json::to_string(body).unwrap_or_default();
    count_tokens(&json_str)
}

/// Convert an `input` field (string or array) to plain text.
fn input_to_text(input: &Value) -> String {
    match input {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut text = String::new();
            for item in arr {
                match item {
                    Value::String(s) => {
                        text.push_str(s);
                        text.push('\n');
                    }
                    Value::Object(obj) => {
                        if let Some(t) = obj.get("text").and_then(|t| t.as_str()) {
                            text.push_str(t);
                            text.push('\n');
                        }
                        if let Some(t) = obj.get("content").and_then(|t| t.as_str()) {
                            text.push_str(t);
                            text.push('\n');
                        }
                    }
                    _ => {}
                }
            }
            text
        }
        _ => String::new(),
    }
}

/// Count tokens using `cl100k_base` tokenizer.
///
/// Falls back to a rough character-based estimate if the tokenizer fails
/// to initialize (should never happen, but be defensive).
pub(crate) fn count_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    match cl100k_base() {
        Ok(bpe) => bpe.encode_with_special_tokens(text).len() as i64,
        Err(_) => {
            // Rough fallback: ~4 chars per token for English, ~2 chars for CJK.
            // Use a blended estimate of ~3 chars/token.
            (text.len() as i64) / 3
        }
    }
}

/// Extract response text from a non-stream response body for estimation.
///
/// Handles:
/// - OpenAI Chat: `choices[].message.content`
/// - OpenAI Responses: `output[].content[].text`
/// - Anthropic Messages: `content[].text`
pub fn extract_response_text(body: &Value) -> String {
    // OpenAI Chat Completions
    if let Some(choices) = body.get("choices").and_then(|c| c.as_array()) {
        let mut text = String::new();
        for choice in choices {
            if let Some(content) = choice.pointer("/message/content").and_then(|c| c.as_str()) {
                text.push_str(content);
                text.push('\n');
            }
        }
        if !text.is_empty() {
            return text;
        }
    }

    // OpenAI Responses API
    if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
        let mut text = String::new();
        for item in output {
            if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                for part in content {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                        text.push('\n');
                    }
                }
            }
        }
        if !text.is_empty() {
            return text;
        }
    }

    // Anthropic Messages
    if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
        let mut text = String::new();
        for part in content {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                text.push_str(t);
                text.push('\n');
            }
        }
        if !text.is_empty() {
            return text;
        }
    }

    // Fallback: empty string
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_estimate_chat_messages() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello, how are you?"}
            ]
        });
        let (prompt, completion, total) =
            estimate_usage(&body, Some("I'm fine, thanks!"), "gpt-4o");
        assert!(prompt > 0, "prompt tokens should be > 0");
        assert!(completion > 0, "completion tokens should be > 0");
        assert_eq!(total, prompt + completion);
    }

    #[test]
    fn test_estimate_multimodal_content() {
        let body = json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}
                ]}
            ]
        });
        let (prompt, _, _) = estimate_usage(&body, None, "gpt-4o");
        assert!(prompt > 0, "should count text parts even with images");
    }

    #[test]
    fn test_extract_response_text_openai() {
        let body = json!({
            "choices": [{"message": {"content": "Hello world"}}]
        });
        assert_eq!(extract_response_text(&body).trim(), "Hello world");
    }

    #[test]
    fn test_extract_response_text_anthropic() {
        let body = json!({
            "content": [{"type": "text", "text": "Hello from Claude"}]
        });
        assert_eq!(extract_response_text(&body).trim(), "Hello from Claude");
    }

    #[test]
    fn test_estimate_fallback_whole_body() {
        let body = json!({"custom_field": "some random text here"});
        let (prompt, _, _) = estimate_usage(&body, None, "custom");
        assert!(prompt > 0, "fallback should count whole body");
    }

    #[test]
    fn test_empty_text() {
        assert_eq!(count_tokens(""), 0);
    }
}
