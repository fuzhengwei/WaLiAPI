use super::budget::BudgetElapsed;
use crate::core::attempt::{
    AttemptFailure, AttemptResult, FailureClass, PreparedAttempt, TokenUsage,
};
use crate::core::channel_identity::ChannelIdentity;
use crate::core::route_plan::{plan_internal_embeddings, EndpointKind, RouteCandidate};
use crate::db::models::Channel;
use crate::db::repository::Repository;
use crate::endpoint_executor::driver::{
    budget_failure, dispatch_channel_with_key_failover, record_channel_mode_outcome,
};
use crate::security::gate::{audit_envelope, DownstreamProtocol, RequestEnvelope};
use rand::SeedableRng;
use std::collections::HashMap;
use std::time::Duration;

/// 受信任的内部知识库身份调用 Embedding，不创建或借用外部 API Key。
/// 与外部网关复用模型/端点路由及执行器；外部请求的权限、额度和审计仍由网关处理。
pub async fn embed(
    texts: &[String],
    model: &str,
    repo: &Repository,
) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(vec![]);
    }
    // 查询和文档统一修复 PDF 部首字形，覆盖 REST、MCP、管理命令入口。
    let texts: Vec<String> = texts
        .iter()
        .map(|t| super::text::normalize_radicals(t))
        .collect();
    let body = serde_json::json!({
        "model": model,
        "input": texts,
        "encoding_format": "float"
    });
    let channels = repo
        .get_enabled_channels_for_mode(
            EndpointKind::Embeddings.as_str(),
            false,
            &crate::utils::time::now_iso(),
        )
        .await
        .map_err(|_| "读取 Embedding 渠道失败".to_string())?;
    let plan = plan_internal_embeddings(
        model,
        &channels,
        &body,
        &mut rand::rngs::StdRng::from_os_rng(),
    )
    .map_err(|error| match error {
        crate::core::route_plan::PlanError::NoEndpointSupported(..) => format!(
            "Embedding 渠道未声明 embeddings 能力 (HTTP 501)，请检查模型 {model} 的渠道端点配置"
        ),
        _ => format!(
            "Embedding 路由不可用 (HTTP {}): {}",
            error.http_status(),
            error.message()
        ),
    })?;
    let lookup: HashMap<_, _> = plan
        .groups
        .iter()
        .flat_map(|group| &group.candidates)
        .map(|candidate| {
            (
                candidate.candidate.id().to_string(),
                candidate.candidate.clone(),
            )
        })
        .collect();
    // 内部文档处理没有外部 Key 的安全策略；使用 gate 构造规范 envelope，
    // 保留原有内部审计语义，不把内部请求伪装成经过外部权限校验的请求。
    let audited = audit_envelope(
        RequestEnvelope {
            downstream_protocol: DownstreamProtocol::Embeddings,
            endpoint: "internal://knowledge/embeddings".to_string(),
            original_json: body,
            safe_forward_headers: vec![],
            query: None,
            model: model.to_string(),
            stream: false,
            trace_id: Some(format!("kb-internal_{}", uuid::Uuid::new_v4())),
        },
        &crate::security::SecuritySettings::default(),
        None,
        vec![],
    )
    .map_err(|_| "无法构造内部 Embedding 请求".to_string())?;
    let expected_count = texts.len();
    let execution = crate::core::plan_executor::execute_plan(
        plan,
        &audited,
        rand::rngs::StdRng::from_os_rng(),
        |attempt| {
            let candidate = lookup.get(&attempt.channel_id).cloned();
            let attempt = attempt.clone();
            async move {
                let Some(RouteCandidate::Channel { channel, identity }) = candidate else {
                    return AttemptResult::Failure(AttemptFailure {
                        failure_class: FailureClass::UpstreamProtocolError,
                        message: "内部 Embedding 渠道不存在".to_string(),
                        status_code: Some(502),
                        retry_after: None,
                    });
                };
                let result = dispatch_validated_batch(&attempt, &channel, &identity, repo).await;
                record_channel_mode_outcome(
                    repo,
                    &channel.id,
                    EndpointKind::Embeddings.as_str(),
                    false,
                    &result,
                )
                .await;
                result
            }
        },
    )
    .await;
    if execution.last_failure.is_some() || !(200..300).contains(&execution.status) {
        // 上游错误正文可能包含凭据或用户文本，只反馈稳定类别与状态。
        let hint = if execution
            .last_failure
            .as_ref()
            .is_some_and(|failure| failure.failure_class == FailureClass::UpstreamProtocolError)
        {
            "上游向量响应格式不兼容，请检查响应或减小 Embedding 批次大小后重试"
        } else {
            "请检查渠道配置或稍后重试"
        };
        return Err(format!(
            "Embedding 请求失败 (HTTP {}, {})，{hint}",
            execution.status,
            execution
                .last_failure
                .as_ref()
                .map(|failure| failure.failure_class.as_str())
                .unwrap_or("upstream_error")
        ));
    }
    let embeddings = parse_embedding_response(&execution.body, expected_count)
        .map_err(|error| error.to_string())?;
    tracing::info!(
        caller = "knowledge_internal",
        channel_id = execution.channel_id.as_deref().unwrap_or_default(),
        model,
        texts = expected_count,
        dim = embeddings[0].len(),
        "Embedding success"
    );
    Ok(embeddings)
}

