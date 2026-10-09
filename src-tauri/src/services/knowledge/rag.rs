use super::model_client::{ModelClient, QueryError};
use super::models::{
    ConversationMessage, RagAnswer, RagDiagnosticStage, RagDiagnostics, RagReasoning,
    ReasoningStatus, RetrievalDetail, SourceInfo, UsageInfo,
};
use super::repository::KbRepository;
use super::retriever;
use super::{budget, embedder};
use crate::core::proxy;
use crate::db::repository::Repository;
use crate::prompt_templates;
use crate::settings_store::SettingsStore;
use sqlx::SqlitePool;
use std::{future::Future, sync::Arc, time::Instant};

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
    let client = ModelClient::Internal {
        pool,
        settings,
        kb_id,
    };
    ask_with_client(
        &client,
        pool,
        kb_id,
        query,
        embedding_model,
        chat_model,
        top_k,
        mcp_only,
        history,
        settings,
        vector_weight,
        keyword_weight,
        search_mode,
        false,
        false,
        false,
        false,
        None,
        None,
    )
    .await
    .map_err(|e| e.to_string())
}

// 沿用现有 RAG 参数，在公共流程中额外传递模型调用身份。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn ask_with_client(
    client: &ModelClient<'_>,
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
    diagnostics_enabled: bool,
    allow_keyword_fallback: bool,
    allow_vector_fallback: bool,
    strict_retrieval: bool,
    candidate_k: Option<usize>,
    reasoning_effort: Option<&str>,
) -> Result<RagAnswer, QueryError> {
    let reasoning_effort = validate_reasoning_request(reasoning_effort, false)?;
    let mut stages = Vec::new();
    let strict = diagnostics_enabled
        || allow_keyword_fallback
        || allow_vector_fallback
        || strict_retrieval
        || budget::current().is_some();
    let request_id = client
        .request_id()
        .or_else(|| budget::current().map(|budget| budget.request_id().to_string()))
        .unwrap_or_default();

    let kb_repo = KbRepository::new(pool.clone());
    validate_candidate_k(top_k, candidate_k)?;
    let ordered_context = candidate_k.is_some();
    let retrieval_k = candidate_k.unwrap_or(top_k);
    let anchors = if ordered_context {
        super::text::query_tokens(query)
    } else {
        Vec::new()
    };

    // C-06/R2：可选多轮查询改写（kb.query_rewrite，默认关）。
    // 多轮对话的指代型问题（「上面说的方案呢」）直接送检索必然 miss——
    // 开启时先用渠道模型把「近几轮对话 + 当前问题」改写成独立完整的检索查询。
    // 失败/超时静默回退原查询（best-effort），多一次 LLM 调用的成本由开关控制。
    let query = if settings.get_bool("kb.query_rewrite", false) && !history.is_empty() {
        rewrite_query_with_llm(client, pool, chat_model, query, history).await?
    } else {
        query.to_string()
    };

    let retrieved = Box::pin(retrieve_with_client(
        client,
        pool,
        kb_id,
        &query,
        embedding_model,
        retrieval_k,
        mcp_only,
        retriever::FusionMode::parse(&settings.get_str("kb.fusion_mode", "rrf")),
        vector_weight,
        keyword_weight,
        search_mode,
        diagnostics_enabled,
        allow_keyword_fallback,
        allow_vector_fallback,
        strict_retrieval,
        true,
    ))
    .await?;
    stages.extend(retrieved.stages);
    let retrieval_started = retrieved.retrieval_started;
    let actual_mode = retrieved.actual_mode;
    let degradation_reason = retrieved.degradation_reason;
    let fallback_enabled = allow_keyword_fallback
        || allow_vector_fallback
        || strict_retrieval
        || degradation_reason.is_some();
    let scored_results = retrieved.scored_results;
    // C-06/R3 第二步：可选 LLM listwise 重排（`kb.rerank_enabled`，默认关）。
    // 走网关自身的渠道跑渠道（proxy::handle_request，kb-internal 路由组），
    // 重排调用的 token 消耗自动计入请求日志；失败静默回退原序（best-effort）。
    let mut scored_results =
        if settings.get_bool("kb.rerank_enabled", false) && scored_results.len() > 1 {
            rerank_with_llm(
                client,
                chat_model,
                &query,
                scored_results,
                ordered_context.then_some(anchors.as_slice()),
            )
            .await?
        } else {
            scored_results
        };
    let retrieval_candidates = scored_results.clone();
    scored_results.truncate(top_k);

    // Extract plain results for context building
    let results: Vec<super::models::SearchResult> =
        scored_results.iter().map(|s| s.result.clone()).collect();

    if strict && results.is_empty() {
        return Err(diagnostic_failure(
            QueryError::new(
                axum::http::StatusCode::NOT_FOUND,
                "未检索到相关片段，请检查知识库内容、索引和检测问题",
            )
            .at_stage("retrieval", "retrieval_empty"),
            "retrieval",
            retrieval_started,
            &stages,
            &request_id,
            diagnostics_enabled,
        ));
    }
    if results.is_empty() {
        // Save to conversation history
        if client.is_internal() && !kb_id.is_empty() {
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
                diagnostics: None,
                retrieval_mode: fallback_enabled.then(|| actual_mode.to_string()),
                degradation_reason: degradation_reason.clone(),
                reasoning: reasoning_info(reasoning_effort, ReasoningStatus::NotSent),
            });
        }
        return Ok(RagAnswer {
            answer: "RAG 中没有找到相关内容。".to_string(),
            sources: vec![],
            usage: None,
            retrieval_details: Some(vec![]),
            diagnostics: None,
            retrieval_mode: fallback_enabled.then(|| actual_mode.to_string()),
            degradation_reason: degradation_reason.clone(),
            reasoning: reasoning_info(reasoning_effort, ReasoningStatus::NotSent),
        });
    }

    let rag_system_prompt = prompt_templates::load(pool, prompt_templates::KEY_RAG_SYSTEM).await;

    // 3. Build context
    let context = build_context(&results);

    // 4. Build prompt with history
    let prompt = build_rag_prompt(&context, &query, history);

    // 5. Token estimation and fallback
    let estimated_tokens = retriever::estimate_tokens(&prompt);
    let model_limit = retriever::get_model_context_limit(chat_model);
    let context_limit = ((model_limit as f64 * 0.7) as usize)
        .saturating_sub(retriever::estimate_tokens(&rag_system_prompt));

    let (final_prompt, context_results) = if estimated_tokens > context_limit {
        // Stage 1: Trim context (remove lowest-scoring chunks)
        let trimmed =
            trim_context_with_order(&results, &query, history, context_limit, ordered_context);
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

    // 6. Call LLM via proxy
    let mut chat_request = serde_json::json!({
        "model": chat_model,
        "messages": [
            {"role": "system", "content": rag_system_prompt},
            {"role": "user", "content": final_prompt}
        ],
        "stream": false
    });
    let reasoning = apply_reasoning_effort(&mut chat_request, reasoning_effort)?;
    let answer_started = Instant::now();
    client
        .ensure_knowledge_access(kb_id, mcp_only)
        .await
        .map_err(|mut error| {
            error.stage = Some("answer".to_string());
            diagnostic_failure(
                error,
                "answer",
                answer_started,
                &stages,
                &request_id,
                diagnostics_enabled,
            )
        })?;
    let proxy_result = run_stage("answer", 1.0, 0.01, client.chat(chat_request, "RAG"))
        .await
        .map_err(|error| {
            diagnostic_failure(
                error,
                "answer",
                answer_started,
                &stages,
                &request_id,
                diagnostics_enabled,
            )
        });

    match proxy_result {
        Ok(result) => {
            let answer = result
                .body
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str());
            record_stage(&mut stages, "answer", "passed", answer_started);
            let validation_started = Instant::now();
            client
                .ensure_knowledge_access_after_answer(kb_id, mcp_only)
                .await
                .map_err(|error| {
                    diagnostic_failure(
                        error,
                        "validation",
                        validation_started,
                        &stages,
                        &request_id,
                        diagnostics_enabled,
                    )
                })?;
            if strict && answer.unwrap_or("").trim().is_empty() {
                return Err(diagnostic_failure(
                    QueryError::new(axum::http::StatusCode::BAD_GATEWAY, "回答模型返回了空答案")
                        .at_stage("validation", "answer_empty"),
                    "validation",
                    validation_started,
                    &stages,
                    &request_id,
                    diagnostics_enabled,
                ));
            }
            // 普通请求保留空字符串；只有缺少 content 时沿用原有回退文案。
            let answer = answer.unwrap_or("生成回答失败").to_string();
            let usage = result.usage;

            // 来源只来自最终发送给模型的切片，裁掉的检索命中仅保留在检索详情。
            let sources: Vec<SourceInfo> = context_results
                .iter()
                .map(|r| source_info(r, &anchors, ordered_context, ordered_context))
                .collect();

            if strict
                && !sources
                    .iter()
                    .any(|source| !source.snippet.trim().is_empty())
            {
                return Err(diagnostic_failure(
                    QueryError::new(
                        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                        "回答没有可核验的知识库来源，请缩短问题或调整上下文限制",
                    )
                    .at_stage("validation", "sources_empty"),
                    "validation",
                    validation_started,
                    &stages,
                    &request_id,
                    diagnostics_enabled,
                ));
            }
            record_stage(&mut stages, "validation", "passed", validation_started);

            // Build retrieval details for visualization
            let retrieval_details: Vec<RetrievalDetail> = retrieval_candidates
                .iter()
                .map(|s| {
                    let meta = &s.result.metadata;
                    RetrievalDetail {
                        chunk_id: s.result.chunk_id.clone(),
                        filename: s.result.filename.clone(),
                        score: s.result.score,
                        vector_score: s.vector_score,
                        keyword_score: s.keyword_score,
                        snippet: if ordered_context {
                            retriever::evidence_window(&s.result.content, &anchors, 200).0
                        } else {
                            s.result.content.chars().take(200).collect()
                        },
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
            if client.is_internal() && !kb_id.is_empty() {
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
                diagnostics: diagnostics_enabled.then_some(RagDiagnostics { request_id, stages }),
                retrieval_mode: fallback_enabled.then(|| actual_mode.to_string()),
                degradation_reason,
                reasoning,
            })
        }
        Err(error) => Err(error),
    }
}

pub(crate) struct RetrievedChunks {
    pub scored_results: Vec<retriever::ScoredSearchResult>,
    pub stages: Vec<RagDiagnosticStage>,
    pub actual_mode: String,
    pub degradation_reason: Option<String>,
    pub retrieval_started: Instant,
}

/// Ask 和 Search 共用检索预算和两路错误策略，不包含客户端业务判断。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn retrieve_with_client(
    client: &ModelClient<'_>,
    pool: &SqlitePool,
    kb_id: &str,
    query: &str,
    embedding_model: &str,
    retrieval_k: usize,
    mcp_only: bool,
    fusion_mode: retriever::FusionMode,
    vector_weight: f32,
    keyword_weight: f32,
    search_mode: &str,
    diagnostics_enabled: bool,
    allow_keyword_fallback: bool,
    allow_vector_fallback: bool,
    strict_retrieval: bool,
    reserve_answer: bool,
) -> Result<RetrievedChunks, QueryError> {
    let request_id = client
        .request_id()
        .or_else(|| budget::current().map(|budget| budget.request_id().to_string()))
        .unwrap_or_default();
    let retrieval_started = Instant::now();
    let embedding_budget = if reserve_answer {
        (0.4, 0.55)
    } else {
        (0.7, 0.2)
    };
    // FTS 是并行阶段，保持原有软限额；Search 最后检索可以使用全部剩余时间。
    let keyword_budget = if reserve_answer {
        (0.2, 0.35)
    } else {
        (0.3, 0.0)
    };
    let final_budget = if reserve_answer {
        (0.2, 0.35)
    } else {
        (1.0, 0.0)
    };
    let hybrid = search_mode == "hybrid" && !kb_id.is_empty();
    let keyword_fallback = hybrid && allow_keyword_fallback;
    let vector_fallback = hybrid && allow_vector_fallback;
    let legacy_local_fallback =
        !strict_retrieval && !allow_keyword_fallback && !allow_vector_fallback;
    let mut stages = Vec::new();
    if hybrid && (keyword_fallback || vector_fallback) {
        run_stage(
            "permission",
            1.0,
            0.0,
            client.ensure_access(
                kb_id,
                embedding_model,
                crate::core::route_plan::EndpointKind::Embeddings,
            ),
        )
        .await
        .map_err(|error| {
            diagnostic_failure(
                error,
                "permission",
                retrieval_started,
                &stages,
                &request_id,
                diagnostics_enabled,
            )
        })?;
    }
    let vector_future = Box::pin(vector_branch(
        Box::pin(client.embed(query, embedding_model)),
        |embedding| async move {
            let count = if hybrid { retrieval_k * 2 } else { retrieval_k };
            if kb_id.is_empty() {
                retriever::search_all_with_details(
                    pool,
                    &embedding,
                    count,
                    mcp_only,
                    strict_retrieval,
                )
                .await
                .map(|(results, partial)| {
                    (
                        results,
                        partial.then(|| "knowledge_base_search_partial".into()),
                    )
                })
                .map_err(|error| match error.as_str() {
                    "client_cancelled" => {
                        QueryError::from_budget(budget::BudgetElapsed::Cancelled, "vector_search")
                    }
                    "rag_deadline_exceeded" => {
                        QueryError::from_budget(budget::BudgetElapsed::Deadline, "vector_search")
                    }
                    code @ ("stage_timeout"
                    | "cross_kb_list_failed"
                    | "cross_kb_pool_failed"
                    | "cross_kb_search_failed") => QueryError::new(
                        if code == "stage_timeout" {
                            axum::http::StatusCode::GATEWAY_TIMEOUT
                        } else {
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR
                        },
                        "跨库检索失败，请根据请求编号检查服务日志",
                    )
                    .at_stage("vector_search", code),
                    _ => QueryError::new(
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "向量检索失败，请根据请求编号检查服务日志",
                    )
                    .at_stage("vector_search", "vector_search_failed"),
                })
            } else {
                retriever::search(pool, kb_id, &embedding, count)
                    .await
                    .map(|results| (results, None))
                    .map_err(|_| {
                        QueryError::new(
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "向量检索失败，请根据请求编号检查服务日志",
                        )
                        .at_stage("vector_search", "vector_search_failed")
                    })
            }
        },
        embedding_budget,
        final_budget,
    ));
    let keyword_future = Box::pin(async {
        let (results, stage) = timed_retrieval_stage(
            "keyword_search",
            if hybrid { keyword_budget } else { final_budget },
            async {
                retriever::keyword_only_search(
                    pool,
                    kb_id,
                    query,
                    if hybrid { retrieval_k * 2 } else { retrieval_k },
                )
                .await
                .map_err(|_| {
                    QueryError::new(
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "关键词检索失败，请根据请求编号检查服务日志",
                    )
                    .at_stage("keyword_search", "keyword_search_failed")
                })
            },
        )
        .await;
        RetrievalBranch {
            results,
            stages: vec![stage],
            degradation_reason: None,
        }
    });
    let mut degradation_reason = None;
    let mut actual_mode = search_mode;
    let outcome = if hybrid {
        let (vector, keyword) = collect_hybrid_branches(vector_future, keyword_future).await;
        let vector_result = vector.map(|branch| {
            stages.extend(branch.stages);
            branch.results
        });
        let keyword_result = keyword.map(|branch| {
            stages.extend(branch.stages);
            branch.results
        });
        let chosen = choose_hybrid_results(
            vector_result,
            keyword_result,
            keyword_fallback,
            vector_fallback,
            strict_retrieval,
        );
        match chosen {
            Ok((vectors, keywords, mode, reason)) => {
                actual_mode = mode;
                degradation_reason = reason;
                // 失败降级前重新核对模型权限、KB 和额度，不复用旧授权快照。
                if degradation_reason.is_some() {
                    run_stage("permission", 1.0, 0.0, async {
                        if stages
                            .iter()
                            .any(|stage| stage.stage == "embedding" && stage.status == "passed")
                        {
                            client
                                .ensure_model_permission_after_answer(kb_id, embedding_model)
                                .await?;
                            client
                                .ensure_knowledge_access_after_answer(kb_id, mcp_only)
                                .await
                        } else {
                            client
                                .ensure_model_permission(kb_id, embedding_model)
                                .await?;
                            client.ensure_knowledge_access(kb_id, mcp_only).await
                        }
                    })
                    .await
                    .map_err(|error| {
                        diagnostic_failure(
                            error,
                            "retrieval",
                            retrieval_started,
                            &stages,
                            &request_id,
                            diagnostics_enabled,
                        )
                    })?;
                }
                let uses_fusion = mode == "hybrid" || legacy_local_fallback;
                let fusion_started = Instant::now();
                let fused = run_stage("fusion", final_budget.0, final_budget.1, async {
                    Ok(finish_hybrid_results(
                        vectors,
                        keywords,
                        mode,
                        legacy_local_fallback,
                        retrieval_k,
                        (vector_weight, keyword_weight),
                        fusion_mode,
                    ))
                })
                .await;
                let status = match &fused {
                    Err(_) => "failed",
                    Ok(_) if !uses_fusion => "skipped",
                    Ok(results) if results.is_empty() => "empty",
                    Ok(_) if degradation_reason.is_some() => "degraded",
                    Ok(_) => "passed",
                };
                record_stage_result(
                    &mut stages,
                    "fusion",
                    status,
                    fusion_started,
                    fused.as_ref().err().and_then(|error| error.code.as_deref()),
                );
                fused
            }
            Err(error) => Err(error),
        }
    } else if search_mode == "keyword" && !kb_id.is_empty() {
        record_stage(&mut stages, "embedding", "skipped", Instant::now());
        let branch = keyword_future.await;
        stages.extend(branch.stages);
        branch
            .results
            .map(|results| score_single_branch(results, false))
    } else {
        // 空 KB 沿用跨库向量检索；不把它标成执行了 FTS 的 hybrid。
        actual_mode = "vector";
        let branch = vector_future.await;
        degradation_reason = branch.degradation_reason;
        stages.extend(branch.stages);
        branch
            .results
            .map(|results| score_single_branch(results, true))
    };
    let mut scored_results = outcome.map_err(|error| {
        diagnostic_failure(
            error,
            "retrieval",
            retrieval_started,
            &stages,
            &request_id,
            diagnostics_enabled,
        )
    })?;
    scored_results.truncate(retrieval_k);
    run_stage(
        "permission",
        1.0,
        0.0,
        client.ensure_knowledge_access_after_answer(kb_id, mcp_only),
    )
    .await
    .map_err(|error| {
        diagnostic_failure(
            error,
            "retrieval",
            retrieval_started,
            &stages,
            &request_id,
            diagnostics_enabled,
        )
    })?;
    record_stage(
        &mut stages,
        "retrieval",
        if scored_results.is_empty() {
            "empty"
        } else if degradation_reason.is_some() {
            "degraded"
        } else {
            "passed"
        },
        retrieval_started,
    );
    Ok(RetrievedChunks {
        scored_results,
        stages,
        actual_mode: actual_mode.to_string(),
        degradation_reason,
        retrieval_started,
    })
}

