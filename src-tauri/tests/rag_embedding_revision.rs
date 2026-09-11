//! 嵌入模型变更后的缓存、向量空间与索引维度回归。
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};
use waliapi_lib::db::repository::Repository;
use waliapi_lib::server::event_bridge::EventSink;
use waliapi_lib::services::knowledge::{
    models::{CreateKbInput, UpdateKbInput},
    processor,
    repository::{ChunkInsert, KbRepository},
    retriever,
};
use waliapi_lib::settings_store::SettingsStore;

async fn pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

async fn wait_for_index(
    events: &mut tokio::sync::broadcast::Receiver<waliapi_lib::server::event_bridge::AdminEvent>,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.event == "kb-index-progress" && event.payload["status"] == "ready" {
                break;
            }
        }
    })
    .await
    .expect("索引任务应完成");
}

#[tokio::test]
async fn model_switch_reembeds_unchanged_text_and_rebuilds_vector_space() {
    let pool = pool().await;
    let repo = KbRepository::new(pool.clone());
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = calls.clone();
    let mock = Router::new().route(
        "/v1/embeddings",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                let model = body["model"].as_str().unwrap();
                captured.lock().unwrap().push(model.to_string());
                let vector = match model {
                    "embed-b" => json!([0.0, 1.0, 0.0]),
                    "embed-c" => json!([0.0, 0.0, 0.0, 1.0, 0.0]),
                    _ => json!([1.0, 0.0, 0.0]),
                };
                let data: Vec<_> = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .map(|(index, _)| json!({"index": index, "embedding": vector}))
                    .collect();
                Json(json!({"data": data}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    Repository::new(pool.clone())
        .create_channel(
            &serde_json::from_value(json!({
                "name": "local-embedding-test", "type": "openai", "base_url": base_url,
                "api_key": "test-only", "models": ["embed-a", "embed-b", "embed-c"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let kb = repo
        .create_kb(
            &serde_json::from_value::<CreateKbInput>(json!({
                "name": "model-switch", "embedding_model": "embed-a"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let directory = std::env::temp_dir().join(format!("rag-model-{}", kb.id));
    std::fs::create_dir_all(&directory).unwrap();
    let settings = SettingsStore::file(directory.join("settings.json"));
    let (tx, _) = tokio::sync::broadcast::channel(100);
    let events = EventSink::headless(tx);
    let mut receiver = events.subscribe();
    let mut docs = Vec::new();
    for name in ["first", "second"] {
        let filename = format!("{name}.txt");
        let file = directory.join(&filename);
        let content = format!("alpha {name} document remains unchanged across model switches");
        std::fs::write(&file, &content).unwrap();
        let doc = repo
            .create_document(
                &kb.id,
                &filename,
                file.to_str(),
                "text",
                content.len() as i64,
                name,
            )
            .await
            .unwrap();
        processor::process_document(
            &pool,
            &events,
            &kb.id,
            &doc.id,
            &filename,
            content.as_bytes(),
            Some("embed-a"),
            &settings,
            &directory,
        )
        .await
        .unwrap();
        wait_for_index(&mut receiver).await;
        docs.push(doc);
    }
    let doc = &docs[0];
    // 同一模型的普通重建仍复用缓存，不增加模型调用。
    let same = repo
        .update_kb(
            &kb.id,
            &serde_json::from_value::<UpdateKbInput>(json!({"embedding_model": "embed-a"}))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(same.embedding_revision, 0);
    processor::reindex_document(&pool, &events, &doc.id, &settings, &directory)
        .await
        .unwrap();
    wait_for_index(&mut receiver).await;
    assert_eq!(calls.lock().unwrap().len(), 2);

    for (revision, model, vector) in [
        (1, "embed-b", vec![0.0, 1.0, 0.0]),
        (2, "embed-c", vec![0.0, 0.0, 0.0, 1.0, 0.0]),
    ] {
        let changed = repo
            .update_kb(
                &kb.id,
                &serde_json::from_value::<UpdateKbInput>(json!({"embedding_model": model}))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(changed.embedding_revision, revision);
        assert_eq!(changed.embedding_dim, 0);
        assert_eq!(changed.index_status, "stale");
        assert!(repo
            .get_chunk_hashes_by_doc(&doc.id)
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .get_chunks_by_kb_with_dim(&kb.id)
            .await
            .unwrap()
            .is_empty());
        assert!(retriever::search(&pool, &kb.id, &vector, 5)
            .await
            .unwrap()
            .is_empty());
        processor::reindex_document(&pool, &events, &doc.id, &settings, &directory)
            .await
            .unwrap();
        wait_for_index(&mut receiver).await;
        let chunks = repo.get_chunk_vectors_by_doc(&doc.id).await.unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(retriever::decode_embedding(&chunks[0].1), vector);
        assert!(repo
            .get_chunk_vectors_by_doc(&docs[1].id)
            .await
            .unwrap()
            .is_empty());
        let found = retriever::search(&pool, &kb.id, &vector, 5).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].doc_id, doc.id);
        let meta = repo.get_index_meta(&kb.id).await.unwrap().unwrap();
        assert_eq!(meta.embedding_dim, vector.len() as i64);
        assert_eq!(meta.chunk_count, 1);
        assert_eq!(
            repo.get_kb(&kb.id).await.unwrap().embedding_dim,
            vector.len() as i64
        );
    }
    assert_eq!(
        *calls.lock().unwrap(),
        ["embed-a", "embed-a", "embed-b", "embed-c"]
    );
    retriever::drop_index(&pool, &kb.id).await.unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn legacy_cache_is_compatible_until_the_effective_model_changes() {
    let pool = pool().await;
    let repo = KbRepository::new(pool.clone());
    let kb = repo
        .create_kb(&serde_json::from_value(json!({"name": "legacy-cache"})).unwrap())
        .await
        .unwrap();
    let doc = repo
        .create_document(&kb.id, "legacy.txt", None, "text", 5, "doc-hash")
        .await
        .unwrap();
    repo.create_chunk(&ChunkInsert {
        id: "legacy-chunk".into(),
        doc_id: doc.id.clone(),
        kb_id: kb.id.clone(),
        chunk_index: 0,
        content: "alpha".into(),
        token_count: 2,
        embedding: retriever::encode_embedding(&[1.0, 0.0]),
        embedding_dim: 2,
        metadata: "{}".into(),
        content_hash: Some("content-hash".into()),
        created_at: "2026-09-11T00:00:00Z".into(),
    })
    .await
    .unwrap();
    repo.update_document_status(&doc.id, "ready", None)
        .await
        .unwrap();
    let unchanged = repo
        .update_kb(
            &kb.id,
            &serde_json::from_value(json!({
                "name": "renamed", "embedding_model": "text-embedding-3-small"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unchanged.embedding_revision, 0);
    let old_cache = repo.get_chunk_hashes_by_doc(&doc.id).await.unwrap();
    assert!(old_cache.contains_key("0:content-hash"));
    assert_eq!(repo.get_chunks_by_kb(&kb.id).await.unwrap().len(), 1);
    repo.update_kb(
        &kb.id,
        &serde_json::from_value(json!({"embedding_model": "embed-next"})).unwrap(),
    )
    .await
    .unwrap();
    assert!(repo
        .get_chunk_hashes_by_doc(&doc.id)
        .await
        .unwrap()
        .is_empty());
    assert!(!old_cache.contains_key("1:content-hash"));
    // 变更前仍在执行的任务，不能回写新模型的期望维度。
    repo.update_kb_embedding_dim(&kb.id, 2, 0).await.unwrap();
    assert_eq!(repo.get_kb(&kb.id).await.unwrap().embedding_dim, 0);
}
