use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn result(id: &str) -> super::super::models::SearchResult {
    super::super::models::SearchResult {
        chunk_id: id.into(),
        doc_id: id.into(),
        filename: format!("{id}.txt"),
        content: format!("original {id}"),
        score: 0.8,
        metadata: serde_json::json!({}),
    }
}

fn failure(stage: &str, code: &str) -> QueryError {
    QueryError::new(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "fixture failure",
    )
    .at_stage(stage, code)
}

#[test]
fn normal_hybrid_keeps_both_channels_and_exact_content() {
    let (v, k, mode, reason) = choose_hybrid_results(
        Some(Ok(vec![result("v")])),
        Some(Ok(vec![result("k")])),
        false,
        false,
        true,
    )
    .unwrap();
    assert_eq!(mode, "hybrid");
    assert!(reason.is_none());
    let fused = retriever::fuse_scored(&v, &k, 5, 0.7, 0.3, retriever::FusionMode::Rrf);
    assert_eq!(fused.len(), 2);
    assert!(fused
        .iter()
        .any(|item| item.vector_score.is_some() && item.result.content == "original v"));
    assert!(fused
        .iter()
        .any(|item| item.keyword_score.is_some() && item.result.content == "original k"));
}

#[test]
fn fallback_flags_authorize_only_their_own_direction() {
    for (allow_k, allow_v) in [(false, false), (false, true)] {
        let error = choose_hybrid_results(
            Some(Err(failure("vector_search", "vector_search_failed"))),
            Some(Ok(vec![result("k")])),
            allow_k,
            allow_v,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("vector_search_failed"));
    }
    let (_, k, mode, reason) = choose_hybrid_results(
        Some(Err(failure("embedding", "stage_timeout"))),
        Some(Ok(vec![result("k")])),
        true,
        false,
        true,
    )
    .unwrap();
    assert_eq!(k.len(), 1);
    assert_eq!(mode, "keyword");
    assert_eq!(reason.as_deref(), Some("stage_timeout"));
    for (allow_k, allow_v) in [(false, false), (true, false)] {
        let error = choose_hybrid_results(
            Some(Ok(vec![result("v")])),
            Some(Err(failure("keyword_search", "keyword_search_failed"))),
            allow_k,
            allow_v,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("keyword_search_failed"));
    }
    let (v, _, mode, reason) = choose_hybrid_results(
        Some(Ok(vec![result("v")])),
        Some(Err(failure("keyword_search", "keyword_search_failed"))),
        false,
        true,
        true,
    )
    .unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(mode, "vector");
    assert_eq!(reason.as_deref(), Some("keyword_search_failed"));
}

#[tokio::test]
async fn permission_quota_parent_deadline_and_cancel_never_degrade() {
    let budget = budget::Budget::new(1000, "terminal");
    budget
        .scope(async {
            for (status, code) in [
                (401, "authentication_failed"),
                (403, "knowledge_access_denied"),
                (402, "upstream_quota_exceeded"),
                (429, "quota_exceeded"),
                (504, "rag_deadline_exceeded"),
                (499, "client_cancelled"),
            ] {
                for vector_fails in [true, false] {
                    let error = QueryError::new(
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        "terminal",
                    )
                    .at_stage("embedding", code);
                    let chosen = if vector_fails {
                        choose_hybrid_results(
                            Some(Err(error)),
                            Some(Ok(vec![result("k")])),
                            true,
                            true,
                            true,
                        )
                    } else {
                        choose_hybrid_results(
                            Some(Ok(vec![result("v")])),
                            Some(Err(error)),
                            true,
                            true,
                            true,
                        )
                    };
                    assert_eq!(chosen.unwrap_err().code.as_deref(), Some(code));
                }
            }
            let error = failure("permission", "permission_check_failed");
            assert!(choose_hybrid_results(
                Some(Err(error)),
                Some(Ok(vec![result("k")])),
                true,
                true,
                true
            )
            .is_err());
        })
        .await;
}

#[test]
fn both_failures_and_empty_survivors_are_not_successful_fallbacks() {
    let error = choose_hybrid_results(
        Some(Err(failure("vector_search", "vector_search_failed"))),
        Some(Err(failure("keyword_search", "keyword_search_failed"))),
        true,
        true,
        true,
    )
    .unwrap_err();
    assert_eq!(error.code.as_deref(), Some("retrieval_both_failed"));
    for vector_fails in [true, false] {
        let error = if vector_fails {
            choose_hybrid_results(
                Some(Err(failure("vector_search", "vector_search_failed"))),
                Some(Ok(vec![])),
                true,
                true,
                true,
            )
        } else {
            choose_hybrid_results(
                Some(Ok(vec![])),
                Some(Err(failure("keyword_search", "keyword_search_failed"))),
                true,
                true,
                true,
            )
        }
        .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("retrieval_empty"));
    }
    let (v, k, mode, reason) =
        choose_hybrid_results(Some(Ok(vec![])), Some(Ok(vec![])), false, false, true).unwrap();
    assert!(v.is_empty() && k.is_empty());
    assert_eq!(mode, "hybrid");
    assert!(reason.is_none());
}

