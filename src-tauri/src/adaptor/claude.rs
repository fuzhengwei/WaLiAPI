use async_trait::async_trait;
use super::*;

pub struct ClaudeAdaptor;

#[async_trait]
impl Adaptor for ClaudeAdaptor {
    fn channel_type(&self) -> &'static str { "claude" }
    fn default_models(&self) -> Vec<&'static str> { vec!["claude-sonnet-4-20250514", "claude-3-7-sonnet-20250219", "claude-3-5-haiku-20241022"] }
    fn default_base_url(&self) -> &str { "https://api.anthropic.com/v1" }

    async fn test(&self, config: &ChannelConfig) -> Result<TestResult, anyhow::Error> {
        let start = std::time::Instant::now();
        let url = format!("{}/messages", config.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": config.models.first().map(|s| s.as_str()).unwrap_or("claude-3-5-haiku-20241022"),
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let client = reqwest::Client::new();
        match client.post(&url)
            .header("x-api-key", &config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send().await {
            Ok(r) => {
                let latency = start.elapsed().as_millis() as u64;
                let status = r.status();
                if status.is_success() {
                    Ok(TestResult { success: true, message: "连接成功".to_string(), latency_ms: latency })
                } else {
                    let body = r.text().await.unwrap_or_default();
                    let err_msg = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()).map(String::from))
                        .unwrap_or(body);
                    Ok(TestResult { success: false, message: format!("HTTP {} {}", status.as_u16(), err_msg), latency_ms: latency })
                }
            }
            Err(e) => Ok(TestResult { success: false, message: format!("连接失败: {}", e), latency_ms: start.elapsed().as_millis() as u64 }),
        }
    }

    async fn forward(&self, request: &ProxyRequest, config: &ChannelConfig) -> Result<(u16, serde_json::Value, Option<TokenUsage>), anyhow::Error> {
        let url = format!("{}/messages", config.base_url.trim_end_matches('/'));
        let openai_body = &request.body;
        
        // Convert OpenAI format to Claude format
        let model = openai_body.get("model").and_then(|m| m.as_str()).unwrap_or("claude-3-5-haiku-20241022");
        let messages = openai_body.get("messages").cloned().unwrap_or(serde_json::Value::Array(vec![]));
        let max_tokens = openai_body.get("max_tokens").and_then(|m| m.as_u64()).unwrap_or(4096);
        let temperature = openai_body.get("temperature").cloned();
        let stream = openai_body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
        
        // Extract system message if present
        let (system, claude_messages) = convert_openai_messages_to_claude(&messages);
        
        let mut claude_body = serde_json::json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": claude_messages,
            "stream": stream,
        });
        if let Some(sys) = system {
            claude_body["system"] = serde_json::Value::String(sys);
        }
        if let Some(temp) = temperature {
            claude_body["temperature"] = temp;
        }

        let client = reqwest::Client::new();
        let resp = client.post(&url)
            .header("x-api-key", &config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&claude_body)
            .send().await?;

        let status = resp.status().as_u16();
        let claude_json: serde_json::Value = resp.json().await?;

        // Convert Claude response to OpenAI format
        let openai_response = convert_claude_to_openai(&claude_json, model);
        let usage = openai_response.get("usage").and_then(|u| Some(TokenUsage {
            prompt_tokens: u.get("prompt_tokens")?.as_u64()?,
            completion_tokens: u.get("completion_tokens")?.as_u64()?,
            total_tokens: u.get("total_tokens")?.as_u64()?,
        }));

        Ok((status, openai_response, usage))
    }

    async fn forward_stream(&self, request: &ProxyRequest, config: &ChannelConfig) -> Result<reqwest::Response, anyhow::Error> {
        let url = format!("{}/messages", config.base_url.trim_end_matches('/'));
        let openai_body = &request.body;
        let model = openai_body.get("model").and_then(|m| m.as_str()).unwrap_or("claude-3-5-haiku-20241022");
        let messages = openai_body.get("messages").cloned().unwrap_or(serde_json::Value::Array(vec![]));
        let max_tokens = openai_body.get("max_tokens").and_then(|m| m.as_u64()).unwrap_or(4096);
        let (system, claude_messages) = convert_openai_messages_to_claude(&messages);
        
        let mut claude_body = serde_json::json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": claude_messages,
            "stream": true,
        });
        if let Some(sys) = system {
            claude_body["system"] = serde_json::Value::String(sys);
        }

        let client = reqwest::Client::new();
        let resp = client.post(&url)
            .header("x-api-key", &config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&claude_body)
            .send().await?;

        Ok(resp)
    }
}

fn convert_openai_messages_to_claude(messages: &serde_json::Value) -> (Option<String>, serde_json::Value) {
    let msgs = match messages.as_array() {
        Some(arr) => arr,
        None => return (None, serde_json::Value::Array(vec![])),
    };

    let mut system = None;
    let mut claude_msgs = Vec::new();

    for msg in msgs {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let content = msg.get("content").cloned().unwrap_or(serde_json::Value::String(String::new()));
        
        if role == "system" {
            if let Some(s) = content.as_str() {
                system = Some(s.to_string());
            }
        } else {
            claude_msgs.push(serde_json::json!({
                "role": if role == "assistant" { "assistant" } else { "user" },
                "content": content,
            }));
        }
    }

    (system, serde_json::Value::Array(claude_msgs))
}

fn convert_claude_to_openai(claude_json: &serde_json::Value, model: &str) -> serde_json::Value {
    let content = claude_json.get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    let prompt_tokens = claude_json.get("usage").and_then(|u| u.get("input_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);
    let completion_tokens = claude_json.get("usage").and_then(|u| u.get("output_tokens")).and_then(|t| t.as_u64()).unwrap_or(0);

    serde_json::json!({
        "id": claude_json.get("id").cloned().unwrap_or(serde_json::Value::String("chatcmpl-converted".to_string())),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content,
            },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    })
}
