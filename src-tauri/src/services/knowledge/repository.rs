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
        // 新知识库默认授权全部已有密钥；事务保证授权失败时创建也回滚。
        let mut tx = self.pool.begin().await?;
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
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO api_key_knowledge_access (api_key_id, kb_id)
             SELECT id, ? FROM api_keys",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;

        let kb =
            sqlx::query_as::<_, KbKnowledgeBase>("SELECT * FROM kb_knowledge_bases WHERE id = ?")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(kb)
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

    /// 已失败的摄入不构成有效重复，允许修复配置后重新导入相同内容。
    pub async fn find_document_by_hash(
        &self,
        kb_id: &str,
        hash: &str,
    ) -> Result<Option<KbDocument>, sqlx::Error> {
        sqlx::query_as::<_, KbDocument>(
            "SELECT * FROM kb_documents WHERE kb_id = ? AND content_hash = ? AND status != 'failed'",
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
            "INSERT INTO kb_chunks (id, doc_id, kb_id, chunk_index, content, token_count, embedding, embedding_dim, metadata, symbol_name, symbol_kind, content_hash, created_at, search_text, search_text_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
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
        .bind(super::text::search_projection(&chunk.content))
        .bind(super::text::SEARCH_PROJECTION_VERSION)
        .execute(connection)
        .await?;
        Ok(())
    }

    /// 升级旧数据/正文变更后补建检索投影。待升级版本和 NULL 共用部分索引。
    /// 分批提交可中断续跑；CAS 防止覆盖并发修改，正文、哈希和向量均不变。
    pub async fn backfill_search_text(&self) -> Result<u64, sqlx::Error> {
        self.backfill_search_text_scope(None).await
    }

    /// 检索只等待当前知识库的遗留投影，不让其他库的积压进入请求热路径。
    pub async fn backfill_search_text_for_kb(&self, kb_id: &str) -> Result<u64, sqlx::Error> {
        self.backfill_search_text_scope(Some(kb_id)).await
    }

    async fn backfill_search_text_scope(&self, kb_id: Option<&str>) -> Result<u64, sqlx::Error> {
        let mut updated = 0;
        loop {
            let rows: Vec<(String, String, Option<String>, i64)> = if let Some(kb_id) = kb_id {
                sqlx::query_as("SELECT id, content, search_text, search_text_version FROM kb_chunks WHERE kb_id = ? AND (search_text IS NULL OR search_text_version < 2) ORDER BY id LIMIT 128")
                    .bind(kb_id).fetch_all(&self.pool).await?
            } else {
                sqlx::query_as("SELECT id, content, search_text, search_text_version FROM kb_chunks WHERE search_text IS NULL OR search_text_version < 2 ORDER BY id LIMIT 128")
                    .fetch_all(&self.pool).await?
            };
            if rows.is_empty() {
                return Ok(updated);
            }
            let mut tx = self.pool.begin().await?;
            for (id, content, old_projection, old_version) in rows {
                updated += Self::update_search_projection(
                    &mut tx,
                    &id,
                    &content,
                    old_projection.as_deref(),
                    old_version,
                )
                .await?;
            }
            tx.commit().await?;
        }
    }

    async fn update_search_projection(
        connection: &mut sqlx::SqliteConnection,
        id: &str,
        content: &str,
        old_projection: Option<&str>,
        old_version: i64,
    ) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "UPDATE kb_chunks SET search_text = ?, search_text_version = ? WHERE id = ? AND content = ? AND search_text IS ? AND search_text_version = ?",
        )
        .bind(super::text::search_projection(content))
        .bind(super::text::SEARCH_PROJECTION_VERSION)
        .bind(id)
        .bind(content)
        .bind(old_projection)
        .bind(old_version)
        .execute(connection)
        .await?
        .rows_affected())
    }

    /// 新切片全部准备好后一次替换；失败时事务回滚，旧文档仍可检索。
    pub async fn replace_document_chunks(
        &self,
        doc_id: &str,
        kb_id: &str,
        chunks: &[ChunkInsert],
        ocr_info: Option<(i64, &str)>,
    ) -> Result<(), sqlx::Error> {
        self.replace_document_chunks_with_pdf_info(doc_id, kb_id, chunks, ocr_info, None)
            .await
    }

    /// PDF 页数和文字层质量与新切片一起发布；普通文档继续使用原入口。
    pub async fn replace_document_chunks_with_pdf_info(
        &self,
        doc_id: &str,
        kb_id: &str,
        chunks: &[ChunkInsert],
        ocr_info: Option<(i64, &str)>,
        pdf_info: Option<&super::parser::PdfTextExtraction>,
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
        if let Some(info) = pdf_info {
            Self::write_pdf_info(&mut tx, doc_id, info, false).await?;
            if ocr_info.is_none() {
                // 普通文字层重处理成功后不能沿用先前的 OCR 标签。
                sqlx::query("UPDATE kb_documents SET ocr_engine = NULL, ocr_failed_pages = '[]' WHERE id = ?")
                    .bind(doc_id).execute(&mut *tx).await?;
            }
        }
        let dim = chunks.first().map(|chunk| chunk.embedding_dim).unwrap_or(0);
        let revision = chunks
            .first()
            .and_then(|chunk| serde_json::from_str::<serde_json::Value>(&chunk.metadata).ok())
            .and_then(|metadata| {
                metadata
                    .get("embedding_revision")
                    .and_then(serde_json::Value::as_i64)
            })
            .unwrap_or(0);
        sqlx::query("UPDATE kb_knowledge_bases SET doc_count = (SELECT COUNT(*) FROM kb_documents WHERE kb_id = ?), chunk_count = (SELECT COUNT(*) FROM kb_chunks WHERE kb_id = ?), total_tokens = (SELECT COALESCE(SUM(token_count), 0) FROM kb_chunks WHERE kb_id = ?), embedding_dim = CASE WHEN embedding_dim = 0 AND embedding_revision = ? THEN ? ELSE embedding_dim END, updated_at = ? WHERE id = ?")
            .bind(kb_id).bind(kb_id).bind(kb_id).bind(revision).bind(dim).bind(&now).bind(kb_id)
            .execute(&mut *tx).await?;
        tx.commit().await
    }

    /// 首次导入失败也可查看文字层质量；就绪文档只能在切片替换事务中更新。
    pub async fn update_document_pdf_info_if_unready(
        &self,
        doc_id: &str,
        info: &super::parser::PdfTextExtraction,
    ) -> Result<(), sqlx::Error> {
        let mut connection = self.pool.acquire().await?;
        Self::write_pdf_info(&mut connection, doc_id, info, true).await
    }

    async fn write_pdf_info(
        connection: &mut sqlx::SqliteConnection,
        doc_id: &str,
        info: &super::parser::PdfTextExtraction,
        only_unready: bool,
    ) -> Result<(), sqlx::Error> {
        let json = serde_json::to_string(info).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        sqlx::query("UPDATE kb_documents SET page_count = ?, doc_meta = json_set(CASE WHEN json_valid(doc_meta) THEN CASE WHEN json_type(doc_meta) = 'object' THEN doc_meta ELSE '{}' END ELSE '{}' END, '$.pdf_text_extraction', json(?)), updated_at = ? WHERE id = ? AND (? = 0 OR status <> 'ready')")
            .bind(info.page_count as i64).bind(json).bind(now_iso()).bind(doc_id).bind(only_unready)
            .execute(connection).await?;
        Ok(())
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

    /// 仅返回完整有效切片集合及维度，不把正文、metadata 或向量 BLOB 传回应用。
    /// 调用方在同一个读事务中校验索引并读取候选，避免两次读取看到不同版本。
    pub async fn search_chunk_identities(
        connection: &mut sqlx::SqliteConnection,
        kb_id: &str,
    ) -> Result<Vec<SearchChunkIdentity>, sqlx::Error> {
        sqlx::query_as(
            "SELECT c.id, c.embedding_dim, length(c.embedding) AS embedding_bytes,
                    kb.embedding_dim AS expected_dim, kb.index_status
             FROM kb_chunks c
             JOIN kb_documents d ON c.doc_id = d.id AND d.kb_id = c.kb_id
             JOIN kb_knowledge_bases kb ON c.kb_id = kb.id
             WHERE c.kb_id = ? AND c.embedding IS NOT NULL AND d.status = 'ready'
               AND COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0) = kb.embedding_revision
             ORDER BY c.id",
        )
        .bind(kb_id)
        .fetch_all(connection)
        .await
    }

    /// 正常 HNSW 路径只读取候选正文；分批 IN 保持 SQLite 参数数有界。
    pub async fn search_chunks_by_ids(
        connection: &mut sqlx::SqliteConnection,
        kb_id: &str,
        ids: &[String],
    ) -> Result<Vec<SearchChunkContent>, sqlx::Error> {
        let mut rows = Vec::with_capacity(ids.len());
        for batch in ids.chunks(128) {
            let mut query = sqlx::QueryBuilder::new(
                "SELECT c.id, c.content, c.metadata, d.filename, c.doc_id
                 FROM kb_chunks c
                 JOIN kb_documents d ON c.doc_id = d.id AND d.kb_id = c.kb_id
                 JOIN kb_knowledge_bases kb ON c.kb_id = kb.id
                 WHERE c.embedding IS NOT NULL AND d.status = 'ready'
                   AND COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0) = kb.embedding_revision
                   AND c.kb_id = ",
            );
            query.push_bind(kb_id).push(" AND c.id IN (");
            let mut separated = query.separated(", ");
            for id in batch {
                separated.push_bind(id);
            }
            separated.push_unseparated(")");
            rows.extend(
                query
                    .build_query_as::<SearchChunkContent>()
                    .fetch_all(&mut *connection)
                    .await?,
            );
        }
        Ok(rows)
    }

    /// 索引缺失或过期时的完整精确扫描输入，不使用 LIMIT 截断候选。
    pub async fn search_vector_chunks(
        connection: &mut sqlx::SqliteConnection,
        kb_id: &str,
    ) -> Result<Vec<ChunkWithEmbedding>, sqlx::Error> {
        sqlx::query_as(
            "SELECT c.id, c.content, c.metadata, c.embedding, c.embedding_dim, d.filename, c.doc_id
             FROM kb_chunks c
             JOIN kb_documents d ON c.doc_id = d.id AND d.kb_id = c.kb_id
             JOIN kb_knowledge_bases kb ON c.kb_id = kb.id
             WHERE c.kb_id = ? AND c.embedding IS NOT NULL AND d.status = 'ready'
               AND COALESCE(json_extract(c.metadata, '$.embedding_revision'), 0) = kb.embedding_revision
               AND (kb.embedding_dim = 0 OR c.embedding_dim = 0 OR c.embedding_dim = kb.embedding_dim)
             ORDER BY c.id",
        )
        .bind(kb_id)
        .fetch_all(connection)
        .await
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

/// HNSW 完整集合校验使用的轻量行。
#[derive(Debug, sqlx::FromRow)]
pub struct SearchChunkIdentity {
    pub id: String,
    pub embedding_dim: i64,
    pub embedding_bytes: i64,
    pub expected_dim: i64,
    pub index_status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct SearchChunkContent {
    pub id: String,
    pub content: String,
    pub metadata: String,
    pub filename: String,
    pub doc_id: String,
}

#[cfg(test)]
mod projection_cas_tests {
    use super::*;

    #[tokio::test]
    async fn stale_projection_snapshot_cannot_overwrite_new_content_or_projection() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO kb_knowledge_bases (id,name,created_at,updated_at) VALUES ('kb','kb','now','now')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO kb_documents (id,kb_id,filename,file_type,content_hash,status,created_at,updated_at) VALUES ('doc','kb','rules.pdf','pdf','hash','ready','now','now')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO kb_chunks (id,doc_id,kb_id,chunk_index,content,created_at,search_text,search_text_version) VALUES ('chunk','doc','kb',0,'旧分⻚规范','now','old',1)")
            .execute(&pool).await.unwrap();
        let mut connection = pool.acquire().await.unwrap();
        // 回填 SELECT 之后，另一个写入先改了正文；使用真实 CAS 更新函数核验竞争。
        sqlx::query("UPDATE kb_chunks SET content='新⻓度规范' WHERE id='chunk'")
            .execute(&mut *connection)
            .await
            .unwrap();
        assert_eq!(
            KbRepository::update_search_projection(
                &mut connection,
                "chunk",
                "旧分⻚规范",
                Some("old"),
                1
            )
            .await
            .unwrap(),
            0
        );
        let pending: (String, Option<String>, i64) = sqlx::query_as(
            "SELECT content,search_text,search_text_version FROM kb_chunks WHERE id='chunk'",
        )
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        assert_eq!(pending, ("新⻓度规范".into(), None, 0));
        // 正文未变、另一回填已升级版本时，旧快照也不能覆盖较新投影。
        sqlx::query(
            "UPDATE kb_chunks SET search_text='newer',search_text_version=3 WHERE id='chunk'",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        assert_eq!(
            KbRepository::update_search_projection(&mut connection, "chunk", "新⻓度规范", None, 0)
                .await
                .unwrap(),
            0
        );
        let latest: (String, i64) = sqlx::query_as(
            "SELECT search_text,search_text_version FROM kb_chunks WHERE id='chunk'",
        )
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        assert_eq!(latest, ("newer".into(), 3));
    }
}
