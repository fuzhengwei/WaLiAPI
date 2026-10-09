use crate::db::models::{ApiKey, ApiKeyStats, CreateApiKeyInput};
use crate::db::repository::Repository;
use crate::AppState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiKeyDto {
    pub id: String,
    pub name: String,
    pub key: String,
    pub status: i64,
    pub allowed_models: Vec<String>,
    pub allowed_channels: Vec<String>,
    pub denied_models: Vec<String>,
    pub denied_channels: Vec<String>,
    pub quota_limit: i64,
    pub quota_used: i64,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<ApiKey> for ApiKeyDto {
    fn from(k: ApiKey) -> Self {
        ApiKeyDto {
            id: k.id,
            name: k.name,
            // FIX-13：列表/详情只回掩码，全量经 get_api_key_full 按需获取。
            key: crate::utils::secret::mask_secret(&k.key),
            status: k.status,
            allowed_models: serde_json::from_str(&k.allowed_models).unwrap_or_default(),
            allowed_channels: serde_json::from_str(&k.allowed_channels).unwrap_or_default(),
            denied_models: serde_json::from_str(&k.denied_models).unwrap_or_default(),
            denied_channels: serde_json::from_str(&k.denied_channels).unwrap_or_default(),
            quota_limit: k.quota_limit,
            quota_used: k.quota_used,
            expires_at: k.expires_at,
            created_at: k.created_at,
            updated_at: k.updated_at,
        }
    }
}

#[tauri::command]
pub async fn get_api_keys(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<ApiKeyDto>, String> {
    get_api_keys_impl(&*state).await
}

pub async fn get_api_keys_impl(state: &std::sync::Arc<AppState>) -> Result<Vec<ApiKeyDto>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_all_api_keys()
        .await
        .map_err(|e| e.to_string())
        .map(|ks| ks.into_iter().map(Into::into).collect())
}

/// FIX-13：按 id 按需取回完整密钥（复制/生成示例代码等显式动作）。
#[tauri::command]
pub async fn get_api_key_full(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
) -> Result<String, String> {
    get_api_key_full_impl(&*state, &id).await
}

pub async fn get_api_key_full_impl(
    state: &std::sync::Arc<AppState>,
    id: &str,
) -> Result<String, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_api_key_by_id(id)
        .await
        .map_err(|e| e.to_string())
        .map(|k| k.key)
}

/// 管理面按 Key ID 查询回答模型，避免为下拉列表把完整密钥发送到前端。
#[tauri::command]
pub async fn get_api_key_answer_models(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
) -> Result<Vec<String>, String> {
    let repo = Repository::new(state.db.pool.clone());
    let key = repo
        .get_api_key_by_id(&id)
        .await
        .map_err(|error| match error {
            sqlx::Error::RowNotFound => "密钥不存在".to_string(),
            _ => "无法读取 API Key".to_string(),
        })?;
    let channels = repo
        .get_enabled_channels_for_mode(
            crate::core::route_plan::EndpointKind::ChatCompletions.as_str(),
            false,
            &crate::utils::time::now_iso(),
        )
        .await
        .map_err(|_| "无法读取回答模型渠道")?;
    // 目录查询保持只读；规划器同样校验账号额度，无需写回额度恢复状态。
    let accounts = repo
        .list_active_auth_accounts()
        .await
        .map_err(|_| "无法读取回答模型账号")?;
    let flags = crate::core::feature_flags::read_feature_flags(&state.settings);
    Ok(crate::server::handlers::collect_api_key_answer_models(
        &key, &channels, &accounts, &flags,
    ))
}

#[tauri::command]
pub async fn create_api_key(
    input: CreateApiKeyInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<ApiKeyDto, String> {
    create_api_key_impl(input, &*state).await
}

pub async fn create_api_key_impl(
    input: CreateApiKeyInput,
    state: &std::sync::Arc<AppState>,
) -> Result<ApiKeyDto, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.create_api_key(&input)
        .await
        .map_err(|e| e.to_string())
        .map(Into::into)
}

#[derive(Debug, Deserialize)]
pub struct UpdateApiKeyInput {
    pub id: String,
    pub name: Option<String>,
    pub quota_limit: Option<i64>,
    pub status: Option<i64>,
    pub allowed_models: Option<Vec<String>>,
    pub allowed_channels: Option<Vec<String>>,
    pub denied_models: Option<Vec<String>>,
    pub denied_channels: Option<Vec<String>>,
}

#[tauri::command]
pub async fn update_api_key(
    input: UpdateApiKeyInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    update_api_key_impl(input, &*state).await
}