#[test]
fn quota_without_explicit_budget_is_not_masked_as_both_searches_failed() {
    assert!(budget::current().is_none());
    let quota = QueryError::new(axum::http::StatusCode::TOO_MANY_REQUESTS, "quota exhausted")
        .at_stage("embedding", "quota_or_rate_limited");
    let error = choose_hybrid_results(
        Some(Err(quota)),
        Some(Err(failure("keyword_search", "keyword_search_failed"))),
        true,
        true,
        true,
    )
    .unwrap_err();
    assert_eq!(error.status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error.code.as_deref(), Some("quota_or_rate_limited"));
}

#[test]
fn legacy_local_failure_preserves_weighted_and_rrf_fusion_scores() {
    for vector_fails in [true, false] {
        for fusion_mode in [retriever::FusionMode::Weighted, retriever::FusionMode::Rrf] {
            let (vectors, keywords, mode, reason) = if vector_fails {
                choose_hybrid_results(
                    Some(Err(failure("vector_search", "vector_search_failed"))),
                    Some(Ok(vec![result("k")])),
                    false,
                    false,
                    false,
                )
            } else {
                choose_hybrid_results(
                    Some(Ok(vec![result("v")])),
                    Some(Err(failure("keyword_search", "keyword_search_failed"))),
                    false,
                    false,
                    false,
                )
            }
            .unwrap();
            assert_eq!(mode, if vector_fails { "keyword" } else { "vector" });
            assert_eq!(
                reason.as_deref(),
                Some(if vector_fails {
                    "vector_search_failed"
                } else {
                    "keyword_search_failed"
                })
            );
            let fused =
                finish_hybrid_results(vectors, keywords, mode, true, 5, (0.7, 0.3), fusion_mode);
            assert_eq!(fused.len(), 1);
            let expected = match fusion_mode {
                retriever::FusionMode::Weighted => {
                    if vector_fails {
                        0.24
                    } else {
                        0.56
                    }
                }
                retriever::FusionMode::Rrf => 1.0 / 61.0,
            };
            assert!((fused[0].result.score - expected).abs() < 1e-6);
            assert_eq!(fused[0].vector_score, (!vector_fails).then_some(0.8));
            assert_eq!(fused[0].keyword_score, vector_fails.then_some(0.8));
            assert_eq!(
                fused[0].result.content,
                if vector_fails {
                    "original k"
                } else {
                    "original v"
                }
            );
        }
    }
}

#[test]
fn legacy_both_local_failures_and_empty_survivor_remain_empty_with_reason() {
    let (vectors, keywords, mode, reason) = choose_hybrid_results(
        Some(Err(failure("vector_search", "vector_search_failed"))),
        Some(Err(failure("keyword_search", "keyword_search_failed"))),
        false,
        false,
        false,
    )
    .unwrap();
    assert_eq!(mode, "none");
    assert_eq!(reason.as_deref(), Some("retrieval_both_failed"));
    assert!(finish_hybrid_results(
        vectors,
        keywords,
        mode,
        true,
        5,
        (0.7, 0.3),
        retriever::FusionMode::Rrf,
    )
    .is_empty());
    for vector_fails in [true, false] {
        let (vectors, keywords, mode, reason) = if vector_fails {
            choose_hybrid_results(
                Some(Err(failure("vector_search", "vector_search_failed"))),
                Some(Ok(vec![])),
                false,
                false,
                false,
            )
        } else {
            choose_hybrid_results(
                Some(Ok(vec![])),
                Some(Err(failure("keyword_search", "keyword_search_failed"))),
                false,
                false,
                false,
            )
        }
        .unwrap();
        assert!(vectors.is_empty() && keywords.is_empty());
        assert_eq!(mode, if vector_fails { "keyword" } else { "vector" });
        assert!(reason.is_some());
    }
}