struct RetrievalBranch {
    results: Result<Vec<super::models::SearchResult>, QueryError>,
    stages: Vec<RagDiagnosticStage>,
    degradation_reason: Option<String>,
}

async fn timed_retrieval_stage<T>(
    stage: &'static str,
    limits: (f64, f64),
    future: impl Future<Output = Result<Vec<T>, QueryError>>,
) -> (Result<Vec<T>, QueryError>, RagDiagnosticStage) {
    let started = Instant::now();
    let result = run_stage(stage, limits.0, limits.1, future)
        .await
        .map_err(|mut error| {
            if error.stage.is_none() {
                error.stage = Some(stage.into());
            }
            if error.code.is_none() {
                error.code = Some(format!("{stage}_failed"));
            }
            error
        });
    let mut stages = Vec::with_capacity(1);
    let status = match &result {
        Ok(results) if results.is_empty() => "empty",
        Ok(_) => "passed",
        Err(_) => "failed",
    };
    record_stage_result(
        &mut stages,
        stage,
        status,
        started,
        result
            .as_ref()
            .err()
            .and_then(|error| error.code.as_deref()),
    );
    (result, stages.pop().unwrap())
}

async fn vector_branch<E, V, F>(
    embedding: E,
    search: V,
    embedding_budget: (f64, f64),
    vector_budget: (f64, f64),
) -> RetrievalBranch
where
    E: Future<Output = Result<Vec<Vec<f32>>, QueryError>>,
    V: FnOnce(Vec<f32>) -> F,
    F: Future<Output = Result<(Vec<super::models::SearchResult>, Option<String>), QueryError>>,
{
    let (embedding, stage) = timed_retrieval_stage("embedding", embedding_budget, async {
        let vectors = embedding.await?;
        vectors
            .into_iter()
            .next()
            .filter(|vector| !vector.is_empty() && vector.iter().all(|value| value.is_finite()))
            .ok_or_else(|| {
                QueryError::new(
                    axum::http::StatusCode::BAD_GATEWAY,
                    "Embedding 响应缺少有效向量",
                )
                .at_stage("embedding", "invalid_embedding_response")
            })
    })
    .await;
    let mut stages = vec![stage];
    let mut degradation_reason = None;
    let results = match embedding {
        Ok(embedding) => {
            // Embedding 一完成立即开始向量检索，FTS 的完成与否不形成屏障。
            let started = Instant::now();
            let result = run_stage(
                "vector_search",
                vector_budget.0,
                vector_budget.1,
                search(embedding),
            )
            .await
            .map_err(|mut error| {
                if error.stage.is_none() {
                    error.stage = Some("vector_search".into());
                }
                if error.code.is_none() {
                    error.code = Some("vector_search_failed".into());
                }
                error
            });
            let status = match &result {
                Err(_) => "failed",
                Ok((_, Some(_))) => "degraded",
                Ok((results, _)) if results.is_empty() => "empty",
                Ok(_) => "passed",
            };
            let code = match &result {
                Err(error) => error.code.as_deref(),
                Ok((_, reason)) => reason.as_deref(),
            };
            record_stage_result(&mut stages, "vector_search", status, started, code);
            result.map(|(results, reason)| {
                degradation_reason = reason;
                results
            })
        }
        Err(error) => Err(error),
    };
    RetrievalBranch {
        results,
        stages,
        degradation_reason,
    }
}

