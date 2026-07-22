use async_trait::async_trait;
use super::*;

pub struct OpenAIAdaptor;

#[async_trait]
impl Adaptor for OpenAIAdaptor {
    fn channel_type(&self) -> &'static str { "openai" }
    fn default_models(&self) -> Vec<&'static str> { vec!["gpt-5.4", "gpt-5.5", "gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo"] }
    fn default_base_url(&self) -> &str { "https://api.openai.com/v1" }

    async fn test(&self, config: &ChannelConfig) -> Result<TestResult, anyhow::Error> {
        let start = std::time::Instant::now();
        let url = format!("{}/models", config.base_url.trim_end_matches('/'));
        
        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;

        match resp {
            Ok(r) => {
                let latency = start.elapsed().as_millis() as u64;
                if r.status().is_success() {
                    Ok(TestResult { success: true, message: "连接成功".to_string(), latency_ms: latency })
                } else {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    Ok(TestResult { success: false, message: format!("HTTP {}: {}", status, body.chars().take(200).collect::<String>()), latency_ms: latency })
                }
            }
            Err(e) => {
                let latency = start.elapsed().as_millis() as u64;
                Ok(TestResult { success: false, message: format!("连接失败: {}", e), latency_ms: latency })
            }
        }
    }

    async fn forward(
        &self,
        request: &ProxyRequest,
        config: &ChannelConfig,
    ) -> Result<(u16, serde_json::Value, Option<TokenUsage>), anyhow::Error> {
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let body = apply_model_mapping(&request.body, &config.model_mapping);

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status().as_u16();
        let json: serde_json::Value = resp.json().await?;

        let usage = json.get("usage").and_then(|u| {
            Some(TokenUsage {
                prompt_tokens: u.get("prompt_tokens")?.as_u64()?,
                completion_tokens: u.get("completion_tokens")?.as_u64()?,
                total_tokens: u.get("total_tokens")?.as_u64()?,
            })
        });

        Ok((status, json, usage))
    }

    async fn forward_stream(
        &self,
        request: &ProxyRequest,
        config: &ChannelConfig,
    ) -> Result<reqwest::Response, anyhow::Error> {
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let body = apply_model_mapping(&request.body, &config.model_mapping);

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        Ok(resp)
    }
}

pub fn apply_model_mapping(body: &serde_json::Value, mapping: &serde_json::Value) -> serde_json::Value {
    if mapping.is_null() || !mapping.is_object() {
        return sanitize_messages(body.clone());
    }
    let mut body = body.clone();
    if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
        if let Some(mapped) = mapping.get(model).and_then(|m| m.as_str()) {
            body["model"] = serde_json::Value::String(mapped.to_string());
        }
    }
    sanitize_messages(body)
}

/// Remove or fix assistant messages with empty content.
///
/// Some upstream APIs (e.g. DeepSeek, OpenAI) reject requests when an
/// assistant message in the `messages` array has empty content. This can
/// happen when a downstream client (e.g. OpenClaw, ChatBox) accumulates
/// long conversation history that includes interrupted or tool-only
/// assistant turns.
///
/// Rules:
/// - If an assistant message has `content` that is null, empty string, or
///   an empty array, and has no `tool_calls`, it is removed entirely.
/// - If an assistant message has empty `content` but has `tool_calls`, the
///   content is set to `null` (which most APIs accept) instead of empty string.
/// - Consecutive messages with the same role are not merged (leave that to
///   the upstream API or a future enhancement).
pub fn sanitize_messages(mut body: serde_json::Value) -> serde_json::Value {
    let messages = match body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(arr) => arr,
        None => return body,
    };

    // First pass: normalize content for assistant messages with tool_calls but empty content
    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }

        let is_empty_content = match msg.get("content") {
            None => true,
            Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.is_empty(),
            Some(serde_json::Value::Array(a)) => a.is_empty(),
            _ => false,
        };

        let has_tool_calls = msg
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        if is_empty_content && has_tool_calls {
            if let Some(obj) = msg.as_object_mut() {
                obj.insert("content".to_string(), serde_json::Value::Null);
            }
        }
    }

    // Second pass: remove assistant messages with empty content and no tool_calls
    messages.retain(|msg| {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "assistant" {
            return true;
        }

        let is_empty_content = match msg.get("content") {
            None => true,
            Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.is_empty(),
            Some(serde_json::Value::Array(a)) => a.is_empty(),
            _ => false,
        };

        let has_tool_calls = msg
            .get("tool_calls")
            .and_then(|t| t.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        !(is_empty_content && !has_tool_calls)
    });

    body
}