#[test]
fn legacy_embedding_failure_never_implicitly_uses_keyword_results() {
    for code in [
        "stage_timeout",
        "invalid_embedding_response",
        "upstream_unavailable",
    ] {
        for keyword_failed in [false, true] {
            let keyword = if keyword_failed {
                Err(failure("keyword_search", "keyword_search_failed"))
            } else {
                Ok(vec![result("k")])
            };
            let error = choose_hybrid_results(
                Some(Err(failure("embedding", code))),
                Some(keyword),
                false,
                false,
                false,
            )
            .unwrap_err();
            assert_eq!(error.stage.as_deref(), Some("embedding"));
            assert_eq!(error.code.as_deref(), Some(code));
        }
        for strict in [false, true] {
            let (_, keywords, mode, reason) = choose_hybrid_results(
                Some(Err(failure("embedding", code))),
                Some(Ok(vec![result("k")])),
                true,
                false,
                strict,
            )
            .unwrap();
            assert_eq!(mode, "keyword");
            assert_eq!(reason.as_deref(), Some(code));
            assert_eq!(keywords.len(), 1);
        }
    }
}

#[test]
fn legacy_soft_timeout_is_local_only_when_its_stage_is_local() {
    for vector_fails in [false, true] {
        let (vectors, keywords, mode, reason) = if vector_fails {
            choose_hybrid_results(
                Some(Err(failure("vector_search", "stage_timeout"))),
                Some(Ok(vec![result("k")])),
                false,
                false,
                false,
            )
        } else {
            choose_hybrid_results(
                Some(Ok(vec![result("v")])),
                Some(Err(failure("keyword_search", "stage_timeout"))),
                false,
                false,
                false,
            )
        }
        .unwrap();
        assert_eq!(mode, if vector_fails { "keyword" } else { "vector" });
        assert_eq!(vectors.len() + keywords.len(), 1);
        assert_eq!(reason.as_deref(), Some("stage_timeout"));
    }
    let error = choose_hybrid_results(
        Some(Err(failure("embedding", "stage_timeout"))),
        Some(Ok(vec![result("k")])),
        false,
        false,
        false,
    )
    .unwrap_err();
    assert_eq!(error.stage.as_deref(), Some("embedding"));
}

#[test]
fn either_explicit_direction_disables_implicit_other_direction() {
    for strict in [false, true] {
        let error = choose_hybrid_results(
            Some(Err(failure("vector_search", "vector_search_failed"))),
            Some(Ok(vec![result("k")])),
            false,
            true,
            strict,
        )
        .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("vector_search_failed"));
        let error = choose_hybrid_results(
            Some(Ok(vec![result("v")])),
            Some(Err(failure("keyword_search", "keyword_search_failed"))),
            true,
            false,
            strict,
        )
        .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("keyword_search_failed"));
    }
}

#[test]
fn explicit_single_branch_fallback_keeps_raw_score_even_when_strict() {
    for vector_fails in [true, false] {
        let (vectors, keywords, mode, _) = if vector_fails {
            choose_hybrid_results(
                Some(Err(failure("vector_search", "vector_search_failed"))),
                Some(Ok(vec![result("k")])),
                true,
                false,
                true,
            )
        } else {
            choose_hybrid_results(
                Some(Ok(vec![result("v")])),
                Some(Err(failure("keyword_search", "keyword_search_failed"))),
                false,
                true,
                true,
            )
        }
        .unwrap();
        let scored = finish_hybrid_results(
            vectors,
            keywords,
            mode,
            false,
            5,
            (0.7, 0.3),
            retriever::FusionMode::Rrf,
        );
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].result.score, 0.8);
    }
}

#[test]
fn legacy_local_compatibility_never_masks_terminal_failure() {
    for (status, code) in [
        (401, "authentication_failed"),
        (403, "knowledge_access_denied"),
        (402, "upstream_quota_exceeded"),
        (429, "quota_or_rate_limited"),
        (504, "rag_deadline_exceeded"),
        (499, "client_cancelled"),
    ] {
        for vector_fails in [true, false] {
            for other_failed in [true, false] {
                let terminal = QueryError::new(
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    "terminal",
                )
                .at_stage(
                    if vector_fails {
                        "vector_search"
                    } else {
                        "keyword_search"
                    },
                    code,
                );
                let other = if other_failed {
                    Err(failure(
                        if vector_fails {
                            "keyword_search"
                        } else {
                            "vector_search"
                        },
                        if vector_fails {
                            "keyword_search_failed"
                        } else {
                            "vector_search_failed"
                        },
                    ))
                } else {
                    Ok(vec![result("survivor")])
                };
                let chosen = if vector_fails {
                    choose_hybrid_results(Some(Err(terminal)), Some(other), false, false, false)
                } else {
                    choose_hybrid_results(Some(other), Some(Err(terminal)), false, false, false)
                };
                let error = chosen.unwrap_err();
                assert_eq!(error.status.as_u16(), status);
                assert_eq!(error.code.as_deref(), Some(code));
            }
        }
    }
    let error = choose_hybrid_results(
        Some(Err(failure("permission", "permission_check_failed"))),
        Some(Ok(vec![result("k")])),
        false,
        false,
        false,
    )
    .unwrap_err();
    assert_eq!(error.stage.as_deref(), Some("permission"));
}

