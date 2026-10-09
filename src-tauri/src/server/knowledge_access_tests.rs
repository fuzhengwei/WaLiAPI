use super::{
    build_router,
    tests::{json_request, request, test_shared, test_state},
};
use crate::{
    db::{models::ApiKey, repository::Repository},
    server::knowledge_access::{get_grants, set_grants},
    services::knowledge::{
        models::KbKnowledgeBase,
        repository::{ChunkInsert, KbRepository},
    },
    AppState,
};
use axum::{body::to_bytes, http::StatusCode, response::Response, routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tower::ServiceExt;

async fn setup() -> (
    Arc<AppState>,
    ApiKey,
    KbKnowledgeBase,
    KbKnowledgeBase,
    Router,
) {
    let state = test_state().await;
    let repo = Repository::new(state.db.pool.clone());
    let key = repo
        .create_api_key(
            &serde_json::from_value(json!({"name":"query-test", "quota_limit":10000})).unwrap(),
        )
        .await
        .unwrap();
    let kb_repo = KbRepository::new(state.db.pool.clone());
    let first = kb_repo
        .create_kb(
            &serde_json::from_value(json!({"name":"granted", "embedding_model":"embed-test"}))
                .unwrap(),
        )
        .await
        .unwrap();
    let second = kb_repo
        .create_kb(
            &serde_json::from_value(json!({"name":"private", "embedding_model":"embed-test"}))
                .unwrap(),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE kb_knowledge_bases SET mcp_enabled = 1")
        .execute(&state.db.pool)
        .await
        .unwrap();
    set_grants(&state.db.pool, &key.id, std::slice::from_ref(&first.id))
        .await
        .unwrap();
    let app = build_router(state.clone(), test_shared(&state, None, None));
    (state, key, first, second, app)
}

#[tokio::test]
async fn candidate_limits_are_validated_before_model_call() {
    let (_, key, first, _, app) = setup().await;
    for count in [0, 4, 101] {
        let response = app.clone().oneshot(json_request("POST", "/api/kb/ask", Some(&key.key),
            &json!({"kb_id":first.id,"question":"备份要求","top_k":5,"candidate_k":count,"search_mode":"keyword"}).to_string())).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app.clone().oneshot(request("GET", &format!(
            "/api/kb/search?q=alpha&kb_id={}&search_mode=keyword&top_k=5&candidate_k={count}", first.id), Some(&key.key))).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body(response).await["error"]["code"], "invalid_query");
    }
}

async fn body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn diagnostic_stage<'a>(result: &'a Value, name: &str) -> &'a Value {
    result["diagnostics"]["stages"]
        .as_array()
        .expect("诊断必须包含阶段数组")
        .iter()
        .rev()
        .find(|stage| stage["stage"] == name)
        .unwrap_or_else(|| panic!("诊断缺少阶段 {name}: {result}"))
}

