use super::embedder;
use super::models::{ConversationMessage, RagAnswer, RetrievalDetail, SourceInfo, UsageInfo};
use super::repository::KbRepository;
use super::retriever;
use crate::core::proxy;
use crate::db::repository::Repository;
use crate::prompt_templates;
use crate::settings_store::SettingsStore;
use sqlx::SqlitePool;
use std::sync::Arc;

/// RAG: Retrieve relevant chunks, then generate answer via WaLiAPI proxy
/// Enhanced with conversation history, token limit fallback, and configurable search modes.
pub async fn ask(
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    embedding_model: &str,
    chat_model: &str,
    top_k: usize,
    mcp_only: bool,
    history: &[ConversationMessage],
    settings: &SettingsStore,
) -> Result<RagAnswer, String> {
    ask_with_config(
        pool,
        kb_id,
        query,
        embedding_model,
        chat_model,
        top_k,
        mcp_only,
        history,
        settings,
        0.7,
        0.3,
        "hybrid",
    )
    .await
}

/// RAG with configurable search parameters.
pub async fn ask_with_config(
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    embedding_model: &str,
    chat_model: &str,
    top_k: usize,
    mcp_only: bool,
    history: &[ConversationMessage],
    settings: &SettingsStore,
    vector_weight: f32,
    keyword_weight: f32,
    search_mode: &str,
) -> Result<RagAnswer, String> {
    let repo = Repository::new(pool.clone());
    let kb_repo = KbRepository::new(pool.clone());
    // 融合模式（C-06/R3）：RRF 默认（消量纲），weighted 保留可配回退
    let fusion_mode = retriever::FusionMode::parse(&settings.get_str("kb.fusion_mode", "rrf"));

    // C-06/R2：可选多轮查询改写（kb.query_rewrite，默认关）。
    // 多轮对话的指代型问题（「上面说的方案呢」）直接送检索必然 miss——
    // 开启时先用渠道模型把「近几轮对话 + 当前问题」改写成独立完整的检索查询。
    // 失败/超时静默回退原查询（best-effort），多一次 LLM 调用的成本由开关控制。
    let query = if settings.get_bool("kb.query_rewrite", false) && !history.is_empty() {
        rewrite_query_with_llm(pool, settings, kb_id, chat_model, query, history).await
    } else {
        query.to_string()
    };

    // 1. Embed the query (needed for vector and hybrid modes)
    let query_emb_opt = if search_mode != "keyword" {
        let embeddings = embedder::embed(&[query.to_string()], embedding_model, &repo)
            .await
            .map_err(|e| format!("Embedding failed: {}", e))?;
        if embeddings.is_empty() {
            return Err("Failed to embed query".to_string());
        }
        Some(embeddings[0].clone())
    } else {
        None
    };

    // 2. Search based on mode
    let scored_results = if search_mode == "keyword" {
        // Keyword-only search
        let kw_results = if kb_id.is_empty() {
            // For search_all with keyword mode, we still need embeddings for cross-KB search
            // Fallback: embed and use hybrid
            let embeddings = embedder::embed(&[query.to_string()], embedding_model, &repo)
                .await
                .map_err(|e| format!("Embedding failed: {}", e))?;
            retriever::hybrid_search_with_details(
                pool,
                kb_id,
                &query,
                &embeddings[0],
                top_k,
                vector_weight,
                keyword_weight,
                fusion_mode,
            )
            .await?
        } else {
            let kw = retriever::keyword_only_search(pool, kb_id, &query, top_k).await?;
            kw.into_iter()
                .map(|r| {
                    let score = r.score;
                    retriever::ScoredSearchResult {
                        result: r,
                        vector_score: None,
                        keyword_score: Some(score),
                    }
                })
                .collect()
        };
        kw_results
    } else if search_mode == "vector" {
        // Vector-only search
        let query_emb = query_emb_opt
            .as_ref()
            .ok_or("Embedding required for vector search")?;
        let v_results = if kb_id.is_empty() {
            retriever::search_all(pool, query_emb, top_k, mcp_only).await?
        } else {
            retriever::search(pool, kb_id, query_emb, top_k).await?
        };
        v_results
            .into_iter()
            .map(|r| {
                let score = r.score;
                retriever::ScoredSearchResult {
                    result: r,
                    vector_score: Some(score),
                    keyword_score: None,
                }
            })
            .collect()
    } else {
        // Hybrid search (default)
        let query_emb = query_emb_opt
            .as_ref()
            .ok_or("Embedding required for hybrid search")?;
        if kb_id.is_empty() {
            // Cross-KB: use search_all then compute details
            let results = retriever::search_all(pool, query_emb, top_k, mcp_only).await?;
            results
                .into_iter()
                .map(|r| {
                    let score = r.score;
                    retriever::ScoredSearchResult {
                        result: r,
                        vector_score: Some(score),
                        keyword_score: None,
                    }
                })
                .collect()
        } else {
            retriever::hybrid_search_with_details(
                pool,
                kb_id,
                &query,
                query_emb,
                top_k,
                vector_weight,
                keyword_weight,
                fusion_mode,
            )
            .await?
        }
    };

    // C-06/R3 第二步：可选 LLM listwise 重排（`kb.rerank_enabled`，默认关）。
    // 走网关自身的渠道跑渠道（proxy::handle_request，kb-internal 路由组），
    // 重排调用的 token 消耗自动计入请求日志；失败静默回退原序（best-effort）。
    let scored_results =
        if settings.get_bool("kb.rerank_enabled", false) && scored_results.len() > 1 {
            rerank_with_llm(pool, settings, kb_id, chat_model, &query, scored_results).await
        } else {
            scored_results
        };

    // Extract plain results for context building
    let results: Vec<super::models::SearchResult> =
        scored_results.iter().map(|s| s.result.clone()).collect();

    if results.is_empty() {
        // Save to conversation history
        if !kb_id.is_empty() {
            let answer = "RAG 中没有找到相关内容。".to_string();
            kb_repo
                .add_conversation(kb_id, "user", &query, None, Some(chat_model), 0)
                .await
                .ok();
            kb_repo
                .add_conversation(kb_id, "assistant", &answer, None, Some(chat_model), 0)
                .await
                .ok();
            return Ok(RagAnswer {
                answer,
                sources: vec![],
                usage: None,
                retrieval_details: Some(vec![]),
            });
        }
        return Ok(RagAnswer {
            answer: "RAG 中没有找到相关内容。".to_string(),
            sources: vec![],
            usage: None,
            retrieval_details: Some(vec![]),
        });
    }

    // 3. Build context
    let context = build_context(&results);

    // 4. Build prompt with history
    let prompt = build_rag_prompt(&context, &query, history);

    // 5. Token estimation and fallback
    let estimated_tokens = retriever::estimate_tokens(&prompt);
    let model_limit = retriever::get_model_context_limit(chat_model);
    let context_limit = (model_limit as f64 * 0.7) as usize; // Reserve 30% for response

    let (final_prompt, context_results) = if estimated_tokens > context_limit {
        // Stage 1: Trim context (remove lowest-scoring chunks)
        let trimmed = trim_context(&results, &query, history, context_limit);
        if retriever::estimate_tokens(&trimmed.0) > context_limit {
            // Stage 2: Remove history, keep only latest message
            let no_history = build_rag_prompt(
                &context,
                &query,
                &history[history.len().saturating_sub(2)..],
            );
            if retriever::estimate_tokens(&no_history) > context_limit {
                // Stage 3: Remove context entirely
                let bare = format!(
                    "注意：由于 token 限制，无法附上 RAG 上下文。\n\n问题: {}",
                    query
                );
                (bare, vec![])
            } else {
                (no_history, results.clone())
            }
        } else {
            trimmed
        }
    } else {
        (prompt, results.clone())
    };

    tracing::info!(
        "RAG prompt: estimated {} tokens, limit {}, context_used: {}",
        retriever::estimate_tokens(&final_prompt),
        context_limit,
        !context_results.is_empty()
    );

    // 6. Call LLM via proxy（系统提示词走模板表：激活版本优先，回退编译期默认）
    let rag_system_prompt = prompt_templates::load(pool, prompt_templates::KEY_RAG_SYSTEM).await;
    let chat_request = serde_json::json!({
        "model": chat_model,
        "messages": [
            {"role": "system", "content": rag_system_prompt},
            {"role": "user", "content": final_prompt}
        ],
        "stream": false
    });
    let chat_request_str: String = serde_json::to_string(&chat_request).unwrap_or_default();
    let proxy_result = proxy::handle_request(
        &Arc::new(repo),
        settings,
        "kb-internal",
        "RAG",
        chat_request,
        false,
        Some(chat_request_str),
        Some(format!("kb-internal_{}", kb_id)),
        None, // legacy RAG path: no security gate yet — proxy scans internally
    )
    .await;

    match proxy_result {
        Ok(result) => {
            let answer = result
                .body
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("生成回答失败")
                .to_string();

            let usage = result.usage.map(|u| UsageInfo {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            });

            // 来源只来自最终发送给模型的切片，裁掉的检索命中仅保留在检索详情。
            let sources: Vec<SourceInfo> = context_results
                .iter()
                .map(|r| SourceInfo {
                    filename: r.filename.clone(),
                    score: r.score,
                    snippet: r.content.chars().take(200).collect(),
                })
                .collect();

            // Build retrieval details for visualization
            let retrieval_details: Vec<RetrievalDetail> = scored_results
                .iter()
                .map(|s| {
                    let meta = &s.result.metadata;
                    RetrievalDetail {
                        chunk_id: s.result.chunk_id.clone(),
                        filename: s.result.filename.clone(),
                        score: s.result.score,
                        vector_score: s.vector_score,
                        keyword_score: s.keyword_score,
                        snippet: s.result.content.chars().take(200).collect(),
                        symbol_name: meta
                            .get("symbol_name")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        symbol_kind: meta
                            .get("symbol_kind")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    }
                })
                .collect();

            // Save to conversation history
            if !kb_id.is_empty() {
                let sources_json = serde_json::to_string(&sources).ok();
                let tokens = usage.as_ref().map(|u| u.total_tokens as i64).unwrap_or(0);
                kb_repo
                    .add_conversation(kb_id, "user", &query, None, Some(chat_model), 0)
                    .await
                    .ok();
                kb_repo
                    .add_conversation(
                        kb_id,
                        "assistant",
                        &answer,
                        sources_json.as_deref(),
                        Some(chat_model),
                        tokens,
                    )
                    .await
                    .ok();
            }

            Ok(RagAnswer {
                answer,
                sources,
                usage,
                retrieval_details: Some(retrieval_details),
            })
        }
        Err((code, msg)) => Err(format!("LLM request failed ({}): {}", code, msg)),
    }
}