async fn collect_hybrid_branches(
    vector: impl Future<Output = RetrievalBranch>,
    keyword: impl Future<Output = RetrievalBranch>,
) -> (Option<RetrievalBranch>, Option<RetrievalBranch>) {
    tokio::pin!(vector, keyword);
    tokio::select! {
        result = &mut vector => {
            if result.results.as_ref().err().is_some_and(retrieval_terminal_error) {
                (Some(result), None)
            } else {
                (Some(result), Some(keyword.await))
            }
        }
        result = &mut keyword => {
            if result.results.as_ref().err().is_some_and(retrieval_terminal_error) {
                (None, Some(result))
            } else {
                (Some(vector.await), Some(result))
            }
        }
    }
}

type HybridResults = (
    Vec<super::models::SearchResult>,
    Vec<super::models::SearchResult>,
    &'static str,
    Option<String>,
);

fn choose_hybrid_results(
    vector: Option<Result<Vec<super::models::SearchResult>, QueryError>>,
    keyword: Option<Result<Vec<super::models::SearchResult>, QueryError>>,
    allow_keyword_fallback: bool,
    allow_vector_fallback: bool,
    strict_retrieval: bool,
) -> Result<HybridResults, QueryError> {
    // 终止失败优先于其他路可用结果；不绕过权限、额度、父截止或取消。
    if let Some(Err(error)) = &vector {
        if retrieval_terminal_error(error) {
            return Err(vector.unwrap().unwrap_err());
        }
    }
    if let Some(Err(error)) = &keyword {
        if retrieval_terminal_error(error) {
            return Err(keyword.unwrap().unwrap_err());
        }
    }
    let legacy_local_fallback =
        !strict_retrieval && !allow_keyword_fallback && !allow_vector_fallback;
    match (vector, keyword) {
        // 旧 hybrid 对本地两路错误使用局部结果；Embedding 从未拥有隐式降级授权。
        (Some(Err(vector)), Some(Err(keyword))) if legacy_local_fallback => {
            if local_retrieval_error(&vector) && local_retrieval_error(&keyword) {
                Ok((vec![], vec![], "none", Some("retrieval_both_failed".into())))
            } else if !local_retrieval_error(&vector) {
                Err(vector)
            } else {
                Err(keyword)
            }
        }
        (Some(Err(error)), Some(Ok(keywords)))
            if legacy_local_fallback && local_retrieval_error(&error) =>
        {
            Ok((vec![], keywords, "keyword", error.code))
        }
        (Some(Ok(vectors)), Some(Err(error)))
            if legacy_local_fallback && local_retrieval_error(&error) =>
        {
            Ok((vectors, vec![], "vector", error.code))
        }
        (Some(Ok(vectors)), Some(Ok(keywords))) => Ok((vectors, keywords, "hybrid", None)),
        (Some(Err(_)), Some(Err(_))) => Err(QueryError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "向量和关键词检索均失败，请根据请求编号检查服务日志",
        )
        .at_stage("retrieval", "retrieval_both_failed")),
        (Some(Err(error)), Some(Ok(keywords)))
            if allow_keyword_fallback && recoverable_retrieval_error(&error) =>
        {
            if keywords.is_empty() {
                return Err(retrieval_empty());
            }
            Ok((
                vec![],
                keywords,
                "keyword",
                Some(error.code.unwrap_or_else(|| "embedding_unavailable".into())),
            ))
        }
        (Some(Ok(vectors)), Some(Err(error)))
            if allow_vector_fallback && recoverable_retrieval_error(&error) =>
        {
            if vectors.is_empty() {
                return Err(retrieval_empty());
            }
            Ok((
                vectors,
                vec![],
                "vector",
                Some(error.code.unwrap_or_else(|| "keyword_search_failed".into())),
            ))
        }
        (Some(Err(error)), _) | (_, Some(Err(error))) => Err(error),
        _ => Err(retrieval_empty()),
    }
}