#[tokio::test]
async fn embedding_starts_vector_before_keyword_finishes_and_times_phases_separately() {
    let vector_started = Arc::new(tokio::sync::Notify::new());
    let vector_finished = Arc::new(tokio::sync::Notify::new());
    let keyword_done = Arc::new(AtomicBool::new(false));
    let vector = vector_branch(
        async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(vec![vec![1.0]])
        },
        |_| async {
            assert!(!keyword_done.load(Ordering::Acquire), "向量检索不应等待FTS");
            vector_started.notify_one();
            tokio::time::sleep(Duration::from_millis(5)).await;
            vector_finished.notify_one();
            Ok((vec![result("v")], None))
        },
        (0.7, 0.2),
        (1.0, 0.0),
    );
    let keyword = async {
        let (results, stage) = timed_retrieval_stage("keyword_search", (0.3, 0.0), async {
            vector_started.notified().await;
            vector_finished.notified().await;
            tokio::time::sleep(Duration::from_millis(40)).await;
            keyword_done.store(true, Ordering::Release);
            Ok(vec![result("k")])
        })
        .await;
        RetrievalBranch {
            results,
            stages: vec![stage],
            degradation_reason: None,
        }
    };
    let (v, k) = collect_hybrid_branches(vector, keyword).await;
    let v = v.unwrap();
    let k = k.unwrap();
    assert_eq!(
        v.stages
            .iter()
            .map(|stage| stage.stage.as_str())
            .collect::<Vec<_>>(),
        ["embedding", "vector_search"]
    );
    assert_eq!(k.stages[0].stage, "keyword_search");
    assert!(v.stages.iter().all(|stage| stage.status == "passed"));
    assert!(
        v.stages[0].elapsed_ms + v.stages[1].elapsed_ms < k.stages[0].elapsed_ms,
        "Embedding和向量耗时不能包含等待FTS的时间"
    );
}

#[tokio::test(start_paused = true)]
async fn search_final_retrieval_uses_remaining_time_but_parallel_fts_keeps_its_cap() {
    let budget = budget::Budget::new(1000, "remaining");
    budget
        .scope(async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let result = run_stage("vector_search", 1.0, 0.0, async {
                tokio::time::sleep(Duration::from_millis(400)).await;
                Ok(7)
            })
            .await;
            assert_eq!(result.unwrap(), 7);
            assert!(budget.remaining() <= Duration::from_millis(200));
        })
        .await;
    let budget = budget::Budget::new(1000, "fts-cap");
    let error = budget
        .scope(run_stage(
            "keyword_search",
            0.3,
            0.0,
            std::future::pending::<Result<(), QueryError>>(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_deref(), Some("stage_timeout"));
    assert!(budget.remaining() >= Duration::from_millis(700));
}

#[tokio::test(start_paused = true)]
async fn ask_reserves_answer_time_and_soft_deadline_is_not_parent_deadline() {
    let budget = budget::Budget::new(1000, "reserve");
    budget
        .scope(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let (result, stage) =
                timed_retrieval_stage::<u8>("vector_search", (0.2, 0.35), std::future::pending())
                    .await;
            assert_eq!(result.unwrap_err().code.as_deref(), Some("stage_timeout"));
            assert_eq!(stage.deadline_scope.as_deref(), Some("stage"));
            assert!(budget.remaining() >= Duration::from_millis(350));
        })
        .await;
    let budget = budget::Budget::new(100, "parent");
    let (result, stage) = budget
        .scope(timed_retrieval_stage::<u8>(
            "vector_search",
            (1.0, 0.0),
            std::future::pending(),
        ))
        .await;
    assert_eq!(
        result.unwrap_err().code.as_deref(),
        Some("rag_deadline_exceeded")
    );
    assert_eq!(stage.deadline_scope.as_deref(), Some("request"));
}