pub async fn update_api_key_impl(
    input: UpdateApiKeyInput,
    state: &std::sync::Arc<AppState>,
) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    if let Some(name) = &input.name {
        repo.update_api_key_name(&input.id, name)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(quota_limit) = input.quota_limit {
        repo.update_api_key_quota(&input.id, quota_limit)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(status) = input.status {
        repo.update_api_key_status(&input.id, status)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(models) = &input.allowed_models {
        repo.update_api_key_allowed_models(&input.id, models)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(channels) = &input.allowed_channels {
        repo.update_api_key_allowed_channels(&input.id, channels)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(models) = &input.denied_models {
        repo.update_api_key_denied_models(&input.id, models)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(channels) = &input.denied_channels {
        repo.update_api_key_denied_channels(&input.id, channels)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_api_key(
    id: String,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<(), String> {
    delete_api_key_impl(&id, &*state).await
}

pub async fn delete_api_key_impl(id: &str, state: &std::sync::Arc<AppState>) -> Result<(), String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.delete_api_key(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_api_key_stats(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<Vec<ApiKeyStats>, String> {
    get_api_key_stats_impl(&*state).await
}

pub async fn get_api_key_stats_impl(
    state: &std::sync::Arc<AppState>,
) -> Result<Vec<ApiKeyStats>, String> {
    let repo = Repository::new(state.db.pool.clone());
    repo.get_api_key_stats().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_api_key_knowledge_access(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
) -> Result<Vec<String>, String> {
    crate::server::knowledge_access::get_grants(&state.db.pool, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_api_key_knowledge_access(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
    kb_ids: Vec<String>,
) -> Result<(), String> {
    crate::server::knowledge_access::set_grants(&state.db.pool, &id, &kb_ids)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct KnowledgeConnectionTest {
    rest_status: u16,
    rest_ok: bool,
    mcp_status: u16,
    mcp_ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KnowledgeHealthTest {
    status: u16,
    ok: bool,
    elapsed_ms: u64,
    answer: String,
    sources: Vec<crate::services::knowledge::models::SourceInfo>,
    error: Option<serde_json::Value>,
    diagnostics: Option<crate::services::knowledge::models::RagDiagnostics>,
}

/// 用户主动触发的真实 RAG 检测；通过本机 HTTP 入口保留 Key 权限、额度和审计。
#[tauri::command]
pub async fn test_api_key_knowledge_health(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
    kb_id: String,
    model: String,
    question: String,
    search_mode: String,
) -> Result<KnowledgeHealthTest, String> {
    if !state
        .server_running
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("请先启动 WaLiAPI 服务".into());
    }
    if kb_id.trim().is_empty() || model.trim().is_empty() || question.trim().is_empty() {
        return Err("请选择知识库并填写回答模型和测试问题".into());
    }
    if !matches!(search_mode.as_str(), "hybrid" | "vector" | "keyword") {
        return Err("不支持的检索模式".into());
    }
    let key = Repository::new(state.db.pool.clone())
        .get_api_key_by_id(&id)
        .await
        .map_err(|_| "密钥不存在")?;
    let port = *state.server_port.read().await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法创建健康检测")?;
    let started = std::time::Instant::now();
    let response = client
        .post(format!("http://127.0.0.1:{port}/api/kb/ask"))
        .bearer_auth(&key.key)
        .json(&serde_json::json!({
            "kb_id": kb_id, "model": model.trim(), "question": question.trim(),
            "search_mode": search_mode, "top_k": 5, "diagnostics": true,
        }))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "RAG 检测超时，请检查本机网关日志".to_string()
            } else {
                "无法连接本机 WaLiAPI 服务".to_string()
            }
        })?;
    let status = response.status().as_u16();
    let body = response
        .json::<serde_json::Value>()
        .await
        .map_err(|_| "健康检测响应格式无效")?;
    Ok(knowledge_health_result(
        status,
        body,
        started.elapsed().as_millis() as u64,
        &search_mode,
    ))
}

fn knowledge_health_result(
    status: u16,
    body: serde_json::Value,
    elapsed_ms: u64,
    search_mode: &str,
) -> KnowledgeHealthTest {
    let answer = body["answer"].as_str().unwrap_or_default().to_string();
    let sources: Vec<crate::services::knowledge::models::SourceInfo> =
        serde_json::from_value(body["sources"].clone()).unwrap_or_default();
    let diagnostics: Option<crate::services::knowledge::models::RagDiagnostics> =
        serde_json::from_value(body["diagnostics"].clone()).ok();
    // 旧服务或不完整响应不能仅凭 HTTP 200 冒充完整链路通过。
    let stages_ok = diagnostics.as_ref().is_some_and(|diagnostics| {
        let stages = &diagnostics.stages;
        !diagnostics.request_id.is_empty()
            && !stages.iter().any(|item| item.status == "failed")
            && ["permission", "retrieval", "answer", "validation"]
                .iter()
                .all(|stage| {
                    stages
                        .iter()
                        .any(|item| item.stage == *stage && item.status == "passed")
                })
            && stages.iter().any(|item| {
                item.stage == "embedding"
                    && item.status
                        == if search_mode == "keyword" {
                            "skipped"
                        } else {
                            "passed"
                        }
            })
    });
    let ok = (200..300).contains(&status)
        && !answer.trim().is_empty()
        && sources
            .iter()
            .any(|source| !source.snippet.trim().is_empty())
        && stages_ok
        && body.get("error").is_none_or(serde_json::Value::is_null);
    let error = body.get("error").filter(|value| value.is_object()).map(|value| {
        let mut safe = serde_json::Map::new();
        for field in ["message", "stage", "code", "request_id"] {
            if let Some(text) = value[field].as_str() { safe.insert(field.into(), text.into()); }
        }
        serde_json::Value::Object(safe)
    }).or_else(|| (!ok).then(|| serde_json::json!({"message": "未取得完整阶段记录、有效答案和来源，不能判定 RAG 检测通过"})));
    KnowledgeHealthTest {
        status,
        ok,
        elapsed_ms,
        answer,
        sources,
        error,
        diagnostics,
    }
}

#[cfg(test)]
mod knowledge_health_tests {
    use super::knowledge_health_result;
    use serde_json::json;

    #[test]
    fn health_requires_complete_pipeline_and_valid_evidence() {
        let mut response = json!({
            "answer": "测试答案", "sources": [{"filename":"test.md", "snippet":"检索依据", "score":1.0}],
            "diagnostics": {"request_id":"test-trace", "stages":
                (["permission", "embedding", "retrieval", "answer", "validation"].map(|stage|
                    json!({"stage":stage,"status":"passed","elapsed_ms":1})))}
        });
        assert!(knowledge_health_result(200, response.clone(), 5, "hybrid").ok);
        response["diagnostics"]["stages"][1]["status"] = json!("skipped");
        assert!(!knowledge_health_result(200, response.clone(), 5, "hybrid").ok);
        assert!(knowledge_health_result(200, response.clone(), 5, "keyword").ok);
        response["answer"] = json!("  ");
        assert!(!knowledge_health_result(200, response.clone(), 5, "keyword").ok);
        response["answer"] = json!("测试答案");
        response["sources"] = json!([]);
        assert!(!knowledge_health_result(200, response, 5, "keyword").ok);
        assert!(!knowledge_health_result(200, json!({"answer":"旧服务答案"}), 5, "hybrid").ok);
    }

    #[test]
    fn health_keeps_safe_error_fields_and_never_passes_failure_status() {
        let result = knowledge_health_result(
            501,
            json!({"error": {
                "message":"请配置 Embeddings", "stage":"embedding", "code":"endpoint_not_configured",
                "request_id":"test-trace", "upstream_body":"must not be forwarded"
            }}),
            5,
            "hybrid",
        );
        assert!(!result.ok);
        let error = result.error.unwrap();
        assert_eq!(error["code"], "endpoint_not_configured");
        assert!(error.get("upstream_body").is_none());
    }
}

/// 经实际监听端口验证授权，不调用模型，也不回传密钥。
#[tauri::command]
pub async fn test_api_key_knowledge_access(
    state: tauri::State<'_, std::sync::Arc<AppState>>,
    id: String,
    kb_id: String,
) -> Result<KnowledgeConnectionTest, String> {
    if !state
        .server_running
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err("请先启动 WaLiAPI 服务".into());
    }
    let key = Repository::new(state.db.pool.clone())
        .get_api_key_by_id(&id)
        .await
        .map_err(|_| "密钥不存在")?;
    let port = *state.server_port.read().await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法创建连接测试")?;
    let mut url =
        url::Url::parse(&format!("http://127.0.0.1:{port}/api/kb/")).map_err(|_| "服务地址无效")?;
    url.path_segments_mut()
        .map_err(|_| "服务地址无效")?
        .pop_if_empty()
        .push(&kb_id);
    let rest = client
        .get(url)
        .bearer_auth(&key.key)
        .send()
        .await
        .map_err(|_| "无法连接本机 WaLiAPI 服务")?;
    let rest_status = rest.status().as_u16();
    let rest_body = rest.json::<serde_json::Value>().await.unwrap_or_default();
    let mcp = client.post(format!("http://127.0.0.1:{port}/mcp")).bearer_auth(&key.key)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_knowledge_base_stats","arguments":{"kb_id":kb_id}}}))
        .send().await.map_err(|_| "无法连接本机 MCP 服务")?;
    let mcp_status = mcp.status().as_u16();
    let mcp_body = mcp.json::<serde_json::Value>().await.unwrap_or_default();
    Ok(KnowledgeConnectionTest {
        rest_status,
        rest_ok: rest_status == 200 && rest_body["id"] == kb_id,
        mcp_status,
        mcp_ok: mcp_status == 200 && mcp_body["result"]["isError"] == false,
    })
}