fn retrieval_empty() -> QueryError {
    QueryError::new(
        axum::http::StatusCode::NOT_FOUND,
        "未检索到相关片段，请检查知识库内容和索引",
    )
    .at_stage("retrieval", "retrieval_empty")
}

fn retrieval_terminal_error(error: &QueryError) -> bool {
    terminal_permission_error(error)
        || (error.status == axum::http::StatusCode::TOO_MANY_REQUESTS
            && error.code.as_deref() != Some("upstream_rate_limited"))
        || error.stage.as_deref() == Some("permission")
        || matches!(
            error.code.as_deref(),
            Some("rag_deadline_exceeded" | "client_cancelled")
        )
}

fn local_retrieval_error(error: &QueryError) -> bool {
    !retrieval_terminal_error(error)
        && match error.stage.as_deref() {
            Some("keyword_search") => matches!(
                error.code.as_deref(),
                Some("keyword_search_failed" | "stage_timeout")
            ),
            Some("vector_search") => matches!(
                error.code.as_deref(),
                Some("vector_search_failed" | "stage_timeout")
            ),
            _ => false,
        }
}

fn recoverable_retrieval_error(error: &QueryError) -> bool {
    !retrieval_terminal_error(error)
        && (recoverable_embedding_error(error)
            || matches!(
                error.code.as_deref(),
                Some("vector_search_failed" | "keyword_search_failed" | "stage_timeout")
            ))
}

