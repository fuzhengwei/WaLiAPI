//! T06 integration tests: `protocol_routing_integration` + `stream_failover`.
//!
//! These drive the REAL T05 facade (`authorize_and_plan` + `execute_plan`) and
//! the REAL streaming commit barrier (`StreamPumpCore` + `StreamSupervisor`)
//! against an in-memory SQLite DB, verifying:
//!   * native-first routing (OpenAI Chat before Anthropic codec G2),
//!   * conversion decode on the non-stream facade path,
//!   * RequestLog + quota accounting on the facade path,
//!   * streaming pre-commit failover (invalid first frame → next candidate) and
//!     the post-commit no-retry barrier.

#![cfg(test)]

use crate::auth_provider::service::AuthService;
use crate::auth_provider::ProviderRegistry;
use crate::core::attempt::{AttemptResult, FailureClass, PreparedAttempt};
use crate::core::feature_flags::FeatureFlags;
use crate::core::route_plan::{authorize_and_plan, EndpointKind};
use crate::core::stream_supervisor::StreamSupervisor;
use crate::db::models::{ApiKey, AuthAccountUpsert, Channel};
use crate::db::repository::Repository;
use crate::endpoint_executor::sse::StreamPumpCore;
use crate::endpoint_executor::StreamAttemptResult;
use crate::protocol::codec::{CodecRegistry, Protocol};
use crate::security::gate::{DownstreamProtocol, RequestEnvelope, RequestFeatures};
use crate::security::SecurityScanResult;
use axum::response::IntoResponse;
use axum::{Json, Router};
use rand::SeedableRng;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn now() -> String {
    crate::utils::time::now_iso()
}

async fn fresh_db() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory db");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrate fresh db");
    pool
}

fn api_key() -> ApiKey {
    ApiKey {
        id: "key-1".into(),
        name: "t".into(),
        key: "sk-test".into(),
        status: 1,
        allowed_models: "[]".into(),
        allowed_channels: "[]".into(),
        denied_models: "[]".into(),
        denied_channels: "[]".into(),
        quota_limit: 0,
        quota_used: 0,
        expires_at: None,
        created_at: now(),
        updated_at: now(),
    }
}

#[allow(clippy::too_many_arguments)]
fn channel(
    id: &str,
    protocol: &str,
    provider: &str,
    native_base: &str,
    endpoints: &[&str],
    priority: i64,
) -> Channel {
    Channel {
        id: id.into(),
        name: format!("ch-{id}"),
        channel_type: if protocol == "anthropic" {
            "claude"
        } else {
            "openai"
        }
        .into(),
        base_url: native_base.into(),
        api_key: "sk-upstream".into(),
        models: json!(["m"]).to_string(),
        status: 1,
        priority,
        weight: 1,
        config: "{}".into(),
        model_mapping: "{}".into(),
        timeout_secs: 30,
        protocol: Some(protocol.into()),
        provider: Some(provider.into()),
        native_base_url: Some(native_base.into()),
        native_endpoints: Some(serde_json::to_string(endpoints).unwrap()),
        preset_revision: Some("test".into()),
        identity_revision: 1,
        legacy_executor_override: None,
        created_at: now(),
        updated_at: now(),
        last_test_at: None,
        last_test_ok: None,
        last_probe_at: None,
        last_probe_ok: None,
        probe_latency_ms: None,
    }
}

fn audited(
    protocol: DownstreamProtocol,
    endpoint: &str,
    model: &str,
    body: Value,
) -> crate::security::gate::AuditedRequest {
    crate::security::gate::AuditedRequest {
        envelope: RequestEnvelope {
            downstream_protocol: protocol,
            endpoint: endpoint.to_string(),
            original_json: body.clone(),
            safe_forward_headers: vec![],
            query: None,
            model: model.to_string(),
            stream: false,
            trace_id: None,
        },
        forward_json: body.clone(),
        sanitized_log_json: body,
        body_hash: "h".into(),
        body_len: 0,
        audit_result: SecurityScanResult::default(),
        request_features: RequestFeatures::default(),
        security_settings: crate::security::SecuritySettings::default(),
    }
}

fn flags(codec_on: bool) -> FeatureFlags {
    FeatureFlags {
        new_routeplan: true,
        cross_protocol_codec: codec_on,
        native_responses: true,
        ollama_native: false,
        prefer_auth_accounts: false,
        prefer_same_protocol: true,
    }
}

/// Split a downstream SSE body into (event, data) frames.  A data-only
/// `[DONE]` frame is labelled `"[DONE]"` so ordering assertions can treat it
/// as an event; other data-only frames get an empty event name.
fn sse_frames(text: &str) -> Vec<(String, String)> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(|f| {
            let mut event = String::new();
            let mut data = String::new();
            for line in f.lines() {
                if let Some(rest) = line.strip_prefix("event:").map(str::trim) {
                    event = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("data:").map(str::trim) {
                    data = rest.to_string();
                }
            }
            if event.is_empty() && data == "[DONE]" {
                event = "[DONE]".to_string();
            }
            (event, data)
        })
        .collect()
}

/// Insert the enabled channels into the pool so the facade's
/// `get_enabled_channels` sees them.
async fn insert_channels(pool: &sqlx::SqlitePool, channels: &[Channel]) {
    for c in channels {
        sqlx::query(
            "INSERT INTO channels (id, name, type, base_url, api_key, models, status, priority, weight, config, model_mapping, timeout_secs, protocol, provider, native_base_url, native_endpoints, preset_revision, identity_revision, legacy_executor_override, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
        )
        .bind(&c.id).bind(&c.name).bind(&c.channel_type).bind(&c.base_url)
        .bind(&c.api_key).bind(&c.models).bind(c.status).bind(c.priority).bind(c.weight)
        .bind(&c.config).bind(&c.model_mapping).bind(c.timeout_secs)
        .bind(&c.protocol).bind(&c.provider).bind(&c.native_base_url).bind(&c.native_endpoints)
        .bind(&c.preset_revision).bind(c.identity_revision).bind(&c.legacy_executor_override)
        .bind(&c.created_at).bind(&c.updated_at)
        .execute(pool)
        .await
        .expect("insert channel");
    }
}

// ── protocol_routing_integration ──────────────────────────────────────────