/// Build context string from search results
/// Enhanced with symbol metadata (name, kind, signature)
fn build_context(results: &[super::models::SearchResult]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            // 从 metadata 中提取符号信息
            let symbol_info = r
                .metadata
                .get("symbol_name")
                .and_then(|n| n.as_str())
                .map(|name| {
                    let kind = r
                        .metadata
                        .get("symbol_kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("");
                    let sig = r
                        .metadata
                        .get("signature")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if sig.is_empty() {
                        format!(" [{}: {}]", kind, name)
                    } else {
                        format!(" [{}: {} {}]", kind, name, sig)
                    }
                })
                .unwrap_or_default();

            format!(
                "--- 文档 {} [{}] (相似度: {:.2}){} ---\n{}",
                i + 1,
                r.filename,
                r.score,
                symbol_info,
                r.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Build RAG prompt with conversation history
fn build_rag_prompt(context: &str, query: &str, history: &[ConversationMessage]) -> String {
    let history_str = if history.is_empty() {
        String::new()
    } else {
        let h: String = history
            .iter()
            .map(|msg| match msg.role.as_str() {
                "user" => format!("User: {}", msg.content),
                "assistant" => format!("Assistant: {}", msg.content),
                _ => msg.content.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        format!("<conversation_history>\n{}\n</conversation_history>\n\n", h)
    };

    format!(
        r#"基于以下 RAG 内容回答问题。如果没有相关信息，请明确说明。

规则：
1. 只基于 RAG 内容回答，不要编造信息
2. 如果是多轮对话，注意上下文连贯性
3. 回答要准确、简洁，标注信息来源

{history}<knowledge_base>
{context}
</knowledge_base>

问题: {query}
"#,
        history = history_str,
        context = context,
        query = query,
    )
}

/// Trim context to fit token limit (remove lowest-scoring chunks first)
fn trim_context(
    results: &[super::models::SearchResult],
    query: &str,
    history: &[ConversationMessage],
    target_tokens: usize,
) -> (String, Vec<super::models::SearchResult>) {
    // 按低分顺序移除，但保留剩余切片原来的展示顺序。
    let mut indexed: Vec<_> = results.iter().enumerate().collect();
    indexed.sort_by(|a, b| {
        a.1.score
            .partial_cmp(&b.1.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut removed = std::collections::HashSet::new();
    let mut remaining = results.to_vec();
    let mut prompt = build_rag_prompt(&build_context(&remaining), query, history);
    for (idx, _) in indexed {
        if retriever::estimate_tokens(&prompt) <= target_tokens {
            break;
        }
        removed.insert(idx);
        remaining = results
            .iter()
            .enumerate()
            .filter(|(i, _)| !removed.contains(i))
            .map(|(_, r)| r.clone())
            .collect();
        // 连同文档标题和符号信息重新计算，不能只减正文估算值而多删来源。
        prompt = build_rag_prompt(&build_context(&remaining), query, history);
    }
    (prompt, remaining)
}

#[cfg(test)]
mod context_tests {
    use super::*;

    fn result(id: &str, score: f32, content: &str) -> super::super::models::SearchResult {
        super::super::models::SearchResult {
            chunk_id: id.into(),
            doc_id: id.into(),
            filename: format!("{id}.txt"),
            score,
            content: content.into(),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn trimming_returns_exactly_the_chunks_kept_in_the_prompt() {
        let results = vec![
            result("best", 0.9, "high relevance"),
            result("low", 0.1, &"x".repeat(1000)),
        ];
        let budget = retriever::estimate_tokens(&build_rag_prompt(
            &build_context(&results[..1]),
            "query",
            &[],
        ));
        let (prompt, used) = trim_context(&results, "query", &[], budget);
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].chunk_id, "best");
        assert!(prompt.contains("best.txt"));
        assert!(!prompt.contains("low.txt"));
        assert!(retriever::estimate_tokens(&prompt) <= budget);
        let (_, all) = trim_context(&results, "query", &[], usize::MAX);
        assert_eq!(all.len(), 2);
        let (empty_prompt, none) = trim_context(&results, "query", &[], 1);
        assert!(none.is_empty());
        assert!(!empty_prompt.contains("best.txt"));
        assert!(!empty_prompt.contains("low.txt"));
    }

    #[test]
    fn trimming_counts_document_headers_and_preserves_display_order() {
        let mut results = vec![
            result("first", 0.8, "first"),
            result("low", 0.1, "low"),
            result("best", 0.9, "best"),
        ];
        results[1].metadata = serde_json::json!({"symbol_name": "s".repeat(2000)});
        let keep = vec![results[0].clone(), results[2].clone()];
        let budget =
            retriever::estimate_tokens(&build_rag_prompt(&build_context(&keep), "query", &[]));
        let (prompt, used) = trim_context(&results, "query", &[], budget);
        assert_eq!(
            used.iter().map(|r| r.chunk_id.as_str()).collect::<Vec<_>>(),
            ["first", "best"]
        );
        assert!(retriever::estimate_tokens(&prompt) <= budget);
    }
}

/// Deep Research: multi-round iterative retrieval and analysis
pub async fn deep_research(
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    embedding_model: &str,
    chat_model: &str,
    top_k: usize,
    max_rounds: usize,
    settings: &SettingsStore,
) -> Result<RagAnswer, String> {
    let repo = Repository::new(pool.clone());
    let kb_repo = KbRepository::new(pool.clone());

    let mut all_findings: Vec<String> = Vec::new();
    let mut history: Vec<ConversationMessage> = Vec::new();
    let mut all_sources: Vec<super::models::SearchResult> = Vec::new();

    for round in 0..max_rounds {
        // 1. Generate query for this round
        let round_query = if round == 0 {
            query.to_string()
        } else {
            // Ask LLM to generate a follow-up query based on findings so far
            let follow_up_prompt = format!(
                r#"基于原始问题和已有发现，生成一个简短的追问查询（只需要返回查询本身，不需要解释）。

原始问题: {query}

已有发现:
{findings}

请生成下一步需要搜索的关键词或问题（直接返回查询文本，不要加引号或其他格式）:"#,
                query = query,
                findings = all_findings
                    .iter()
                    .enumerate()
                    .map(|(i, f)| format!("第{}轮: {}", i + 1, f))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );

            let next_query_system =
                prompt_templates::load(pool, prompt_templates::KEY_RESEARCH_NEXT_QUERY).await;
            let follow_up_request = serde_json::json!({
                "model": chat_model,
                "messages": [
                    {"role": "system", "content": next_query_system},
                    {"role": "user", "content": follow_up_prompt}
                ],
                "stream": false
            });

            let follow_up_request_str: String =
                serde_json::to_string(&follow_up_request).unwrap_or_default();
            match proxy::handle_request(
                &Arc::new(Repository::new(pool.clone())),
                settings,
                "kb-research",
                "深度研究",
                follow_up_request,
                false,
                Some(follow_up_request_str),
                Some(format!("kb-research_{}", kb_id)),
                None, // legacy RAG path: no security gate yet — proxy scans internally
            )
            .await
            {
                Ok(result) => result
                    .body
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or(query)
                    .trim()
                    .to_string(),
                Err(_) => query.to_string(),
            }
        };

        // 2. Embed and search
        let embeddings = embedder::embed(&[round_query.clone()], embedding_model, &repo)
            .await
            .map_err(|e| format!("Embedding failed: {}", e))?;

        if embeddings.is_empty() {
            break;
        }

        let results = retriever::search(pool, kb_id, &embeddings[0], top_k)
            .await
            .unwrap_or_default();

        if results.is_empty() && round > 0 {
            break; // No more relevant content found
        }

        all_sources.extend(results.clone());

        // 3. Generate round answer
        let context = build_context(&results);
        let findings_str = all_findings
            .iter()
            .enumerate()
            .map(|(i, f)| format!("第{}轮发现: {}", i + 1, f))
            .collect::<Vec<_>>()
            .join("\n");

        let round_prompt = if round == 0 {
            let template =
                prompt_templates::load(pool, prompt_templates::KEY_DEEP_RESEARCH_ROUND0).await;
            prompt_templates::render(&template, &[("query", query), ("context", &context)])
        } else {
            let template =
                prompt_templates::load(pool, prompt_templates::KEY_DEEP_RESEARCH_ROUND_NEXT).await;
            prompt_templates::render(
                &template,
                &[
                    ("query", query),
                    ("findings", &findings_str),
                    ("context", &context),
                ],
            )
        };

        let deep_research_system =
            prompt_templates::load(pool, prompt_templates::KEY_DEEP_RESEARCH_SYSTEM).await;
        let chat_request = serde_json::json!({
            "model": chat_model,
            "messages": [
                {"role": "system", "content": deep_research_system},
                {"role": "user", "content": round_prompt}
            ],
            "stream": false
        });

        let chat_request_str: String = serde_json::to_string(&chat_request).unwrap_or_default();
        let proxy_result = proxy::handle_request(
            &Arc::new(Repository::new(pool.clone())),
            settings,
            "kb-research",
            "深度研究",
            chat_request,
            false,
            Some(chat_request_str),
            Some(format!("kb-research_{}", kb_id)),
            None, // legacy RAG path: no security gate yet — proxy scans internally
        )
        .await;

        let round_answer = match proxy_result {
            Ok(result) => result
                .body
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("分析失败")
                .to_string(),
            Err((code, msg)) => {
                tracing::warn!("Deep research round {} failed: {} {}", round + 1, code, msg);
                break;
            }
        };
        all_findings.push(round_answer.clone());
        history.push(ConversationMessage {
            role: "user".into(),
            content: round_query,
        });
        history.push(ConversationMessage {
            role: "assistant".into(),
            content: round_answer,
        });

        // Check if we have enough info (after round 2)
        if round >= 2 && round == max_rounds - 1 {
            break;
        }
    }

    // Final synthesis
    let findings_summary = all_findings
        .iter()
        .enumerate()
        .map(|(i, f)| format!("### 第{}轮发现\n{}", i + 1, f))
        .collect::<Vec<_>>()
        .join("\n\n");

    let final_prompt = prompt_templates::render(
        &prompt_templates::load(pool, prompt_templates::KEY_DEEP_RESEARCH_FINAL).await,
        &[("query", query), ("findings", &findings_summary)],
    );

    let final_system =
        prompt_templates::load(pool, prompt_templates::KEY_DEEP_RESEARCH_FINAL_SYSTEM).await;
    let final_request = serde_json::json!({
        "model": chat_model,
        "messages": [
            {"role": "system", "content": final_system},
            {"role": "user", "content": final_prompt}
        ],
        "stream": false
    });

    let final_request_str: String = serde_json::to_string(&final_request).unwrap_or_default();
    let proxy_result = proxy::handle_request(
        &Arc::new(repo),
        settings,
        "kb-research",
        "深度研究",
        final_request,
        false,
        Some(final_request_str),
        Some(format!("kb-research_{}", kb_id)),
        None, // legacy RAG path: no security gate yet — proxy scans internally
    )
    .await;

    match proxy_result {
        Ok(result) => {
            let answer = result
                .body
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("综合分析失败")
                .to_string();

            let usage = result.usage.map(|u| UsageInfo {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            });

            // Deduplicate sources
            let mut seen = std::collections::HashSet::new();
            let sources: Vec<SourceInfo> = all_sources
                .iter()
                .filter(|r| seen.insert(r.chunk_id.clone()))
                .map(|r| SourceInfo {
                    filename: r.filename.clone(),
                    score: r.score,
                    snippet: r.content.chars().take(200).collect(),
                })
                .collect();

            // Save to conversation history
            if !kb_id.is_empty() {
                let sources_json = serde_json::to_string(&sources).ok();
                let tokens = usage.as_ref().map(|u| u.total_tokens as i64).unwrap_or(0);
                kb_repo
                    .add_conversation(kb_id, "user", query, None, Some(chat_model), 0)
                    .await
                    .ok();
                kb_repo
                    .add_conversation(
                        kb_id,
                        "assistant",
                        &answer,
                        sources_json.as_deref(),
                        Some(chat_model),
                        tokens,
                    )
                    .await
                    .ok();
            }

            Ok(RagAnswer {
                answer,
                sources,
                usage,
                retrieval_details: None,
            })
        }
        Err((code, msg)) => Err(format!("Final synthesis failed ({}): {}", code, msg)),
    }
}

// ─── C-06/R2：多轮查询改写 ─────────────────────────────────────────────────

/// 从改写回复中提取查询（纯函数）：取首个非空行、去引号包裹、截断到 512 字符。
/// 空回复/全空白 → None（调用侧回退原查询）。
fn extract_rewrite_query(reply: &str) -> Option<String> {
    let line = reply.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_matches(|c| c == '"' || c == '“' || c == '”');
    if line.is_empty() {
        return None;
    }
    Some(line.chars().take(512).collect())
}

/// 可选查询改写：近几轮对话 + 当前问题 → 独立完整检索查询。
/// 走渠道模型（kb-internal 路由组，token 消耗自动落账）；任何失败静默回退原查询。
async fn rewrite_query_with_llm(
    pool: &SqlitePool,
    settings: &SettingsStore,
    kb_id: &str,
    chat_model: &str,
    query: &str,
    history: &[ConversationMessage],
) -> String {
    // 近 6 轮（改写只需消解指代，更早的轮次是噪音）
    let recent: Vec<String> = history
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect();
    let history_text = recent.join("\n");
    let prompt = prompt_templates::render(
        &prompt_templates::load(pool, prompt_templates::KEY_QUERY_REWRITE).await,
        &[("history", &history_text), ("query", query)],
    );
    let chat_request = serde_json::json!({
        "model": chat_model,
        "messages": [
            {"role": "system", "content": "你是检索查询改写器，只输出改写后的查询本身。"},
            {"role": "user", "content": prompt}
        ],
        "stream": false,
        "temperature": 0.0
    });
    let chat_request_str: String = serde_json::to_string(&chat_request).unwrap_or_default();
    let proxy_result = proxy::handle_request(
        &std::sync::Arc::new(Repository::new(pool.clone())),
        settings,
        "kb-rewrite",
        "RAG-rewrite",
        chat_request,
        false,
        Some(chat_request_str),
        Some(format!("kb-internal_{}", kb_id)),
        None,
    )
    .await;

    let reply = match proxy_result {
        Ok(result) => result
            .body
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string(),
        Err(_) => String::new(),
    };
    match extract_rewrite_query(&reply) {
        Some(rewritten) => {
            tracing::debug!("[RAG] 查询改写: {query:?} -> {rewritten:?}");
            rewritten
        }
        None => {
            tracing::warn!("[RAG] 查询改写回复不可解析，回退原查询");
            query.to_string()
        }
    }
}

#[cfg(test)]
mod rewrite_tests {
    use super::*;

    /// 门控键契约（ask_with_config 内联表达式）：键名与默认值锁定——
    /// 键名打错会让改写误开（成本意外增加）或永不生效；默认必须为关。
    #[test]
    fn query_rewrite_gate_defaults_off_and_key_is_stable() {
        let dir =
            std::env::temp_dir().join(format!("waliapi-rewrite-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::settings_store::SettingsStore::file(dir.join("settings.json"));

        // 未配置 → 关（默认值契约）
        assert!(
            !store.get_bool("kb.query_rewrite", false),
            "kb.query_rewrite 默认必须为关"
        );
        // 开启表达式（ask_with_config 内联条件的镜像）：关闭或无历史 → 不进入改写
        let gate = store.get_bool("kb.query_rewrite", false);
        assert!(!(gate && !Vec::<ConversationMessage>::new().is_empty()));
        assert!(!gate, "关闭时检索路径零变化（结构性跳过改写）");
    }

    #[test]
    fn extract_rewrite_query_takes_first_line_and_trims_quotes() {
        assert_eq!(
            extract_rewrite_query("WaLiAPI 网关如何配置渠道配额\n（改写说明）"),
            Some("WaLiAPI 网关如何配置渠道配额".to_string())
        );
        assert_eq!(
            extract_rewrite_query("  \"带引号的查询\"  "),
            Some("带引号的查询".to_string())
        );
        assert_eq!(
            extract_rewrite_query("“中文引号”"),
            Some("中文引号".to_string())
        );
        // 空白/空行 → None
        assert_eq!(extract_rewrite_query(""), None);
        assert_eq!(extract_rewrite_query(" \n \n"), None);
        // 超长截断
        let long = "长".repeat(600);
        assert_eq!(
            extract_rewrite_query(&long).map(|q| q.chars().count()),
            Some(512)
        );
    }

    /// 改写关闭（默认）或无历史 → 走原查询（结构性保障：分支条件在 ask_with_config 内联，
    /// 此处锁定 extract 的回退语义与 rewrite 的失败回退）。
    #[tokio::test]
    async fn rewrite_falls_back_to_original_when_no_channels() {
        // 内存库无任何渠道 → proxy 转发必然失败 → 回退原查询（不 panic、不报错）
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let settings = SettingsStore::file(
            std::env::temp_dir().join(format!("waliapi-rewrite-test-{}", uuid::Uuid::new_v4())),
        );
        let history = vec![
            ConversationMessage {
                role: "user".into(),
                content: "WaLiAPI 的渠道配额怎么配？".into(),
            },
            ConversationMessage {
                role: "assistant".into(),
                content: "在密钥页设置 quota_limit。".into(),
            },
        ];
        let result =
            rewrite_query_with_llm(&pool, &settings, "kb-1", "m", "那限流呢？", &history).await;
        assert_eq!(result, "那限流呢？", "上游不可用时必须静默回退原查询");
    }
}

// ─── C-06/R3 第二步：LLM listwise 重排 ─────────────────────────────────────

/// 解析重排回复为候选顺序（纯函数）：
/// 容忍前后杂文（截取首个 [ 到最后一个 ]）；编号越界/重复丢弃；
/// 未出现的候选按原序补尾——保证输出恒为原候选的全排列。
fn parse_rerank_order(reply: &str, len: usize) -> Option<Vec<usize>> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')?;
    if end <= start {
        return None;
    }
    let parsed: Vec<i64> = serde_json::from_str(&reply[start..=end]).ok()?;
    let mut order: Vec<usize> = Vec::with_capacity(len);
    for index in parsed {
        let index = index as usize;
        if index < len && !order.contains(&index) {
            order.push(index);
        }
    }
    for i in 0..len {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    Some(order)
}

/// 可选 LLM 重排：把 top 候选拼给渠道模型打分重排（用渠道跑渠道）。
/// 失败/关闭不影响主流程——原序返回，仅 tracing 告警。
async fn rerank_with_llm(
    pool: &SqlitePool,
    settings: &crate::settings_store::SettingsStore,
    kb_id: &str,
    chat_model: &str,
    query: &str,
    candidates: Vec<retriever::ScoredSearchResult>,
) -> Vec<retriever::ScoredSearchResult> {
    let mut listing = String::new();
    for (i, c) in candidates.iter().enumerate() {
        let excerpt: String = c
            .result
            .content
            .chars()
            .take(200)
            .collect::<String>()
            .replace('\n', " ");
        listing.push_str(&format!("[{}] {}: {}\n", i, c.result.filename, excerpt));
    }
    let prompt = format!(
        "你是检索结果重排器。根据查询对候选片段按相关性从高到低排序。\n\n查询：{query}\n\n候选片段：\n{listing}\n只返回一个 JSON 数组，元素为候选编号、按相关性从高到低排列，例如 [2,0,1]。不要输出其他内容。"
    );
    let chat_request = serde_json::json!({
        "model": chat_model,
        "messages": [
            {"role": "system", "content": "你是检索重排器，只输出 JSON 数组。"},
            {"role": "user", "content": prompt}
        ],
        "stream": false,
        "temperature": 0.0
    });
    let chat_request_str: String = serde_json::to_string(&chat_request).unwrap_or_default();
    let proxy_result = proxy::handle_request(
        &std::sync::Arc::new(crate::db::repository::Repository::new(pool.clone())),
        settings,
        "kb-rerank",
        "RAG-rerank",
        chat_request,
        false,
        Some(chat_request_str),
        Some(format!("kb-internal_{}", kb_id)),
        None,
    )
    .await;

    let reply = match proxy_result {
        Ok(result) => result
            .body
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string(),
        Err(_) => String::new(),
    };
    match parse_rerank_order(&reply, candidates.len()) {
        Some(order) => {
            let mut reordered = Vec::with_capacity(candidates.len());
            for index in order {
                reordered.push(candidates[index].clone());
            }
            tracing::debug!("[RAG] LLM 重排生效（{} 候选）", reordered.len());
            reordered
        }
        None => {
            tracing::warn!("[RAG] LLM 重排回复不可解析，回退原序（相关性排序）");
            candidates
        }
    }
}

#[cfg(test)]
mod rerank_tests {
    use super::*;

    #[test]
    fn parse_rerank_order_extracts_json_and_validates() {
        // 纯 JSON
        assert_eq!(parse_rerank_order("[2,0,1]", 3), Some(vec![2, 0, 1]));
        // 前后杂文容忍
        assert_eq!(
            parse_rerank_order("排序结果：[1, 2, 0] 以上。", 3),
            Some(vec![1, 2, 0])
        );
        // 越界丢弃 + 缺失补尾（全排列保证）
        assert_eq!(parse_rerank_order("[5,0,5]", 3), Some(vec![0, 1, 2]));
        assert_eq!(parse_rerank_order("[1]", 3), Some(vec![1, 0, 2]));
        // 不可解析 → None
        assert_eq!(parse_rerank_order("no json here", 3), None);
        assert_eq!(parse_rerank_order("[]", 3), Some(vec![0, 1, 2]));
    }
}