/// 重复索引可能来自上游内部拆批；丢弃坏响应，缩小输入后重新请求，绝不猜测向量归属。
/// 渠道、映射模型及总截止时间保持不变，任一子批失败均不发布部分结果。
async fn dispatch_validated_batch(
    attempt: &PreparedAttempt,
    channel: &Channel,
    identity: &ChannelIdentity,
    repo: &Repository,
) -> AttemptResult {
    let Some(inputs) = attempt
        .encoded_body
        .get("input")
        .and_then(serde_json::Value::as_array)
    else {
        return invalid_response("内部 Embedding 请求缺少输入数组");
    };
    // 候选预算不计算闭包内的拆批调用，另限 32 次批次调度；每次沿用执行器
    // 最多 3 个 Key 的限制（每候选最多 96 次物理请求），并共享下方总时限。
    const MAX_BATCH_DISPATCHES: usize = 32;
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(channel.timeout_secs.max(1) as u64);
    let work = async {
        let mut batch_size = inputs.len();
        let mut offset = 0;
        let mut combined = Vec::new();
        let mut dimension = None;
        let mut usage = TokenUsage::default();
        for request_no in 1..=MAX_BATCH_DISPATCHES {
            if inputs.len() > 1 && tokio::time::Instant::now() >= deadline {
                return AttemptResult::Failure(budget_failure(BudgetElapsed::ChannelTimeout));
            }
            let end = (offset + batch_size).min(inputs.len());
            let mut batch_attempt = attempt.clone();
            batch_attempt.encoded_body["input"] = serde_json::json!(&inputs[offset..end]);
            let result = dispatch_channel_with_key_failover(
                EndpointKind::Embeddings,
                &batch_attempt,
                channel,
                identity,
                &[],
                None,
                repo,
            )
            .await;
            let AttemptResult::Success(mut success) = result else {
                return result;
            };
            if let Some(tokens) = &success.usage {
                usage.prompt_tokens = usage.prompt_tokens.saturating_add(tokens.prompt_tokens);
                usage.completion_tokens = usage
                    .completion_tokens
                    .saturating_add(tokens.completion_tokens);
                usage.total_tokens = usage.total_tokens.saturating_add(tokens.total_tokens);
                usage.cached_tokens = usage.cached_tokens.saturating_add(tokens.cached_tokens);
            }
            let embeddings = match parse_embedding_response(&success.body, end - offset) {
                Ok(embeddings) => embeddings,
                Err(EmbeddingResponseError::DuplicateIndex(index)) if end - offset > 1 => {
                    batch_size = ((end - offset) / 2).max(1);
                    tracing::warn!(channel_id = %channel.id, model = %attempt.upstream_model,
                        request_no, offset, input_count = end - offset, duplicate_index = index,
                        next_batch_size = batch_size, "Embedding 索引重复，缩小批次重新请求");
                    continue;
                }
                Err(error) => return invalid_response(error),
            };
            // 正常批次及单条查询直接沿用原响应，不改其用量和元数据。
            if request_no == 1 {
                return AttemptResult::Success(success);
            }
            if dimension
                .is_some_and(|dim| embeddings.iter().any(|embedding| embedding.len() != dim))
            {
                return invalid_response("Embedding 子批次向量维度不一致");
            }
            dimension = embeddings.first().map(Vec::len);
            for embedding in embeddings {
                combined.push(serde_json::json!({"index": combined.len(), "embedding": embedding}));
            }
            offset = end;
            if offset == inputs.len() {
                success.body["data"] = serde_json::Value::Array(combined);
                success.body["usage"] = serde_json::to_value(&usage).unwrap_or_default();
                success.usage = Some(usage);
                return AttemptResult::Success(success);
            }
        }
        invalid_response("Embedding 索引重复后的拆批请求已达上限，请减小 Embedding 批次大小")
    };
    // 单条调用保持原行为；批量恢复共用从首次请求开始的渠道时限。
    // 执行器内仍继承父 RAG 预算和取消信号，不会为每次拆批重置父预算。
    if inputs.len() <= 1 {
        return work.await;
    }
    tokio::time::timeout_at(deadline, work)
        .await
        .unwrap_or_else(|_| AttemptResult::Failure(budget_failure(BudgetElapsed::ChannelTimeout)))
}

