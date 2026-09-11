//! 用真实 SQLite 迁移和 FTS5 验证中文搜索，不需要网络或个人知识库。
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use waliapi_lib::services::knowledge::{
    parser,
    repository::{ChunkInsert, KbRepository},
    retriever,
};

async fn fixture() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    // 先执行旧版本迁移，插入与线上同类的旧文本，再升级；确保覆盖真实升级路径。
    let migrator = sqlx::migrate!("./migrations");
    let mut old = sqlx::migrate::Migrator {
        migrations: migrator
            .iter()
            .filter(|m| m.version < 37)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.set_ignore_missing(false);
    old.run(&pool).await.unwrap();
    for kb in ["kb", "other"] {
        sqlx::query("INSERT INTO kb_knowledge_bases (id,name,created_at,updated_at) VALUES (?,?, 'now','now')")
            .bind(kb).bind(kb).execute(&pool).await.unwrap();
    }
    for (doc, kb, status) in [
        ("doc", "kb", "ready"),
        ("other-doc", "other", "ready"),
        ("failed-doc", "kb", "failed"),
    ] {
        sqlx::query("INSERT INTO kb_documents (id,kb_id,filename,file_type,content_hash,status,created_at,updated_at) VALUES (?,?,'sample.pdf','pdf','source-hash',?,'now','now')")
            .bind(doc).bind(kb).bind(status).execute(&pool).await.unwrap();
    }
    for (id, doc, kb) in [
        ("legacy", "doc", "kb"),
        ("other-chunk", "other-doc", "other"),
        ("failed-chunk", "failed-doc", "kb"),
    ] {
        sqlx::query("INSERT INTO kb_chunks (id,doc_id,kb_id,chunk_index,content,embedding,embedding_dim,content_hash,metadata,created_at) VALUES (?,?,?,0,?, ?,2,'original-hash','{}','now')")
            .bind(id).bind(doc).bind(kb).bind("必须记录异常⽇志，处理⽅法保留 JavaException 堆栈")
            .bind(retriever::encode_embedding(&[1.0,0.0])).execute(&pool).await.unwrap();
    }
    migrator.run(&pool).await.unwrap();
    pool
}

async fn ids(pool: &SqlitePool, query: &str) -> Vec<String> {
    retriever::keyword_only_search(pool, "kb", query, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.chunk_id)
        .collect()
}

#[tokio::test]
async fn migration_backfills_existing_cjk_without_rewriting_content_or_vectors() {
    let pool = fixture().await;
    let before: (String, Vec<u8>, String) =
        sqlx::query_as("SELECT content,embedding,content_hash FROM kb_chunks WHERE id='legacy'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let repo = KbRepository::new(pool.clone());
    assert_eq!(repo.backfill_search_text().await.unwrap(), 3);
    assert_eq!(repo.backfill_search_text().await.unwrap(), 0);
    for q in [
        "日志",
        "⽇志",
        "方法",
        "⽅法",
        "异常日志",
        "JavaException",
        "日",
    ] {
        assert_eq!(ids(&pool, q).await, vec!["legacy"], "query={q}");
    }
    assert!(ids(&pool, "，。！ OR NOT NEAR").await.is_empty());
    let after: (String, Vec<u8>, String) =
        sqlx::query_as("SELECT content,embedding,content_hash FROM kb_chunks WHERE id='legacy'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn new_chunks_and_content_updates_keep_fts_in_sync() {
    let pool = fixture().await;
    let repo = KbRepository::new(pool.clone());
    repo.create_chunk(&ChunkInsert {
        id: "new".into(),
        doc_id: "doc".into(),
        kb_id: "kb".into(),
        chunk_index: 1,
        content: "并发线程池使用有界队列".into(),
        token_count: 8,
        embedding: retriever::encode_embedding(&[0.0, 1.0]),
        embedding_dim: 2,
        metadata: "{}".into(),
        content_hash: Some("new-hash".into()),
        created_at: "now".into(),
    })
    .await
    .unwrap();
    assert_eq!(ids(&pool, "线程").await, vec!["new"]);
    sqlx::query("UPDATE kb_chunks SET content='参数绑定预防注入' WHERE id='new'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(ids(&pool, "线程").await.is_empty());
    assert_eq!(ids(&pool, "绑定").await, vec!["new"]);
    sqlx::query("DELETE FROM kb_chunks WHERE id='new'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(ids(&pool, "绑定").await.is_empty());
}

#[test]
fn source_code_literals_are_not_normalized_as_pdf_text() {
    let source = "String s = \"⽇Ａ①\";";
    match parser::parse_file("Example.java", source.as_bytes()).unwrap() {
        parser::ParsedContent::Code { text, .. } => assert_eq!(text, source),
        _ => panic!("expected code"),
    }
}
