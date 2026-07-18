pub mod openai;
pub mod claude;
pub mod gemini;
pub mod deepseek;
pub mod custom;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub model_mapping: serde_json::Value,
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRequest {
    pub model: String,
    pub body: serde_json::Value,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub success: bool,
    pub message: String,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[async_trait]
pub trait Adaptor: Send + Sync {
    fn channel_type(&self) -> &'static str;
    fn default_models(&self) -> Vec<&'static str>;
    fn default_base_url(&self) -> &str;

    async fn test(&self, config: &ChannelConfig) -> Result<TestResult, anyhow::Error>;

    async fn forward(
        &self,
        request: &ProxyRequest,
        config: &ChannelConfig,
    ) -> Result<(u16, serde_json::Value, Option<TokenUsage>), anyhow::Error>;

    async fn forward_stream(
        &self,
        request: &ProxyRequest,
        config: &ChannelConfig,
    ) -> Result<reqwest::Response, anyhow::Error>;
}

pub fn get_adaptor(channel_type: &str) -> Box<dyn Adaptor> {
    match channel_type {
        "openai" => Box::new(openai::OpenAIAdaptor),
        "deepseek" => Box::new(deepseek::DeepSeekAdaptor),
        "claude" => Box::new(claude::ClaudeAdaptor),
        "gemini" => Box::new(gemini::GeminiAdaptor),
        "custom" => Box::new(custom::CustomAdaptor),
        _ => Box::new(custom::CustomAdaptor),
    }
}

pub fn channel_types() -> Vec<ChannelTypeInfo> {
    vec![
        ChannelTypeInfo { value: "openai", label: "OpenAI", category: "international", default_base_url: "https://api.openai.com/v1", models: vec!["gpt-5.4", "gpt-5.5", "gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo"] },
        ChannelTypeInfo { value: "deepseek", label: "DeepSeek", category: "international", default_base_url: "https://api.deepseek.com/v1", models: vec!["deepseek-chat", "deepseek-coder", "deepseek-reasoner"] },
        ChannelTypeInfo { value: "claude", label: "Anthropic Claude", category: "international", default_base_url: "https://api.anthropic.com", models: vec!["claude-sonnet-4-20250514", "claude-3-7-sonnet-20250219", "claude-3-5-haiku-20241022"] },
        ChannelTypeInfo { value: "gemini", label: "Google Gemini", category: "international", default_base_url: "https://generativelanguage.googleapis.com", models: vec!["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.0-flash"] },
        ChannelTypeInfo { value: "qwen", label: "通义千问", category: "domestic", default_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", models: vec!["qwen-max", "qwen-plus", "qwen-turbo"] },
        ChannelTypeInfo { value: "zhipu", label: "智谱 GLM", category: "domestic", default_base_url: "https://open.bigmodel.cn/api/paas/v4", models: vec!["glm-4-plus", "glm-4-flash", "glm-4-air"] },
        ChannelTypeInfo { value: "moonshot", label: "Moonshot AI", category: "domestic", default_base_url: "https://api.moonshot.cn/v1", models: vec!["moonshot-v1-8k", "moonshot-v1-32k", "moonshot-v1-128k"] },
        ChannelTypeInfo { value: "doubao", label: "字节豆包", category: "domestic", default_base_url: "https://ark.cn-beijing.volces.com/api/v3", models: vec!["doubao-pro-32k", "doubao-pro-128k", "doubao-lite-32k"] },
        ChannelTypeInfo { value: "ollama", label: "Ollama (本地)", category: "local", default_base_url: "http://localhost:11434/v1", models: vec!["llama3.1", "qwen2.5", "mistral"] },
        ChannelTypeInfo { value: "custom", label: "自定义", category: "custom", default_base_url: "", models: vec![] },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelTypeInfo {
    pub value: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub default_base_url: &'static str,
    pub models: Vec<&'static str>,
}
