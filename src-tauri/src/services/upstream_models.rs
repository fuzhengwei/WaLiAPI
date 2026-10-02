//! 上游模型列表同步（T14）。
//!
//! 渠道编辑页「同步上游模型」的后端：按渠道协议拉取上游模型 ID 列表。
//! 协议分派沿用渠道身份解析（`build_draft_identity`）的判定，鉴权 header
//! 复用 `endpoint_executor::auth_scheme_for` / `auth_headers`，与真实请求一致。
//!
//! | 协议 | 接口 | 解析 | 鉴权 |
//! |---|---|---|---|
//! | openai | `GET {base}/models` | `data[].id` | Bearer |
//! | anthropic | `GET {base}/models`（Base 自带 /v1） | `data[].id`（兼容网关也可能返回 OpenAI 格式） | x-api-key + anthropic-version |
//! | ollama | `GET {base}/api/tags` | `models[].name` | 可选 Bearer |

use crate::core::channel_identity::ChannelIdentity;
use crate::endpoint_executor::{auth_headers, auth_scheme_for, final_url};
use crate::services::channel_test::{build_draft_identity, DraftChannelTestInput};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 同步结果：上游模型 ID 列表 + 判定出的协议（供前端打标/展示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamModelsResult {
    /// 上游返回的模型 ID 列表（openai `data[].id` / ollama `models[].name`）。
    pub models: Vec<String>,
    /// 判定出的上游协议：`openai` / `anthropic` / `ollama`。
    pub protocol: String,
    /// 拉取时使用的根 URL（便于前端展示/排障）。
    pub base_url: String,
}

/// 拉取上游模型列表。
///
/// `api_key` 必须是已解析的有效 Key（调用方复用 `resolve_draft_api_key` 的草稿语义）。
/// 失败返回可读错误，绝不覆盖调用方的已有模型列表。
pub async fn fetch_upstream_models(
    input: &DraftChannelTestInput,
    api_key: &str,
    timeout_secs: u64,
) -> Result<UpstreamModelsResult, String> {
    let identity = build_draft_identity(
        &input.protocol,
        &input.provider,
        &input.native_base_url,
        &input.native_endpoints,
        &input.channel_type,
        &input.base_url,
        input
            .config
            .as_ref()
            .unwrap_or(&Value::Object(Default::default())),
        &input.legacy_executor_override,
    );
    let base_url = normalize_base(&identity);
    if base_url.trim().is_empty() {
        return Err("Base URL 不能为空，无法同步模型".to_string());
    }

    // 协议分派：openai/anthropic 都用 OpenAI 兼容 `/models`（`data[].id`），
    // ollama 用 `/api/tags`（`models[].name`）。
    let (path, parse): (&str, fn(&Value) -> Vec<String>) = match identity.protocol.as_str() {
        // main 分支约定：anthropic Base 自带 /v1，端点只补 /models。
        "anthropic" => ("/models", parse_openai_list),
        "ollama" => ("api/tags", parse_ollama_tags),
        _ => ("models", parse_openai_list),
    };
    let url = ensure_scheme(&final_url(&base_url, path, None));

    // 鉴权方案与真实请求一致（anthropic → x-api-key，ollama → 可选 Bearer，其余 Bearer）。
    let scheme = auth_scheme_for(&identity);
    let mut headers = auth_headers(scheme, api_key);
    if identity.protocol == "anthropic" {
        headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
    }

    // 出站代理：草稿渠道的 config.proxy（global/direct/custom）与全局设置一致解析。
    // 未填写 proxy 时也必须回退到全局代理；否则模型同步会意外直连，
    // 在本机只能通过代理访问上游时会让所有同步请求失败。
    let proxy_url = crate::adaptor::ProxySetting::from_extra(
        input.config.as_ref().unwrap_or(&serde_json::Value::Null),
    )
    .map(|p| p.resolve())
    .unwrap_or_else(crate::adaptor::global_proxy_url);
    let client = crate::adaptor::with_proxy(
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(timeout_secs.max(1))),
        proxy_url.as_deref(),
    )
    .build()
    .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))?;

    let mut req = client.get(&url).header("content-type", "application/json");
    for (k, v) in &headers {
        req = req.header(k, v);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("连接上游失败（{url}）：{e}（不会覆盖已有模型列表）"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "同步模型失败：HTTP {status}（{url}）— {}（不会覆盖已有模型列表）",
            concise_error(&body)
        ));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("上游返回无法解析的 JSON（{url}）：{e}（不会覆盖已有模型列表）"))?;

    let models = parse(&body);
    if models.is_empty() {
        return Err("上游返回的模型列表为空，未同步任何模型".to_string());
    }

    Ok(UpstreamModelsResult {
        models,
        protocol: identity.protocol,
        base_url,
    })
}

/// 解析后的 Native 根：直接使用身份解析出的规范根（不含尾部斜杠）。
/// Anthropic 的 Base 自带 `/v1`，`/models` 由 path 拼接，因此最终为 `{base}/models`。
fn normalize_base(identity: &ChannelIdentity) -> String {
    identity.native_base_url.trim_end_matches('/').to_string()
}

