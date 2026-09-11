use super::models::*;
use crate::db::models::now_iso;
use sqlx::SqlitePool;

pub struct KbRepository {
    pool: SqlitePool,
}

impl KbRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    // ==================== Knowledge Base ====================

    pub async fn get_all_kbs(&self) -> Result<Vec<KbKnowledgeBase>, sqlx::Error> {
        sqlx::query_as::<_, KbKnowledgeBase>(
            "SELECT * FROM kb_knowledge_bases ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_kb(&self, id: &str) -> Result<KbKnowledgeBase, sqlx::Error> {
        sqlx::query_as::<_, KbKnowledgeBase>("SELECT * FROM kb_knowledge_bases WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn create_kb(&self, input: &CreateKbInput) -> Result<KbKnowledgeBase, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        sqlx::query(
            "INSERT INTO kb_knowledge_bases (id, name, description, status, doc_count, chunk_count, total_tokens, embedding_model, embedding_channel_id, mcp_enabled, chunk_size, chunk_overlap, excluded_dirs, excluded_files, included_files, embedding_dim, index_status, embedding_batch_size, ocr_model, created_at, updated_at)
             VALUES (?, ?, ?, 1, 0, 0, 0, ?, ?, 1, 512, 64, '', '', '', 0, 'none', 32, ?, ?, ?)"
        )
        .bind(&id)
        .bind(&input.name)
        .bind(&input.description)
        .bind(&input.embedding_model)
        .bind(&input.embedding_channel_id)
        .bind(&input.ocr_model)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        self.get_kb(&id).await
    }

    pub async fn update_kb(
        &self,
        id: &str,
        input: &UpdateKbInput,
    ) -> Result<KbKnowledgeBase, sqlx::Error> {
        let now = now_iso();
        let mut q = sqlx::QueryBuilder::new("UPDATE kb_knowledge_bases SET updated_at = ");
        q.push_bind(now);

        if let Some(name) = &input.name {
            q.push(", name = ").push_bind(name);
        }
        if let Some(desc) = &input.description {
            q.push(", description = ").push_bind(desc);
        }
        if let Some(model) = &input.embedding_model {
            // 同一 UPDATE 中的表达式均读取旧值；重复保存相同模型不使缓存失效。
            for (column, changed) in [
                ("embedding_revision", "embedding_revision + 1"),
                ("embedding_dim", "0"),
                ("index_status", "'stale'"),
            ] {
                q.push(format!(", {column} = CASE WHEN COALESCE(embedding_model, 'text-embedding-3-small') != "))
                    .push_bind(model)
                    .push(format!(" THEN {changed} ELSE {column} END"));
            }
            q.push(", embedding_model = ").push_bind(model);
        }
        if let Some(ch) = &input.embedding_channel_id {
            q.push(", embedding_channel_id = ").push_bind(ch);
        }
        if let Some(status) = input.status {
            q.push(", status = ").push_bind(status);
        }
        if let Some(mcp_enabled) = input.mcp_enabled {
            q.push(", mcp_enabled = ").push_bind(mcp_enabled);
        }
        if let Some(chunk_size) = input.chunk_size {
            q.push(", chunk_size = ").push_bind(chunk_size);
        }
        if let Some(chunk_overlap) = input.chunk_overlap {
            q.push(", chunk_overlap = ").push_bind(chunk_overlap);
        }
        if let Some(excluded_dirs) = &input.excluded_dirs {
            q.push(", excluded_dirs = ").push_bind(excluded_dirs);
        }
        if let Some(excluded_files) = &input.excluded_files {
            q.push(", excluded_files = ").push_bind(excluded_files);
        }
        if let Some(included_files) = &input.included_files {
            q.push(", included_files = ").push_bind(included_files);
        }
        if let Some(embedding_batch_size) = input.embedding_batch_size {
            q.push(", embedding_batch_size = ")
                .push_bind(embedding_batch_size);
        }
        if let Some(ocr_model) = &input.ocr_model {
            q.push(", ocr_model = ").push_bind(ocr_model);
        }

        q.push(" WHERE id = ").push_bind(id);
        q.build().execute(&self.pool).await?;

        self.get_kb(id).await
    }

    pub async fn delete_kb(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM kb_knowledge_bases WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_kb_counts(&self, kb_id: &str) -> Result<(), sqlx::Error> {
        let doc_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM kb_documents WHERE kb_id = ?")
                .bind(kb_id)
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);

        let chunk_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM kb_chunks WHERE kb_id = ?")
            .bind(kb_id)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

        let total_tokens: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(token_count), 0) FROM kb_chunks WHERE kb_id = ?",
        )
        .bind(kb_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);

        let now = now_iso();
        sqlx::query("UPDATE kb_knowledge_bases SET doc_count = ?, chunk_count = ?, total_tokens = ?, updated_at = ? WHERE id = ?")
            .bind(doc_count)
            .bind(chunk_count)
            .bind(total_tokens)
            .bind(&now)
            .bind(kb_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    pub async fn update_kb_index_status(
        &self,
        kb_id: &str,
        status: &str,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE kb_knowledge_bases SET index_status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(kb_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ==================== Document ====================

    pub async fn get_documents(&self, kb_id: &str) -> Result<Vec<KbDocument>, sqlx::Error> {
        sqlx::query_as::<_, KbDocument>(
            "SELECT * FROM kb_documents WHERE kb_id = ? ORDER BY created_at DESC",
        )
        .bind(kb_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_document(&self, id: &str) -> Result<KbDocument, sqlx::Error> {
        sqlx::query_as::<_, KbDocument>("SELECT * FROM kb_documents WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn find_document_by_hash(
        &self,
        kb_id: &str,
        hash: &str,
    ) -> Result<Option<KbDocument>, sqlx::Error> {
        sqlx::query_as::<_, KbDocument>(
            "SELECT * FROM kb_documents WHERE kb_id = ? AND content_hash = ?",
        )
        .bind(kb_id)
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn create_document(
        &self,
        kb_id: &str,
        filename: &str,
        file_path: Option<&str>,
        file_type: &str,
        file_size: i64,
        content_hash: &str,
    ) -> Result<KbDocument, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        sqlx::query(
            "INSERT INTO kb_documents (id, kb_id, filename, file_path, file_type, file_size, content_hash, chunk_count, token_count, status, source_type, doc_meta, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 0, 0, 'pending', 'upload', '{}', ?, ?)"
        )
        .bind(&id)
        .bind(kb_id)
        .bind(filename)
        .bind(file_path)
        .bind(file_type)
        .bind(file_size)
        .bind(content_hash)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        self.get_document(&id).await
    }

    pub async fn create_document_with_source(
        &self,
        kb_id: &str,
        filename: &str,
        file_path: Option<&str>,
        file_type: &str,
        file_size: i64,
        content_hash: &str,
        source_type: &str,
        source_url: Option<&str>,
        source_path: Option<&str>,
    ) -> Result<KbDocument, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        sqlx::query(
            "INSERT INTO kb_documents (id, kb_id, filename, file_path, file_type, file_size, content_hash, chunk_count, token_count, status, source_type, source_url, source_path, doc_meta, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 0, 0, 'pending', ?, ?, ?, '{}', ?, ?)"
        )
        .bind(&id)
        .bind(kb_id)
        .bind(filename)
        .bind(file_path)
        .bind(file_type)
        .bind(file_size)
        .bind(content_hash)
        .bind(source_type)
        .bind(source_url)
        .bind(source_path)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        self.get_document(&id).await
    }

    pub async fn update_document_status(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query(
            "UPDATE kb_documents SET status = ?, error_message = ?, updated_at = ? WHERE id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_document_counts(
        &self,
        id: &str,
        chunk_count: i64,
        token_count: i64,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query(
            "UPDATE kb_documents SET chunk_count = ?, token_count = ?, updated_at = ? WHERE id = ?",
        )
        .bind(chunk_count)
        .bind(token_count)
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_document(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM kb_documents WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// OCR 完成后回填文档的识别信息（引擎、页数、失败页码 JSON）。
    pub async fn update_document_ocr_info(
        &self,
        id: &str,
        ocr_engine: &str,
        page_count: i64,
        failed_pages_json: &str,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query(
            "UPDATE kb_documents SET ocr_engine = ?, page_count = ?, ocr_failed_pages = ?, updated_at = ? WHERE id = ?",
        )
        .bind(ocr_engine)
        .bind(page_count)
        .bind(failed_pages_json)
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ==================== Chunk ====================

    pub async fn create_chunk(&self, chunk: &ChunkInsert) -> Result<(), sqlx::Error> {
        let mut connection = self.pool.acquire().await?;
        Self::insert_chunk(&mut connection, chunk).await
    }

    async fn insert_chunk(
        connection: &mut sqlx::SqliteConnection,
        chunk: &ChunkInsert,
    ) -> Result<(), sqlx::Error> {
        // 从 metadata JSON 中提取 symbol_name / symbol_kind
        let meta: serde_json::Value = serde_json::from_str(&chunk.metadata).unwrap_or_default();
        let symbol_name = meta.get("symbol_name").and_then(|v| v.as_str());
        let symbol_kind = meta.get("symbol_kind").and_then(|v| v.as_str());

        sqlx::query(
            "INSERT INTO kb_chunks (id, doc_id, kb_id, chunk_index, content, token_count, embedding, embedding_dim, metadata, symbol_name, symbol_kind, content_hash, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&chunk.id)
        .bind(&chunk.doc_id)
        .bind(&chunk.kb_id)
        .bind(chunk.chunk_index)
        .bind(&chunk.content)
        .bind(chunk.token_count)
        .bind(&chunk.embedding)
        .bind(chunk.embedding_dim)
        .bind(&chunk.metadata)
        .bind(symbol_name)
        .bind(symbol_kind)
        .bind(&chunk.content_hash)
        .bind(&chunk.created_at)
        .execute(connection)
        .await?;
        Ok(())
    }

    /// 新切片全部准备好后一次替换；失败时事务回滚，旧文档仍可检索。
    pub async fn replace_document_chunks(
        &self,
        doc_id: &str,
        kb_id: &str,
        chunks: &[ChunkInsert],
        ocr_info: Option<(i64, &str)>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM kb_chunks WHERE doc_id = ?")
            .bind(doc_id)
            .execute(&mut *tx)
            .await?;
        for chunk in chunks {
            Self::insert_chunk(&mut tx, chunk).await?;
        }
        let now = now_iso();
        let total_tokens: i64 = chunks.iter().map(|chunk| chunk.token_count).sum();
        sqlx::query("UPDATE kb_documents SET chunk_count = ?, token_count = ?, status = 'ready', error_message = NULL, updated_at = ? WHERE id = ? AND kb_id = ?")
            .bind(chunks.len() as i64).bind(total_tokens).bind(&now).bind(doc_id).bind(kb_id)
            .execute(&mut *tx).await?;
        if let Some((pages, failed_pages)) = ocr_info {
            sqlx::query("UPDATE kb_documents SET ocr_engine = 'vlm', page_count = ?, ocr_failed_pages = ? WHERE id = ?")
                .bind(pages).bind(failed_pages).bind(doc_id).execute(&mut *tx).await?;
        }
        let dim = chunks.first().map(|chunk| chunk.embedding_dim).unwrap_or(0);
        let revision = chunks.first()
            .and_then(|chunk| serde_json::from_str::<serde_json::Value>(&chunk.metadata).ok())
            .and_then(|metadata| metadata.get("embedding_revision").and_then(serde_json::Value::as_i64))
            .unwrap_or(0);
        sqlx::query("UPDATE kb_knowledge_bases SET doc_count = (SELECT COUNT(*) FROM kb_documents WHERE kb_id = ?), chunk_count = (SELECT COUNT(*) FROM kb_chunks WHERE kb_id = ?), total_tokens = (SELECT COALESCE(SUM(token_count), 0) FROM kb_chunks WHERE kb_id = ?), embedding_dim = CASE WHEN embedding_dim = 0 AND embedding_revision = ? THEN ? ELSE embedding_dim END, updated_at = ? WHERE id = ?")
            .bind(kb_id).bind(kb_id).bind(kb_id).bind(revision).bind(dim).bind(&now).bind(kb_id)
            .execute(&mut *tx).await?;
        tx.commit().await
    }

    /// 缓存键同时包含配置版本和内容哈希，防止同维度模型切换时复用旧向量。
    /// 将版本保留在键中，也能拒绝读取缓存之后才发生的配置变更。
    pub async fn get_chunk_hashes_by_doc(
        &self,
        doc_id: &str,
    ) -> Result<std::collections::HashMap<String, Vec<u8>>, sqlx::Error> {
        let rows: Vec<(String, Vec<u8>, i64)> = sqlx::query_as(
            "SELECT c.content_hash, c.embedding, COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0)
             FROM kb_chunks c JOIN kb_knowledge_bases kb ON c.kb_id = kb.id
             WHERE c.doc_id = ? AND c.content_hash IS NOT NULL AND c.embedding IS NOT NULL
               AND COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0) = kb.embedding_revision",
        )
        .bind(doc_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(hash, embedding, revision)| (format!("{revision}:{hash}"), embedding))
            .collect())
    }

    /// 该文档现存 chunk 的 (chunk_id, embedding)（文档 ready 且向量非空），
    /// 增量索引差集的库侧输入。
    pub async fn get_chunk_vectors_by_doc(
        &self,
        doc_id: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, sqlx::Error> {
        sqlx::query_as(
            "SELECT c.id, c.embedding FROM kb_chunks c \
             JOIN kb_documents d ON c.doc_id = d.id \
             JOIN kb_knowledge_bases kb ON c.kb_id = kb.id \
             WHERE c.doc_id = ? AND c.embedding IS NOT NULL AND d.status = 'ready' \
               AND COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0) = kb.embedding_revision",
        )
        .bind(doc_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn delete_chunks_by_doc(&self, doc_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM kb_chunks WHERE doc_id = ?")
            .bind(doc_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_chunks_by_kb(
        &self,
        kb_id: &str,
    ) -> Result<Vec<(String, String, String, Vec<u8>, String, String)>, sqlx::Error> {
        sqlx::query_as(
            "SELECT c.id, c.content, c.metadata, c.embedding, d.filename, c.doc_id
             FROM kb_chunks c
             JOIN kb_documents d ON c.doc_id = d.id
             JOIN kb_knowledge_bases kb ON c.kb_id = kb.id
             WHERE c.kb_id = ? AND c.embedding IS NOT NULL AND d.status = 'ready'
               AND COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0) = kb.embedding_revision
             ORDER BY c.id",
        )
        .bind(kb_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_chunks_by_kb_with_dim(
        &self,
        kb_id: &str,
    ) -> Result<Vec<ChunkWithEmbedding>, sqlx::Error> {
        sqlx::query_as(
            "SELECT c.id, c.content, c.metadata, c.embedding, c.embedding_dim, d.filename, c.doc_id
             FROM kb_chunks c
             JOIN kb_documents d ON c.doc_id = d.id
             JOIN kb_knowledge_bases kb ON c.kb_id = kb.id
             WHERE c.kb_id = ? AND c.embedding IS NOT NULL AND d.status = 'ready'
               AND COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0) = kb.embedding_revision",
        )
        .bind(kb_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_chunk_count_by_kb(&self, kb_id: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM kb_chunks WHERE kb_id = ? AND embedding IS NOT NULL",
        )
        .bind(kb_id)
        .fetch_one(&self.pool)
        .await
    }

    // ==================== Task ====================

    pub async fn create_task(
        &self,
        kb_id: &str,
        doc_id: Option<&str>,
        task_type: &str,
        total_items: i64,
    ) -> Result<KbTask, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        sqlx::query(
            "INSERT INTO kb_tasks (id, kb_id, doc_id, task_type, status, progress, total_items, done_items, created_at)
             VALUES (?, ?, ?, ?, 'running', 0, ?, 0, ?)"
        )
        .bind(&id)
        .bind(kb_id)
        .bind(doc_id)
        .bind(task_type)
        .bind(total_items)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        sqlx::query_as::<_, KbTask>("SELECT * FROM kb_tasks WHERE id = ?")
            .bind(&id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn update_task_progress(
        &self,
        id: &str,
        done_items: i64,
        progress: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE kb_tasks SET done_items = ?, progress = ? WHERE id = ?")
            .bind(done_items)
            .bind(progress)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn complete_task(&self, id: &str, error: Option<&str>) -> Result<(), sqlx::Error> {
        let now = now_iso();
        let status = if error.is_some() {
            "failed"
        } else {
            "completed"
        };
        sqlx::query("UPDATE kb_tasks SET status = ?, error_message = ?, progress = 100, completed_at = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_tasks(&self, kb_id: &str) -> Result<Vec<KbTask>, sqlx::Error> {
        sqlx::query_as::<_, KbTask>(
            "SELECT * FROM kb_tasks WHERE kb_id = ? ORDER BY created_at DESC LIMIT 20",
        )
        .bind(kb_id)
        .fetch_all(&self.pool)
        .await
    }

    // ==================== Conversation History ====================

    pub async fn get_conversations(&self, kb_id: &str) -> Result<Vec<KbConversation>, sqlx::Error> {
        sqlx::query_as::<_, KbConversation>(
            "SELECT * FROM kb_conversations WHERE kb_id = ? ORDER BY created_at ASC",
        )
        .bind(kb_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn add_conversation(
        &self,
        kb_id: &str,
        role: &str,
        content: &str,
        sources: Option<&str>,
        model: Option<&str>,
        tokens_used: i64,
    ) -> Result<(), sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        sqlx::query(
            "INSERT INTO kb_conversations (id, kb_id, role, content, sources, model, tokens_used, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(kb_id)
        .bind(role)
        .bind(content)
        .bind(sources)
        .bind(model)
        .bind(tokens_used)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_conversations(&self, kb_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM kb_conversations WHERE kb_id = ?")
            .bind(kb_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ==================== Sources ====================

    pub async fn get_sources(&self, kb_id: &str) -> Result<Vec<KbSource>, sqlx::Error> {
        sqlx::query_as::<_, KbSource>(
            "SELECT * FROM kb_sources WHERE kb_id = ? ORDER BY created_at DESC",
        )
        .bind(kb_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn create_source(
        &self,
        kb_id: &str,
        source_type: &str,
        source_url: Option<&str>,
        source_path: Option<&str>,
        branch: Option<&str>,
    ) -> Result<KbSource, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        sqlx::query(
            "INSERT INTO kb_sources (id, kb_id, source_type, source_url, source_path, branch, status, file_count, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, 'fetching', 0, ?, ?)"
        )
        .bind(&id)
        .bind(kb_id)
        .bind(source_type)
        .bind(source_url)
        .bind(source_path)
        .bind(branch)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        sqlx::query_as::<_, KbSource>("SELECT * FROM kb_sources WHERE id = ?")
            .bind(&id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn update_source_status(
        &self,
        id: &str,
        status: &str,
        file_count: i64,
        error: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query("UPDATE kb_sources SET status = ?, file_count = ?, error = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(file_count)
            .bind(error)
            .bind(&now)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_source(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM kb_sources WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ==================== Index Meta ====================

    pub async fn get_index_meta(&self, kb_id: &str) -> Result<Option<KbIndexMeta>, sqlx::Error> {
        sqlx::query_as::<_, KbIndexMeta>("SELECT * FROM kb_index_meta WHERE kb_id = ?")
            .bind(kb_id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn upsert_index_meta(
        &self,
        kb_id: &str,
        dim: i64,
        chunk_count: i64,
        index_path: Option<&str>,
        status: &str,
    ) -> Result<(), sqlx::Error> {
        let now = now_iso();
        sqlx::query(
            "INSERT INTO kb_index_meta (kb_id, index_type, embedding_dim, chunk_count, index_path, built_at, status)
             VALUES (?, 'hnsw', ?, ?, ?, ?, ?)
             ON CONFLICT(kb_id) DO UPDATE SET embedding_dim = ?, chunk_count = ?, index_path = ?, built_at = ?, status = ?"
        )
        .bind(kb_id)
        .bind(dim)
        .bind(chunk_count)
        .bind(index_path)
        .bind(&now)
        .bind(status)
        .bind(dim)
        .bind(chunk_count)
        .bind(index_path)
        .bind(&now)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

pub struct ChunkInsert {
    pub id: String,
    pub doc_id: String,
    pub kb_id: String,
    pub chunk_index: i64,
    pub content: String,
    pub token_count: i64,
    pub embedding: Vec<u8>,
    pub embedding_dim: i64,
    pub metadata: String,
    /// chunk 内容 SHA-256（C-06/R1 哈希复用）；旧行为 None
    pub content_hash: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChunkWithEmbedding {
    pub id: String,
    pub content: String,
    pub metadata: String,
    pub embedding: Vec<u8>,
    pub embedding_dim: i64,
    pub filename: String,
    pub doc_id: String,
}