/// 隐式本地容错沿用旧融合分数；显式单路降级保留该路原始分数。
fn finish_hybrid_results(
    vectors: Vec<super::models::SearchResult>,
    keywords: Vec<super::models::SearchResult>,
    mode: &str,
    legacy_local_fallback: bool,
    top_k: usize,
    weights: (f32, f32),
    fusion_mode: retriever::FusionMode,
) -> Vec<retriever::ScoredSearchResult> {
    if mode == "hybrid" || legacy_local_fallback {
        retriever::fuse_scored(
            &vectors,
            &keywords,
            top_k,
            weights.0,
            weights.1,
            fusion_mode,
        )
    } else if mode == "vector" {
        score_single_branch(vectors, true)
    } else {
        score_single_branch(keywords, false)
    }
}

fn score_single_branch(
    results: Vec<super::models::SearchResult>,
    vector: bool,
) -> Vec<retriever::ScoredSearchResult> {
    results
        .into_iter()
        .map(|result| {
            let score = result.score;
            retriever::ScoredSearchResult {
                result,
                vector_score: vector.then_some(score),
                keyword_score: (!vector).then_some(score),
            }
        })
        .collect()
}

pub(crate) fn validate_candidate_k(
    top_k: usize,
    candidate_k: Option<usize>,
) -> Result<(), QueryError> {
    if candidate_k.is_some_and(|count| count < top_k || count > 100) {
        return Err(QueryError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "candidate_k必须介于top_k与100之间",
        )
        .at_stage("permission", "invalid_query"));
    }
    Ok(())
}
/// 返回规范档位；默认不注入参数。深研究有多次生成，首版不虚称整条链路已发送。
pub(crate) fn validate_reasoning_request(
    effort: Option<&str>,
    deep_research: bool,
) -> Result<Option<&str>, QueryError> {
    let effort = match effort {
        None | Some("default") => return Ok(None),
        Some(effort @ ("none" | "low" | "medium" | "high")) => effort,
        Some(_) => {
            return Err(QueryError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "reasoning_effort只允许default、none、low、medium、high",
            )
            .at_stage("permission", "invalid_reasoning_effort"));
        }
    };
    if deep_research {
        return Err(QueryError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "deep_research暂不支持显式reasoning_effort档位，请省略或使用default",
        )
        .at_stage("permission", "invalid_reasoning_effort"));
    }
    Ok(Some(effort))
}