/// OpenAI 兼容 `/models`：`data[].id`。
fn parse_openai_list(body: &Value) -> Vec<String> {
    body.get("data")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Ollama `/api/tags`：`models[].name`。
fn parse_ollama_tags(body: &Value) -> Vec<String> {
    body.get("models")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// reqwest 要求 URL 带 scheme；用户在表单里通常只填 host（如 `127.0.0.1:11434`），
/// 缺省补 `http://`（本地 Ollama 最常见，也是默认行为）。
fn ensure_scheme(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

/// 从错误响应体提取一段可读信息（避免把原始 body 直接透给前端）。
fn concise_error(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            return msg.chars().take(300).collect();
        }
        if let Some(msg) = v.get("message").and_then(Value::as_str) {
            return msg.chars().take(300).collect();
        }
    }
    let s = body.trim();
    s.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn draft(protocol: &str, base_url: &str) -> DraftChannelTestInput {
        DraftChannelTestInput {
            model_mapping_disabled: None,
            // v0.3.3 为该结构体新增了自定义请求头字段，测试构造点需同步补齐。
            request_headers: None,
            id: None,
            name: "t".to_string(),
            channel_type: match protocol {
                "anthropic" => "claude".to_string(),
                "ollama" => "ollama".to_string(),
                _ => "openai".to_string(),
            },
            base_url: base_url.to_string(),
            api_key: "k".to_string(),
            clear_api_key: None,
            models: vec![],
            test_models: None,
            priority: None,
            weight: None,
            config: Some(json!({})),
            model_mapping: None,
            timeout_secs: Some(60),
            protocol: Some(protocol.to_string()),
            provider: Some(match protocol {
                "anthropic" => "anthropic".to_string(),
                "ollama" => "ollama".to_string(),
                _ => "custom".to_string(),
            }),
            native_base_url: Some(base_url.to_string()),
            native_endpoints: Some(match protocol {
                "anthropic" => vec!["messages".to_string()],
                "ollama" => vec!["api_chat".to_string()],
                _ => vec!["chat_completions".to_string()],
            }),
            preset_revision: None,
            legacy_executor_override: None,
        }
    }

    struct Mock {
        addr: String,
        received: std::sync::Arc<tokio::sync::Mutex<Vec<(String, String, String)>>>,
    }

    /// 最小 HTTP mock：记录 (request line, authorization, x-api-key)，返回固定 JSON body。
    async fn start_mock(body: Value) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let received = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let recv = received.clone();
        let body = serde_json::to_vec(&body).unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let req = String::from_utf8_lossy(&buf).to_string();
                let first_line = req.lines().next().unwrap_or("").to_string();
                let auth = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization"))
                    .unwrap_or("")
                    .to_string();
                let xapi = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("x-api-key"))
                    .unwrap_or("")
                    .to_string();
                recv.lock().await.push((first_line, auth, xapi));
                let _ = write_all_response(&mut socket, &body).await;
            }
        });
        Mock { addr, received }
    }

    async fn write_all_response(
        socket: &mut tokio::net::TcpStream,
        body: &[u8],
    ) -> std::io::Result<()> {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(body).await?;
        socket.flush().await
    }

    #[tokio::test]
    async fn openai_parses_data_ids_and_bearer() {
        let m =
            start_mock(json!({"data": [{"id": "oc/gpt-5"}, {"id": "oc/deepseek-v4-flash"}]})).await;
        let input = draft("openai", &m.addr);
        let r = fetch_upstream_models(&input, "secret", 5).await.unwrap();
        assert_eq!(r.models, vec!["oc/gpt-5", "oc/deepseek-v4-flash"]);
        assert_eq!(r.protocol, "openai");
        let reqs = m.received.lock().await.clone();
        assert!(reqs[0].0.contains("GET /models"));
        assert!(reqs[0].1.contains("Bearer secret"));
    }

    #[tokio::test]
    async fn anthropic_uses_models_path_and_x_api_key() {
        // main 分支约定：Anthropic Base 自带 /v1，path 模板是 /models；
        // 这里 mock base 是根，因此请求应为 GET /models（不再是 /v1/models）。
        let m = start_mock(json!({"data": [{"id": "claude-sonnet-5"}]})).await;
        let input = draft("anthropic", &m.addr);
        let r = fetch_upstream_models(&input, "k2", 5).await.unwrap();
        assert_eq!(r.models, vec!["claude-sonnet-5"]);
        assert_eq!(r.protocol, "anthropic");
        let reqs = m.received.lock().await.clone();
        assert!(reqs[0].0.contains("GET /models"));
        assert!(reqs[0].2.contains("k2"), "should send x-api-key header");
    }

    #[tokio::test]
    async fn ollama_parses_tags_names() {
        let m =
            start_mock(json!({"models": [{"name": "qwen3:32b"}, {"name": "llama3.3:70b"}]})).await;
        let input = draft("ollama", &m.addr);
        let r = fetch_upstream_models(&input, "", 5).await.unwrap();
        assert_eq!(r.models, vec!["qwen3:32b", "llama3.3:70b"]);
        assert_eq!(r.protocol, "ollama");
    }

    #[tokio::test]
    async fn empty_base_url_rejected() {
        let input = draft("openai", "");
        let err = fetch_upstream_models(&input, "k", 5).await.unwrap_err();
        assert!(err.contains("Base URL"));
    }

    #[tokio::test]
    async fn non_2xx_returns_readable_error() {
        // 未启动服务的端口 → 连接失败路径
        let input = draft("openai", "http://127.0.0.1:1");
        let err = fetch_upstream_models(&input, "k", 2).await.unwrap_err();
        assert!(err.contains("不会覆盖已有模型列表"));
    }

    #[tokio::test]
    async fn empty_model_list_is_error() {
        let m = start_mock(json!({"data": []})).await;
        let input = draft("openai", &m.addr);
        let err = fetch_upstream_models(&input, "k", 5).await.unwrap_err();
        assert!(err.contains("为空"));
    }
}