fn invalid_response(error: impl std::fmt::Display) -> AttemptResult {
    // 只接收本地校验错误，不记录上游正文、文档文本或向量。
    let message = error.to_string();
    tracing::warn!(reason = %message, "Embedding 响应校验失败");
    AttemptResult::Failure(AttemptFailure {
        failure_class: FailureClass::UpstreamProtocolError,
        message,
        status_code: Some(502),
        retry_after: None,
    })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum EmbeddingResponseError {
    #[error("Duplicate embedding index: {0}")]
    DuplicateIndex(usize),
    #[error("{0}")]
    Invalid(String),
}

/// 按响应索引还原输入顺序，整批校验后才允许调用方写入文档切片。
/// 兼容完全不提供 index 的旧渠道；一旦提供索引就必须完整且唯一。
pub(crate) fn parse_embedding_response(
    response: &serde_json::Value,
    expected_count: usize,
) -> Result<Vec<Vec<f32>>, EmbeddingResponseError> {
    let data = response
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            EmbeddingResponseError::Invalid("Invalid embedding response: missing data array".into())
        })?;
    if data.len() != expected_count {
        return Err(EmbeddingResponseError::Invalid(format!(
            "Embedding count mismatch: expected {}, got {}",
            expected_count,
            data.len()
        )));
    }

    let indexed = data.iter().any(|item| item.get("index").is_some());
    let mut embeddings = vec![Vec::new(); expected_count];
    let mut dimension = None;
    for (position, item) in data.iter().enumerate() {
        let index = if indexed {
            item.get("index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .filter(|&index| index < expected_count)
                .ok_or_else(|| {
                    EmbeddingResponseError::Invalid(format!(
                        "Invalid embedding index at item {position}"
                    ))
                })?
        } else {
            position
        };
        if !embeddings[index].is_empty() {
            return Err(EmbeddingResponseError::DuplicateIndex(index));
        }
        let embedding: Vec<f32> = serde_json::from_value(
            item.get("embedding").cloned().unwrap_or_default(),
        )
        .map_err(|_| {
            EmbeddingResponseError::Invalid(format!(
                "Embedding item {position} is not a float vector"
            ))
        })?;
        if embedding.is_empty() || embedding.iter().any(|value| !value.is_finite()) {
            return Err(EmbeddingResponseError::Invalid(format!(
                "Invalid embedding vector at item {position}"
            )));
        }
        if dimension.is_some_and(|dim| dim != embedding.len()) {
            return Err(EmbeddingResponseError::Invalid(format!(
                "Inconsistent embedding dimensions at item {position}"
            )));
        }
        dimension = Some(embedding.len());
        embeddings[index] = embedding;
    }
    Ok(embeddings)
}