fn reasoning_info(effort: Option<&str>, status: ReasoningStatus) -> Option<RagReasoning> {
    effort.map(|requested| RagReasoning {
        requested: requested.to_string(),
        status,
    })
}

/// 只写网关已有的通用协议字段，不从模型别名推断供应商能力。
fn apply_reasoning_effort(
    request: &mut serde_json::Value,
    effort: Option<&str>,
) -> Result<Option<RagReasoning>, QueryError> {
    let effort = validate_reasoning_request(effort, false)?;
    if let Some(effort) = effort {
        request["reasoning_effort"] = serde_json::json!(effort);
    }
    Ok(reasoning_info(effort, ReasoningStatus::Requested))
}

#[cfg(test)]
mod reasoning_tests {
    use super::*;

    #[test]
    fn defaults_leave_the_request_unchanged_and_explicit_levels_use_only_canonical_field() {
        for effort in [
            None,
            Some("default"),
            Some("none"),
            Some("low"),
            Some("medium"),
            Some("high"),
        ] {
            let mut request =
                serde_json::json!({"model":"opaque-alias","max_tokens":123,"stream":false});
            let original = request.clone();
            let info = apply_reasoning_effort(&mut request, effort).unwrap();
            if let Some(effort @ ("none" | "low" | "medium" | "high")) = effort {
                assert_eq!(request["reasoning_effort"], effort);
                assert_eq!(request["max_tokens"], 123);
                assert_eq!(request.as_object().unwrap().len(), 4);
                let info = info.unwrap();
                assert_eq!(info.requested, effort);
                assert_eq!(info.status, ReasoningStatus::Requested);
            } else {
                assert_eq!(request, original);
                assert!(info.is_none());
            }
        }
    }

    #[test]
    fn invalid_levels_and_deep_research_do_not_mutate_the_body() {
        for effort in ["", "HIGH", "auto", "max", " low"] {
            let mut request = serde_json::json!({"model":"alias"});
            let error = apply_reasoning_effort(&mut request, Some(effort)).unwrap_err();
            assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
            assert_eq!(error.code.as_deref(), Some("invalid_reasoning_effort"));
            assert_eq!(request, serde_json::json!({"model":"alias"}));
        }
        for effort in ["none", "low", "medium", "high"] {
            let error = validate_reasoning_request(Some(effort), true).unwrap_err();
            assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
            assert!(error.message.contains("deep_research"));
        }
        assert_eq!(validate_reasoning_request(None, true).unwrap(), None);
        assert_eq!(
            validate_reasoning_request(Some("default"), true).unwrap(),
            None
        );
    }

    #[test]
    fn legacy_input_and_answer_keep_optional_reasoning_fields_absent() {
        let input: super::super::models::AskInput =
            serde_json::from_value(serde_json::json!({"question":"q"})).unwrap();
        assert!(input.reasoning_effort.is_none());
        let old = serde_json::json!({"answer":"a","sources":[],"usage":null});
        let answer: RagAnswer = serde_json::from_value(old).unwrap();
        assert!(serde_json::to_value(answer)
            .unwrap()
            .get("reasoning")
            .is_none());
        let info = reasoning_info(Some("none"), ReasoningStatus::NotSent).unwrap();
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({"requested":"none","status":"not_sent"})
        );
    }
}

