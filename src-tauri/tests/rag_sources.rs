//! 实际模型请求、响应来源与持久化会话来源必须一致。
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use waliapi_lib::db::repository::Repository;
use waliapi_lib::services::knowledge::{
    models::ConversationMessage,
    rag,
    repository::{ChunkInsert, KbRepository},
    retriever,
};
use waliapi_lib::settings_store::SettingsStore;

#[tokio::test]
async fn ask_reports_only_sources_that_were_sent_to_the_model() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = calls.clone();
    let mock = Router::new().route("/v1/chat/completions", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            captured.lock().unwrap().push(body.clone());
            Json(json!({"id":"source-test", "object":"chat.completion", "model":body["model"],
                "choices":[{"index":0,"message":{"role":"assistant","content":"test answer"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    Repository::new(pool.clone()).create_channel(&serde_json::from_value(json!({
        "name":"local-chat-test", "type":"openai", "base_url":base_url, "api_key":"test-only",
        "models":["chat-test"], "protocol":"openai", "provider":"custom", "native_base_url":base_url,
        "native_endpoints":["chat_completions"]
    })).unwrap()).await.unwrap();
    let repo = KbRepository::new(pool.clone());
    let kb = repo
        .create_kb(&serde_json::from_value(json!({"name":"sources"})).unwrap())
        .await
        .unwrap();
    let doc = repo
        .create_document(&kb.id, "source.txt", None, "text", 40, "doc-hash")
        .await
        .unwrap();
    let text = "alpha answer must be grounded in this exact source";
    repo.create_chunk(&ChunkInsert {
        id: "source-chunk".into(),
        doc_id: doc.id.clone(),
        kb_id: kb.id.clone(),
        chunk_index: 0,
        content: text.into(),
        token_count: 12,
        embedding: retriever::encode_embedding(&[1.0, 0.0]),
        embedding_dim: 2,
        metadata: "{}".into(),
        content_hash: None,
        created_at: "2026-09-11T00:00:00Z".into(),
    })
    .await
    .unwrap();
    repo.update_document_status(&doc.id, "ready", None)
        .await
        .unwrap();
    let directory = std::env::temp_dir().join(format!("rag-sources-{}", kb.id));
    std::fs::create_dir_all(&directory).unwrap();
    let settings = SettingsStore::file(directory.join("settings.json"));
    // 正常、上下文裁空、连历史也超预算三种实际调用。
    for (history_chars, expect_source) in [(0, true), (22600, false), (30000, false)] {
        let history = if history_chars == 0 {
            vec![]
        } else {
            vec![ConversationMessage {
                role: "user".into(),
                content: "x".repeat(history_chars),
            }]
        };
        let answer = rag::ask_with_config(
            &pool,
            &kb.id,
            "alpha",
            "unused",
            "chat-test",
            5,
            false,
            &history,
            &settings,
            0.7,
            0.3,
            "keyword",
        )
        .await
        .unwrap();
        assert_eq!(answer.answer, "test answer");
        let prompt = calls.lock().unwrap().last().unwrap()["messages"][1]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            prompt.contains(text),
            expect_source,
            "history_chars={history_chars}"
        );
        assert_eq!(!answer.sources.is_empty(), expect_source);
        assert_eq!(answer.retrieval_details.as_ref().unwrap().len(), 1);
        if expect_source {
            assert_eq!(answer.sources[0].filename, "source.txt");
        }
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT sources FROM kb_conversations WHERE kb_id = ? AND role = 'assistant' ORDER BY rowid DESC LIMIT 1"
        ).bind(&kb.id).fetch_one(&pool).await.unwrap();
        let stored: Vec<Value> = serde_json::from_str(stored.as_deref().unwrap()).unwrap();
        assert_eq!(stored.len(), answer.sources.len());
    }
    assert_eq!(calls.lock().unwrap().len(), 3);
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
