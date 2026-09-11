//! API Key 的知识库授权存储。

pub async fn get_grants(pool: &sqlx::SqlitePool, id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT kb_id FROM api_key_knowledge_access WHERE api_key_id = ? ORDER BY kb_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}

pub async fn set_grants(
    pool: &sqlx::SqlitePool,
    id: &str,
    kb_ids: &[String],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    // 即使撤销全部授权，也先检查 Key 存在；外键保证不存在的 KB 不会留下半次更新。
    sqlx::query_scalar::<_, String>("SELECT id FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM api_key_knowledge_access WHERE api_key_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for kb_id in kb_ids.iter().collect::<std::collections::BTreeSet<_>>() {
        sqlx::query("INSERT INTO api_key_knowledge_access (api_key_id, kb_id) VALUES (?, ?)")
            .bind(id)
            .bind(kb_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}