fn rpc(key: &str, name: &str, arguments: Value) -> axum::http::Request<axum::body::Body> {
    json_request("POST", "/mcp", Some(key), &json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":name,"arguments":arguments}}).to_string())
}

async fn document(state: &AppState, kb_id: &str, content: &str) -> String {
    let repo = KbRepository::new(state.db.pool.clone());
    let doc = repo
        .create_document(
            kb_id,
            "test.txt",
            None,
            "txt",
            content.len() as i64,
            content,
        )
        .await
        .unwrap();
    repo.create_chunk(&ChunkInsert {
        id: uuid::Uuid::new_v4().to_string(),
        doc_id: doc.id.clone(),
        kb_id: kb_id.to_string(),
        chunk_index: 0,
        content: content.into(),
        token_count: 8,
        embedding: bincode::serialize(&vec![1.0_f32, 0.0, 0.0]).unwrap(),
        embedding_dim: 3,
        metadata: "{}".into(),
        content_hash: None,
        created_at: crate::utils::time::now_iso(),
    })
    .await
    .unwrap();
    repo.update_document_status(&doc.id, "ready", None)
        .await
        .unwrap();
    doc.id
}

#[tokio::test]
async fn newly_created_key_can_list_all_existing_kbs_over_rest_and_mcp() {
    let (state, _, first, second, app) = setup().await;
    let key = Repository::new(state.db.pool.clone())
        .create_api_key(&serde_json::from_value(json!({"name":"default-grants"})).unwrap())
        .await
        .unwrap();
    let result = body(
        app.clone()
            .oneshot(request("GET", "/api/kb", Some(&key.key)))
            .await
            .unwrap(),
    )
    .await;
    let ids = result["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kb| kb["id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids, [first.id.as_str(), second.id.as_str()].into());
    let result = body(
        app.oneshot(rpc(&key.key, "list_knowledge_bases", json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["result"]["isError"], false);
    assert!(result.to_string().contains(&first.id));
    assert!(result.to_string().contains(&second.id));
}

#[tokio::test]
async fn newly_created_kb_is_visible_to_existing_keys_and_can_be_revoked() {
    let (state, key, first, private, app) = setup().await;
    let kb = KbRepository::new(state.db.pool.clone())
        .create_kb(&serde_json::from_value(json!({"name":"new-shared"})).unwrap())
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/kb/{}/stats", kb.id),
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(
        app.clone()
            .oneshot(rpc(
                &key.key,
                "get_knowledge_base_stats",
                json!({"kb_id":kb.id}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["result"]["isError"], false);
    // 新库的默认授权不恢复此前被显式撤销的其他库。
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/kb/{}/stats", private.id),
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    set_grants(&state.db.pool, &key.id, &[first.id])
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/kb/{}/stats", kb.id),
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let result = body(
        app.oneshot(rpc(
            &key.key,
            "get_knowledge_base_stats",
            json!({"kb_id":kb.id}),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(result["result"]["isError"], true);
}

#[tokio::test]
async fn grants_filter_lists_and_block_management_and_cross_kb_reads() {
    let (state, key, first, second, app) = setup().await;
    let foreign_doc = document(&state, &second.id, "private secret").await;
    let result = body(
        app.clone()
            .oneshot(request("GET", "/api/kb", Some(&key.key)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["data"].as_array().unwrap().len(), 1);
    assert_eq!(result["data"][0]["id"], first.id);
    for (method, path) in [
        ("GET", format!("/api/kb/{}", second.id)),
        ("DELETE", format!("/api/kb/{}", first.id)),
        ("POST", format!("/api/kb/{}/documents", first.id)),
        ("POST", format!("/api/kb/{}/index", first.id)),
        ("GET", format!("/api/kb/{}/conversations", first.id)),
        ("GET", "/api/wiki/projects".into()),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, &path, Some(&key.key)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
    }
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/kb/{}/documents/{foreign_doc}", first.id),
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let result = body(
        app.clone()
            .oneshot(rpc(
                &key.key,
                "read_document",
                json!({"kb_id":first.id,"doc_id":foreign_doc}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["result"]["isError"], true);
    assert!(!result.to_string().contains("private secret"));
    let response = app
        .oneshot(request(
            "GET",
            &format!("/api/kb/{}/stats", first.id),
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn query_requires_explicit_granted_kb_and_bounds() {
    let (_, key, first, second, app) = setup().await;
    for (input, status) in [
        (json!({"question":"alpha"}), StatusCode::BAD_REQUEST),
        (
            json!({"question":"alpha","kb_id":second.id}),
            StatusCode::FORBIDDEN,
        ),
        (
            json!({"question":"alpha","kb_id":first.id,"top_k":1000}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"question":"alpha","kb_id":first.id,"deep_research":true}),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let response = app
        .oneshot(request("GET", "/api/kb/search?q=alpha", Some(&key.key)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn revocation_expiry_and_disabled_keys_take_effect_without_restart() {
    let (state, key, first, _, app) = setup().await;
    for (column, value, expected) in [
        ("status", "0", StatusCode::UNAUTHORIZED),
        (
            "expires_at",
            "'2000-01-01T00:00:00Z'",
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        sqlx::query(&format!(
            "UPDATE api_keys SET {column} = {value} WHERE id = ?"
        ))
        .bind(&key.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
        let response = app
            .clone()
            .oneshot(request("GET", "/api/kb", Some(&key.key)))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        sqlx::query("UPDATE api_keys SET status = 1, expires_at = NULL WHERE id = ?")
            .bind(&key.id)
            .execute(&state.db.pool)
            .await
            .unwrap();
    }
    // 非法保存整体回滚，不丢失原授权；清空后立即失效。
    assert!(set_grants(
        &state.db.pool,
        &key.id,
        &[first.id.clone(), "missing-kb".into()]
    )
    .await
    .is_err());
    assert_eq!(
        get_grants(&state.db.pool, &key.id).await.unwrap(),
        vec![first.id]
    );
    set_grants(&state.db.pool, &key.id, &[]).await.unwrap();
    assert_eq!(
        app.oneshot(request("GET", "/api/kb", Some(&key.key)))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn mcp_limits_tools_exposure_and_legacy_sessions() {
    let (state, key, first, _, app) = setup().await;
    let result = body(
        app.clone()
            .oneshot(json_request(
                "POST",
                "/mcp",
                Some(&key.key),
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["result"]["tools"].as_array().unwrap().len(), 5);
    assert!(!result.to_string().contains("delete_knowledge_base"));
    let result = body(
        app.clone()
            .oneshot(rpc(
                &key.key,
                "delete_knowledge_base",
                json!({"kb_id":first.id}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["error"]["code"], -32601);
    for path in ["/mcp?session_id=foreign", "/mcp/sse"] {
        assert_eq!(
            app.clone()
                .oneshot(json_request("POST", path, Some(&key.key), "{}"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        app.clone()
            .oneshot(request("GET", "/mcp", Some(&key.key)))
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    sqlx::query("UPDATE kb_knowledge_bases SET mcp_enabled = 0 WHERE id = ?")
        .bind(&first.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
    let result = body(
        app.clone()
            .oneshot(rpc(
                &key.key,
                "get_knowledge_base_stats",
                json!({"kb_id":first.id}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["result"]["isError"], true);
    let result = body(
        app.oneshot(rpc(&key.key, "list_knowledge_bases", json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert!(!result.to_string().contains(&first.id));
}

async fn mock_model(state: &AppState) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    mock_model_with_answer(state, "BCD").await
}

async fn mock_model_with_answer(
    state: &AppState,
    answer_text: &'static str,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    mock_model_with_delay(state, answer_text, std::time::Duration::ZERO).await
}

async fn mock_model_with_delay(
    state: &AppState,
    answer_text: &'static str,
    embedding_delay: std::time::Duration,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    mock_model_with_delays(
        state,
        answer_text,
        embedding_delay,
        std::time::Duration::ZERO,
    )
    .await
}

async fn mock_model_with_delays(
    state: &AppState,
    answer_text: &'static str,
    embedding_delay: std::time::Duration,
    answer_delay: std::time::Duration,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    mock_model_with_capture(state, answer_text, embedding_delay, answer_delay, None).await
}

async fn mock_model_with_capture(
    state: &AppState,
    answer_text: &'static str,
    embedding_delay: std::time::Duration,
    answer_delay: std::time::Duration,
    captured: Option<Arc<std::sync::Mutex<Vec<Value>>>>,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let embed_calls = calls.clone();
    let chat_calls = calls.clone();
    let upstream = Router::new()
        .route("/v1/embeddings", post(move || { let calls = embed_calls.clone(); async move {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(embedding_delay).await;
            Json(json!({"object":"list","data":[{"object":"embedding","index":0,"embedding":[1.0,0.0,0.0]}],"model":"embed-test","usage":{"prompt_tokens":7,"total_tokens":7}}))
        }}))
        .route("/v1/chat/completions", post(move |Json(body): Json<Value>| { let calls = chat_calls.clone(); let captured = captured.clone(); async move {
            calls.fetch_add(1, Ordering::SeqCst);
            if let Some(captured) = captured { captured.lock().unwrap().push(body.clone()); }
            tokio::time::sleep(answer_delay).await;
            let system = body["messages"][0]["content"].as_str().unwrap_or("");
            let answer = if system.contains("改写器") { "alpha" } else if system.contains("重排器") { "[1,0]" } else { answer_text };
            Json(json!({"id":"test-answer","object":"chat.completion","model":"chat-test","choices":[{"index":0,"message":{"role":"assistant","content":answer},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}))
        }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let channel = Repository::new(state.db.pool.clone()).create_channel(&serde_json::from_value(json!({
        "name":"local-mock", "type":"openai", "base_url":format!("http://127.0.0.1:{port}/v1"), "api_key":"mock-upstream",
        "models":["chat-test","embed-test"], "protocol":"openai", "provider":"custom", "native_base_url":format!("http://127.0.0.1:{port}/v1"), "native_endpoints":["chat_completions","embeddings"]
    })).unwrap()).await.unwrap();
    (channel.id, calls, task)
}

#[tokio::test]
async fn rag_reasoning_levels_reach_the_canonical_gateway_body_without_claiming_application() {
    let (state, key, first, _, app) = setup().await;
    let captured = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let (_, calls, task) = mock_model_with_capture(
        &state,
        "BCD",
        std::time::Duration::ZERO,
        std::time::Duration::ZERO,
        Some(captured.clone()),
    )
    .await;
    document(&state, &first.id, "alpha answer is BCD").await;
    for (index, level) in [
        None,
        Some("default"),
        Some("none"),
        Some("low"),
        Some("medium"),
        Some("high"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"keyword"});
        if let Some(level) = level {
            input["reasoning_effort"] = json!(level);
        }
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = body(response).await;
        let sent = captured.lock().unwrap()[index].clone();
        if let Some(level @ ("none" | "low" | "medium" | "high")) = level {
            assert_eq!(
                result["reasoning"],
                json!({"requested":level,"status":"requested"})
            );
            assert_eq!(sent["reasoning_effort"], level);
        } else {
            assert!(result.get("reasoning").is_none());
            assert!(sent.get("reasoning_effort").is_none());
        }
        assert_eq!(sent["model"], "chat-test");
        for provider_field in ["thinking", "enable_thinking", "reasoning"] {
            assert!(sent.get(provider_field).is_none());
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        6,
        "每个请求仅一次生成，不重试或换模型"
    );
    task.abort();
}

#[tokio::test]
async fn rag_invalid_reasoning_and_explicit_deep_research_are_rejected_before_upstream() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    for (effort, deep) in [
        ("", false),
        ("HIGH", false),
        ("auto", false),
        ("max", false),
        ("high", true),
        ("none", true),
    ] {
        let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","reasoning_effort":effort,"deep_research":deep});
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let result = body(response).await;
        assert_eq!(result["error"]["code"], "invalid_reasoning_effort");
        if deep {
            assert!(result["error"]["message"]
                .as_str()
                .unwrap()
                .contains("deep_research"));
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn rag_empty_retrieval_records_reasoning_as_not_sent() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    for effort in [None, Some("default"), Some("high")] {
        let mut input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"keyword"});
        if let Some(effort) = effort {
            input["reasoning_effort"] = json!(effort);
        }
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = body(response).await;
        if effort == Some("high") {
            assert_eq!(
                result["reasoning"],
                json!({"requested":"high","status":"not_sent"})
            );
        } else {
            assert!(result.get("reasoning").is_none());
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn rag_calls_embedding_and_chat_with_the_callers_quota_and_logs() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha answer is BCD").await;
    let input =
        json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"vector"});
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/kb/ask",
            Some(&key.key),
            &input.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(result["answer"], "BCD");
    assert_eq!(result["sources"].as_array().unwrap().len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let quota: i64 = sqlx::query_scalar("SELECT quota_used FROM api_keys WHERE id = ?")
        .bind(&key.id)
        .fetch_one(&state.db.pool)
        .await
        .unwrap();
    assert_eq!(quota, 21);
    let logs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM request_logs WHERE api_key_id = ? AND status_code = 200",
    )
    .bind(&key.id)
    .fetch_one(&state.db.pool)
    .await
    .unwrap();
    assert_eq!(logs, 2);
    let result = body(
        app.oneshot(rpc(&key.key, "ask_knowledge_base", input))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["result"]["isError"], false);
    assert!(result.to_string().contains("BCD"));
    let history: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kb_conversations WHERE kb_id = ?")
        .bind(&first.id)
        .fetch_one(&state.db.pool)
        .await
        .unwrap();
    assert_eq!(
        history, 0,
        "external callers must not share conversation history"
    );
    task.abort();
}

#[tokio::test]
async fn denied_models_channels_and_exhausted_quota_never_reach_upstream() {
    let (state, key, first, _, app) = setup().await;
    let (channel, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha answer is BCD").await;
    for (column, value, mode, expected) in [
        (
            "allowed_models",
            json!(["other"]).to_string(),
            "vector",
            StatusCode::FORBIDDEN,
        ),
        (
            "denied_models",
            json!(["embed-test"]).to_string(),
            "vector",
            StatusCode::FORBIDDEN,
        ),
        (
            "denied_models",
            json!(["chat-test"]).to_string(),
            "keyword",
            StatusCode::FORBIDDEN,
        ),
        (
            "denied_channels",
            json!([channel]).to_string(),
            "vector",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "allowed_channels",
            json!(["other"]).to_string(),
            "vector",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        sqlx::query(&format!("UPDATE api_keys SET {column} = ? WHERE id = ?"))
            .bind(value)
            .bind(&key.id)
            .execute(&state.db.pool)
            .await
            .unwrap();
        let input =
            json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":mode});
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{column} {mode}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        sqlx::query(&format!("UPDATE api_keys SET {column} = '[]' WHERE id = ?"))
            .bind(&key.id)
            .execute(&state.db.pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE api_keys SET quota_used = quota_limit WHERE id = ?")
        .bind(&key.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
    let response = app
        .oneshot(request(
            "GET",
            &format!("/api/kb/search?q=alpha&kb_id={}", first.id),
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn rewrite_and_rerank_use_the_same_key_and_accounting() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha first paragraph").await;
    document(&state, &first.id, "alpha second paragraph").await;
    state
        .settings
        .set_many(&[
            ("kb.query_rewrite".into(), json!(true)),
            ("kb.rerank_enabled".into(), json!(true)),
        ])
        .unwrap();
    let input = json!({"kb_id":first.id,"question":"alpha?","model":"chat-test","search_mode":"vector","history":[{"role":"user","content":"alpha"}]});
    let response = app
        .oneshot(json_request(
            "POST",
            "/api/kb/ask",
            Some(&key.key),
            &input.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["answer"], "BCD");
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let quota: i64 = sqlx::query_scalar("SELECT quota_used FROM api_keys WHERE id = ?")
        .bind(&key.id)
        .fetch_one(&state.db.pool)
        .await
        .unwrap();
    assert_eq!(quota, 49);
    task.abort();
}

#[tokio::test]
async fn embedding_spend_is_checked_before_answer_generation() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha answer BCD").await;
    sqlx::query("UPDATE api_keys SET quota_limit = 7 WHERE id = ?")
        .bind(&key.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
    let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"vector","diagnostics":true});
    let response = app
        .oneshot(json_request(
            "POST",
            "/api/kb/ask",
            Some(&key.key),
            &input.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let result = body(response).await;
    assert_eq!(result["error"]["stage"], "answer");
    assert_eq!(result["error"]["code"], "quota_or_rate_limited");
    assert_eq!(diagnostic_stage(&result, "embedding")["status"], "passed");
    assert_eq!(diagnostic_stage(&result, "answer")["status"], "failed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn connection_test_checks_real_rest_and_mcp_authorization() {
    let (state, key, first, _, app) = setup().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    *state.server_port.write().await = listener.local_addr().unwrap().port();
    state.server_running.store(true, Ordering::SeqCst);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let shared = test_shared(&state, None, None);
    let result = crate::commands::api_key::test_api_key_knowledge_access(
        shared.state_static.clone(),
        key.id.clone(),
        first.id.clone(),
    )
    .await
    .unwrap();
    let result = json!(result);
    assert_eq!(result["rest_ok"], true);
    assert_eq!(result["mcp_ok"], true);
    crate::commands::api_key::set_api_key_knowledge_access(
        shared.state_static.clone(),
        key.id.clone(),
        vec![],
    )
    .await
    .unwrap();
    let result = crate::commands::api_key::test_api_key_knowledge_access(
        shared.state_static,
        key.id,
        first.id,
    )
    .await
    .unwrap();
    let result = json!(result);
    assert_eq!(result["rest_status"], 403);
    assert_eq!(result["rest_ok"], false);
    assert_eq!(result["mcp_ok"], false);
    task.abort();
}

#[tokio::test]
async fn rag_diagnostics_cover_real_stages_and_correlate_gateway_logs() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha answer is BCD").await;
    let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"vector","diagnostics":true});
    let mut request = json_request("POST", "/api/kb/ask", Some(&key.key), &input.to_string());
    request
        .headers_mut()
        .insert("x-request-id", "sk-untrusted-client-value".parse().unwrap());
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(result["answer"], "BCD");
    assert!(!result["sources"].as_array().unwrap().is_empty());
    let diagnostics = &result["diagnostics"];
    let request_id = diagnostics["request_id"].as_str().unwrap();
    assert!(uuid::Uuid::parse_str(request_id).is_ok());
    let stages = diagnostics["stages"].as_array().unwrap();
    assert_eq!(
        stages
            .iter()
            .map(|s| s["stage"].as_str().unwrap())
            .filter(|stage| matches!(
                *stage,
                "permission" | "embedding" | "retrieval" | "answer" | "validation"
            ))
            .collect::<Vec<_>>(),
        [
            "permission",
            "embedding",
            "retrieval",
            "answer",
            "validation"
        ]
    );
    assert!(stages.iter().all(|s| s["elapsed_ms"].is_u64()));
    for stage in [
        "permission",
        "embedding",
        "vector_search",
        "retrieval",
        "answer",
        "validation",
    ] {
        assert_eq!(diagnostic_stage(&result, stage)["status"], "passed");
    }
    let logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE api_key_id = ? AND trace_id = ? AND status_code = 200")
        .bind(&key.id).bind(request_id).fetch_one(&state.db.pool).await.unwrap();
    assert_eq!(logs, 2, "诊断编号必须对应实际网关日志");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!result.to_string().contains("sk-untrusted-client-value"));
    task.abort();
}

#[tokio::test]
async fn rag_diagnostics_identify_missing_embedding_capability_without_calling_upstream() {
    let (state, key, first, _, app) = setup().await;
    let (channel, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha answer is BCD").await;
    sqlx::query("UPDATE channels SET native_endpoints = '[\"chat_completions\"]' WHERE id = ?")
        .bind(&channel)
        .execute(&state.db.pool)
        .await
        .unwrap();
    for mode in ["hybrid", "vector"] {
        let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":mode,"diagnostics":true});
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let result = body(response).await;
        assert_eq!(result["error"]["stage"], "embedding");
        assert_eq!(result["error"]["code"], "endpoint_not_configured");
        assert_eq!(diagnostic_stage(&result, "permission")["status"], "passed");
        assert_eq!(diagnostic_stage(&result, "embedding")["status"], "failed");
        let trace = result["error"]["request_id"].as_str().unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM request_logs WHERE trace_id = ? AND status_code = 501",
        )
        .bind(trace)
        .fetch_one(&state.db.pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"keyword","diagnostics":true});
    let response = app
        .oneshot(json_request(
            "POST",
            "/api/kb/ask",
            Some(&key.key),
            &input.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(diagnostic_stage(&result, "embedding")["status"], "skipped");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn rag_diagnostics_reject_empty_retrieval_without_changing_regular_ask() {
    let (_, key, first, _, app) = setup().await;
    for diagnostics in [false, true] {
        let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"keyword","diagnostics":diagnostics});
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if diagnostics {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            }
        );
        let result = body(response).await;
        if diagnostics {
            assert_eq!(result["error"]["code"], "retrieval_empty");
            assert_eq!(result["error"]["stage"], "retrieval");
            assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "failed");
        } else {
            assert!(result["sources"].as_array().unwrap().is_empty());
            assert!(result.get("diagnostics").is_none());
        }
    }
}

#[tokio::test]
async fn rag_diagnostics_reject_empty_answers_and_missing_sources() {
    for (answer_text, history, content, expected_status, expected_code) in [
        (
            "   ",
            "",
            "alpha answer is BCD",
            StatusCode::BAD_GATEWAY,
            "answer_empty",
        ),
        (
            "BCD",
            "x",
            "alpha answer is BCD",
            StatusCode::UNPROCESSABLE_ENTITY,
            "sources_empty",
        ),
        (
            "BCD",
            "",
            "   ",
            StatusCode::UNPROCESSABLE_ENTITY,
            "sources_empty",
        ),
    ] {
        let (state, key, first, _, app) = setup().await;
        let (_, calls, task) = mock_model_with_answer(&state, answer_text).await;
        document(&state, &first.id, content).await;
        let mut input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"vector","diagnostics":true});
        if !history.is_empty() {
            input["history"] = json!([{"role":"user","content":history.repeat(40000)}]);
        }
        let response = app
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        let result = body(response).await;
        assert_eq!(result["error"]["code"], expected_code);
        assert_eq!(result["error"]["stage"], "validation");
        assert_eq!(diagnostic_stage(&result, "validation")["status"], "failed");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        task.abort();
    }
}

#[tokio::test]
async fn rag_regular_ask_preserves_empty_answer_while_diagnostics_rejects_it() {
    let (state, key, first, _, app) = setup().await;
    let (_, _, task) = mock_model_with_answer(&state, "").await;
    document(&state, &first.id, "alpha answer is BCD").await;
    for diagnostics in [false, true] {
        let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"vector","diagnostics":diagnostics});
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &input.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if diagnostics {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            }
        );
        let result = body(response).await;
        if diagnostics {
            assert_eq!(result["error"]["code"], "answer_empty");
        } else {
            assert_eq!(result["answer"], "");
        }
    }
    task.abort();
}

#[tokio::test]
async fn rag_diagnostics_identify_real_upstream_timeout_without_changing_status() {
    let (state, key, first, _, app) = setup().await;
    let (channel, calls, task) =
        mock_model_with_delay(&state, "BCD", std::time::Duration::from_secs(3)).await;
    sqlx::query("UPDATE channels SET timeout_secs = 1 WHERE id = ?")
        .bind(channel)
        .execute(&state.db.pool)
        .await
        .unwrap();
    state
        .settings
        .set_many(&[("retry.enabled".into(), json!(false))])
        .unwrap();
    document(&state, &first.id, "alpha answer is BCD").await;
    let input = json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"vector","diagnostics":true});
    let response = app
        .oneshot(json_request(
            "POST",
            "/api/kb/ask",
            Some(&key.key),
            &input.to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let result = body(response).await;
    assert_eq!(result["error"]["stage"], "embedding");
    assert_eq!(result["error"]["code"], "model_timeout");
    assert_eq!(diagnostic_stage(&result, "embedding")["status"], "failed");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "关闭重试后只进行一次真实模型请求"
    );
    let request_id = result["error"]["request_id"].as_str().unwrap();
    let logs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE api_key_id = ? AND trace_id = ? AND status_code = 502")
        .bind(&key.id).bind(request_id).fetch_one(&state.db.pool).await.unwrap();
    assert_eq!(logs, 1);
    task.abort();
}

#[tokio::test]
async fn rag_budget_stops_slow_embedding_without_another_key_or_answer() {
    let (state, key, first, _, app) = setup().await;
    let (channel, calls, task) =
        mock_model_with_delay(&state, "BCD", std::time::Duration::from_secs(2)).await;
    Repository::new(state.db.pool.clone())
        .replace_channel_api_keys(
            &channel,
            &serde_json::from_value::<Vec<crate::db::models::ChannelApiKeyInput>>(
                json!([{"api_key":"extra-one","weight":1},{"api_key":"extra-two","weight":1}]),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    document(&state, &first.id, "alpha answer BCD").await;
    let started = std::time::Instant::now();
    let response = app.oneshot(json_request("POST", "/api/kb/ask", Some(&key.key),
        &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":200,"diagnostics":true}).to_string())).await.unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < std::time::Duration::from_millis(700));
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_string();
    let result = body(response).await;
    assert_eq!(result["error"]["code"], "stage_timeout");
    assert_eq!(result["error"]["request_id"], request_id);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "预算到期后不能轮换 Key 或生成回答"
    );
    task.abort();
}

#[tokio::test]
async fn rag_explicit_keyword_fallback_uses_remaining_budget_and_reports_actual_mode() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) =
        mock_model_with_delay(&state, "BCD", std::time::Duration::from_secs(2)).await;
    document(&state, &first.id, "alpha answer BCD").await;
    let started = std::time::Instant::now();
    let response = app.oneshot(json_request("POST", "/api/kb/ask", Some(&key.key),
        &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":1000,"allow_keyword_fallback":true,"diagnostics":true}).to_string())).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(result["answer"], "BCD");
    assert_eq!(result["retrieval_mode"], "keyword");
    assert_eq!(result["degradation_reason"], "stage_timeout");
    assert_eq!(diagnostic_stage(&result, "embedding")["status"], "failed");
    assert_eq!(
        diagnostic_stage(&result, "embedding")["code"],
        "stage_timeout"
    );
    assert_eq!(
        diagnostic_stage(&result, "embedding")["deadline_scope"],
        "stage"
    );
    assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "degraded");
    assert!(!result["sources"].as_array().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    task.abort();
}

#[tokio::test]
async fn rag_fallback_cannot_bypass_revoked_models_or_exhausted_quota() {
    for quota in [false, true] {
        let (state, key, first, _, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        document(&state, &first.id, "alpha answer BCD").await;
        if quota {
            sqlx::query("UPDATE api_keys SET quota_used = quota_limit WHERE id = ?")
                .bind(&key.id)
                .execute(&state.db.pool)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE api_keys SET denied_models = '[\"embed-test\"]' WHERE id = ?")
                .bind(&key.id)
                .execute(&state.db.pool)
                .await
                .unwrap();
        }
        let response = app.oneshot(json_request("POST", "/api/kb/ask", Some(&key.key),
            &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":500,"allow_keyword_fallback":true}).to_string())).await.unwrap();
        assert_eq!(
            response.status(),
            if quota {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::FORBIDDEN
            }
        );
        let result = body(response).await;
        assert!(result.get("answer").is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        task.abort();
    }
}

#[tokio::test]
async fn rag_future_abort_does_not_continue_to_answer() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) =
        mock_model_with_delay(&state, "BCD", std::time::Duration::from_millis(350)).await;
    document(&state, &first.id, "alpha answer BCD").await;
    let request = json_request("POST", "/api/kb/ask", Some(&key.key),
        &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":2000,"allow_keyword_fallback":true}).to_string());
    let ask = tokio::spawn(async move { app.oneshot(request).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    ask.abort();
    assert!(ask.await.unwrap_err().is_cancelled());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn rag_fallback_with_empty_material_is_an_error_even_without_diagnostics() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) =
        mock_model_with_delay(&state, "BCD", std::time::Duration::from_secs(2)).await;
    let response = app.oneshot(json_request("POST", "/api/kb/ask", Some(&key.key),
        &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":500,"allow_keyword_fallback":true}).to_string())).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body(response).await["error"]["code"], "retrieval_empty");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn rag_revocation_during_answer_blocks_the_output() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model_with_delays(
        &state,
        "BCD",
        std::time::Duration::ZERO,
        std::time::Duration::from_millis(350),
    )
    .await;
    document(&state, &first.id, "alpha answer BCD").await;
    let request = json_request(
        "POST",
        "/api/kb/ask",
        Some(&key.key),
        &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":2000})
            .to_string(),
    );
    let ask = tokio::spawn(async move { app.oneshot(request).await.unwrap() });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    set_grants(&state.db.pool, &key.id, &[]).await.unwrap();
    let response = ask.await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(body(response).await.get("answer").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn rag_completed_answer_can_exhaust_quota_without_losing_its_response() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha answer BCD").await;
    sqlx::query("UPDATE api_keys SET quota_limit = 21 WHERE id = ?")
        .bind(&key.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
    let response = app
        .oneshot(json_request(
            "POST",
            "/api/kb/ask",
            Some(&key.key),
            &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":2000})
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["answer"], "BCD");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn rag_explicit_budget_rejects_empty_answers_without_diagnostics() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model_with_answer(&state, "").await;
    document(&state, &first.id, "alpha answer BCD").await;
    let response = app
        .oneshot(json_request(
            "POST",
            "/api/kb/ask",
            Some(&key.key),
            &json!({"kb_id":first.id,"question":"alpha","model":"chat-test","timeout_ms":2000})
                .to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let result = body(response).await;
    assert_eq!(result["error"]["code"], "answer_empty");
    assert!(result.get("diagnostics").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn generic_search_preserves_legacy_data_and_returns_authorized_original_content() {
    let (state, key, first, second, app) = setup().await;
    let original = "alpha 规范原文：值是 'a\u{a0}b'，运算符 !=；😀保持原文。";
    let doc_id = document(&state, &first.id, original).await;
    document(&state, &second.id, "alpha PRIVATE MATERIAL").await;
    for extended in [false, true] {
        let suffix = if extended {
            "&candidate_k=20&timeout_ms=2000&allow_keyword_fallback=true&diagnostics=true"
        } else {
            ""
        };
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/api/kb/search?q=alpha&kb_id={}&search_mode=keyword&top_k=5{suffix}",
                    first.id
                ),
                Some(&key.key),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_string();
        let result = body(response).await;
        let data = result["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["doc_id"], doc_id);
        assert_eq!(data[0]["content"], original, "原文不是摘要或业务加工文本");
        assert!(!data[0]["chunk_id"].as_str().unwrap().is_empty());
        assert!(!result.to_string().contains("PRIVATE MATERIAL"));
        if extended {
            assert_eq!(result["request_id"], request_id);
            assert_eq!(result["retrieval_mode"], "keyword");
            assert_eq!(result["diagnostics"]["request_id"], request_id);
            assert_eq!(diagnostic_stage(&result, "embedding")["status"], "skipped");
            assert_eq!(
                diagnostic_stage(&result, "keyword_search")["status"],
                "passed"
            );
        } else {
            assert_eq!(
                result.as_object().unwrap().len(),
                1,
                "旧 search 只包含 data"
            );
        }
    }
    let response = app
        .oneshot(request(
            "GET",
            &format!(
                "/api/kb/search?q=missing&kb_id={}&search_mode=keyword&diagnostics=true",
                first.id
            ),
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result = body(response).await;
    assert_eq!(result["data"], json!([]));
    assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "empty");
}

#[tokio::test]
async fn generic_search_uses_only_embedding_even_when_generation_features_are_enabled() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha original first paragraph").await;
    document(&state, &first.id, "alpha original second paragraph").await;
    state
        .settings
        .set_many(&[
            ("kb.query_rewrite".into(), json!(true)),
            ("kb.rerank_enabled".into(), json!(true)),
        ])
        .unwrap();
    for mode in ["vector", "hybrid"] {
        let response = app.clone().oneshot(request("GET", &format!(
            "/api/kb/search?q=alpha&kb_id={}&search_mode={mode}&top_k=1&candidate_k=20&timeout_ms=3000&diagnostics=true", first.id),
            Some(&key.key))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = body(response).await;
        assert_eq!(result["data"].as_array().unwrap().len(), 1);
        assert_eq!(result["retrieval_mode"], mode);
        let stages = result["diagnostics"]["stages"].as_array().unwrap();
        assert_eq!(
            stages
                .iter()
                .map(|s| s["stage"].as_str().unwrap())
                .filter(|stage| matches!(*stage, "permission" | "embedding" | "retrieval"))
                .collect::<Vec<_>>(),
            ["permission", "embedding", "retrieval"]
        );
        assert_eq!(
            diagnostic_stage(&result, "vector_search")["status"],
            "passed"
        );
        if mode == "hybrid" {
            assert_eq!(
                diagnostic_stage(&result, "keyword_search")["status"],
                "passed"
            );
            assert_eq!(diagnostic_stage(&result, "fusion")["status"], "passed");
        }
        assert!(result.get("answer").is_none());
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "Search 不调用回答/改写/重排模型"
    );
    let quota: i64 = sqlx::query_scalar("SELECT quota_used FROM api_keys WHERE id = ?")
        .bind(&key.id)
        .fetch_one(&state.db.pool)
        .await
        .unwrap();
    assert_eq!(quota, 14, "每次仅计一次 Embedding 用量");
    task.abort();
}

#[tokio::test]
async fn generic_search_budget_and_opt_in_fallback_share_the_retrieval_path() {
    for fallback in [false, true] {
        let (state, key, first, _, app) = setup().await;
        let (_, calls, task) =
            mock_model_with_delay(&state, "unused", std::time::Duration::from_secs(3)).await;
        document(&state, &first.id, "alpha usable original evidence").await;
        // 预热确切非流式客户端；不将冷初始化误当作目标网络超时。
        let _ = crate::adaptor::blocking_client(60, None);
        let started = std::time::Instant::now();
        let response = app.oneshot(request("GET", &format!(
            "/api/kb/search?q=alpha&kb_id={}&search_mode=hybrid&top_k=5&candidate_k=20&timeout_ms=1000&allow_keyword_fallback={fallback}&diagnostics=true", first.id),
            Some(&key.key))).await.unwrap();
        assert_eq!(
            response.status(),
            if fallback {
                StatusCode::OK
            } else {
                StatusCode::GATEWAY_TIMEOUT
            }
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(1400));
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_string();
        let result = body(response).await;
        if fallback {
            assert_eq!(result["retrieval_mode"], "keyword");
            assert_eq!(result["degradation_reason"], "stage_timeout");
            assert_eq!(
                result["data"][0]["content"],
                "alpha usable original evidence"
            );
            assert_eq!(diagnostic_stage(&result, "embedding")["status"], "failed");
            assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "degraded");
        } else {
            assert_eq!(result["error"]["code"], "stage_timeout");
            assert_eq!(result["error"]["request_id"], request_id);
        }
        assert_eq!(
            diagnostic_stage(&result, "embedding")["code"],
            "stage_timeout"
        );
        assert_eq!(
            diagnostic_stage(&result, "embedding")["deadline_scope"],
            "stage"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "超时不换 Key 或调用生成模型"
        );
        task.abort();
    }
}

#[tokio::test]
async fn generic_search_fallback_cannot_bypass_model_permission_or_quota() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha original evidence").await;
    for quota in [false, true] {
        sqlx::query(if quota {
            "UPDATE api_keys SET quota_used = quota_limit, denied_models = '[]' WHERE id = ?"
        } else {
            "UPDATE api_keys SET denied_models = '[\"embed-test\"]' WHERE id = ?"
        })
        .bind(&key.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
        let response = app.clone().oneshot(request("GET", &format!(
            "/api/kb/search?q=alpha&kb_id={}&search_mode=hybrid&timeout_ms=1000&allow_keyword_fallback=true&diagnostics=true", first.id),
            Some(&key.key))).await.unwrap();
        assert_eq!(
            response.status(),
            if quota {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::FORBIDDEN
            }
        );
        let result = body(response).await;
        assert!(result.get("data").is_none());
        assert_eq!(result["error"]["stage"], "permission");
        assert_eq!(
            result["error"]["code"],
            if quota {
                "quota_exceeded"
            } else {
                "access_denied"
            }
        );
        assert_eq!(diagnostic_stage(&result, "permission")["status"], "failed");
        assert!(result["diagnostics"]["stages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|stage| stage["stage"] != "embedding"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    task.abort();
}

#[tokio::test]
async fn generic_search_rechecks_revocation_after_the_embedding_await() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) =
        mock_model_with_delay(&state, "unused", std::time::Duration::from_millis(150)).await;
    document(&state, &first.id, "alpha original evidence").await;
    let input = request(
        "GET",
        &format!(
            "/api/kb/search?q=alpha&kb_id={}&search_mode=vector&timeout_ms=2000&diagnostics=true",
            first.id
        ),
        Some(&key.key),
    );
    let search = tokio::spawn(async move { app.oneshot(input).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    set_grants(&state.db.pool, &key.id, &[]).await.unwrap();
    let response = search.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let result = body(response).await;
    assert!(result.get("data").is_none());
    assert_eq!(result["error"]["code"], "knowledge_access_denied");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn generic_search_future_abort_does_not_start_another_request() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) =
        mock_model_with_delay(&state, "unused", std::time::Duration::from_millis(250)).await;
    document(&state, &first.id, "alpha original evidence").await;
    let input = request("GET", &format!(
        "/api/kb/search?q=alpha&kb_id={}&search_mode=hybrid&timeout_ms=2000&allow_keyword_fallback=true", first.id), Some(&key.key));
    let search = tokio::spawn(async move { app.oneshot(input).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    search.abort();
    assert!(search.await.unwrap_err().is_cancelled());
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn admin_deep_research_rejects_explicit_retrieval_policies_before_model_io() {
    let (state, _, first, _, _) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha deep research original").await;
    let admin = "test-admin-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, Some(admin), None));
    for policy in [
        "strict_retrieval",
        "allow_keyword_fallback",
        "allow_vector_fallback",
    ] {
        let mut input = json!({
            "kb_id":first.id,"question":"alpha","model":"chat-test",
            "deep_research":true,"max_rounds":1
        });
        input[policy] = json!(true);
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(admin),
                &input.to_string(),
            ))
            .await
            .unwrap();
        let status = response.status();
        let result = body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{policy}: {result}");
        assert_eq!(result["error"]["code"], "unsupported_retrieval_policy");
        assert_eq!(result["error"]["stage"], "permission");
        assert!(result.get("answer").is_none());
        assert!(result.get("sources").is_none());
        assert!(!result.to_string().contains("deep research original"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "拒绝不支持的组合时不发送Embedding或生成请求"
        );
    }
    task.abort();
}

#[tokio::test]
async fn admin_search_new_options_keep_cross_kb_vector_fallback_without_expanding_api_key_access() {
    let (state, key, first, second, _) = setup().await;
    let (channel, calls, task) = mock_model(&state).await;
    sqlx::query("UPDATE channels SET models = '[\"chat-test\",\"embed-test\",\"text-embedding-3-small\"]' WHERE id = ?")
        .bind(channel).execute(&state.db.pool).await.unwrap();
    document(&state, &first.id, "alpha first original material").await;
    document(&state, &second.id, "alpha second original material").await;
    let admin = "test-admin-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, Some(admin), None));
    let legacy = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/kb/search?q=alpha&search_mode=keyword&top_k=2",
            Some(admin),
        ))
        .await
        .unwrap();
    assert_eq!(legacy.status(), StatusCode::OK);
    let legacy = body(legacy).await;
    assert_eq!(legacy["data"].as_array().unwrap().len(), 2);
    let mut expected: Vec<_> = legacy["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["chunk_id"].as_str().unwrap().to_string())
        .collect();
    expected.sort();
    for mode in ["keyword", "hybrid", "vector"] {
        let response = app.clone().oneshot(request("GET", &format!(
            "/api/kb/search?q=alpha&search_mode={mode}&top_k=2&candidate_k=20&timeout_ms=2000&allow_keyword_fallback=true"), Some(admin))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = body(response).await;
        assert_eq!(result["retrieval_mode"], "vector", "报告实际跨库检索方式");
        let mut actual: Vec<_> = result["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["chunk_id"].as_str().unwrap().to_string())
            .collect();
        actual.sort();
        assert_eq!(actual, expected, "新参数不改变管理员原有跨库结果");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4, "各次检索仅一次Embedding");
    let response = app
        .oneshot(request(
            "GET",
            "/api/kb/search?q=alpha&search_mode=keyword&top_k=2&candidate_k=20&timeout_ms=2000",
            Some(&key.key),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "API Key 仍必须指定单个已授权KB"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    task.abort();
}

fn generic_fallback_request(
    kb_id: &str,
    key: &str,
    keyword: bool,
    vector: Option<bool>,
    timeout_ms: u64,
) -> axum::http::Request<axum::body::Body> {
    let mut uri = format!(
        "/api/kb/search?q=alpha&kb_id={kb_id}&search_mode=hybrid&top_k=5&candidate_k=20&timeout_ms={timeout_ms}&allow_keyword_fallback={keyword}&diagnostics=true"
    );
    if let Some(vector) = vector {
        uri.push_str(&format!("&allow_vector_fallback={vector}"));
    }
    request("GET", &uri, Some(key))
}

async fn wait_for_mock_request(calls: &AtomicUsize) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("本地 mock 应收到 Embedding 请求");
}

#[test]
fn generic_fallback_vector_flag_is_opt_in_in_legacy_ask_input() {
    use crate::services::knowledge::models::AskInput;
    for value in [
        json!({"question":"alpha"}),
        json!({"question":"alpha","allow_keyword_fallback":true}),
    ] {
        let input: AskInput = serde_json::from_value(value).unwrap();
        assert!(!input.allow_vector_fallback);
        assert!(!input.strict_retrieval);
    }
    let input: AskInput =
        serde_json::from_value(json!({"question":"alpha","allow_vector_fallback":true})).unwrap();
    assert!(input.allow_vector_fallback);
    assert!(!input.allow_keyword_fallback);
    assert!(!input.strict_retrieval);
    for direction in ["allow_keyword_fallback", "allow_vector_fallback"] {
        let mut value = json!({"question":"alpha","strict_retrieval":true});
        value[direction] = json!(true);
        let input: AskInput = serde_json::from_value(value).unwrap();
        assert!(input.strict_retrieval);
        assert!(input.allow_keyword_fallback || input.allow_vector_fallback);
    }
}

#[tokio::test]
async fn generic_fallback_legacy_local_errors_keep_historical_fusion_scores() {
    for failed_route in ["keyword", "vector"] {
        for fusion in ["rrf", "weighted"] {
            let (state, key, first, _, app) = setup().await;
            let (_, calls, task) = mock_model(&state).await;
            let doc_id = document(&state, &first.id, "alpha legacy partial original").await;
            state
                .settings
                .set_many(&[("kb.fusion_mode".into(), json!(fusion))])
                .unwrap();
            sqlx::query(if failed_route == "keyword" {
                "DROP TABLE kb_chunks_fts"
            } else {
                "ALTER TABLE kb_chunks RENAME COLUMN embedding TO fixture_missing_embedding"
            })
            .execute(&state.db.pool)
            .await
            .unwrap();
            // timeout/diagnostics 本身不切换严格策略；显式 false 也等同旧缺省。
            for policy in [
                "",
                "&strict_retrieval=false&allow_keyword_fallback=false&allow_vector_fallback=false",
            ] {
                let response = app.clone().oneshot(request("GET", &format!(
                    "/api/kb/search?q=alpha&kb_id={}&search_mode=hybrid&timeout_ms=3000&diagnostics=true{policy}", first.id),
                    Some(&key.key))).await.unwrap();
                let status = response.status();
                let result = body(response).await;
                assert_eq!(status, StatusCode::OK, "{failed_route}/{fusion}: {result}");
                assert_eq!(result["data"].as_array().unwrap().len(), 1);
                assert_eq!(result["data"][0]["doc_id"], doc_id);
                assert_eq!(
                    result["data"][0]["content"],
                    "alpha legacy partial original"
                );
                assert_eq!(
                    result["retrieval_mode"],
                    if failed_route == "keyword" {
                        "vector"
                    } else {
                        "keyword"
                    }
                );
                assert_eq!(
                    result["degradation_reason"],
                    format!("{failed_route}_search_failed")
                );
                assert_eq!(
                    diagnostic_stage(&result, &format!("{failed_route}_search"))["status"],
                    "failed"
                );
                assert_eq!(diagnostic_stage(&result, "fusion")["status"], "degraded");
                assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "degraded");
                let expected = if fusion == "rrf" {
                    1.0 / 61.0
                } else if failed_route == "keyword" {
                    0.7
                } else {
                    0.3
                };
                assert!(
                    (result["data"][0]["score"].as_f64().unwrap() - expected).abs() < 1e-6,
                    "旧单路部分成功仍沿用融合分数: {result}"
                );
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "各次仅一次Embedding，不生成或重试"
            );
            task.abort();
        }
    }
}

#[tokio::test]
async fn generic_fallback_strict_and_direction_flags_require_exact_authorization() {
    for failed_route in ["keyword", "vector"] {
        for (strict, keyword, vector) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, false),
            (true, false, true),
        ] {
            let (state, key, first, _, app) = setup().await;
            let (_, calls, task) = mock_model(&state).await;
            document(&state, &first.id, "alpha explicit policy original").await;
            sqlx::query(if failed_route == "keyword" {
                "DROP TABLE kb_chunks_fts"
            } else {
                "ALTER TABLE kb_chunks RENAME COLUMN embedding TO fixture_missing_embedding"
            })
            .execute(&state.db.pool)
            .await
            .unwrap();
            let response = app.oneshot(request("GET", &format!(
                "/api/kb/search?q=alpha&kb_id={}&search_mode=hybrid&timeout_ms=3000&diagnostics=true&strict_retrieval={strict}&allow_keyword_fallback={keyword}&allow_vector_fallback={vector}", first.id),
                Some(&key.key))).await.unwrap();
            let status = response.status();
            let result = body(response).await;
            let authorized = if failed_route == "keyword" {
                vector
            } else {
                keyword
            };
            if authorized {
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "strict={strict}/keyword={keyword}/vector={vector}: {result}"
                );
                assert_eq!(
                    result["data"][0]["content"],
                    "alpha explicit policy original"
                );
                assert_eq!(
                    result["retrieval_mode"],
                    if failed_route == "keyword" {
                        "vector"
                    } else {
                        "keyword"
                    }
                );
                assert_eq!(result["data"][0]["score"], 1.0, "显式单路降级保留原始分数");
                assert_eq!(diagnostic_stage(&result, "fusion")["status"], "skipped");
                assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "degraded");
            } else {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{result}");
                assert_eq!(
                    result["error"]["code"],
                    format!("{failed_route}_search_failed")
                );
                assert!(result.get("data").is_none());
            }
            assert_eq!(
                diagnostic_stage(&result, &format!("{failed_route}_search"))["status"],
                "failed"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "strict与allow可同时传入，不添加模型请求"
            );
            task.abort();
        }
    }
}

#[tokio::test]
async fn generic_fallback_vector_requires_new_policy_after_real_fts_error() {
    for vector in [None, Some(false), Some(true)] {
        let (state, key, first, second, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        let doc_id = document(&state, &first.id, "alpha authorized vector original").await;
        document(&state, &second.id, "alpha PRIVATE vector original").await;
        // 仅破坏本 fixture 的 FTS；向量表和授权记录仍正常。
        sqlx::query("DROP TABLE kb_chunks_fts")
            .execute(&state.db.pool)
            .await
            .unwrap();
        let response = app
            .oneshot(generic_fallback_request(
                &first.id, &key.key, true, vector, 3000,
            ))
            .await
            .unwrap();
        let status = response.status();
        let result = body(response).await;
        assert_eq!(
            diagnostic_stage(&result, "keyword_search")["status"],
            "failed"
        );
        assert_eq!(
            diagnostic_stage(&result, "keyword_search")["code"],
            "keyword_search_failed"
        );
        if vector == Some(true) {
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["retrieval_mode"], "vector");
            assert_eq!(result["degradation_reason"], "keyword_search_failed");
            assert_eq!(result["data"].as_array().unwrap().len(), 1);
            assert_eq!(result["data"][0]["doc_id"], doc_id);
            assert_eq!(
                result["data"][0]["content"],
                "alpha authorized vector original"
            );
            assert_eq!(
                diagnostic_stage(&result, "vector_search")["status"],
                "passed"
            );
            assert_eq!(diagnostic_stage(&result, "fusion")["status"], "skipped");
            assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "degraded");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(result["error"]["code"], "keyword_search_failed");
            assert_eq!(result["error"]["stage"], "keyword_search");
            assert!(result.get("data").is_none());
            assert!(calls.load(Ordering::SeqCst) <= 1);
        }
        assert!(!result.to_string().contains("PRIVATE vector original"));
        assert!(!result.to_string().contains("no such table"));
        task.abort();
    }
}

#[tokio::test]
async fn generic_fallback_keyword_requires_old_policy_after_real_vector_error() {
    for keyword in [false, true] {
        let (state, key, first, second, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        let doc_id = document(&state, &first.id, "alpha authorized keyword original").await;
        document(&state, &second.id, "alpha PRIVATE keyword original").await;
        // 向量 SELECT 所需列缺失，但 FTS 和正文连接查询仍可工作。
        sqlx::query("ALTER TABLE kb_chunks RENAME COLUMN embedding TO fixture_missing_embedding")
            .execute(&state.db.pool)
            .await
            .unwrap();
        let response = app
            .oneshot(generic_fallback_request(
                &first.id,
                &key.key,
                keyword,
                Some(true),
                3000,
            ))
            .await
            .unwrap();
        let status = response.status();
        let result = body(response).await;
        assert_eq!(
            diagnostic_stage(&result, "vector_search")["status"],
            "failed"
        );
        assert_eq!(
            diagnostic_stage(&result, "vector_search")["code"],
            "vector_search_failed"
        );
        if keyword {
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["retrieval_mode"], "keyword");
            assert_eq!(result["degradation_reason"], "vector_search_failed");
            assert_eq!(result["data"].as_array().unwrap().len(), 1);
            assert_eq!(result["data"][0]["doc_id"], doc_id);
            assert_eq!(
                result["data"][0]["content"],
                "alpha authorized keyword original"
            );
            assert_eq!(
                diagnostic_stage(&result, "keyword_search")["status"],
                "passed"
            );
            assert_eq!(result["data"][0]["score"], 1.0, "保留关键词单路原始分数");
            assert_eq!(diagnostic_stage(&result, "fusion")["status"], "skipped");
            assert_eq!(diagnostic_stage(&result, "retrieval")["status"], "degraded");
        } else {
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(result["error"]["code"], "vector_search_failed");
            assert_eq!(result["error"]["stage"], "vector_search");
            assert!(result.get("data").is_none());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!result.to_string().contains("PRIVATE keyword original"));
        assert!(!result.to_string().contains("fixture_missing_embedding"));
        task.abort();
    }
}

#[tokio::test]
async fn generic_fallback_vector_policy_reaches_ask_without_extra_generation() {
    for allow_vector in [false, true] {
        let (state, key, first, _, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        document(&state, &first.id, "alpha authorized answer evidence").await;
        sqlx::query("DROP TABLE kb_chunks_fts")
            .execute(&state.db.pool)
            .await
            .unwrap();
        let response = app
            .oneshot(json_request(
                "POST",
                "/api/kb/ask",
                Some(&key.key),
                &json!({
                    "kb_id":first.id,"question":"alpha","model":"chat-test",
                    "search_mode":"hybrid","timeout_ms":3000,"diagnostics":true,
                    "allow_keyword_fallback":true,"allow_vector_fallback":allow_vector,"strict_retrieval":true
                })
                .to_string(),
            ))
            .await
            .unwrap();
        let status = response.status();
        let result = body(response).await;
        if allow_vector {
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["answer"], "BCD");
            assert_eq!(result["retrieval_mode"], "vector");
            assert_eq!(result["degradation_reason"], "keyword_search_failed");
            assert_eq!(result["sources"].as_array().unwrap().len(), 1);
            assert_eq!(
                result["sources"][0]["snippet"],
                "alpha authorized answer evidence"
            );
            assert_eq!(diagnostic_stage(&result, "fusion")["status"], "skipped");
            assert_eq!(diagnostic_stage(&result, "answer")["status"], "passed");
            assert_eq!(calls.load(Ordering::SeqCst), 2, "仅一次Embedding和一次生成");
        } else {
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{result}");
            assert_eq!(result["error"]["code"], "keyword_search_failed");
            assert!(result.get("answer").is_none());
            assert_eq!(calls.load(Ordering::SeqCst), 1, "未授权降级不能生成");
        }
        task.abort();
    }
}

#[tokio::test]
async fn generic_fallback_cannot_report_success_when_both_routes_fail_or_survivor_is_empty() {
    for both_fail in [true, false] {
        let (state, key, first, _, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        document(&state, &first.id, "alpha fixture original").await;
        if both_fail {
            sqlx::query(
                "ALTER TABLE kb_chunks RENAME COLUMN embedding TO fixture_missing_embedding",
            )
            .execute(&state.db.pool)
            .await
            .unwrap();
        } else {
            sqlx::query("UPDATE kb_chunks SET embedding = NULL")
                .execute(&state.db.pool)
                .await
                .unwrap();
        }
        sqlx::query("DROP TABLE kb_chunks_fts")
            .execute(&state.db.pool)
            .await
            .unwrap();
        let response = app
            .oneshot(generic_fallback_request(
                &first.id,
                &key.key,
                true,
                Some(true),
                3000,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if both_fail {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::NOT_FOUND
            }
        );
        let result = body(response).await;
        assert_eq!(
            result["error"]["code"],
            if both_fail {
                "retrieval_both_failed"
            } else {
                "retrieval_empty"
            }
        );
        assert!(result.get("data").is_none());
        assert_eq!(
            diagnostic_stage(&result, "keyword_search")["code"],
            "keyword_search_failed"
        );
        if both_fail {
            assert_eq!(
                diagnostic_stage(&result, "vector_search")["code"],
                "vector_search_failed"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}

#[tokio::test]
async fn generic_fallback_flags_cannot_bypass_identity_grants_model_permission_or_quota() {
    for fault in ["disabled", "private_kb", "denied_model", "quota"] {
        let (state, key, first, second, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        document(&state, &first.id, "alpha authorized original").await;
        let kb_id = if fault == "private_kb" {
            &second.id
        } else {
            &first.id
        };
        if fault != "private_kb" {
            sqlx::query(match fault {
                "disabled" => "UPDATE api_keys SET status = 0 WHERE id = ?",
                "denied_model" => {
                    "UPDATE api_keys SET denied_models = '[\"embed-test\"]' WHERE id = ?"
                }
                "quota" => "UPDATE api_keys SET quota_used = quota_limit WHERE id = ?",
                _ => unreachable!(),
            })
            .bind(&key.id)
            .execute(&state.db.pool)
            .await
            .unwrap();
        }
        let response = app
            .oneshot(generic_fallback_request(
                kb_id,
                &key.key,
                true,
                Some(true),
                2000,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            match fault {
                "disabled" => StatusCode::UNAUTHORIZED,
                "quota" => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::FORBIDDEN,
            },
            "{fault}"
        );
        let result = body(response).await;
        assert!(result.get("data").is_none());
        assert_eq!(result["error"]["stage"], "permission");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{fault}");
        task.abort();
    }
}

#[tokio::test]
async fn generic_fallback_rechecks_grants_before_returning_surviving_vector_material() {
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) =
        mock_model_with_delay(&state, "unused", std::time::Duration::from_millis(250)).await;
    document(&state, &first.id, "alpha authorized original").await;
    sqlx::query("DROP TABLE kb_chunks_fts")
        .execute(&state.db.pool)
        .await
        .unwrap();
    let input = generic_fallback_request(&first.id, &key.key, true, Some(true), 2000);
    let search = tokio::spawn(async move { app.oneshot(input).await });
    wait_for_mock_request(&calls).await;
    set_grants(&state.db.pool, &key.id, &[]).await.unwrap();
    let response = search.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let result = body(response).await;
    assert!(result.get("data").is_none());
    assert_eq!(result["error"]["code"], "knowledge_access_denied");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn generic_fallback_parent_deadline_cannot_return_available_keyword_material() {
    use std::ffi::{c_int, c_uint, c_void, CStr};

    struct KeywordComplete {
        notify: tokio::sync::Notify,
        rows: AtomicUsize,
    }
    unsafe extern "C" fn observe_keyword_query(
        event: c_uint,
        context: *mut c_void,
        statement: *mut c_void,
        _: *mut c_void,
    ) -> c_int {
        // 只观察此隔离池的真实 FTS SELECT；不依赖 tracing 的全局 callsite 缓存。
        let sql = unsafe { libsqlite3_sys::sqlite3_sql(statement.cast()) };
        if sql.is_null() {
            return 0;
        }
        const KEYWORD_SQL: &[u8] = b"SELECT c.id, c.content, c.metadata, d.filename, c.doc_id \
            FROM kb_chunks_fts fts \
            JOIN kb_chunks c ON fts.chunk_id = c.id \
            JOIN kb_documents d ON c.doc_id = d.id \
            WHERE c.kb_id = ? AND d.status = 'ready' AND kb_chunks_fts MATCH ? \
            ORDER BY rank \
            LIMIT ?";
        if unsafe { CStr::from_ptr(sql) }.to_bytes() != KEYWORD_SQL {
            return 0;
        }
        // 每个注册连接持有一个原始 Arc 强引用；解除回调后才释放。
        let complete = unsafe { &*context.cast::<KeywordComplete>() };
        if event == libsqlite3_sys::SQLITE_TRACE_ROW {
            complete.rows.fetch_add(1, Ordering::SeqCst);
        } else if event == libsqlite3_sys::SQLITE_TRACE_PROFILE
            && complete.rows.load(Ordering::SeqCst) > 0
        {
            complete.notify.notify_one();
        }
        0
    }
    let (state, key, first, _, app) = setup().await;
    let (_, calls, task) =
        mock_model_with_delay(&state, "unused", std::time::Duration::from_secs(60)).await;
    document(&state, &first.id, "alpha usable keyword original").await;
    let _ = crate::adaptor::blocking_client(60, None);
    let keyword_complete = Arc::new(KeywordComplete {
        notify: tokio::sync::Notify::new(),
        rows: AtomicUsize::new(0),
    });
    // 同时取出五个连接，保证随后本请求无论使用哪一个，都有独立于日志的屏障。
    let mut traced_connections = Vec::new();
    for _ in 0..5 {
        let mut connection = state.db.pool.acquire().await.unwrap();
        let mut handle = connection.lock_handle().await.unwrap();
        let context = Arc::into_raw(keyword_complete.clone());
        let result = unsafe {
            libsqlite3_sys::sqlite3_trace_v2(
                handle.as_raw_handle().as_ptr(),
                libsqlite3_sys::SQLITE_TRACE_ROW | libsqlite3_sys::SQLITE_TRACE_PROFILE,
                Some(observe_keyword_query),
                context.cast_mut().cast(),
            )
        };
        assert_eq!(result, libsqlite3_sys::SQLITE_OK);
        drop(handle);
        traced_connections.push(connection);
    }
    drop(traced_connections);
    // 真实 SQL/HTTP 握手使用原准备预算，避免并行测试先耗完 FTS 阶段软限额。
    // 完成屏障之后用本 runtime 的虚拟时钟验证父截止，不实际等待10秒。
    let input = generic_fallback_request(&first.id, &key.key, true, Some(true), 10_000);
    let mut search = tokio::spawn(async move { app.oneshot(input).await });
    tokio::select! {
        _ = keyword_complete.notify.notified() => {},
        response = &mut search => {
            let response = response.unwrap().unwrap();
            let status = response.status();
            panic!("阶段屏障之前请求已结束: {status}, {}", body(response).await);
        },
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
            panic!("未观察到本请求非空 FTS 查询完成: rows={}, physical={}",
                keyword_complete.rows.load(Ordering::SeqCst), calls.load(Ordering::SeqCst));
        },
    }
    wait_for_mock_request(&calls).await;
    // 权限初查已通过、Embedding已发出、关键词已完成；仅让降级后的授权复核等待父截止。
    let mut connections = tokio::time::timeout(std::time::Duration::from_millis(500), async {
        let mut connections = Vec::new();
        for _ in 0..5 {
            connections.push(state.db.pool.acquire().await.unwrap());
        }
        connections
    })
    .await
    .expect("应能占满fixture连接池");
    assert_eq!(keyword_complete.rows.load(Ordering::SeqCst), 1);
    for connection in &mut connections {
        let mut handle = connection.lock_handle().await.unwrap();
        let result = unsafe {
            libsqlite3_sys::sqlite3_trace_v2(
                handle.as_raw_handle().as_ptr(),
                0,
                None,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, libsqlite3_sys::SQLITE_OK);
        // 连接已独占且 SQLx worker 已停在锁外，解除后回调不再访问上下文。
        // 测试提前 panic 时不释放原始强引用，避免尚在运行的回调访问悬空指针。
        unsafe { drop(Arc::from_raw(Arc::as_ptr(&keyword_complete))) };
    }
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    tokio::time::resume();
    let response = tokio::time::timeout(std::time::Duration::from_secs(3), search)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let status = response.status();
    let result = body(response).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{result}");
    assert_eq!(result["error"]["code"], "rag_deadline_exceeded");
    assert_eq!(
        diagnostic_stage(&result, "keyword_search")["status"],
        "passed"
    );
    assert!(result.get("data").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(connections);
    task.abort();
}

#[tokio::test]
async fn generic_fallback_completed_embedding_can_exhaust_quota_without_losing_material() {
    for fts_failure in [false, true] {
        let (state, key, first, _, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        let doc_id = document(&state, &first.id, "alpha authorized original").await;
        sqlx::query("UPDATE api_keys SET quota_limit = 7 WHERE id = ?")
            .bind(&key.id)
            .execute(&state.db.pool)
            .await
            .unwrap();
        if fts_failure {
            sqlx::query("DROP TABLE kb_chunks_fts")
                .execute(&state.db.pool)
                .await
                .unwrap();
        }
        let response = app
            .clone()
            .oneshot(generic_fallback_request(
                &first.id,
                &key.key,
                true,
                Some(true),
                3000,
            ))
            .await
            .unwrap();
        let status = response.status();
        let result = body(response).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(
            result["retrieval_mode"],
            if fts_failure { "vector" } else { "hybrid" }
        );
        assert_eq!(result["data"][0]["doc_id"], doc_id);
        assert_eq!(result["data"][0]["content"], "alpha authorized original");
        let quota_used: i64 = sqlx::query_scalar("SELECT quota_used FROM api_keys WHERE id = ?")
            .bind(&key.id)
            .fetch_one(&state.db.pool)
            .await
            .unwrap();
        assert_eq!(quota_used, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let response = app
            .oneshot(generic_fallback_request(
                &first.id,
                &key.key,
                true,
                Some(true),
                3000,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(body(response).await.get("data").is_none());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "额度耗尽后禁止再次调用模型"
        );
        task.abort();
    }
}

fn mcp_json_content(response: &Value) -> Value {
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("MCP 应返回 text content"),
    )
    .expect("该 MCP 请求应返回可解析 JSON")
}

#[tokio::test]
async fn generic_mcp_search_preserves_legacy_array_and_opt_in_metadata() {
    let (state, key, first, second, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    let doc_id = document(&state, &first.id, "alpha authorized MCP material").await;
    document(&state, &second.id, "alpha PRIVATE MCP material").await;
    sqlx::query("UPDATE kb_chunks SET metadata = ? WHERE kb_id = ?")
        .bind(r#"{"heading":"authorized heading","page_no":3}"#)
        .bind(&first.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE kb_chunks SET metadata = ? WHERE kb_id = ?")
        .bind(r#"{"private_marker":"PRIVATE MCP metadata"}"#)
        .bind(&second.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
    for extra in [
        json!({}),
        json!({"allow_vector_fallback":true}),
        json!({"timeout_ms":2000}),
        json!({"diagnostics":true}),
    ] {
        let mut arguments = json!({"kb_id":first.id,"query":"alpha","search_mode":"hybrid"});
        arguments
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let response = body(
            app.clone()
                .oneshot(rpc(&key.key, "search_knowledge_base", arguments))
                .await
                .unwrap(),
        )
        .await;
        let value = mcp_json_content(&response);
        let data = if extra.as_object().unwrap().is_empty() {
            assert!(value.is_array(), "旧请求保持数组结构");
            &value
        } else {
            assert!(value["request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()));
            assert_eq!(value["retrieval_mode"], "hybrid");
            assert!(value.get("degradation_reason").is_none());
            if extra["diagnostics"] == true {
                assert_eq!(
                    diagnostic_stage(&value, "keyword_search")["status"],
                    "passed"
                );
                assert_eq!(
                    diagnostic_stage(&value, "vector_search")["status"],
                    "passed"
                );
            } else {
                assert!(value.get("diagnostics").is_none());
            }
            &value["data"]
        };
        assert_eq!(data.as_array().unwrap().len(), 1);
        assert_eq!(data[0]["doc_id"], doc_id);
        assert_eq!(data[0]["content"], "alpha authorized MCP material");
        assert_eq!(
            data[0]["metadata"],
            json!({"heading":"authorized heading","page_no":3})
        );
        assert!(!response.to_string().contains("PRIVATE MCP"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4, "各请求仅一次Embedding");
    task.abort();
}

#[tokio::test]
async fn generic_mcp_vector_fallback_requires_flag_and_preserves_scoped_metadata() {
    for allow_vector in [None, Some(false), Some(true)] {
        let (state, key, first, second, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        let doc_id = document(&state, &first.id, "alpha authorized MCP vector original").await;
        document(&state, &second.id, "alpha PRIVATE MCP vector original").await;
        sqlx::query("UPDATE kb_chunks SET metadata = ? WHERE kb_id = ?")
            .bind(r#"{"heading":"authorized vector heading","page_no":4}"#)
            .bind(&first.id)
            .execute(&state.db.pool)
            .await
            .unwrap();
        sqlx::query("DROP TABLE kb_chunks_fts")
            .execute(&state.db.pool)
            .await
            .unwrap();
        let mut arguments = json!({
            "kb_id":first.id,"query":"alpha","search_mode":"hybrid",
            "allow_keyword_fallback":true,"diagnostics":true
        });
        if let Some(value) = allow_vector {
            arguments["allow_vector_fallback"] = json!(value);
        }
        let response = body(
            app.oneshot(rpc(&key.key, "search_knowledge_base", arguments))
                .await
                .unwrap(),
        )
        .await;
        if allow_vector == Some(true) {
            let value = mcp_json_content(&response);
            assert_eq!(value["retrieval_mode"], "vector");
            assert_eq!(value["degradation_reason"], "keyword_search_failed");
            assert_eq!(value["data"].as_array().unwrap().len(), 1);
            assert_eq!(value["data"][0]["doc_id"], doc_id);
            assert_eq!(
                value["data"][0]["content"],
                "alpha authorized MCP vector original"
            );
            assert_eq!(
                value["data"][0]["metadata"],
                json!({"heading":"authorized vector heading","page_no":4})
            );
            assert_eq!(
                diagnostic_stage(&value, "keyword_search")["code"],
                "keyword_search_failed"
            );
            assert_eq!(
                diagnostic_stage(&value, "vector_search")["status"],
                "passed"
            );
            assert_eq!(diagnostic_stage(&value, "fusion")["status"], "skipped");
            assert_eq!(diagnostic_stage(&value, "retrieval")["status"], "degraded");
        } else {
            assert_eq!(response["result"]["isError"], true);
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.starts_with("HTTP 500:"), "{response}");
            assert!(!text.contains("authorized MCP vector original"));
        }
        assert!(!response.to_string().contains("PRIVATE MCP vector original"));
        assert!(!response.to_string().contains("no such table"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}

#[tokio::test]
async fn generic_mcp_tools_schema_keeps_both_fallback_directions_disabled_by_default() {
    let (state, key, _, _, _) = setup().await;
    let mcp_token = "test-mcp-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, None, Some(mcp_token)));
    for token in [key.key.as_str(), mcp_token] {
        let response = body(
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/mcp",
                    Some(token),
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                ))
                .await
                .unwrap(),
        )
        .await;
        for name in ["search_knowledge_base", "ask_knowledge_base"] {
            let tool = response["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            for flag in [
                "allow_keyword_fallback",
                "allow_vector_fallback",
                "strict_retrieval",
            ] {
                assert_eq!(tool["inputSchema"]["properties"][flag]["type"], "boolean");
                assert_eq!(tool["inputSchema"]["properties"][flag]["default"], false);
                assert!(!tool["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == flag));
            }
        }
    }
}

#[tokio::test]
async fn generic_mcp_admin_cannot_query_disabled_or_unexposed_explicit_kb() {
    for fault in ["disabled", "mcp_off"] {
        let (state, _, first, _, _) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        document(&state, &first.id, "alpha forbidden MCP material").await;
        sqlx::query(if fault == "disabled" {
            "UPDATE kb_knowledge_bases SET status = 0 WHERE id = ?"
        } else {
            "UPDATE kb_knowledge_bases SET mcp_enabled = 0 WHERE id = ?"
        })
        .bind(&first.id)
        .execute(&state.db.pool)
        .await
        .unwrap();
        let token = "test-mcp-0123456789abcdef0123456789abcdef";
        let app = build_router(state.clone(), test_shared(&state, None, Some(token)));
        for name in ["search_knowledge_base", "ask_knowledge_base"] {
            let response = body(
                app.clone()
                    .oneshot(rpc(
                        token,
                        name,
                        json!({
                            "kb_id":first.id,"query":"alpha","question":"alpha","model":"chat-test",
                            "search_mode":"hybrid","timeout_ms":2000,
                            "allow_keyword_fallback":true,"allow_vector_fallback":true
                        }),
                    ))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(
                response["error"]["code"], -32603,
                "{fault}/{name}: {response}"
            );
            assert!(response.get("result").is_none());
            assert!(response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("知识库未开启 MCP 查询"));
            assert!(!response.to_string().contains("forbidden MCP material"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        task.abort();
    }
}

#[tokio::test]
async fn generic_mcp_admin_diagnostics_are_rejected_without_upstream_calls() {
    let (state, _, first, _, _) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha MCP original").await;
    let token = "test-mcp-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, None, Some(token)));
    for name in ["search_knowledge_base", "ask_knowledge_base"] {
        let response = body(
            app.clone()
                .oneshot(rpc(
                    token,
                    name,
                    json!({
                        "kb_id":first.id,"query":"alpha","question":"alpha","model":"chat-test",
                        "diagnostics":true,"allow_vector_fallback":true,"timeout_ms":2000
                    }),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(response["error"]["code"], -32603, "{name}: {response}");
        assert!(response.get("result").is_none());
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("知识库健康检测需要使用普通 API Key"));
        assert!(!response.to_string().contains("alpha MCP original"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn generic_mcp_admin_ask_rejects_invalid_query_limits_before_model_io() {
    let (state, _, first, _, _) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha MCP original must not be returned").await;
    let token = "test-mcp-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, None, Some(token)));
    for invalid in [
        json!({"top_k":0}),
        json!({"top_k":51}),
        json!({"search_mode":"unsupported"}),
        json!({"vector_weight":1e100}),
        json!({"keyword_weight":1e100}),
        json!({"question":" "}),
    ] {
        let mut arguments = json!({
            "kb_id":first.id,"question":"alpha","model":"chat-test",
            "search_mode":"hybrid","allow_keyword_fallback":true,"allow_vector_fallback":true
        });
        arguments
            .as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        let response = body(
            app.clone()
                .oneshot(rpc(token, "ask_knowledge_base", arguments))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(response["error"]["code"], -32603, "{invalid}: {response}");
        assert!(response.get("result").is_none());
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid query"));
        assert!(!response
            .to_string()
            .contains("alpha MCP original must not be returned"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{invalid}");
    }
    task.abort();
}

#[tokio::test]
async fn generic_mcp_admin_default_search_keeps_rrf_text_and_scores_across_settings() {
    let (state, _, first, _, _) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha MCP fusion compatibility material").await;
    let token = "test-mcp-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, None, Some(token)));
    let mut responses = Vec::new();
    for mode in ["weighted", "rrf"] {
        state
            .settings
            .set_many(&[("kb.fusion_mode".into(), json!(mode))])
            .unwrap();
        assert_eq!(state.settings.get_str("kb.fusion_mode", ""), mode);
        let response = body(
            app.clone()
                .oneshot(rpc(
                    token,
                    "search_knowledge_base",
                    json!({
                        "kb_id":first.id,"query":"alpha"
                    }),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{mode}: {response}");
        let content = &response["result"]["content"];
        assert_eq!(content.as_array().unwrap().len(), 1);
        let text = content[0]["text"].as_str().unwrap();
        // 同一首位候选的 RRF 总分约 0.0328，weighted 为 1.0；fixture 能区分接线错误。
        assert_eq!(text, "[test.txt] (score: 0.03, vec: 1.00, kw: 1.00)\nalpha MCP fusion compatibility material");
        responses.push(content.clone());
    }
    assert_eq!(
        responses[0], responses[1],
        "全局设置不得改变管理 MCP 的旧 RRF 文本与分数"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "两次检索各一次Embedding");
    task.abort();
}

#[tokio::test]
async fn generic_mcp_admin_legacy_keyword_and_cross_kb_text_keep_original_format() {
    let (state, _, first, second, _) = setup().await;
    let (channel, calls, task) = mock_model(&state).await;
    sqlx::query("UPDATE channels SET models = '[\"chat-test\",\"embed-test\",\"text-embedding-3-small\"]' WHERE id = ?")
        .bind(channel).execute(&state.db.pool).await.unwrap();
    document(&state, &first.id, "alpha first legacy original").await;
    document(&state, &second.id, "alpha second legacy original").await;
    let token = "test-mcp-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, None, Some(token)));
    let keyword = body(
        app.clone()
            .oneshot(rpc(
                token,
                "search_knowledge_base",
                json!({
                    "kb_id":first.id,"query":"alpha","search_mode":"keyword"
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(keyword["result"]["isError"], false, "{keyword}");
    assert_eq!(
        keyword["result"]["content"],
        json!([{
            "type":"text","text":"[test.txt] (score: 1.00) [keyword]\nalpha first legacy original"
        }])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let cross_kb = body(
        app.oneshot(rpc(
            token,
            "search_knowledge_base",
            json!({"query":"alpha"}),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(cross_kb["result"]["isError"], false, "{cross_kb}");
    let mut texts: Vec<_> = cross_kb["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["text"].as_str().unwrap())
        .collect();
    texts.sort();
    assert_eq!(
        texts,
        [
            "[test.txt] (score: 1.00)\nalpha first legacy original",
            "[test.txt] (score: 1.00)\nalpha second legacy original"
        ]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn generic_mcp_legacy_partial_keeps_first_array_and_appends_safe_metadata() {
    let (state, key, first, second, app) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    let doc_id = document(&state, &first.id, "alpha legacy scoped partial original").await;
    document(&state, &second.id, "alpha PRIVATE legacy scoped partial").await;
    sqlx::query("DROP TABLE kb_chunks_fts")
        .execute(&state.db.pool)
        .await
        .unwrap();
    let response = body(
        app.oneshot(rpc(
            &key.key,
            "search_knowledge_base",
            json!({"kb_id":first.id,"query":"alpha"}),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(response["result"]["isError"], false, "{response}");
    let content = response["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "保留旧数组首块，降级元数据独立追加");
    let data: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(data.as_array().unwrap().len(), 1);
    assert_eq!(data[0]["doc_id"], doc_id);
    assert_eq!(data[0]["content"], "alpha legacy scoped partial original");
    assert!((data[0]["score"].as_f64().unwrap() - 1.0 / 61.0).abs() < 1e-6);
    let metadata: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["retrieval_mode"], "vector");
    assert_eq!(metadata["degradation_reason"], "keyword_search_failed");
    assert!(uuid::Uuid::parse_str(metadata["request_id"].as_str().unwrap()).is_ok());
    assert!(metadata.get("data").is_none());
    assert!(!metadata.to_string().contains("original"));
    assert!(!response
        .to_string()
        .contains("PRIVATE legacy scoped partial"));
    assert!(!response.to_string().contains("no such table"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn generic_mcp_admin_legacy_partial_keeps_first_text_and_appends_safe_metadata() {
    let (state, _, first, second, _) = setup().await;
    let (_, calls, task) = mock_model(&state).await;
    document(&state, &first.id, "alpha legacy admin partial original").await;
    document(&state, &second.id, "alpha unrelated legacy admin partial").await;
    sqlx::query("DROP TABLE kb_chunks_fts")
        .execute(&state.db.pool)
        .await
        .unwrap();
    let token = "test-mcp-0123456789abcdef0123456789abcdef";
    let app = build_router(state.clone(), test_shared(&state, None, Some(token)));
    let response = body(
        app.oneshot(rpc(
            token,
            "search_knowledge_base",
            json!({"kb_id":first.id,"query":"alpha"}),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(response["result"]["isError"], false, "{response}");
    let content = response["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(
        content[0]["text"],
        "[test.txt] (score: 0.02, vec: 1.00)\nalpha legacy admin partial original"
    );
    let metadata: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["retrieval_mode"], "vector");
    assert_eq!(metadata["degradation_reason"], "keyword_search_failed");
    assert!(uuid::Uuid::parse_str(metadata["request_id"].as_str().unwrap()).is_ok());
    assert!(metadata.get("data").is_none());
    assert!(!metadata.to_string().contains("original"));
    assert!(!response
        .to_string()
        .contains("unrelated legacy admin partial"));
    assert!(!response.to_string().contains("no such table"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn generic_mcp_explicit_failure_diagnostics_preserve_safe_stages_and_legacy_text() {
    let mut legacy_texts = Vec::new();
    for diagnostics in [false, true] {
        let (state, key, first, _, app) = setup().await;
        let (_, calls, task) = mock_model(&state).await;
        document(
            &state,
            &first.id,
            "alpha MCP diagnostic original must remain private",
        )
        .await;
        sqlx::query("DROP TABLE kb_chunks_fts")
            .execute(&state.db.pool)
            .await
            .unwrap();
        let response = body(
            app.oneshot(rpc(
                &key.key,
                "search_knowledge_base",
                json!({
                    "kb_id":first.id,"query":"alpha","search_mode":"hybrid",
                    "allow_vector_fallback":false,"strict_retrieval":true,"diagnostics":diagnostics
                }),
            ))
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        let content = response["result"]["content"].as_array().unwrap();
        assert_eq!(content.len(), if diagnostics { 2 } else { 1 });
        assert_eq!(content[0]["type"], "text");
        let legacy_text = content[0]["text"].as_str().unwrap();
        assert!(legacy_text.starts_with("HTTP 500:"));
        legacy_texts.push(legacy_text.to_string());
        if diagnostics {
            assert_eq!(content[1]["type"], "text");
            let details: Value = serde_json::from_str(content[1]["text"].as_str().unwrap())
                .expect("显式失败诊断使用与REST相同的安全JSON结构");
            assert_eq!(details["error"]["code"], "keyword_search_failed");
            assert_eq!(details["error"]["stage"], "keyword_search");
            let request_id = details["error"]["request_id"].as_str().unwrap();
            assert!(!request_id.is_empty());
            assert_eq!(details["diagnostics"]["request_id"], request_id);
            assert_eq!(
                diagnostic_stage(&details, "keyword_search")["status"],
                "failed"
            );
            assert_eq!(
                diagnostic_stage(&details, "keyword_search")["code"],
                "keyword_search_failed"
            );
            assert_eq!(diagnostic_stage(&details, "embedding")["status"], "passed");
            assert_eq!(
                diagnostic_stage(&details, "vector_search")["status"],
                "passed"
            );
            assert!(details.get("data").is_none());
            assert!(details.get("sources").is_none());
            assert!(!details.to_string().contains("\"object\":\"list\""));
        }
        let encoded = response.to_string();
        assert!(!encoded.contains("MCP diagnostic original must remain private"));
        assert!(!encoded.contains("no such table"));
        assert!(!encoded.contains("mock-upstream"));
        assert!(!encoded.contains(&key.key));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "仅一次Embedding，不生成或重试"
        );
        task.abort();
    }
    assert_eq!(legacy_texts[0], legacy_texts[1], "首个错误文本保持旧合同");
}