/// 阶段预算只作用于显式总预算请求，给后续检索/回答预留时间。
fn run_stage<T>(
    stage: &'static str,
    fraction: f64,
    reserve: f64,
    future: impl Future<Output = Result<T, QueryError>>,
) -> impl Future<Output = Result<T, QueryError>> {
    let future = Box::pin(future);
    async move {
        let Some(parent) = budget::current() else {
            return future.await;
        };
        parent
            .check_request()
            .map_err(|elapsed| QueryError::from_budget(elapsed, stage))?;
        let cap = parent.total_duration().mul_f64(fraction);
        let child = parent.stage(stage, cap, parent.total_duration().mul_f64(reserve));
        let result = child.scope(budget::run(cap, future)).await;
        // 权限和额度错误保持可信分类，绝不能因阶段计时边缘而变成可降级失败。
        if matches!(&result, Ok(Err(error)) if terminal_permission_error(error) || error.stage.as_deref() == Some("permission"))
        {
            return result.unwrap();
        }
        parent
            .check_request()
            .map_err(|elapsed| QueryError::from_budget(elapsed, stage))?;
        let timeout_error = |elapsed| {
            if elapsed == budget::BudgetElapsed::Deadline {
                QueryError::new(
                    axum::http::StatusCode::GATEWAY_TIMEOUT,
                    "RAG 阶段达到软超时上限",
                )
                .at_stage(stage, "stage_timeout")
            } else {
                QueryError::from_budget(elapsed, stage)
            }
        };
        child.check().map_err(timeout_error)?;
        let result = result.map_err(timeout_error)?;
        result.map_err(|error| {
            if error.code.as_deref() == Some("rag_deadline_exceeded") {
                timeout_error(budget::BudgetElapsed::Deadline)
            } else {
                error
            }
        })
    }
}

fn terminal_permission_error(error: &QueryError) -> bool {
    error.code.as_deref() == Some("upstream_authentication_failed")
        || (error.status == axum::http::StatusCode::TOO_MANY_REQUESTS
            && budget::current().is_some()
            && error.code.as_deref() != Some("upstream_rate_limited"))
        || matches!(
            error.status,
            axum::http::StatusCode::UNAUTHORIZED
                | axum::http::StatusCode::FORBIDDEN
                | axum::http::StatusCode::PAYMENT_REQUIRED
                | axum::http::StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS
        )
}

fn recoverable_embedding_error(error: &QueryError) -> bool {
    !terminal_permission_error(error)
        && matches!(
            error.code.as_deref(),
            Some(
                "stage_timeout"
                    | "model_timeout"
                    | "upstream_transport_failed"
                    | "upstream_unavailable"
                    | "invalid_embedding_response"
                    | "invalid_model_response"
                    | "upstream_protocol_error"
            )
        )
}

pub(crate) fn record_stage(
    stages: &mut Vec<RagDiagnosticStage>,
    stage: &str,
    status: &str,
    started: Instant,
) {
    record_stage_result(stages, stage, status, started, None);
}

fn record_stage_result(
    stages: &mut Vec<RagDiagnosticStage>,
    stage: &str,
    status: &str,
    started: Instant,
    code: Option<&str>,
) {
    let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    let deadline_scope = match code {
        Some("stage_timeout") => Some("stage"),
        Some("model_timeout") => Some("channel"),
        Some("rag_deadline_exceeded" | "client_cancelled") => Some("request"),
        _ => None,
    };
    tracing::info!(
        stage,
        status,
        elapsed_ms,
        code,
        deadline_scope,
        "RAG 阶段完成"
    );
    stages.push(RagDiagnosticStage {
        stage: stage.to_string(),
        status: status.to_string(),
        elapsed_ms,
        code: code.map(str::to_string),
        deadline_scope: deadline_scope.map(str::to_string),
    });
}

pub(crate) fn diagnostic_failure(
    mut error: QueryError,
    stage: &str,
    started: Instant,
    stages: &[RagDiagnosticStage],
    request_id: &str,
    enabled: bool,
) -> QueryError {
    if error.stage.is_none() {
        error.stage = Some(stage.to_string());
    }
    if error.code.is_none() {
        error.code = Some(format!("{stage}_failed"));
    }
    if !request_id.is_empty() {
        error.request_id = Some(request_id.to_string());
    }
    tracing::warn!(
        request_id,
        stage,
        code = error.code.as_deref().unwrap_or("unknown"),
        status = error.status.as_u16(),
        "知识库查询阶段失败"
    );
    if enabled {
        let mut stages = stages.to_vec();
        record_stage_result(&mut stages, stage, "failed", started, error.code.as_deref());
        error.diagnostics = Some(Box::new(RagDiagnostics {
            request_id: request_id.to_string(),
            stages,
        }));
    }
    error
}