/// Non-stream Chat with BOTH a native OpenAI channel (low priority) and an
/// Anthropic codec channel (high priority): the native group must win, and the
/// conversion attempt's encoded body must be the codec-shaped Messages body.
#[tokio::test]
async fn protocol_routing_integration_chat_native_first_then_conversion() {
    let pool = fresh_db().await;
    let repo = Arc::new(Repository::new(pool.clone()));
    let native = channel(
        "n1",
        "openai",
        "deepseek",
        "https://api.deepseek.com",
        &["chat_completions"],
        1,
    );
    let conv = channel(
        "c1",
        "anthropic",
        "deepseek",
        "https://api.deepseek.com/anthropic/v1",
        &["messages"],
        100,
    );
    insert_channels(&pool, &[native, conv]).await;

    let key = api_key();
    let audited = audited(
        DownstreamProtocol::ChatCompletions,
        "chat_completions",
        "m",
        json!({"model":"m","messages":[{"role":"user","content":"hi"}]}),
    );
    let channels = repo.get_enabled_channels().await.unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let plan = authorize_and_plan(
        &key,
        "m",
        EndpointKind::ChatCompletions,
        &channels,
        &flags(true),
        &audited.forward_json,
        &mut rng,
    )
    .unwrap();

    assert_eq!(plan.groups.len(), 2);
    assert_eq!(plan.groups[0].tier.as_str(), "native");
    assert_eq!(plan.groups[0].candidates[0].candidate.id(), "n1");
    assert_eq!(plan.groups[1].tier.as_str(), "conversion");
    assert_eq!(plan.groups[1].candidates[0].candidate.id(), "c1");

    // Drive execute_plan with a mock executor that records which attempt it
    // saw and classifies by upstream protocol: native succeeds, conversion
    // would run only if native fails.
    let mut seen = Vec::new();
    let out = crate::core::plan_executor::execute_plan(
        plan,
        &audited,
        rand::rngs::StdRng::seed_from_u64(7),
        |attempt| {
            seen.push(attempt.upstream_protocol.clone());
            // Own a clone so the returned future does not borrow `attempt`.
            let attempt = attempt.clone();
            let p = attempt.upstream_protocol.clone();
            async move {
                if p == "openai" {
                    crate::core::plan_executor::ok_result(
                        200,
                        json!({"id":"x","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}),
                        Some(crate::core::attempt::TokenUsage { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5, cached_tokens: 0 }),
                    )
                } else {
                    // Anthropic conversion attempt: verify the prepared body is
                    // the codec-shaped Messages request, then fail so the flow
                    // can continue (this should not happen because native wins).
                    assert!(attempt.encoded_body.get("system").is_some() || attempt.encoded_body.get("max_tokens").is_some());
                    crate::core::attempt::AttemptResult::Failure(crate::core::attempt::AttemptFailure {
                        failure_class: crate::core::attempt::FailureClass::Retryable,
                        message: "unexpected".into(),
                        status_code: Some(502),
                        retry_after: None,
                    })
                }
            }
        },
    )
    .await;
    assert_eq!(out.status, 200);
    assert_eq!(seen, vec!["openai"], "native group must be attempted first");
    assert_eq!(out.channel_id.as_deref(), Some("n1"));
}

/// Messages routing: native Anthropic G1 before OpenAI Chat G2.
#[tokio::test]
async fn protocol_routing_integration_messages_native_anthropic_first() {
    let pool = fresh_db().await;
    let repo = Arc::new(Repository::new(pool.clone()));
    let ant = channel(
        "a1",
        "anthropic",
        "anthropic",
        "https://api.anthropic.com",
        &["messages"],
        1,
    );
    let oai = channel(
        "o1",
        "openai",
        "openai",
        "https://api.openai.com/v1",
        &["chat_completions"],
        100,
    );
    insert_channels(&pool, &[ant, oai]).await;

    let key = api_key();
    let audited = audited(
        DownstreamProtocol::Messages,
        "messages",
        "m",
        json!({"model":"m","max_tokens":64,"messages":[{"role":"user","content":"hi"}]}),
    );
    let channels = repo.get_enabled_channels().await.unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let plan = authorize_and_plan(
        &key,
        "m",
        EndpointKind::Messages,
        &channels,
        &flags(true),
        &audited.forward_json,
        &mut rng,
    )
    .unwrap();
    assert_eq!(plan.groups.len(), 2);
    assert_eq!(plan.groups[0].candidates[0].candidate.id(), "a1");
    assert_eq!(plan.groups[1].candidates[0].candidate.id(), "o1");
}

/// Non-stream CountTokens: only Anthropic channels with the capability are
/// candidates; an OpenAI channel produces NoEndpointSupported → 501.
#[tokio::test]
async fn protocol_routing_integration_count_tokens_capability_gated() {
    let pool = fresh_db().await;
    let repo = Arc::new(Repository::new(pool.clone()));
    let ant = channel(
        "a1",
        "anthropic",
        "anthropic",
        "https://api.anthropic.com",
        &["messages", "count_tokens"],
        1,
    );
    let oai = channel(
        "o1",
        "openai",
        "openai",
        "https://api.openai.com/v1",
        &["chat_completions"],
        100,
    );
    insert_channels(&pool, &[ant, oai]).await;
    let key = api_key();
    let audited = audited(
        DownstreamProtocol::CountTokens,
        "count_tokens",
        "m",
        json!({"model":"m","messages":[]}),
    );
    let channels = repo.get_enabled_channels().await.unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let plan = authorize_and_plan(
        &key,
        "m",
        EndpointKind::CountTokens,
        &channels,
        &flags(true),
        &audited.forward_json,
        &mut rng,
    )
    .unwrap();
    assert_eq!(plan.groups.len(), 1);
    assert_eq!(plan.groups[0].candidates[0].candidate.id(), "a1");
    assert_eq!(plan.groups[0].upstream_endpoint, "count_tokens");
}

// ── auth_account ─────────────────────────────────────────────────────────

/// T7 integration coverage: the Codex account adapter always receives an SSE
/// request, while the driver presents the requested downstream protocol for all
/// three supported endpoints on both facade paths.
mod auth_account {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use async_trait::async_trait;
    use axum::{
        body::Bytes,
        extract::State,
        http::{header, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use serde_json::{json, Value};
    use tokio::sync::Mutex;

    use super::*;
    use crate::auth_provider::service::AuthService;
    use crate::{
        auth_provider::{
            LoginResult, LoginRuntime, Provider, ProviderError, ProviderKind, ProviderModels,
            ProviderPayload, ProviderRegistry, ProviderRequest, RefreshedPayload,
        },
        core::route_plan::authorize_and_plan_with_accounts,
        db::models::{AuthAccountUpsert, ModelState, ModelStates},
        endpoint_executor::driver::{
            route_plan_response_with_auth_service, route_stream_plan_with_auth_service,
        },
    };

    #[derive(Clone, Default)]
    struct MockState {
        hits: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<Value>>>,
        fail_upstream: bool,
    }

    async fn responses(State(state): State<MockState>, body: Bytes) -> Response {
        state.hits.fetch_add(1, Ordering::SeqCst);
        state
            .seen
            .lock()
            .await
            .push(serde_json::from_slice(&body).unwrap());
        if state.fail_upstream {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "fixture upstream failure",
            )
                .into_response();
        }
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"Shanghai\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"m\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"Shanghai\\\"}\"}],\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
            "data: [DONE]\n\n"
        );
        ([(header::CONTENT_TYPE, "text/event-stream")], sse).into_response()
    }

    #[derive(Clone)]
    struct LocalProvider {
        endpoint: String,
    }

    #[async_trait]
    impl Provider for LocalProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Codex
        }

        async fn login(
            &self,
            _: &crate::auth_provider::ProviderLoginContext,
            _: &dyn LoginRuntime,
        ) -> Result<LoginResult, ProviderError> {
            Err(ProviderError::LoginFailed)
        }

        async fn import(&self, _: &[u8]) -> Result<LoginResult, ProviderError> {
            Err(ProviderError::ImportFailed)
        }

        async fn refresh(
            &self,
            payload: &ProviderPayload,
        ) -> Result<RefreshedPayload, ProviderError> {
            Ok(RefreshedPayload {
                payload: payload.clone(),
                last_refreshed_at: None,
                next_refresh_after: None,
                next_retry_after: None,
            })
        }

        async fn outbound(
            &self,
            request: ProviderRequest<'_>,
        ) -> Result<reqwest::Response, ProviderError> {
            reqwest::Client::new()
                .post(&self.endpoint)
                .headers(request.headers.clone())
                .json(request.body)
                .send()
                .await
                .map_err(|_| ProviderError::Retryable)
        }

        async fn list_models(
            &self,
            _account: &crate::db::models::AuthAccount,
            _payload: &ProviderPayload,
        ) -> Result<ProviderModels, ProviderError> {
            Ok(vec![])
        }
    }

    async fn setup_with_failure(
        fail_upstream: bool,
    ) -> (
        Arc<Repository>,
        Arc<AuthService>,
        MockState,
        crate::db::models::AuthAccount,
    ) {
        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool));
        let state = MockState {
            fail_upstream,
            ..MockState::default()
        };
        let app = Router::new()
            .route("/responses", post(responses))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/responses", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let account = repo
            .upsert_by_provider_account_id(&AuthAccountUpsert {
                provider: "codex".into(),
                label: "Codex fixture".into(),
                account_id: "remote-account".into(),
                attributes: json!({}),
                payload: json!({"access_token":"fixture", "expires_at":"2099-01-01T00:00:00Z"}),
                last_refreshed_at: None,
                next_refresh_after: None,
                next_retry_after: None,
            })
            .await
            .unwrap();
        repo.update_models_if_success(
            &account.id,
            &ModelStates {
                version: 1,
                models: vec![ModelState {
                    id: "m".into(),
                    status: "available".into(),
                    unavailable: false,
                    next_retry_after: None,
                    last_error: None,
                    protocol: None,
                }],
            },
            &now(),
        )
        .await
        .unwrap();
        let account = repo.get_auth_account(&account.id).await.unwrap();
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(LocalProvider { endpoint }));
        let service = Arc::new(AuthService::new(repo.clone(), registry));
        (repo, service, state, account)
    }

    async fn setup() -> (
        Arc<Repository>,
        Arc<AuthService>,
        MockState,
        crate::db::models::AuthAccount,
    ) {
        setup_with_failure(false).await
    }

    fn make_request(
        endpoint: EndpointKind,
        stream: bool,
    ) -> (
        crate::security::gate::AuditedRequest,
        &'static str,
        &'static str,
    ) {
        match endpoint {
            EndpointKind::ChatCompletions => (
                audited(
                    DownstreamProtocol::ChatCompletions,
                    "chat_completions",
                    "m",
                    json!({"model":"m","messages":[{"role":"user","content":"hi"}],"tools":[{"type":"function","function":{"name":"weather","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}}],"stream":stream}),
                ),
                "chat",
                "chat.completion",
            ),
            EndpointKind::Messages => (
                audited(
                    DownstreamProtocol::Messages,
                    "messages",
                    "m",
                    json!({"model":"m","max_tokens":32,"messages":[{"role":"user","content":"hi"}],"tools":[{"name":"weather","input_schema":{"type":"object","properties":{"city":{"type":"string"}}}}],"stream":stream}),
                ),
                "anthropic",
                "message",
            ),
            EndpointKind::Responses => (
                audited(
                    DownstreamProtocol::Responses,
                    "responses",
                    "m",
                    json!({"model":"m","input":"hi","stream":stream}),
                ),
                "responses",
                "output",
            ),
            _ => unreachable!("account routes only support three downstream endpoints"),
        }
    }

    fn plan(
        key: &ApiKey,
        account: &crate::db::models::AuthAccount,
        endpoint: EndpointKind,
        request: &crate::security::gate::AuditedRequest,
    ) -> crate::core::route_plan::RoutePlan {
        authorize_and_plan_with_accounts(
            key,
            "m",
            endpoint,
            &[],
            std::slice::from_ref(account),
            &flags(true),
            &request.forward_json,
            &mut rand::rngs::StdRng::seed_from_u64(7),
        )
        .unwrap()
    }

    fn assert_non_stream_shape(endpoint: EndpointKind, body: &[u8]) {
        let json: Value = serde_json::from_slice(body).unwrap();
        match endpoint {
            EndpointKind::ChatCompletions => {
                assert_eq!(json["object"], "chat.completion");
                assert_eq!(json["choices"][0]["finish_reason"], "tool_calls");
                assert_eq!(json["usage"]["prompt_tokens"], 3);
                assert_eq!(json["usage"]["completion_tokens"], 2);
                assert_eq!(
                    json["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
                    "weather"
                );
                assert_eq!(
                    json["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
                    "{\"city\":\"Shanghai\"}"
                );
            }
            EndpointKind::Messages => {
                assert_eq!(json["type"], "message");
                assert_eq!(json["stop_reason"], "tool_use");
                assert_eq!(json["usage"]["input_tokens"], 3);
                assert_eq!(json["usage"]["output_tokens"], 2);
                assert_eq!(json["content"][0]["type"], "tool_use");
                assert_eq!(json["content"][0]["name"], "weather");
                assert_eq!(json["content"][0]["input"]["city"], "Shanghai");
            }
            EndpointKind::Responses => {
                assert_eq!(json["id"], "resp_1");
                assert_eq!(json["status"], "completed");
                assert_eq!(json["usage"]["input_tokens"], 3);
                assert_eq!(json["usage"]["output_tokens"], 2);
                assert_eq!(json["output"][0]["type"], "function_call");
                assert_eq!(json["output"][0]["name"], "weather");
                assert_eq!(json["output"][0]["arguments"], "{\"city\":\"Shanghai\"}");
            }
            _ => unreachable!("account routes only support three downstream endpoints"),
        }
    }

    fn assert_stream_shape(endpoint: EndpointKind, body: &[u8]) {
        let text = String::from_utf8_lossy(body);
        match endpoint {
            EndpointKind::ChatCompletions => {
                assert!(text.contains("\"finish_reason\":\"tool_calls\""), "{text}");
                assert!(text.contains("\"prompt_tokens\":3"), "{text}");
                assert!(text.contains("\"completion_tokens\":2"), "{text}");
                assert!(text.contains("\"name\":\"weather\""), "{text}");
                assert!(text.contains("Shanghai"), "{text}");
            }
            EndpointKind::Messages => {
                assert!(text.contains("content_block_start"), "{text}");
                assert!(text.contains("\"type\":\"tool_use\""), "{text}");
                assert!(text.contains("\"name\":\"weather\""), "{text}");
                assert!(text.contains("Shanghai"), "{text}");
                assert!(text.contains("\"stop_reason\":\"tool_use\""), "{text}");
            }
            EndpointKind::Responses => {
                assert!(text.contains("response.completed"), "{text}");
                assert!(text.contains("\"type\":\"function_call\""), "{text}");
                assert!(text.contains("\"name\":\"weather\""), "{text}");
                assert!(text.contains("\"input_tokens\":3"), "{text}");
                assert!(text.contains("\"output_tokens\":2"), "{text}");
            }
            _ => unreachable!("account routes only support three downstream endpoints"),
        }
    }

    #[tokio::test]
    async fn three_protocols_stream_and_non_stream_force_responses_sse_and_log_account_source() {
        let (repo, service, state, account) = setup().await;
        let key = api_key();
        for endpoint in [
            EndpointKind::ChatCompletions,
            EndpointKind::Messages,
            EndpointKind::Responses,
        ] {
            let (request, mode, expected) = make_request(endpoint, false);
            let response = route_plan_response_with_auth_service(
                plan(&key, &account, endpoint, &request),
                &request,
                &key,
                &[],
                mode,
                &repo,
                "{}",
                None,
                service.clone(),
            )
            .await;
            assert_eq!(
                response.status(),
                axum::http::StatusCode::OK,
                "{endpoint:?} non-stream"
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_non_stream_shape(endpoint, &body);
            assert!(
                String::from_utf8_lossy(&body).contains(expected),
                "{endpoint:?} non-stream body: {}",
                String::from_utf8_lossy(&body)
            );

            let (request, mode, expected) = make_request(endpoint, true);
            let response = route_stream_plan_with_auth_service(
                plan(&key, &account, endpoint, &request),
                &request,
                &key,
                &[],
                mode,
                &repo,
                "{}",
                None,
                service.clone(),
                Default::default(),
            )
            .await;
            if response.status() != axum::http::StatusCode::OK {
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                panic!(
                    "{endpoint:?} stream status {status}: {}",
                    String::from_utf8_lossy(&body)
                );
            }
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_stream_shape(endpoint, &body);
            let stream_expected = if endpoint == EndpointKind::Responses {
                "response.completed"
            } else {
                expected
            };
            assert!(
                String::from_utf8_lossy(&body).contains(stream_expected),
                "{endpoint:?} stream body: {}",
                String::from_utf8_lossy(&body)
            );
        }
        tokio::task::yield_now().await;
        let seen = state.seen.lock().await;
        assert_eq!(seen.len(), 6);
        assert!(seen.iter().all(|body| body["stream"] == true));
        drop(seen);
        let logs = repo.get_logs(20, 0).await.unwrap();
        assert!(logs.len() >= 5, "all completed facade paths must be logged");
        assert!(logs.iter().all(|log| log.upstream_type == "auth_account"));
    }

    /// CR-1 #5: exhausting a single Auth Account must retain the LAST attempted
    /// candidate metadata in both facade failure logs.  A pre-plan rejection is
    /// the only case permitted to have no upstream candidate fields.
    #[tokio::test]
    async fn exhausted_auth_account_failure_logs_keep_candidate_metadata_for_stream_and_non_stream()
    {
        let (repo, service, state, account) = setup_with_failure(true).await;
        let key = api_key();

        let (request, mode, _) = make_request(EndpointKind::ChatCompletions, false);
        let response = route_plan_response_with_auth_service(
            plan(&key, &account, EndpointKind::ChatCompletions, &request),
            &request,
            &key,
            &[],
            mode,
            &repo,
            "{}",
            None,
            service.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        let (stream_request, stream_mode, _) = make_request(EndpointKind::ChatCompletions, true);
        let response = route_stream_plan_with_auth_service(
            plan(
                &key,
                &account,
                EndpointKind::ChatCompletions,
                &stream_request,
            ),
            &stream_request,
            &key,
            &[],
            stream_mode,
            &repo,
            "{}",
            None,
            service,
            Default::default(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

        assert_eq!(state.hits.load(Ordering::SeqCst), 2);
        let logs = repo.get_logs(10, 0).await.unwrap();
        assert_eq!(logs.len(), 2);
        for log in logs {
            assert_eq!(log.upstream_type, "auth_account");
            assert_eq!(log.channel_id.as_deref(), Some(account.id.as_str()));
            assert_eq!(log.channel_name.as_deref(), Some(account.label.as_str()));
            assert_eq!(log.provider.as_deref(), Some("codex"));
            assert_eq!(log.upstream_protocol.as_deref(), Some("responses"));
            assert_eq!(log.upstream_endpoint.as_deref(), Some("responses"));
            assert_eq!(log.codec_version.as_deref(), Some("chat_to_responses_v1"));
        }
    }
}

// ── codex_responses_anthropic (V5 path ①) ─────────────────────────────────

/// End-to-end 路径①: a codex Responses request routed to an Anthropic Messages
/// channel (conversion group `responses_to_messages_v1`).  Drives the REAL
/// streaming facade (`authorize_and_plan` + `route_stream_plan`) against a local
/// axum mock upstream speaking Messages SSE.  Asserts:
///   * the mock received the codec-encoded Messages body (mapped upstream model,
///     max_tokens=32000, system from instructions, messages from input,
///     thinking from reasoning.effort);
///   * the downstream body is Responses SSE with the required event sequence
///     (`response.created` / `response.output_text.delta` / `response.completed`
///     / `[DONE]`) and the converted usage.
mod codex_responses_anthropic {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use axum::{
        body::Bytes,
        extract::State,
        http::{header, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::*;
    use crate::endpoint_executor::driver::route_stream_plan;

    #[derive(Clone)]
    struct MockState {
        hits: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    /// Mock Anthropic Messages upstream: records the request body, then replies
    /// with a small thinking + text Messages SSE stream when the request is a
    /// stream, or a single non-stream Messages JSON document otherwise.
    async fn messages(State(state): State<MockState>, body: Bytes) -> Response {
        let req: Value = serde_json::from_slice(&body).unwrap();
        state.hits.fetch_add(1, Ordering::SeqCst);
        state.seen.lock().await.push(req.clone());
        if req.get("stream").and_then(Value::as_bool).unwrap_or(false) {
            return messages_sse().into_response();
        }
        (
            [(header::CONTENT_TYPE, "application/json")],
            messages_json().to_string(),
        )
            .into_response()
    }

    fn messages_sse() -> &'static str {
        concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"oc/deepseek-v4-flash-free\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"thinking trace\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":15}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
            "data: [DONE]\n\n"
        )
    }

    /// Non-stream Messages response matching the SSE fixture above.
    fn messages_json() -> Value {
        serde_json::json!({
            "type": "message",
            "id": "msg_02",
            "model": "oc/deepseek-v4-flash-free",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello world"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 12, "output_tokens": 15}
        })
    }

    #[tokio::test]
    async fn responses_downstream_via_anthropic_messages_upstream() {
        let state = MockState {
            hits: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/messages", post(messages))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool.clone()));
        let mut ch = channel("a1", "anthropic", "deepseek", &base, &["messages"], 1);
        ch.model_mapping = json!({ "m": "oc/deepseek-v4-flash-free" }).to_string();
        insert_channels(&pool, &[ch]).await;

        let key = api_key();
        let body = json!({
            "model": "m",
            "input": "hi",
            "instructions": "You are a helpful assistant.",
            "reasoning": {"effort": "high"},
            "stream": true,
        });
        let audited = audited(
            DownstreamProtocol::Responses,
            "responses",
            "m",
            body.clone(),
        );

        let channels = repo.get_enabled_channels().await.unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &channels,
            &flags(true),
            &body,
            &mut rng,
        )
        .expect("V5 conversion plan");

        // The anthropic channel must form a single conversion group routed at the
        // Messages upstream endpoint.
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier.as_str(), "conversion");
        assert_eq!(plan.groups[0].candidates[0].candidate.id(), "a1");
        assert_eq!(plan.groups[0].upstream_endpoint, "messages");

        let resp = route_stream_plan(
            plan,
            &audited,
            &key,
            &[],
            "responses",
            &repo,
            &serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default(),
            None,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "V5 stream must commit a 200");
        let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();

        // ── upstream body: codec-encoded Messages request ─────────────────────
        assert_eq!(
            state.hits.load(Ordering::SeqCst),
            1,
            "exactly one upstream call"
        );
        let seen = state.seen.lock().await;
        assert_eq!(seen.len(), 1);
        let req = &seen[0];
        assert_eq!(
            req["model"], "oc/deepseek-v4-flash-free",
            "mapped upstream model"
        );
        assert_eq!(req["max_tokens"], 32000, "V5 default output cap");
        assert_eq!(req["system"][0]["text"], "You are a helpful assistant.");
        assert_eq!(req["messages"][0]["role"], "user");
        assert_eq!(req["messages"][0]["content"][0]["type"], "text");
        assert_eq!(req["messages"][0]["content"][0]["text"], "hi");
        assert_eq!(req["stream"], true);
        assert_eq!(
            req["thinking"]["type"], "enabled",
            "reasoning.effort -> thinking"
        );
        assert_eq!(req["thinking"]["budget_tokens"], 24576);
        assert_eq!(req["output_config"]["effort"], "high");
        drop(seen);

        // ── downstream body: Responses SSE event sequence ────────────────────
        let frames = sse_frames(&text);
        assert!(!frames.is_empty(), "downstream body must not be empty");
        let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();

        let find = |name: &str| names.iter().position(|&n| n == name);
        let created = find("response.created").expect("response.created");
        let in_progress = find("response.in_progress").expect("response.in_progress");
        let delta = find("response.output_text.delta").expect("response.output_text.delta");
        let completed = find("response.completed").expect("response.completed");
        let done = find("[DONE]").expect("[DONE]");
        assert!(
            created < in_progress && in_progress < delta && delta < completed && completed < done,
            "event order must be created -> in_progress -> output_text.delta -> completed -> [DONE]\n{names:?}"
        );

        // The created event carries the mapped upstream model.
        let created_data: Value =
            serde_json::from_str(&frames[created].1).expect("created data JSON");
        assert_eq!(
            created_data["response"]["model"],
            "oc/deepseek-v4-flash-free"
        );

        // The text deltas carry "Hello" then " world".
        let delta_datas: Vec<String> = frames
            .iter()
            .filter(|(e, _)| e == "response.output_text.delta")
            .map(|(_, d)| d.clone())
            .collect();
        assert_eq!(delta_datas.len(), 2, "two text deltas");
        let first: Value = serde_json::from_str(&delta_datas[0]).unwrap();
        let second: Value = serde_json::from_str(&delta_datas[1]).unwrap();
        assert_eq!(first["delta"], "Hello");
        assert_eq!(second["delta"], " world");

        // The completed event carries the usage observed upstream (12 in / 15 out).
        let completed_data: Value =
            serde_json::from_str(&frames[completed].1).expect("completed data JSON");
        assert_eq!(completed_data["response"]["status"], "completed");
        assert_eq!(completed_data["response"]["usage"]["input_tokens"], 12);
        assert_eq!(completed_data["response"]["usage"]["output_tokens"], 15);
        assert_eq!(
            completed_data["response"]["output"][1]["content"][0]["text"], "Hello world",
            "completed output text must be the accumulated deltas"
        );
    }

    /// Non-stream V5: a codex Responses request with `stream:false` routed to an
    /// Anthropic Messages channel must decode the upstream Messages JSON into a
    /// Responses JSON document (not fail as an "unknown codec version").
    #[tokio::test]
    async fn non_stream_responses_downstream_via_anthropic_messages_upstream() {
        let state = MockState {
            hits: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/messages", post(messages))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool.clone()));
        let mut ch = channel("a1", "anthropic", "deepseek", &base, &["messages"], 1);
        ch.model_mapping = json!({ "m": "oc/deepseek-v4-flash-free" }).to_string();
        insert_channels(&pool, &[ch]).await;

        let key = api_key();
        let body = json!({
            "model": "m",
            "input": "hi",
            "instructions": "You are a helpful assistant.",
            "reasoning": {"effort": "high"},
            "stream": false,
        });
        let audited = audited(
            DownstreamProtocol::Responses,
            "responses",
            "m",
            body.clone(),
        );

        let channels = repo.get_enabled_channels().await.unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &channels,
            &flags(true),
            &body,
            &mut rng,
        )
        .expect("V5 conversion plan (non-stream)");

        let resp = crate::endpoint_executor::driver::route_plan_response(
            plan,
            &audited,
            &key,
            &[],
            "responses",
            &repo,
            &serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default(),
            None,
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "non-stream V5 must decode Messages -> Responses (no 502 unknown-codec)"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let out: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(out["status"], "completed");
        assert!(out["finish_reason"].is_null());
        assert_eq!(out["model"], "oc/deepseek-v4-flash-free");
        assert_eq!(out["output"][0]["type"], "message");
        assert_eq!(out["output"][0]["content"][0]["text"], "Hello world");
        assert_eq!(out["usage"]["input_tokens"], 12);
        assert_eq!(out["usage"]["output_tokens"], 15);
    }
}

// ── responses_via_chat (Cell 5) ────────────────────────────────────────────

/// End-to-end Cell 5: a codex Responses request routed to a chat-only OpenAI
/// channel (conversion group `responses_via_chat_v1`).  Drives the REAL
/// streaming facade against a local axum mock upstream speaking OpenAI Chat
/// Completions SSE.  This is the legacy Responses→Chat debt path that
/// `attempt.rs:275` builds via `responses_to_openai` (not the codec registry),
/// and had NO executor/facade coverage before these tests.
mod responses_via_chat {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use axum::{
        body::Bytes,
        extract::State,
        http::{header, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::*;
    use crate::endpoint_executor::driver::route_stream_plan;

    #[derive(Clone)]
    struct MockState {
        hits: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    /// Mock OpenAI Chat Completions upstream: records the request body, then
    /// replies with a small chat SSE stream when stream=true, or a single chat
    /// JSON document otherwise.
    async fn chat(State(state): State<MockState>, body: Bytes) -> Response {
        let req: Value = serde_json::from_slice(&body).unwrap();
        state.hits.fetch_add(1, Ordering::SeqCst);
        state.seen.lock().await.push(req.clone());
        if req.get("stream").and_then(Value::as_bool).unwrap_or(false) {
            return chat_sse().into_response();
        }
        (
            [(header::CONTENT_TYPE, "application/json")],
            chat_json().to_string(),
        )
            .into_response()
    }

    fn chat_sse() -> &'static str {
        concat!(
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        )
    }

    /// Non-stream chat response matching the SSE fixture above.
    fn chat_json() -> Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "model": "deepseek-v4-flash",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello world"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
        })
    }

    fn chat_only_channel(base: &str) -> Channel {
        // DeepSeek preset shape: protocol=openai, native [chat_completions] only.
        let mut ch = channel("ds", "openai", "deepseek", base, &["chat_completions"], 1);
        ch.model_mapping = json!({ "m": "deepseek-v4-flash" }).to_string();
        ch
    }

    /// Stream Cell 5: a codex Responses request routed to a chat-only channel
    /// must (a) send the chat-shaped body upstream (messages, stream, mapped
    /// model — no `input`), and (b) decode the chat SSE into Responses SSE
    /// events ending in response.completed + [DONE].
    #[tokio::test]
    async fn responses_via_chat_stream_e2e() {
        let state = MockState {
            hits: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/chat/completions", post(chat))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool.clone()));
        insert_channels(&pool, &[chat_only_channel(&base)]).await;

        let key = api_key();
        let body = json!({
            "model": "m",
            "input": "hi",
            "instructions": "You are a helpful assistant.",
            "stream": true,
        });
        let audited = audited(
            DownstreamProtocol::Responses,
            "responses",
            "m",
            body.clone(),
        );

        let channels = repo.get_enabled_channels().await.unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &channels,
            &flags(true),
            &body,
            &mut rng,
        )
        .expect("responses_via_chat plan");

        // The chat-only channel forms a single conversion group at chat_completions.
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier.as_str(), "conversion");
        assert_eq!(plan.groups[0].candidates[0].candidate.id(), "ds");
        assert_eq!(plan.groups[0].upstream_endpoint, "chat_completions");

        let resp = route_stream_plan(
            plan,
            &audited,
            &key,
            &[],
            "responses",
            &repo,
            &serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default(),
            None,
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "Cell 5 stream must commit a 200"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();

        // ── upstream body: chat-shaped, not Responses-shaped ────────────────
        assert_eq!(
            state.hits.load(Ordering::SeqCst),
            1,
            "exactly one upstream call"
        );
        let seen = state.seen.lock().await;
        assert_eq!(seen.len(), 1);
        let req = &seen[0];
        assert_eq!(req["model"], "deepseek-v4-flash", "mapped upstream model");
        assert_eq!(
            req["messages"][0]["role"], "system",
            "instructions -> system message"
        );
        assert_eq!(
            req["messages"][0]["content"], "You are a helpful assistant.",
            "instructions content"
        );
        assert_eq!(req["messages"][1]["content"], "hi", "input -> user message");
        assert_eq!(req["stream"], true);
        assert!(req.get("input").is_none(), "no Responses `input` upstream");
        drop(seen);

        // ── downstream: Responses SSE event sequence ────────────────────────
        let frames = super::sse_frames(&text);
        assert!(!frames.is_empty(), "downstream body must not be empty");
        let names: Vec<&str> = frames.iter().map(|(e, _)| e.as_str()).collect();

        let find = |name: &str| names.iter().position(|&n| n == name);
        let added = find("response.output_item.added").expect("output_item.added");
        let delta = find("response.output_text.delta").expect("output_text.delta");
        let completed = find("response.completed").expect("response.completed");
        let done = find("[DONE]").expect("[DONE]");
        assert!(
            added < delta && delta < completed && completed < done,
            "event order must be output_item.added -> output_text.delta -> completed -> [DONE]\n{names:?}"
        );

        // The text deltas carry "Hello" then " world".
        let delta_datas: Vec<String> = frames
            .iter()
            .filter(|(e, _)| e == "response.output_text.delta")
            .map(|(_, d)| d.clone())
            .collect();
        assert_eq!(delta_datas.len(), 2, "two text deltas");
        let first: Value = serde_json::from_str(&delta_datas[0]).unwrap();
        let second: Value = serde_json::from_str(&delta_datas[1]).unwrap();
        assert_eq!(first["delta"], "Hello");
        assert_eq!(second["delta"], " world");

        // The completed event carries the usage observed upstream (5 in / 2 out).
        let completed_data: Value =
            serde_json::from_str(&frames[completed].1).expect("completed data JSON");
        assert_eq!(completed_data["response"]["status"], "completed");
        assert_eq!(completed_data["response"]["usage"]["input_tokens"], 5);
        assert_eq!(completed_data["response"]["usage"]["output_tokens"], 2);
    }

    /// Non-stream Cell 5: `stream:false` Responses request to a chat-only channel
    /// decodes the upstream chat JSON into a Responses JSON document
    /// (`openai_to_responses`), never a 502 unknown-codec.
    #[tokio::test]
    async fn responses_via_chat_non_stream_e2e() {
        let state = MockState {
            hits: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/chat/completions", post(chat))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool.clone()));
        insert_channels(&pool, &[chat_only_channel(&base)]).await;

        let key = api_key();
        let body = json!({
            "model": "m",
            "input": "hi",
            "instructions": "You are a helpful assistant.",
            "stream": false,
        });
        let audited = audited(
            DownstreamProtocol::Responses,
            "responses",
            "m",
            body.clone(),
        );

        let channels = repo.get_enabled_channels().await.unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::Responses,
            &channels,
            &flags(true),
            &body,
            &mut rng,
        )
        .expect("responses_via_chat plan (non-stream)");

        let resp = crate::endpoint_executor::driver::route_plan_response(
            plan,
            &audited,
            &key,
            &[],
            "responses",
            &repo,
            &serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default(),
            None,
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "non-stream Cell 5 must decode chat -> Responses (no 502 unknown-codec)"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let out: Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(out["status"], "completed");
        assert_eq!(out["finish_reason"], "stop");
        assert_eq!(out["model"], "deepseek-v4-flash");
        assert_eq!(out["output"][0]["type"], "message");
        assert_eq!(out["output"][0]["content"][0]["text"], "Hello world");
        assert_eq!(out["usage"]["input_tokens"], 5);
        assert_eq!(out["usage"]["output_tokens"], 2);
    }
}

// ── stream_failover ───────────────────────────────────────────────────────

/// Pre-commit: an invalid first frame allows an upstream swap (retry the next
/// candidate); post-commit the barrier forbids swapping.
#[test]
fn stream_failover_commit_barrier() {
    // Candidate 1: connect + headers + invalid first frame → swap allowed.
    let mut s = StreamSupervisor::new();
    s.begin_connect().unwrap();
    s.on_upstream_headers().unwrap();
    assert!(
        s.swap_upstream().is_ok(),
        "invalid first frame → swap before commit"
    );
    assert_eq!(s.upstream_swaps(), 1);

    // Candidate 2: re-walk to commit.
    s.on_upstream_headers().unwrap();
    s.on_first_frame_validated().unwrap();
    s.commit_downstream().unwrap();
    s.begin_streaming().unwrap();
    // Post-commit: no retry possible.
    let err = s.swap_upstream().unwrap_err();
    assert_eq!(
        err,
        crate::core::stream_supervisor::StreamTransitionError::RetryAfterCommit
    );
    assert!(s.committed());
}

/// First-frame validation: a well-formed SSE record validates; malformed JSON
/// fails closed (pre-commit failover).
#[test]
fn stream_failover_first_frame_validation() {
    assert!(crate::endpoint_executor::sse::validate_native_first_record(
        b"data: {\"choices\":[]}\n\n"
    )
    .is_ok());
    assert!(
        crate::endpoint_executor::sse::validate_native_first_record(b"data: not-json\n\n").is_err()
    );
    assert!(crate::endpoint_executor::sse::validate_native_first_record(
        b"event: message_start\ndata: {}\n\n"
    )
    .is_ok());
}

/// A client cancel records exactly once and aborts; a second cancel is rejected.
#[test]
fn stream_failover_client_cancel_exactly_once() {
    let mut s = StreamSupervisor::new();
    s.begin_connect().unwrap();
    s.client_cancel().unwrap();
    assert!(s.client_cancelled());
    assert!(s.client_cancel().is_err(), "exactly-once finalizer");
}

/// End-to-end pump: a native stream commits on first frame and passes raw bytes
/// through, terminating exactly once on [DONE].
#[test]
fn stream_failover_pump_native_passthrough() {
    let mut sup = StreamSupervisor::new();
    sup.begin_connect().unwrap();
    sup.on_upstream_headers().unwrap();
    sup.on_first_frame_validated().unwrap();
    let mut pump = StreamPumpCore::new(
        sup,
        CodecRegistry::prepare_pair(
            Protocol::Chat,
            Protocol::Chat,
            "m",
            &json!({"model":"m", "messages":[]}),
        )
        .unwrap()
        .codec
        .new_stream_decoder(),
        b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n".to_vec(),
        Vec::new(),
    )
    .unwrap();
    let first = pump.start().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("data: {"));
    let done = pump.push(b"data: [DONE]\n\n").unwrap();
    assert_eq!(String::from_utf8_lossy(&done), "data: [DONE]\n\n");
    let fin = pump.finish().unwrap();
    assert!(fin.is_empty());
    assert!(pump.terminated());
}

/// I-3: a streaming pre-commit terminal outcome (all candidates exhausted /
/// codec rejection / authorize rejection) must write a RequestLog row so
/// failed streaming requests stay observable.
#[tokio::test]
async fn stream_precommit_failure_writes_request_log() {
    let pool = fresh_db().await;
    let repo = Arc::new(Repository::new(pool.clone()));
    // Insert the API key so quota/accounting can reference it.
    sqlx::query(
        "INSERT INTO api_keys (id, name, key, status, allowed_models, allowed_channels, quota_limit, quota_used, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )
    .bind("key-1").bind("t").bind("sk-test").bind(1i64)
    .bind("[]").bind("[]").bind(0i64).bind(0i64)
    .bind(now()).bind(now())
    .execute(&pool)
    .await
    .expect("insert api key");

    let key = api_key();
    let audited = audited(
        DownstreamProtocol::ChatCompletions,
        "chat_completions",
        "m",
        json!({"model": "m", "messages": []}),
    );
    crate::endpoint_executor::driver::write_stream_precommit_failure_log(
        &repo,
        &key,
        &audited,
        "chat",
        true,
        503,
        "no channel available",
        "{\"model\":\"m\"}",
        None,
    )
    .await;

    let row: (i64, Option<String>, i64) = sqlx::query_as(
        "SELECT status_code, error_message, is_stream FROM request_logs WHERE api_key_id = 'key-1'",
    )
    .fetch_one(&pool)
    .await
    .expect("request log row written");
    assert_eq!(row.0, 503);
    assert_eq!(row.1.as_deref(), Some("no channel available"));
    assert_eq!(
        row.2, 1,
        "streaming pre-commit failure must be flagged is_stream=1"
    );
}

// ── chat_downstream_via_responses_channel ──────────────────────────────────

/// End-to-end: an opencode Chat request routed to an OpenAI-compatible channel
/// that exposes only a native `/responses` endpoint (conversion group
/// `chat_to_responses_v1`).  Drives the REAL streaming facade
/// (`authorize_and_plan` + `route_stream_plan`) against a local axum mock
/// upstream speaking Responses SSE.  Asserts:
///   * the planner forms a single Conversion group at the `responses` endpoint;
///   * the mock received the codec-encoded Responses body (mapped upstream
///     model, `input` array, `stream:true`);
///   * the downstream body is Chat-completion SSE with the text deltas, the
///     usage frame, `finish_reason:"stop"`, and a terminal `[DONE]`.
mod chat_downstream_via_responses_channel {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use axum::{
        body::Bytes,
        extract::State,
        http::{header, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    use super::*;
    use crate::endpoint_executor::driver::route_stream_plan;

    #[derive(Clone)]
    struct MockState {
        hits: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    /// Mock OpenAI Responses upstream: records the request body, then replies
    /// with a text-only Responses SSE stream.
    async fn responses(State(state): State<MockState>, body: Bytes) -> Response {
        let req: Value = serde_json::from_slice(&body).unwrap();
        state.hits.fetch_add(1, Ordering::SeqCst);
        state.seen.lock().await.push(req.clone());
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            responses_sse().to_string(),
        )
            .into_response()
    }

    fn responses_sse() -> &'static str {
        concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"up/m\"}}\n\n",
            "event: response.in_progress\n",
            "data: {\"type\":\"response.in_progress\",\"response\":{\"id\":\"resp_1\",\"model\":\"up/m\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"Hello\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\" world\"}\n\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"text\":\"Hello world\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"up/m\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"input_tokens\":12,\"output_tokens\":15}}}\n\n",
            "data: [DONE]\n\n"
        )
    }

    #[tokio::test]
    async fn chat_downstream_via_responses_channel_upstream() {
        let state = MockState {
            hits: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/responses", post(responses))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let pool = fresh_db().await;
        let repo = Arc::new(Repository::new(pool.clone()));
        let mut ch = channel("r1", "openai", "deepseek", &base, &["responses"], 1);
        ch.model_mapping = json!({ "m": "up/m" }).to_string();
        insert_channels(&pool, &[ch]).await;

        let key = api_key();
        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        });
        let audited = audited(
            DownstreamProtocol::ChatCompletions,
            "chat_completions",
            "m",
            body.clone(),
        );

        let channels = repo.get_enabled_channels().await.unwrap();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let plan = authorize_and_plan(
            &key,
            "m",
            EndpointKind::ChatCompletions,
            &channels,
            &flags(true),
            &body,
            &mut rng,
        )
        .expect("chat→responses conversion plan");

        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].tier.as_str(), "conversion");
        assert_eq!(plan.groups[0].candidates[0].candidate.id(), "r1");
        assert_eq!(plan.groups[0].upstream_endpoint, "responses");

        let resp = route_stream_plan(
            plan,
            &audited,
            &key,
            &[],
            "chat",
            &repo,
            &serde_json::to_string(&audited.sanitized_log_json).unwrap_or_default(),
            None,
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "chat→responses stream must commit a 200"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();

        // ── upstream body: codec-encoded Responses request ──────────────────
        assert_eq!(
            state.hits.load(Ordering::SeqCst),
            1,
            "exactly one upstream call"
        );
        let seen = state.seen.lock().await;
        assert_eq!(seen.len(), 1);
        let req = &seen[0];
        assert_eq!(req["model"], "up/m", "mapped upstream model");
        assert_eq!(req["stream"], true);
        assert_eq!(req["input"][0]["type"], "message");
        assert_eq!(req["input"][0]["role"], "user");
        assert_eq!(req["input"][0]["content"][0]["text"], "hi");
        drop(seen);

        // ── downstream body: Chat-completion SSE ────────────────────────────
        assert!(
            text.contains(r#""content":"Hello""#),
            "first text delta missing:\n{text}"
        );
        assert!(text.contains(r#""content":" world""#), "second text delta");
        assert!(
            text.contains(r#""finish_reason":"stop""#),
            "terminal finish_reason"
        );
        assert!(
            text.contains(r#""prompt_tokens":12"#),
            "usage prompt tokens"
        );
        assert!(
            text.contains(r#""completion_tokens":15"#),
            "usage completion tokens"
        );
        assert!(text.contains("data: [DONE]"), "terminal [DONE]");
    }
}

// ─────────────────────────────────────────────────────────────────────────
// C6: Kimi auth executor framing (Json profile: Chat / Messages beta)
// ─────────────────────────────────────────────────────────────────────────

/// Build a PreparedAttempt with the given framing + a prepared codec.
/// `protocol` is the codec protocol (Chat or Messages) used to encode the body;
/// `upstream_protocol`/`upstream_endpoint` are the frozen wire profile values.
fn kimi_attempt(
    channel_id: &str,
    downstream: EndpointKind,
    body: Value,
    upstream_protocol: &str,
    upstream_endpoint: &str,
    framing: crate::core::route_plan::AuthNonStreamFraming,
    model: &str,
) -> PreparedAttempt {
    use crate::core::protocol_boundary::{
        downstream_protocol, upstream_protocol as upstream_proto,
    };
    let downstream_p = downstream_protocol(downstream).expect("downstream codec");
    let upstream_p = upstream_proto(
        match upstream_protocol {
            "openai" => crate::core::route_plan::UpstreamProtocol::OpenAI,
            "anthropic" => crate::core::route_plan::UpstreamProtocol::Anthropic,
            "responses" => crate::core::route_plan::UpstreamProtocol::Responses,
            _ => crate::core::route_plan::UpstreamProtocol::OpenAI,
        },
        upstream_endpoint,
    )
    .expect("upstream codec");
    let prepared = CodecRegistry::prepare_pair(downstream_p, upstream_p, model, &body)
        .expect("kimi attempt must prepare");
    PreparedAttempt {
        channel_id: channel_id.into(),
        channel_name: "Kimi Code".into(),
        upstream_type: "auth_account".into(),
        route_group: format!("{}_g1_native", downstream.as_str()),
        upstream_protocol: upstream_protocol.to_string(),
        upstream_endpoint: upstream_endpoint.to_string(),
        upstream_model: model.to_string(),
        native_base_url: "http://kimi.invalid/coding".into(),
        auth_provider: Some("kimi".into()),
        auth_non_stream_framing: Some(framing),
        codec_version: Some(prepared.codec.label().to_string()),
        prepared_codec: Some(prepared.codec),
        encoded_body: prepared.encoded_request,
        conversion_report: Some(json!({})),
        is_retry: false,
        attempt_no: 1,
    }
}

/// A Kimi mock that serves Chat JSON and Messages JSON non-stream/stream.
#[derive(Clone)]
struct KimiMock {
    chat_hits: Arc<AtomicUsize>,
    messages_hits: Arc<AtomicUsize>,
    chat_bodies: Arc<Mutex<Vec<Value>>>,
    messages_bodies: Arc<Mutex<Vec<Value>>>,
    chat_headers: Arc<Mutex<Vec<axum::http::HeaderMap>>>,
    messages_headers: Arc<Mutex<Vec<axum::http::HeaderMap>>>,
    fail_401: Arc<AtomicBool>,
    /// Fail exactly one /coding/v1/chat/completions request with 401 then go
    /// back to success, so a test can exercise a real single refresh-replay.
    fail_chat_once: Arc<AtomicBool>,
}

impl Default for KimiMock {
    fn default() -> Self {
        Self {
            chat_hits: Arc::new(AtomicUsize::new(0)),
            messages_hits: Arc::new(AtomicUsize::new(0)),
            chat_bodies: Arc::new(Mutex::new(Vec::new())),
            messages_bodies: Arc::new(Mutex::new(Vec::new())),
            chat_headers: Arc::new(Mutex::new(Vec::new())),
            messages_headers: Arc::new(Mutex::new(Vec::new())),
            fail_401: Arc::new(AtomicBool::new(false)),
            fail_chat_once: Arc::new(AtomicBool::new(false)),
        }
    }
}

async fn kimi_mock() -> (
    Arc<AuthService>,
    KimiMock,
    Arc<Repository>,
    crate::db::models::AuthAccount,
) {
    let pool = fresh_db().await;
    let repo = Arc::new(Repository::new(pool));
    let state = KimiMock::default();
    let app_state = state.clone();
    let app = Router::new()
        .route(
            "/oauth/token",
            axum::routing::post(move |_: axum::extract::State<KimiMock>, _b: axum::body::Bytes| async {
                (axum::http::StatusCode::OK, Json(json!({
                    "access_token": "tok",
                    "refresh_token": "rot",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "scope": ""
                })))
            }),
        )
        .route(
            "/coding/v1/chat/completions",
            axum::routing::post(move |s: axum::extract::State<KimiMock>, h: axum::http::HeaderMap, b: axum::body::Bytes| {
                let s = s.clone();
                async move {
                    s.chat_hits.fetch_add(1, Ordering::SeqCst);
                    s.chat_bodies.lock().unwrap().push(serde_json::from_slice(&b).unwrap_or(Value::Null));
                    s.chat_headers.lock().unwrap().push(h.clone());
                    if s.fail_chat_once.load(Ordering::SeqCst) {
                        // Consume the one-shot 401 flag; the _next_ request
                        // replays into the success branch.
                        s.fail_chat_once.store(false, Ordering::SeqCst);
                        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
                    }
                    if s.fail_401.load(Ordering::SeqCst) {
                        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response();
                    }
                    let body: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
                    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
                        let stream = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\ndata: [DONE]\n\n";
                        return (axum::http::StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/event-stream")], stream).into_response();
                    }
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({
                            "id": "1", "object": "chat.completion", "model": body["model"],
                            "choices": [{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        })),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/coding/v1/messages",
            axum::routing::post(move |s: axum::extract::State<KimiMock>, h: axum::http::HeaderMap, b: axum::body::Bytes| {
                let s = s.clone();
                async move {
                    s.messages_hits.fetch_add(1, Ordering::SeqCst);
                    s.messages_bodies.lock().unwrap().push(serde_json::from_slice(&b).unwrap_or(Value::Null));
                    s.messages_headers.lock().unwrap().push(h.clone());
                    let body: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
                    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
                        let stream = "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\"}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
                        return (axum::http::StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/event-stream")], stream).into_response();
                    }
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({
                            "id": "m1", "type": "message", "role": "assistant",
                            "content": [{"type":"text","text":"hi"}],
                            "model": body["model"],
                            "usage": {"input_tokens": 1, "output_tokens": 1}
                        })),
                    )
                        .into_response()
                }
            }),
        )
        .with_state(app_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let account = repo
        .upsert_by_provider_account_id(&AuthAccountUpsert {
            provider: "kimi".into(),
            label: "Kimi fixture".into(),
            account_id: "kimi-device-id".into(),
            attributes: json!({}),
            payload: json!({"access_token":"tok","device_id":"kimi-device-id","expires_at":"2099-01-01T00:00:00Z"}),
            last_refreshed_at: None,
            next_refresh_after: None,
            next_retry_after: None,
        })
        .await
        .unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(
        crate::auth_provider::kimi_backend::KimiProvider::with_endpoints(
            format!("http://{addr}/coding"),
            format!("http://{addr}/oauth/device"),
            format!("http://{addr}/oauth/token"),
        ),
    ));
    let service = Arc::new(AuthService::new(repo.clone(), registry));
    (service, state, repo, account)
}

#[tokio::test]
async fn kimi_chat_non_stream_uses_json_framing_and_decodes_chat() {
    let (service, state, _repo, account) = kimi_mock().await;
    let body = json!({
        "model": "kimi-k2.5",
        "messages": [{"role":"user","content":"hi"}],
        "stream": true,
        "stream_options": {"include_usage": false},
    });
    let attempt = kimi_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        "openai",
        "chat_completions",
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "kimi-k2.5",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::ChatCompletions,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Success(success) = result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(success.status, 200);
    assert!(success.body.get("choices").is_some());
    // Non-stream body: stream=false, stream_options removed.
    let sent = state.chat_bodies.lock().unwrap()[0].clone();
    assert_eq!(sent["stream"], false);
    assert!(sent.get("stream_options").is_none());
}

#[tokio::test]
async fn kimi_chat_stream_injects_include_usage_for_all_entries() {
    // Chat and Messages downstream convert to Chat; Responses downstream
    // would need a Responses-shaped body (the codec rejects `messages`),
    // which is out of scope for this framing assertion.
    for (downstream, expected_route) in [
        (EndpointKind::ChatCompletions, "chat.completion"),
        (EndpointKind::Messages, "chat.completion"),
    ] {
        let (service, state, _repo, account) = kimi_mock().await;
        let body = json!({
            "model": "kimi-k2.5",
            "messages": [{"role":"user","content":"hi"}],
        });
        let attempt = kimi_attempt(
            &account.id,
            downstream,
            body,
            "openai",
            "chat_completions",
            crate::core::route_plan::AuthNonStreamFraming::Json,
            "kimi-k2.5",
        );
        let result = crate::endpoint_executor::dispatch_auth_account_stream_executor(
            &attempt,
            &service,
            &[],
        )
        .await;
        let StreamAttemptResult::Connected(_) = result else {
            panic!("{downstream:?} expected connected stream");
        };
        let sent = state.chat_bodies.lock().unwrap()[0].clone();
        assert_eq!(sent["stream"], true, "{downstream:?}");
        assert_eq!(
            sent["stream_options"]["include_usage"], true,
            "{downstream:?} must force include_usage"
        );
        let _ = expected_route;
    }
}

#[tokio::test]
async fn kimi_anthropic_stream_has_no_chat_stream_options_and_fixed_betas() {
    let (service, state, _repo, account) = kimi_mock().await;
    let body = json!({
        "model": "kimi-anthropic",
        "messages": [{"role":"user","content":"hi"}],
    });
    let attempt = kimi_attempt(
        &account.id,
        EndpointKind::Messages,
        body,
        "anthropic",
        "messages_beta",
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "kimi-anthropic",
    );
    let result =
        crate::endpoint_executor::dispatch_auth_account_stream_executor(&attempt, &service, &[])
            .await;
    let StreamAttemptResult::Connected(_) = result else {
        panic!("expected connected");
    };
    let sent = state.messages_bodies.lock().unwrap()[0].clone();
    assert_eq!(sent["stream"], true);
    // Messages beta: no Chat-only stream_options.
    assert!(sent.get("stream_options").is_none());
    // Fixed betas token present exactly once.
    let betas = sent["betas"].as_array().unwrap();
    let count = betas
        .iter()
        .filter(|b| b.as_str() == Some("interleaved-thinking-2025-05-14"))
        .count();
    assert_eq!(count, 1, "fixed beta token must be present exactly once");
}

#[tokio::test]
async fn kimi_anthropic_non_stream_keeps_fixed_betas_and_decodes_messages() {
    let (service, state, _repo, account) = kimi_mock().await;
    let body = json!({
        "model": "kimi-anthropic",
        "messages": [{"role":"user","content":"hi"}],
    });
    let attempt = kimi_attempt(
        &account.id,
        EndpointKind::Messages,
        body,
        "anthropic",
        "messages_beta",
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "kimi-anthropic",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::Messages,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Success(success) = result else {
        panic!("expected success, got {result:?}");
    };
    assert!(success.body.get("content").is_some());
    let sent = state.messages_bodies.lock().unwrap()[0].clone();
    assert_eq!(sent["stream"], false);
    let betas = sent["betas"].as_array().unwrap();
    assert_eq!(
        betas
            .iter()
            .filter(|b| b.as_str() == Some("interleaved-thinking-2025-05-14"))
            .count(),
        1
    );
}

#[tokio::test]
async fn kimi_chat_401_triggers_single_refresh_replay() {
    let (service, state, repo, account) = kimi_mock().await;
    state.fail_401.store(true, Ordering::SeqCst);
    // Give the account a refresh token so the 401 path refreshes via the mock
    // /oauth/token and replays once.  `fail_401` stays on, so the replay also
    // returns 401 → ChannelAuthTerminal.
    repo.update_tokens(
        &account.id,
        &json!({"access_token":"tok","refresh_token":"rot","device_id":"d","expires_at":"2099-01-01T00:00:00Z"}),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let body = json!({"model":"kimi-k2.5","messages":[{"role":"user","content":"hi"}]});
    let attempt = kimi_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        "openai",
        "chat_completions",
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "kimi-k2.5",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::ChatCompletions,
        &attempt,
        &service,
        &[],
    )
    .await;
    // 401 on replay → ChannelAuthTerminal (502 downstream).  Two upstream hits:
    // the original and the single refresh replay.
    let AttemptResult::Failure(failure) = result else {
        panic!("expected auth-terminal failure, got {result:?}");
    };
    assert_eq!(failure.failure_class, FailureClass::ChannelAuthTerminal);
    assert_eq!(state.chat_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn kimi_chat_401_refreshes_and_replays_once_successfully() {
    let (service, state, repo, account) = kimi_mock().await;
    // Full credential so the lazy/force refresh can rotate via the mock
    // /oauth/token.  The one-shot 401 flag makes only the first chat request
    // fail; the replay goes through and returns 200.
    repo.update_tokens(
        &account.id,
        &json!({"access_token":"tok","refresh_token":"rot","device_id":"d","expires_at":"2099-01-01T00:00:00Z"}),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    state.fail_chat_once.store(true, Ordering::SeqCst);
    let body = json!({"model":"kimi-k2.5","messages":[{"role":"user","content":"hi"}]});
    let attempt = kimi_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        "openai",
        "chat_completions",
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "kimi-k2.5",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::ChatCompletions,
        &attempt,
        &service,
        &[],
    )
    .await;
    // One 401, then a successful replay → Success, exactly two upstream hits.
    let AttemptResult::Success(success) = result else {
        panic!("expected success after one refresh replay, got {result:?}");
    };
    assert_eq!(success.status, 200);
    assert_eq!(state.chat_hits.load(Ordering::SeqCst), 2);
}

// ─────────────────────────────────────────────────────────────────────────
// Gemini auth executor (Json profile: generateContent / streamGenerateContent)
// ─────────────────────────────────────────────────────────────────────────

fn gemini_attempt(
    channel_id: &str,
    downstream: EndpointKind,
    body: Value,
    framing: crate::core::route_plan::AuthNonStreamFraming,
    model: &str,
) -> PreparedAttempt {
    use crate::core::protocol_boundary::{
        downstream_protocol, upstream_protocol as upstream_proto,
    };
    let downstream_p = downstream_protocol(downstream).expect("downstream codec");
    let upstream_p = upstream_proto(
        crate::core::route_plan::UpstreamProtocol::Gemini,
        "generate_content",
    )
    .expect("gemini codec");
    let prepared = CodecRegistry::prepare_pair(downstream_p, upstream_p, model, &body)
        .expect("gemini attempt must prepare");
    PreparedAttempt {
        channel_id: channel_id.into(),
        channel_name: "Gemini".into(),
        upstream_type: "auth_account".into(),
        route_group: format!("{}_g2_conversion", downstream.as_str()),
        upstream_protocol: "gemini".into(),
        upstream_endpoint: "generate_content".into(),
        upstream_model: model.to_string(),
        native_base_url: "http://gemini.invalid".into(),
        auth_provider: Some("gemini".into()),
        auth_non_stream_framing: Some(framing),
        codec_version: Some(prepared.codec.label().to_string()),
        prepared_codec: Some(prepared.codec),
        encoded_body: prepared.encoded_request,
        conversion_report: Some(json!({})),
        is_retry: false,
        attempt_no: 1,
    }
}

#[derive(Clone)]
struct GeminiMock {
    generate_hits: Arc<AtomicUsize>,
    stream_hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<axum::http::HeaderMap>>>,
    fail_401: Arc<AtomicBool>,
    fail_once: Arc<AtomicBool>,
    fail_403: Arc<AtomicBool>,
}

impl Default for GeminiMock {
    fn default() -> Self {
        Self {
            generate_hits: Arc::new(AtomicUsize::new(0)),
            stream_hits: Arc::new(AtomicUsize::new(0)),
            bodies: Arc::new(Mutex::new(Vec::new())),
            headers: Arc::new(Mutex::new(Vec::new())),
            fail_401: Arc::new(AtomicBool::new(false)),
            fail_once: Arc::new(AtomicBool::new(false)),
            fail_403: Arc::new(AtomicBool::new(false)),
        }
    }
}

async fn gemini_mock() -> (
    Arc<AuthService>,
    GeminiMock,
    Arc<Repository>,
    crate::db::models::AuthAccount,
) {
    let pool = fresh_db().await;
    let repo = Arc::new(Repository::new(pool));
    let state = GeminiMock::default();
    let app_state = state.clone();
    let app = Router::new()
        .route(
            "/token",
            axum::routing::post(|| async {
                Json(json!({
                    "access_token": "tok",
                    "refresh_token": "rot",
                    "expires_in": 3600,
                    "token_type": "Bearer"
                }))
            }),
        )
        .route(
            "/v1internal:generateContent",
            axum::routing::post(
                |s: axum::extract::State<GeminiMock>,
                 h: axum::http::HeaderMap,
                 b: axum::body::Bytes| {
                    let s = s.clone();
                    async move {
                        s.generate_hits.fetch_add(1, Ordering::SeqCst);
                        s.bodies
                            .lock()
                            .unwrap()
                            .push(serde_json::from_slice(&b).unwrap_or(Value::Null));
                        s.headers.lock().unwrap().push(h.clone());
                        if s.fail_once.load(Ordering::SeqCst) {
                            s.fail_once.store(false, Ordering::SeqCst);
                            return (
                                axum::http::StatusCode::UNAUTHORIZED,
                                Json(json!({"error": {"message": "unauthorized"}})),
                            )
                                .into_response();
                        }
                        if s.fail_401.load(Ordering::SeqCst) {
                            return (
                                axum::http::StatusCode::UNAUTHORIZED,
                                Json(json!({"error": {"message": "unauthorized"}})),
                            )
                                .into_response();
                        }
                        if s.fail_403.load(Ordering::SeqCst) {
                            return (
                                axum::http::StatusCode::FORBIDDEN,
                                Json(json!({"error": {"message": "permission denied"}})),
                            )
                                .into_response();
                        }
                        Json(json!({
                            "response": {
                                "candidates": [{
                                    "content": { "role": "model", "parts": [{ "text": "hi" }] },
                                    "finishReason": "STOP"
                                }],
                                "usageMetadata": {
                                    "promptTokenCount": 1,
                                    "candidatesTokenCount": 1
                                }
                            }
                        }))
                        .into_response()
                    }
                },
            ),
        )
        .route(
            "/v1internal:streamGenerateContent",
            axum::routing::post(
                |s: axum::extract::State<GeminiMock>,
                 h: axum::http::HeaderMap,
                 b: axum::body::Bytes| {
                    let s = s.clone();
                    async move {
                        s.stream_hits.fetch_add(1, Ordering::SeqCst);
                        s.bodies
                            .lock()
                            .unwrap()
                            .push(serde_json::from_slice(&b).unwrap_or(Value::Null));
                        s.headers.lock().unwrap().push(h.clone());
                        let stream = concat!(
                            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}}\n\n",
                            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}}\n\n"
                        );
                        (
                            axum::http::StatusCode::OK,
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            stream,
                        )
                            .into_response()
                    }
                },
            ),
        )
        .with_state(app_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let account = repo
        .upsert_by_provider_account_id(&AuthAccountUpsert {
            provider: "gemini".into(),
            label: "Gemini fixture".into(),
            account_id: "user@gmail.com".into(),
            attributes: json!({"email":"user@gmail.com","project_id":"proj-1"}),
            payload: json!({
                "version": 2,
                "oauth_client": "antigravity",
                "access_token":"tok",
                "refresh_token":"rot",
                "expires_at":"2099-01-01T00:00:00Z"
            }),
            last_refreshed_at: None,
            next_refresh_after: None,
            next_retry_after: None,
        })
        .await
        .unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(
        crate::auth_provider::gemini_backend::GeminiProvider::with_endpoints(
            format!("http://{addr}"),
            format!("http://{addr}/oauth2/v2/userinfo"),
            crate::auth_provider::gemini_login::GeminiLogin::with_endpoints(
                "http://127.0.0.1/authorize",
                format!("http://{addr}/token"),
            ),
        ),
    ));
    let service = Arc::new(AuthService::new(repo.clone(), registry));
    (service, state, repo, account)
}

#[tokio::test]
async fn gemini_chat_non_stream_does_not_inject_stream_and_decodes() {
    let (service, state, _repo, account) = gemini_mock().await;
    let body = json!({
        "model": "gemini-2.5-flash",
        "messages": [{"role":"user","content":"hi"}],
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    let attempt = gemini_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "gemini-2.5-flash",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::ChatCompletions,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Success(success) = result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(success.status, 200);
    assert_eq!(success.body["choices"][0]["message"]["content"], "hi");
    let sent = state.bodies.lock().unwrap()[0].clone();
    assert!(sent["request"].get("stream").is_none());
    assert!(sent["request"].get("stream_options").is_none());
    assert_eq!(sent["project"], "proj-1");
    assert_eq!(sent["model"], "gemini-2.5-flash");
}

#[tokio::test]
async fn gemini_messages_non_stream_converts_through_chat() {
    let (service, _state, _repo, account) = gemini_mock().await;
    let body = json!({
        "model": "gemini-2.5-flash",
        "max_tokens": 32,
        "messages": [{"role":"user","content":"hi"}]
    });
    let attempt = gemini_attempt(
        &account.id,
        EndpointKind::Messages,
        body,
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "gemini-2.5-flash",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::Messages,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Success(success) = result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(success.body["type"], "message");
    assert_eq!(success.body["role"], "assistant");
}

#[tokio::test]
async fn gemini_chat_stream_uses_stream_generate_content_without_inner_stream() {
    let (service, state, _repo, account) = gemini_mock().await;
    let body = json!({
        "model": "gemini-2.5-flash",
        "messages": [{"role":"user","content":"hi"}]
    });
    let attempt = gemini_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "gemini-2.5-flash",
    );
    let result =
        crate::endpoint_executor::dispatch_auth_account_stream_executor(&attempt, &service, &[])
            .await;
    let StreamAttemptResult::Connected(_) = result else {
        panic!("expected connected stream");
    };
    assert_eq!(state.stream_hits.load(Ordering::SeqCst), 1);
    let sent = state.bodies.lock().unwrap()[0].clone();
    assert!(sent["request"].get("stream").is_none());
    let headers = state.headers.lock().unwrap();
    let accept = headers[0]
        .get(axum::http::header::ACCEPT)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(accept, "text/event-stream");
}

#[tokio::test]
async fn gemini_chat_401_refreshes_and_replays_once_successfully() {
    let (service, state, _repo, account) = gemini_mock().await;
    state.fail_once.store(true, Ordering::SeqCst);
    let body = json!({
        "model": "gemini-2.5-flash",
        "messages": [{"role":"user","content":"hi"}]
    });
    let attempt = gemini_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "gemini-2.5-flash",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::ChatCompletions,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Success(success) = result else {
        panic!("expected success after refresh replay, got {result:?}");
    };
    assert_eq!(success.status, 200);
    assert_eq!(state.generate_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn gemini_chat_401_replay_still_unauthorized_is_terminal() {
    let (service, state, _repo, account) = gemini_mock().await;
    state.fail_401.store(true, Ordering::SeqCst);
    let body = json!({
        "model": "gemini-2.5-flash",
        "messages": [{"role":"user","content":"hi"}]
    });
    let attempt = gemini_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "gemini-2.5-flash",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::ChatCompletions,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Failure(failure) = result else {
        panic!("expected auth-terminal failure, got {result:?}");
    };
    assert_eq!(failure.failure_class, FailureClass::ChannelAuthTerminal);
    assert_eq!(state.generate_hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn gemini_responses_non_stream_converts_through_chat() {
    let (service, state, _repo, account) = gemini_mock().await;
    let body = json!({
        "model": "gemini-2.5-flash",
        "input": "hi"
    });
    let attempt = gemini_attempt(
        &account.id,
        EndpointKind::Responses,
        body,
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "gemini-2.5-flash",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::Responses,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Success(success) = result else {
        panic!("expected success, got {result:?}");
    };
    assert_eq!(success.body["object"], "response");
    assert_eq!(success.body["status"], "completed");
    assert_eq!(success.body["output"][0]["content"][0]["text"], "hi");
    let sent = state.bodies.lock().unwrap()[0].clone();
    assert_eq!(sent["project"], "proj-1");
    assert_eq!(
        sent["request"]["session_id"],
        format!("waliapi-{}", account.id)
    );
    assert!(sent["user_prompt_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
}

#[tokio::test]
async fn gemini_chat_403_is_terminal_and_marks_permission_denied() {
    let (service, state, repo, account) = gemini_mock().await;
    state.fail_403.store(true, Ordering::SeqCst);
    let body = json!({
        "model": "gemini-2.5-flash",
        "messages": [{"role":"user","content":"hi"}]
    });
    let attempt = gemini_attempt(
        &account.id,
        EndpointKind::ChatCompletions,
        body,
        crate::core::route_plan::AuthNonStreamFraming::Json,
        "gemini-2.5-flash",
    );
    let result = crate::endpoint_executor::dispatch_auth_account_executor(
        EndpointKind::ChatCompletions,
        &attempt,
        &service,
        &[],
    )
    .await;
    let AttemptResult::Failure(failure) = result else {
        panic!("expected auth-terminal failure, got {result:?}");
    };
    assert_eq!(failure.failure_class, FailureClass::ChannelAuthTerminal);
    assert_eq!(failure.status_code, Some(403));
    let stored = repo.get_auth_account(&account.id).await.unwrap();
    assert_eq!(stored.status, "invalid");
    let attributes: Value = serde_json::from_str(&stored.attributes_json).unwrap();
    assert_eq!(attributes["invalidation_reason"], "permission_denied");
}
