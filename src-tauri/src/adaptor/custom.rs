use async_trait::async_trait;
use super::*;
use super::openai::apply_model_mapping;

pub struct CustomAdaptor;

#[async_trait]
impl Adaptor for CustomAdaptor {
    fn channel_type(&self) -> &'static str { "custom" }
    fn default_models(&self) -> Vec<&'static str> { vec![] }
    fn default_base_url(&self) -> &str { "" }

    async fn test(&self, config: &ChannelConfig) -> Result<TestResult, anyhow::Error> {
        let start = std::time::Instant::now();
        let url = format!("{}/models", config.base_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        match client.get(&url).header("Authorization", format!("Bearer {}", config.api_key)).timeout(std::time::Duration::from_secs(10)).send().await {
            Ok(r) => {
                let latency = start.elapsed().as_millis() as u64;
                if r.status().is_success() {
                    Ok(TestResult { success: true, message: "连接成功".to_string(), latency_ms: latency })
                } else {
                    Ok(TestResult { success: false, message: format!("HTTP {}", r.status()), latency_ms: latency })
                }
            }
            Err(e) => Ok(TestResult { success: false, message: format!("连接失败: {}", e), latency_ms: start.elapsed().as_millis() as u64 }),
        }
    }

    async fn forward(&self, request: &ProxyRequest, config: &ChannelConfig) -> Result<(u16, serde_json::Value, Option<TokenUsage>), anyhow::Error> {
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let body = apply_model_mapping(&request.body, &config.model_mapping);
        let client = reqwest::Client::new();
        let resp = client.post(&url).header("Authorization", format!("Bearer {}", config.api_key)).header("Content-Type", "application/json").json(&body).send().await?;
        let status = resp.status().as_u16();
        let json: serde_json::Value = resp.json().await?;
        let usage = json.get("usage").and_then(|u| Some(TokenUsage {
            prompt_tokens: u.get("prompt_tokens")?.as_u64()?,
            completion_tokens: u.get("completion_tokens")?.as_u64()?,
            total_tokens: u.get("total_tokens")?.as_u64()?,
        }));
        Ok((status, json, usage))
    }

    async fn forward_stream(&self, request: &ProxyRequest, config: &ChannelConfig) -> Result<reqwest::Response, anyhow::Error> {
        let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let body = apply_model_mapping(&request.body, &config.model_mapping);
        let client = reqwest::Client::new();
        let resp = client.post(&url).header("Authorization", format!("Bearer {}", config.api_key)).header("Content-Type", "application/json").json(&body).send().await?;
        Ok(resp)
    }
}
