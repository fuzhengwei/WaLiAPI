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

async fn body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
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
    let calls = Arc::new(AtomicUsize::new(0));
    let embed_calls = calls.clone();
    let chat_calls = calls.clone();
    let upstream = Router::new()
        .route("/v1/embeddings", post(move || { let calls = embed_calls.clone(); async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({"object":"list","data":[{"object":"embedding","index":0,"embedding":[1.0,0.0,0.0]}],"model":"embed-test","usage":{"prompt_tokens":7,"total_tokens":7}}))
        }}))
        .route("/v1/chat/completions", post(move |Json(body): Json<Value>| { let calls = chat_calls.clone(); async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let system = body["messages"][0]["content"].as_str().unwrap_or("");
            let answer = if system.contains("改写器") { "alpha" } else if system.contains("重排器") { "[1,0]" } else { "BCD" };
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
    let input =
        json!({"kb_id":first.id,"question":"alpha","model":"chat-test","search_mode":"vector"});
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
