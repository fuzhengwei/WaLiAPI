//! 真实 HTTP 动态响应覆盖重复索引恢复、输入对应关系和恢复预算。
use axum::{http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use waliapi_lib::{
    db::repository::Repository,
    services::knowledge::{budget::Budget, embedder},
};

const MODEL: &str = "batch-public-alias";
const PRIVATE_ERROR: &str = "upstream-private-message-must-not-leak";

struct Upstream {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    arrived: Arc<Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Upstream {
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    async fn wait_requests(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.arrived.notified();
                if self.requests.lock().unwrap().len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("模拟上游没有收到预期请求");
    }
}

async fn upstream(
    respond: impl Fn(usize, &Value) -> (StatusCode, Value, Duration) + Send + Sync + 'static,
) -> Upstream {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let arrived = Arc::new(Notify::new());
    let notify = arrived.clone();
    let respond = Arc::new(respond);
    let app = Router::new().route(
        "/native/v1/embeddings",
        post(move |Json(body): Json<Value>| {
            let observed = observed.clone();
            let notify = notify.clone();
            let respond = respond.clone();
            async move {
                let call = {
                    let mut requests = observed.lock().unwrap();
                    requests.push(body.clone());
                    requests.len()
                };
                notify.notify_one();
                let (status, response, delay) = respond(call, &body);
                tokio::time::sleep(delay).await;
                (status, Json(response))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/native/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Upstream {
        url,
        requests,
        arrived,
        server,
    }
}

async fn repository(server: &Upstream, timeout_secs: u64) -> Repository {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let repo = Repository::new(pool);
    repo.create_channel(
        &serde_json::from_value(json!({
            "name":"batch-recovery", "type":"openai", "base_url":server.url,
            "api_key":"local-test-only", "models":["upstream-a","upstream-b"],
            "model_mapping":{MODEL:["upstream-a","upstream-b"]},
            "timeout_secs":timeout_secs,
            "protocol":"openai", "provider":"custom", "native_base_url":server.url,
            "native_endpoints":["embeddings"], "config":{"proxy":{"mode":"direct"}}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    repo
}

fn texts(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("item-{i}")).collect()
}

/// 向量包含输入编号，响应倒序，确保测试能识别错配、重复和丢失输入。
fn response(body: &Value, index_period: usize, dimension: usize) -> Value {
    let mut data: Vec<_> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(position, text)| {
            let number: usize = text
                .as_str()
                .unwrap()
                .strip_prefix("item-")
                .unwrap()
                .parse()
                .unwrap();
            let mut embedding = vec![1.0; dimension];
            embedding[0] = number as f32;
            json!({"index":position % index_period,"embedding":embedding})
        })
        .collect();
    data.reverse();
    json!({"data":data})
}

#[tokio::test]
async fn repeated_indexes_recover_without_reordering_inputs_or_remapping_model() {
    for count in [32, 9, 22, 100] {
        let server =
            upstream(|_, body| (StatusCode::OK, response(body, 8, 2), Duration::ZERO)).await;
        let repo = repository(&server, 10).await;
        let result = embedder::embed(&texts(count), MODEL, &repo).await.unwrap();
        let expected: Vec<_> = (0..count).map(|i| vec![i as f32, 1.0]).collect();
        assert_eq!(result, expected, "恢复后每个向量必须对应原输入: {count}");
        let requests = server.requests();
        assert!(requests.len() > 1 && requests.len() <= 32);
        assert_eq!(requests[0]["input"].as_array().unwrap().len(), count);
        assert!(matches!(
            requests[0]["model"].as_str(),
            Some("upstream-a" | "upstream-b")
        ));
        for request in &requests {
            assert_eq!(
                request["model"], requests[0]["model"],
                "拆批不能重新抽样模型映射"
            );
            assert_eq!(request["encoding_format"], "float");
        }
    }
}

#[tokio::test]
async fn valid_batches_and_single_queries_do_not_add_requests() {
    for count in [1, 32] {
        let server = upstream(|_, body| {
            (
                StatusCode::OK,
                response(body, usize::MAX, 2),
                Duration::ZERO,
            )
        })
        .await;
        let repo = repository(&server, 10).await;
        let result = embedder::embed(&texts(count), MODEL, &repo).await.unwrap();
        assert_eq!(
            result,
            (0..count).map(|i| vec![i as f32, 1.0]).collect::<Vec<_>>()
        );
        assert_eq!(server.requests().len(), 1, "合法响应不能触发拆批");
    }
}

#[derive(Clone, Copy, Debug)]
enum ChildFailure {
    Count,
    Dimension,
    Authentication,
    Timeout,
}

#[tokio::test]
async fn later_child_failures_reject_the_whole_batch_without_leaking_upstream_body() {
    for failure in [
        ChildFailure::Count,
        ChildFailure::Dimension,
        ChildFailure::Authentication,
        ChildFailure::Timeout,
    ] {
        let server = upstream(move |call, body| {
            if call == 1 {
                return (StatusCode::OK, response(body, 2, 2), Duration::ZERO);
            }
            if call == 2 {
                return (
                    StatusCode::OK,
                    response(body, usize::MAX, 2),
                    Duration::ZERO,
                );
            }
            match failure {
                ChildFailure::Count => {
                    let mut result = response(body, usize::MAX, 2);
                    result["data"].as_array_mut().unwrap().pop();
                    (StatusCode::OK, result, Duration::ZERO)
                }
                ChildFailure::Dimension => (
                    StatusCode::OK,
                    response(body, usize::MAX, 3),
                    Duration::ZERO,
                ),
                ChildFailure::Authentication => (
                    StatusCode::UNAUTHORIZED,
                    json!({"error":{"message":PRIVATE_ERROR}}),
                    Duration::ZERO,
                ),
                ChildFailure::Timeout => (
                    StatusCode::OK,
                    response(body, usize::MAX, 2),
                    Duration::from_secs(3),
                ),
            }
        })
        .await;
        let repo = repository(&server, 1).await;
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            embedder::embed(&texts(4), MODEL, &repo),
        )
        .await
        .expect("错误子批必须在渠道时限内结束")
        .unwrap_err();
        assert!(!error.contains(PRIVATE_ERROR), "不得显示原始供应商错误正文");
        assert_eq!(
            server.requests().len(),
            3,
            "错误后不能继续拆分或返回部分结果: {failure:?}"
        );
    }
}

#[tokio::test]
async fn channel_deadline_is_shared_by_initial_attempt_and_all_children() {
    let server = upstream(|call, body| {
        (
            StatusCode::OK,
            response(body, if call == 1 { 2 } else { usize::MAX }, 2),
            Duration::from_millis(600),
        )
    })
    .await;
    let repo = repository(&server, 1).await;
    let start = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        embedder::embed(&texts(4), MODEL, &repo),
    )
    .await
    .unwrap();
    assert!(result.is_err(), "不能给每个600ms子请求重新分配1秒预算");
    assert!(start.elapsed() < Duration::from_secs(2));
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(server.requests().len(), 2, "截止后不能启动最后一个子批");
}

#[tokio::test]
async fn parent_deadline_and_cancellation_stop_remaining_children() {
    for cancel in [false, true] {
        let server = upstream(|call, body| {
            (
                StatusCode::OK,
                response(body, if call == 1 { 2 } else { usize::MAX }, 2),
                if call == 1 {
                    Duration::ZERO
                } else {
                    Duration::from_secs(2)
                },
            )
        })
        .await;
        let repo = repository(&server, 10).await;
        let budget = Budget::new(if cancel { 5_000 } else { 500 }, "batch-test");
        let active = budget.clone();
        let task =
            tokio::spawn(
                async move { active.scope(embedder::embed(&texts(4), MODEL, &repo)).await },
            );
        server.wait_requests(2).await;
        if cancel {
            budget.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "父截止或取消必须终止整个恢复操作");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(server.requests().len(), 2, "不能在父预算终止后发出下一批");
    }
}

#[tokio::test]
async fn permanently_repeated_indexes_have_a_hard_request_limit() {
    let server = upstream(|_, body| (StatusCode::OK, response(body, 1, 2), Duration::ZERO)).await;
    let repo = repository(&server, 10).await;
    let result = embedder::embed(&texts(100), MODEL, &repo).await;
    assert!(result.is_err(), "不能返回调用上限之前得到的少量单条向量");
    assert_eq!(server.requests().len(), 32, "恢复请求数量必须有硬上限");
}