#[tokio::test]
async fn empty_and_failed_stages_never_report_passed() {
    let (result, empty) =
        timed_retrieval_stage::<u8>("keyword_search", (0.3, 0.0), async { Ok(vec![]) }).await;
    assert!(result.unwrap().is_empty());
    assert_eq!(empty.status, "empty");
    let (_, failed) = timed_retrieval_stage::<u8>("keyword_search", (0.3, 0.0), async {
        Err(failure("keyword_search", "keyword_search_failed"))
    })
    .await;
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.code.as_deref(), Some("keyword_search_failed"));
    assert!(failed.deadline_scope.is_none());
    let search_started = AtomicBool::new(false);
    let invalid = vector_branch(
        async { Ok(vec![vec![f32::NAN]]) },
        |_| async {
            search_started.store(true, Ordering::Release);
            Ok((vec![], None))
        },
        (0.7, 0.2),
        (1.0, 0.0),
    )
    .await;
    assert!(!search_started.load(Ordering::Acquire));
    assert_eq!(
        invalid.results.unwrap_err().code.as_deref(),
        Some("invalid_embedding_response")
    );
    assert_eq!(invalid.stages.len(), 1);
    assert_eq!(invalid.stages[0].status, "failed");
}

#[tokio::test]
async fn cross_kb_partial_vector_results_never_report_full_success() {
    for empty in [false, true] {
        let partial = vector_branch(
            async { Ok(vec![vec![1.0]]) },
            |_| async {
                Ok((
                    if empty {
                        vec![]
                    } else {
                        vec![result("survivor")]
                    },
                    Some("knowledge_base_search_partial".into()),
                ))
            },
            (0.7, 0.2),
            (1.0, 0.0),
        )
        .await;
        assert_eq!(partial.results.unwrap().is_empty(), empty);
        assert_eq!(
            partial.degradation_reason.as_deref(),
            Some("knowledge_base_search_partial")
        );
        assert_eq!(partial.stages[0].status, "passed");
        assert_eq!(partial.stages[1].stage, "vector_search");
        assert_eq!(partial.stages[1].status, "degraded");
        assert_eq!(
            partial.stages[1].code.as_deref(),
            Some("knowledge_base_search_partial")
        );
        assert!(partial.stages[1].deadline_scope.is_none());
    }
}

#[tokio::test]
async fn terminal_branch_drops_other_pending_work() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let pending = async move {
        let _guard = guard;
        std::future::pending::<RetrievalBranch>().await
    };
    let failed = async {
        RetrievalBranch {
            results: Err(QueryError::new(axum::http::StatusCode::FORBIDDEN, "denied")
                .at_stage("embedding", "upstream_authentication_failed")),
            stages: vec![],
            degradation_reason: None,
        }
    };
    let (v, k) = collect_hybrid_branches(failed, pending).await;
    assert!(v.unwrap().results.is_err());
    assert!(k.is_none());
    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn total_cancel_wakes_and_drops_both_branches_without_waiting_for_caps() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let budget = budget::Budget::new(1000, "cancel");
    let vector_dropped = Arc::new(AtomicBool::new(false));
    let keyword_dropped = Arc::new(AtomicBool::new(false));
    let vector_guard = Dropped(vector_dropped.clone());
    let keyword_guard = Dropped(keyword_dropped.clone());
    let vector_started = Arc::new(tokio::sync::Notify::new());
    let keyword_started = Arc::new(tokio::sync::Notify::new());
    let vector = async {
        let _guard = vector_guard;
        let (results, stage) = timed_retrieval_stage::<super::super::models::SearchResult>(
            "vector_search",
            (1.0, 0.0),
            async {
                vector_started.notify_one();
                std::future::pending().await
            },
        )
        .await;
        RetrievalBranch {
            results,
            stages: vec![stage],
            degradation_reason: None,
        }
    };
    let keyword = async {
        let _guard = keyword_guard;
        let (results, stage) = timed_retrieval_stage::<super::super::models::SearchResult>(
            "keyword_search",
            (0.3, 0.0),
            async {
                keyword_started.notify_one();
                std::future::pending().await
            },
        )
        .await;
        RetrievalBranch {
            results,
            stages: vec![stage],
            degradation_reason: None,
        }
    };
    let ((v, k), ()) = budget
        .scope(async {
            tokio::join!(collect_hybrid_branches(vector, keyword), async {
                vector_started.notified().await;
                keyword_started.notified().await;
                budget.cancel();
            })
        })
        .await;
    let error = v.or(k).unwrap().results.unwrap_err();
    assert_eq!(error.code.as_deref(), Some("client_cancelled"));
    assert_eq!(budget.remaining(), Duration::from_secs(1));
    assert!(vector_dropped.load(Ordering::Acquire) && keyword_dropped.load(Ordering::Acquire));
}