fn source_info(
    result: &super::models::SearchResult,
    anchors: &[String],
    windowed: bool,
    include_content: bool,
) -> SourceInfo {
    let (snippet, start) = if windowed {
        retriever::evidence_window(&result.content, anchors, 200)
    } else {
        (result.content.chars().take(200).collect(), 0)
    };
    SourceInfo {
        filename: result.filename.clone(),
        score: result.score,
        snippet,
        chunk_id: windowed.then(|| result.chunk_id.clone()),
        doc_id: windowed.then(|| result.doc_id.clone()),
        section: windowed
            .then(|| {
                retriever::section_at(&result.content, start).or_else(|| {
                    result
                        .metadata
                        .get("heading")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
            })
            .flatten(),
        page_no: windowed
            .then(|| result.metadata.get("page_no").and_then(|v| v.as_i64()))
            .flatten(),
        snippet_start: windowed.then_some(start),
        // 原文始终来自最终实际使用的上下文，供客户端核对引用。
        evidence_text: include_content.then(|| result.content.clone()),
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
#[cfg(test)]
fn trim_context(
    results: &[super::models::SearchResult],
    query: &str,
    history: &[ConversationMessage],
    target_tokens: usize,
) -> (String, Vec<super::models::SearchResult>) {
    trim_context_with_order(results, query, history, target_tokens, false)
}

fn trim_context_with_order(
    results: &[super::models::SearchResult],
    query: &str,
    history: &[ConversationMessage],
    target_tokens: usize,
    ordered: bool,
) -> (String, Vec<super::models::SearchResult>) {
    // 按低分顺序移除，但保留剩余切片原来的展示顺序。
    let mut indexed: Vec<_> = results.iter().enumerate().collect();
    if ordered {
        indexed.reverse();
    } else {
        indexed.sort_by(|a, b| {
            a.1.score
                .partial_cmp(&b.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
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

    #[test]
    fn ordered_trim_keeps_rerank_priority_even_when_old_scores_disagree() {
        let mut high_priority = result("selected", 0.1, &"证据".repeat(200));
        high_priority.metadata = serde_json::json!({});
        let low_priority = result("discarded", 0.9, &"背景".repeat(200));
        let budget = retriever::estimate_tokens(&build_rag_prompt(
            &build_context(&[high_priority.clone()]),
            "query",
            &[],
        ));
        let (_, used) =
            trim_context_with_order(&[high_priority, low_priority], "query", &[], budget, true);
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].chunk_id, "selected");
    }

    #[test]
    fn source_window_points_into_exact_final_content() {
        let content = format!(
            "{}3.2.2. 【强制】不支持 FETCH 控制语句。{}",
            "页眉\n".repeat(100),
            "尾注\n".repeat(100)
        );
        let search_result = result("actual", 0.1, &content);
        let source = source_info(&search_result, &["FETCH".into()], true, true);
        assert!(source.snippet.contains("不支持 FETCH"));
        assert_eq!(source.section.as_deref(), Some("3.2.2"));
        assert_eq!(source.evidence_text.as_deref(), Some(content.as_str()));
        assert_eq!(
            source.snippet,
            content
                .chars()
                .skip(source.snippet_start.unwrap())
                .take(200)
                .collect::<String>()
        );
        let ordinary = source_info(&search_result, &[], false, false);
        assert!(ordinary.chunk_id.is_none() && ordinary.evidence_text.is_none());
    }

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
                .map(|r| source_info(r, &[], false, false))
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
                diagnostics: None,
                retrieval_mode: None,
                degradation_reason: None,
                reasoning: None,
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
    client: &ModelClient<'_>,
    pool: &SqlitePool,
    chat_model: &str,
    query: &str,
    history: &[ConversationMessage],
) -> Result<String, QueryError> {
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
    let proxy_result = run_stage(
        "rewrite",
        0.1,
        0.85,
        Box::pin(client.chat(chat_request, "RAG-rewrite")),
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
        Err(error) if terminal_permission_error(&error) => return Err(error),
        Err(_) => String::new(),
    };
    match extract_rewrite_query(&reply) {
        Some(rewritten) => {
            tracing::debug!("[RAG] 查询改写完成");
            Ok(rewritten)
        }
        None => {
            tracing::warn!("[RAG] 查询改写回复不可解析，回退原查询");
            Ok(query.to_string())
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
        let result = rewrite_query_with_llm(
            &ModelClient::Internal {
                pool: &pool,
                settings: &settings,
                kb_id: "kb-1",
            },
            &pool,
            "m",
            "那限流呢？",
            &history,
        )
        .await;
        assert_eq!(
            result.unwrap(),
            "那限流呢？",
            "上游不可用时必须静默回退原查询"
        );
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
    client: &ModelClient<'_>,
    chat_model: &str,
    query: &str,
    candidates: Vec<retriever::ScoredSearchResult>,
    window_anchors: Option<&[String]>,
) -> Result<Vec<retriever::ScoredSearchResult>, QueryError> {
    let mut listing = String::new();
    for (i, c) in candidates.iter().enumerate() {
        let excerpt = if let Some(anchors) = window_anchors {
            let query_anchors;
            let anchors = if anchors.is_empty() {
                query_anchors = super::text::query_tokens(query);
                query_anchors.as_slice()
            } else {
                anchors
            };
            retriever::evidence_window(&c.result.content, anchors, 400).0
        } else {
            c.result.content.chars().take(200).collect()
        }
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
    let proxy_result =
        run_stage("rerank", 0.1, 0.35, client.chat(chat_request, "RAG-rerank")).await;

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
        Err(error) if terminal_permission_error(&error) => return Err(error),
        Err(_) => String::new(),
    };
    match parse_rerank_order(&reply, candidates.len()) {
        Some(order) => {
            let mut reordered = Vec::with_capacity(candidates.len());
            for index in order {
                reordered.push(candidates[index].clone());
            }
            tracing::debug!("[RAG] LLM 重排生效（{} 候选）", reordered.len());
            Ok(reordered)
        }
        None => {
            tracing::warn!("[RAG] LLM 重排回复不可解析，回退原序（相关性排序）");
            Ok(candidates)
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

#[cfg(test)]
#[path = "rag_retrieval_tests.rs"]
mod retrieval_tests;
