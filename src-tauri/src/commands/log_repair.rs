//! 历史 499 日志的一次性修复命令。
//!
//! # 背景
//!
//! 0.2.8 之前，流式请求的成功日志写在 `async_stream::stream!` 生成器**最后一次
//! `yield` 之后**，只有消费者再轮询一次才会执行。Agent 类客户端（Codex / Claude
//! Code / Node undici）拿到终止帧 `[DONE]` 就立刻关连接，hyper 停止轮询并 drop
//! 掉 body，`StreamLogFinalizer::drop` 兜底路径于是把一条完整成功的流写成
//! `499 / client_cancelled / 0 token / 空响应`。
//!
//! 前向缺陷已由 `endpoint_executor::driver` 修复；**已经落库的历史行**需要本命令
//! 单独恢复。
//!
//! # 为什么可以逐行判定，而不是按 duration 猜
//!
//! Agent 的对话历史是累积的：若第 N 轮真的产出了被下游采用的回复，那么同会话
//! 第 N+1 轮请求的 `messages` 里必然多出第 N 轮那条 assistant 消息。这是内容级
//! 证据，不是启发式打分。据此既能判定"本轮其实成功了"，又能顺带把本轮的响应正文
//! 和 completion tokens 一起恢复出来。
//!
//! 参照行的 `messages` 必须与目标行**前缀逐条相等**才采信（否则说明换了会话，
//! 或客户端压缩过历史），判定偏保守：拿不到证据的行保持 499。
//!
//! # 安全边界
//!
//! - 默认 dry-run，只报告不写库；`apply=true` 才落库。
//! - 单次最多处理 `limit` 行，可反复调用直到 `remaining == 0`，避免长任务卡住一次请求。
//! - 只碰 `usage_source IS NULL` 的行，天然幂等，重复执行不会二次改写。
//! - **不改 `quota_used`**：配额语义是否要同步是独立决策，留给使用者。
//! - 回填出的数字一律标 `usage_source='repaired'`，与上游实测值可区分。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::endpoint_executor::estimate_usage::{count_tokens, estimate_usage};
use crate::AppState;

/// 采信插值所需的字节比窗口。超出即认为参照行不属于同一对话状态。
///
/// 本机 434 对"两行都有上游真实 prompt_tokens"的相邻样本留一验证：
/// 比值落在 1.00–1.02 时中位误差 **0.01%**、P90 0.13%；1.05–1.15 恶化到 2.9%；
/// ≥1.5 恶化到 25%。故窗口收紧在 0.9–1.1。
///
/// **已知局限（实测）**：以后继行为锚点时，本层在本机只命中 **0.8%**（7/873）——
/// 因为 499 行往往连续成串（回溯链中位 44 行、P90 123 行），后继本身通常也是
/// prompt_tokens=0 的 499 行，没有可锚的真实值。剩下 99.2% 全部落到 BPE，
/// 这就是全量回填在 1.7 GB 库上要跑约 23 分钟的原因。改用前驱行为锚点可把命中
/// 提到 91.9%，但链式插值会 telescoping 成 `prompt(锚点) × chars(N)/chars(锚点)`，
/// 等价于直接用远锚点，严格窗口下覆盖率只回升到 17.3% —— 精度受益、耗时不受益。
/// 真正能同时改善两者的是增量法（只对每轮新增的几 KB 跑 BPE 再累加），
/// 属后续优化，本 PR 未做。
const INTERP_MIN_RATIO: f64 = 0.9;
const INTERP_MAX_RATIO: f64 = 1.1;

/// 默认单次处理行数。
const DEFAULT_LIMIT: i64 = 50;

/// dry-run 报告里最多给出多少条明细样例。
const SAMPLE_LIMIT: usize = 10;

/// `request_logs.usage_source` 的取值：本命令回填的行。
const USAGE_SOURCE_REPAIRED: &str = "repaired";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMethod {
    /// 同会话邻近真实行按字节比插值（实测中位误差 0.01%）
    Interpolated,
    /// 本地 BPE 估算（`cl100k_base`，对非 OpenAI 系模型自述 ±10–20% 偏差）
    Estimated,
    /// 本次范围不含用量回填，未做计算
    Skipped,
}

/// 对单条历史行算出的修复计划。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepairPlan {
    /// 有内容证据证明本轮回复被下游采用 → 可以改判 200
    pub delivered: bool,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub prompt_method: PromptMethod,
    /// 从下一轮请求里恢复出的本轮回复（证据成立时才有）
    pub response_choices: Option<String>,
}

/// 一条待修复的历史行。
#[derive(Debug, Clone, sqlx::FromRow)]
struct Candidate {
    id: String,
    seq: i64,
    model: String,
    request_body: String,
    body_chars: i64,
}

/// 同会话的参照行（目标行 seq 之后紧邻的一行）。
///
/// `plan_repair` 的公开入参类型，故必须 pub。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Successor {
    request_body: String,
    body_chars: i64,
    prompt_tokens: i64,
}

/// 修复范围。状态改判与用量回填是两件独立的事：
/// 前者只要比对消息前缀（秒级），后者要对 request_body 跑 BPE（大库上分钟级），
/// 且精度来源不同，所以必须能分开选。
///
/// ⚠ 两个 scope **不能先后各跑一次**：候选条件是 `usage_source IS NULL`，
/// 先跑 `reclassify` 会把行打上 `repaired` 标记，第二次跑 `backfill_usage`
/// 就再也扫不到这些行了。要么一次跑完（缺省），要么只跑其中一种。
#[derive(Debug, Clone, Copy)]
pub struct RepairScope {
    /// 有内容证据的行 499 → 200，并清掉 client_cancelled / error_message
    pub reclassify: bool,
    /// 补记 prompt / completion / total tokens 并恢复 response_choices
    pub backfill_usage: bool,
}

impl Default for RepairScope {
    fn default() -> Self {
        Self {
            reclassify: true,
            backfill_usage: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepairInput {
    /// false（默认）= 只报告不写库
    pub apply: Option<bool>,
    /// 单次最多处理多少行
    pub limit: Option<i64>,
    /// 是否执行状态改判，缺省 true
    pub reclassify: Option<bool>,
    /// 是否执行用量回填，缺省 true
    pub backfill_usage: Option<bool>,
}

impl RepairInput {
    /// 两个开关同时关闭会让本命令什么都不做（只会打 usage_source 标记），
    /// 那是误用而不是意图，显式拦掉。
    fn scope(&self) -> Result<RepairScope, String> {
        let scope = RepairScope {
            reclassify: self.reclassify.unwrap_or(true),
            backfill_usage: self.backfill_usage.unwrap_or(true),
        };
        if !scope.reclassify && !scope.backfill_usage {
            return Err(
                "reclassify 与 backfill_usage 不能同时为 false，那样不会改动任何数据".to_string(),
            );
        }
        Ok(scope)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RepairSample {
    pub seq: i64,
    pub model: String,
    pub delivered: bool,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub prompt_method: PromptMethod,
    pub restored_response_chars: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepairReport {
    pub dry_run: bool,
    /// 本次生效的修复范围（让读报告的人知道数字是否真的写进了库）
    pub reclassify: bool,
    pub backfill_usage: bool,
    pub scanned: i64,
    /// 有内容证据证明其实成功交付的行数（是否真改状态取决于 `reclassify`）
    pub reclassified: i64,
    /// 无证据、保持 499 的行数
    pub kept_cancelled: i64,
    pub prompt_tokens_added: i64,
    pub completion_tokens_added: i64,
    pub response_choices_restored: i64,
    /// request_body 无法解析、只打标记不改数值的行数
    pub skipped_unparsable: i64,
    /// 仍待处理的行数（apply 模式下收敛到 0 表示全部处理完）
    pub remaining: i64,
    pub samples: Vec<RepairSample>,
}

fn messages_of(value: &Value) -> Option<&Vec<Value>> {
    value.get("messages").and_then(Value::as_array)
}

fn is_assistant(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("assistant")
}

/// 取 successor 相对 target 多出来的 assistant 消息，即本轮的实际回复。
///
/// 返回 `None` 表示不构成证据：successor 不比 target 长、前缀不一致（换了会话或
/// 历史被压缩）、或多出来的部分里没有 assistant 消息。
fn recovered_assistant_turns(target: &[Value], successor: &[Value]) -> Option<Vec<Value>> {
    if successor.len() <= target.len() {
        return None;
    }
    // 前缀必须逐条相等：zip 到较短的 target 为止，正好覆盖 target 全部消息。
    if !successor.iter().zip(target.iter()).all(|(s, t)| s == t) {
        return None;
    }
    let turns: Vec<Value> = successor[target.len()..]
        .iter()
        .filter(|m| is_assistant(m))
        .cloned()
        .collect();
    if turns.is_empty() {
        None
    } else {
        Some(turns)
    }
}

/// 把恢复出的 assistant 消息合并成日志详情页使用的 `response_choices` 形状，
/// 与 `StreamPumpCore::build_response_choices` 产出的结构保持一致。
fn build_response_choices(turns: &[Value]) -> Option<String> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for turn in turns {
        if let Some(text) = turn.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(text);
            }
        }
        if let Some(text) = turn
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(text);
        }
        if let Some(calls) = turn.get("tool_calls").and_then(Value::as_array) {
            for (idx, call) in calls.iter().enumerate() {
                let mut call = call.clone();
                call["index"] = json!(tool_calls.len() + idx);
                tool_calls.push(call);
            }
        }
    }

    if content.is_empty() && reasoning.is_empty() && tool_calls.is_empty() {
        return None;
    }

    let mut message = json!({ "role": "assistant" });
    if !content.is_empty() {
        message["content"] = json!(&content);
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(&reasoning);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    let choices = vec![json!({
        "index": 0,
        "message": message,
        "finish_reason": "stop",
    })];
    serde_json::to_string(&choices).ok()
}

/// 恢复出的回复一共多少 token（正文 + 思考 + 工具调用参数）。
fn recovered_tokens(turns: &[Value]) -> i64 {
    let mut text = String::new();
    for turn in turns {
        if let Some(t) = turn.get("content").and_then(Value::as_str) {
            text.push_str(t);
        }
        if let Some(t) = turn.get("reasoning_content").and_then(Value::as_str) {
            text.push_str(t);
        }
        if let Some(calls) = turn.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    text.push_str(args);
                }
            }
        }
    }
    count_tokens(&text)
}

/// 纯函数：对一条历史行 + 其同会话参照行算出修复计划。
///
/// `successor` 为 `None` 表示找不到参照行（会话末尾 / 该组没有后续请求）。
///
/// `with_usage=false` 时**完全跳过 token 计算**（插值和 BPE 都不做）。这不是省
/// 几行代码的问题：BPE 要跑完整 request_body（本机 714 MB / 约 23 分钟），而只要
/// 改判状态的话，证据判定靠的是消息前缀比对，两者成本差两个数量级。
pub fn plan_repair(
    target_body: &str,
    target_chars: i64,
    model: &str,
    successor: Option<&Successor>,
    with_usage: bool,
) -> Option<RepairPlan> {
    let target: Value = serde_json::from_str(target_body).ok()?;
    let target_msgs = messages_of(&target)?;

    let mut delivered = false;
    let mut turns: Vec<Value> = Vec::new();

    if let Some(succ) = successor {
        if let Ok(succ_json) = serde_json::from_str::<Value>(&succ.request_body) {
            if let Some(succ_msgs) = messages_of(&succ_json) {
                if let Some(recovered) = recovered_assistant_turns(target_msgs, succ_msgs) {
                    delivered = true;
                    turns = recovered;
                }
            }
        }
    }

    if !with_usage {
        return Some(RepairPlan {
            delivered,
            prompt_tokens: 0,
            completion_tokens: 0,
            prompt_method: PromptMethod::Skipped,
            response_choices: None,
        });
    }

    // prompt：优先同会话邻近真实行按字节比插值，窗口外退到本地 BPE 估算。
    let interpolated = successor.and_then(|succ| {
        if succ.prompt_tokens <= 0 || succ.body_chars <= 0 {
            return None;
        }
        let ratio = target_chars as f64 / succ.body_chars as f64;
        if (INTERP_MIN_RATIO..=INTERP_MAX_RATIO).contains(&ratio) {
            Some((succ.prompt_tokens as f64 * ratio).round() as i64)
        } else {
            None
        }
    });
    let (prompt_tokens, prompt_method) = match interpolated {
        Some(tokens) => (tokens, PromptMethod::Interpolated),
        None => (
            estimate_usage(&target, None, model).0,
            PromptMethod::Estimated,
        ),
    };

    let completion_tokens = if turns.is_empty() {
        0
    } else {
        recovered_tokens(&turns)
    };
    let response_choices = if turns.is_empty() {
        None
    } else {
        build_response_choices(&turns)
    };

    Some(RepairPlan {
        delivered,
        prompt_tokens,
        completion_tokens,
        prompt_method,
        response_choices,
    })
}

const CANDIDATE_WHERE: &str = "status_code = 499 AND client_cancelled = 1 AND total_tokens = 0 \
                               AND usage_source IS NULL AND request_body IS NOT NULL";

async fn fetch_candidates(pool: &SqlitePool, limit: i64) -> Result<Vec<Candidate>, String> {
    sqlx::query_as::<_, Candidate>(&format!(
        "SELECT id, seq, model, request_body, length(request_body) AS body_chars \
         FROM request_logs WHERE {CANDIDATE_WHERE} ORDER BY seq ASC LIMIT ?"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("读取待修复日志失败: {e}"))
}

async fn count_pending(pool: &SqlitePool) -> Result<i64, String> {
    sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM request_logs WHERE {CANDIDATE_WHERE}"
    ))
    .fetch_one(pool)
    .await
    .map_err(|e| format!("统计待修复日志失败: {e}"))
}

/// 取同 (api_key_id, model, channel_id) 分组内、seq 紧邻的下一行作为参照。
async fn fetch_successor(
    pool: &SqlitePool,
    candidate: &Candidate,
) -> Result<Option<Successor>, String> {
    let group: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT api_key_id, channel_id FROM request_logs WHERE id = ?")
            .bind(&candidate.id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("读取分组键失败: {e}"))?;
    let Some((Some(api_key_id), Some(channel_id))) = group else {
        return Ok(None);
    };
    let successor = sqlx::query_as::<_, Successor>(
        "SELECT request_body, length(request_body) AS body_chars, prompt_tokens \
         FROM request_logs \
         WHERE seq > ? AND api_key_id IS ? AND channel_id IS ? AND model = ? \
           AND request_body IS NOT NULL \
         ORDER BY seq ASC LIMIT 1",
    )
    .bind(candidate.seq)
    .bind(&api_key_id)
    .bind(&channel_id)
    .bind(&candidate.model)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("读取参照行失败: {e}"))?;
    Ok(successor)
}

/// 无法解析的行：数值一律不动，只打 `usage_source` 让扫描收敛。
async fn mark_processed(pool: &SqlitePool, candidate: &Candidate) -> Result<(), String> {
    sqlx::query("UPDATE request_logs SET usage_source = ? WHERE id = ?")
        .bind(USAGE_SOURCE_REPAIRED)
        .bind(&candidate.id)
        .execute(pool)
        .await
        .map_err(|e| format!("标记已处理失败: {e}"))?;
    Ok(())
}

/// 按 scope 拼 UPDATE。SET 子句全部来自固定字面量，不含任何外部输入。
/// 绑定顺序必须与拼出来的占位符顺序严格一致。
async fn apply_plan(
    pool: &SqlitePool,
    candidate: &Candidate,
    plan: &RepairPlan,
    scope: RepairScope,
) -> Result<(), String> {
    let mut sets: Vec<&str> = Vec::new();
    let do_status = scope.reclassify && plan.delivered;
    if do_status {
        sets.extend([
            "status_code = 200",
            "client_cancelled = 0",
            "error_message = NULL",
        ]);
    }
    if scope.backfill_usage {
        // 无证据的行也补 prompt：上游确实消耗过它。completion / response_choices
        // 在没有证据时本来就是 0 / NULL，写回去等价于不动。
        sets.extend([
            "prompt_tokens = ?",
            "completion_tokens = ?",
            "total_tokens = ?",
            "response_choices = ?",
        ]);
    }
    sets.push("usage_source = ?");

    let sql = format!("UPDATE request_logs SET {} WHERE id = ?", sets.join(", "));
    let mut query = sqlx::query(&sql);
    if scope.backfill_usage {
        query = query
            .bind(plan.prompt_tokens)
            .bind(plan.completion_tokens)
            .bind(plan.prompt_tokens + plan.completion_tokens)
            .bind(&plan.response_choices);
    }
    query
        .bind(USAGE_SOURCE_REPAIRED)
        .bind(&candidate.id)
        .execute(pool)
        .await
        .map_err(|e| format!("写入修复结果失败: {e}"))?;
    Ok(())
}

/// 执行一轮修复。直接接收连接池而不是 `AppState`，让集成测试可以用内存库跑通
/// 整条"扫描 → 判定 → 落库"链路。
pub async fn repair_pool(pool: &SqlitePool, input: RepairInput) -> Result<RepairReport, String> {
    let apply = input.apply.unwrap_or(false);
    let limit = input.limit.unwrap_or(DEFAULT_LIMIT).max(1);
    let scope = input.scope()?;

    let pending_before = count_pending(pool).await?;
    let candidates = fetch_candidates(pool, limit).await?;

    let mut report = RepairReport {
        dry_run: !apply,
        reclassify: scope.reclassify,
        backfill_usage: scope.backfill_usage,
        scanned: 0,
        reclassified: 0,
        kept_cancelled: 0,
        prompt_tokens_added: 0,
        completion_tokens_added: 0,
        response_choices_restored: 0,
        skipped_unparsable: 0,
        remaining: pending_before,
        samples: Vec::new(),
    };

    for candidate in candidates {
        let successor = fetch_successor(pool, &candidate).await?;
        let Some(plan) = plan_repair(
            &candidate.request_body,
            candidate.body_chars,
            &candidate.model,
            successor.as_ref(),
            scope.backfill_usage,
        ) else {
            // request_body 不是合法 JSON 或没有 messages：数值不动，
            // 但必须打上 usage_source，否则 `remaining` 永远收敛不到 0、调用方会死循环。
            report.scanned += 1;
            report.skipped_unparsable += 1;
            if apply {
                mark_processed(pool, &candidate).await?;
            }
            continue;
        };

        report.scanned += 1;
        if plan.delivered {
            report.reclassified += 1;
        } else {
            report.kept_cancelled += 1;
        }
        if scope.backfill_usage {
            report.prompt_tokens_added += plan.prompt_tokens;
            report.completion_tokens_added += plan.completion_tokens;
            if plan.response_choices.is_some() {
                report.response_choices_restored += 1;
            }
        }
        if report.samples.len() < SAMPLE_LIMIT {
            report.samples.push(RepairSample {
                seq: candidate.seq,
                model: candidate.model.clone(),
                delivered: plan.delivered,
                prompt_tokens: plan.prompt_tokens,
                completion_tokens: plan.completion_tokens,
                prompt_method: plan.prompt_method,
                restored_response_chars: plan
                    .response_choices
                    .as_ref()
                    .map(|s| s.chars().count())
                    .unwrap_or(0),
            });
        }
        if apply {
            apply_plan(pool, &candidate, &plan, scope).await?;
        }
    }

    report.remaining = count_pending(pool).await?;
    Ok(report)
}

#[tauri::command]
pub async fn repair_stream_cancel_logs(
    input: RepairInput,
    state: tauri::State<'_, std::sync::Arc<AppState>>,
) -> Result<RepairReport, String> {
    repair_pool(&state.db.pool, input).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(messages: Vec<Value>) -> String {
        json!({ "model": "m", "messages": messages }).to_string()
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": text })
    }

    fn assistant(text: &str) -> Value {
        json!({ "role": "assistant", "content": text })
    }

    fn succ(request_body: String, body_chars: i64, prompt_tokens: i64) -> Successor {
        Successor {
            request_body,
            body_chars,
            prompt_tokens,
        }
    }

    #[test]
    fn successor_with_new_assistant_turn_proves_delivery() {
        let target = body(vec![user("hi")]);
        let next = body(vec![user("hi"), assistant("hello there")]);
        let plan = plan_repair(&target, 100, "m", Some(&succ(next, 100, 1000)), true).unwrap();
        assert!(plan.delivered);
        assert_eq!(plan.completion_tokens, count_tokens("hello there"));
        let choices = plan.response_choices.unwrap();
        assert!(choices.contains("hello there"), "{choices}");
        assert!(choices.contains("\"finish_reason\":\"stop\""));
    }

    #[test]
    fn prefix_mismatch_is_not_evidence() {
        // 换了会话：下一行的第一条消息都不一样
        let target = body(vec![user("hi")]);
        let next = body(vec![user("totally different"), assistant("x")]);
        let plan = plan_repair(&target, 100, "m", Some(&succ(next, 120, 1000)), true).unwrap();
        assert!(!plan.delivered);
        assert_eq!(plan.completion_tokens, 0);
        assert!(plan.response_choices.is_none());
    }

    #[test]
    fn shorter_or_equal_successor_is_not_evidence() {
        let target = body(vec![user("a"), assistant("b"), user("c")]);
        let next = body(vec![user("a"), assistant("b")]);
        let plan = plan_repair(&target, 100, "m", Some(&succ(next, 90, 1000)), true).unwrap();
        assert!(!plan.delivered);
    }

    #[test]
    fn growth_without_assistant_turn_is_not_evidence() {
        // 只多了 tool 结果，本轮没有 assistant 回复
        let target = body(vec![user("hi")]);
        let next = body(vec![
            user("hi"),
            json!({"role": "tool", "content": "result"}),
        ]);
        let plan = plan_repair(&target, 100, "m", Some(&succ(next, 110, 1000)), true).unwrap();
        assert!(!plan.delivered);
        assert!(plan.response_choices.is_none());
    }

    #[test]
    fn prompt_interpolated_only_inside_byte_ratio_window() {
        let target = body(vec![user("hi")]);
        let next = body(vec![user("hi"), assistant("ok")]);
        // 比值 100/105 ≈ 0.95 → 落在窗口内，走插值
        let plan = plan_repair(
            &target,
            100,
            "m",
            Some(&succ(next.clone(), 105, 1000)),
            true,
        )
        .unwrap();
        assert_eq!(plan.prompt_method, PromptMethod::Interpolated);
        assert_eq!(
            plan.prompt_tokens,
            (1000.0f64 * (100.0f64 / 105.0f64)).round() as i64
        );
        // 比值 100/500 = 0.2 → 窗口外，退到估算
        let far = plan_repair(&target, 100, "m", Some(&succ(next, 500, 1000)), true).unwrap();
        assert_eq!(far.prompt_method, PromptMethod::Estimated);
        assert_ne!(far.prompt_tokens, plan.prompt_tokens);
    }

    #[test]
    fn no_successor_still_estimates_prompt() {
        let target = body(vec![user("hi there how are you doing today")]);
        let plan = plan_repair(&target, 100, "m", None, true).unwrap();
        assert!(!plan.delivered);
        assert_eq!(plan.prompt_method, PromptMethod::Estimated);
        assert!(plan.prompt_tokens > 0, "prompt 必须估出来，不能记 0");
    }

    #[test]
    fn unparsable_body_yields_no_plan() {
        assert!(plan_repair("not json", 10, "m", None, true).is_none());
        assert!(plan_repair(r#"{"model":"m"}"#, 10, "m", None, true).is_none());
    }

    #[test]
    fn tool_call_turns_are_merged_and_reindexed() {
        let target = body(vec![user("hi")]);
        let next = body(vec![
            user("hi"),
            json!({"role":"assistant","content":null,"tool_calls":[
                {"id":"c1","type":"function","index":0,"function":{"name":"f","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"c1","content":"r"}),
            json!({"role":"assistant","content":"done","tool_calls":[
                {"id":"c2","type":"function","index":0,"function":{"name":"g","arguments":"{}"}}]}),
        ]);
        let plan = plan_repair(&target, 100, "m", Some(&succ(next, 105, 1000)), true).unwrap();
        assert!(plan.delivered);
        let choices = plan.response_choices.unwrap();
        assert!(choices.contains("done"), "{choices}");
        assert!(
            choices.contains("\"c1\"") && choices.contains("\"c2\""),
            "{choices}"
        );
        // 两条 tool_call 的 index 必须重排成 0/1，不能都留 0
        assert!(choices.contains("\"index\":1"), "{choices}");
        assert!(plan.completion_tokens > 0);
    }

    #[tokio::test]
    async fn candidate_query_skips_already_repaired_rows() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        // 用 async fn 而不是返回 future 的闭包：闭包会把 `Option<&str>` 的
        // 生命周期和 async block 的返回类型对不上（E0623 类错误）。
        async fn insert(pool: &SqlitePool, seq: i64, usage_source: Option<&str>) {
            sqlx::query(
                "INSERT INTO request_logs (id, seq, model, mode, status_code, prompt_tokens, \
                 completion_tokens, total_tokens, duration_ms, is_stream, is_retry, \
                 created_at, request_body, risk_level, risk_score, security_action, \
                 sanitized, client_cancelled, stream_committed, usage_source) \
                 VALUES (?, ?, 'm', 'chat', 499, 0, 0, 0, 1, 1, 0, \
                         '2026-09-04T00:00:00.000Z', ?, 'clean', 0, 'allow', 0, 1, 1, ?)",
            )
            .bind(format!("id-{seq}"))
            .bind(seq)
            .bind(json!({"model":"m","messages":[{"role":"user","content":"hi"}]}).to_string())
            .bind(usage_source)
            .execute(pool)
            .await
            .unwrap();
        }
        insert(&pool, 1, None).await;
        insert(&pool, 2, Some("repaired")).await;

        assert_eq!(count_pending(&pool).await.unwrap(), 1);
        let cands = fetch_candidates(&pool, 10).await.unwrap();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].seq, 1);
        pool.close().await;
    }

    /// 端到端跑通"扫描 → 证据判定 → 落库 → 幂等"整条链路（内存库 + 真实迁移）。
    /// scope 开关必须真的生效：status-only 不改 token，usage-only 不改状态。
    #[tokio::test]
    async fn scope_flags_are_respected() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        async fn seed(pool: &SqlitePool) {
            sqlx::query(
                "INSERT INTO request_logs (id, seq, api_key_id, channel_id, model, mode, \
                 status_code, prompt_tokens, completion_tokens, total_tokens, duration_ms, \
                 is_stream, is_retry, created_at, request_body, error_message, risk_level, \
                 risk_score, security_action, sanitized, client_cancelled, stream_committed) \
                 VALUES ('a', 1, 'k', 'c', 'm', 'chat', 499, 0, 0, 0, 1, 1, 0, \
                         '2026-09-04T00:00:00.000Z', ?, 'client_cancelled', 'clean', 0, \
                         'allow', 0, 1, 1)",
            )
            .bind(json!({"messages":[{"role":"user","content":"hi"}]}).to_string())
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO request_logs (id, seq, api_key_id, channel_id, model, mode, \
                 status_code, prompt_tokens, completion_tokens, total_tokens, duration_ms, \
                 is_stream, is_retry, created_at, request_body, risk_level, risk_score, \
                 security_action, sanitized, client_cancelled, stream_committed) \
                 VALUES ('b', 2, 'k', 'c', 'm', 'chat', 200, 1000, 5, 1005, 1, 1, 0, \
                         '2026-09-04T00:01:00.000Z', ?, 'clean', 0, 'allow', 0, 0, 1)",
            )
            .bind(
                json!({"messages":[{"role":"user","content":"hi"},
                                   {"role":"assistant","content":"hello there"}]})
                .to_string(),
            )
            .execute(pool)
            .await
            .unwrap();
        }

        // ── status-only：改判 200，但 token 必须保持 0（说明 BPE 根本没跑）──
        seed(&pool).await;
        let r = repair_pool(
            &pool,
            RepairInput {
                apply: Some(true),
                limit: Some(10),
                reclassify: Some(true),
                backfill_usage: Some(false),
            },
        )
        .await
        .unwrap();
        assert_eq!(r.reclassified, 1);
        assert_eq!(
            r.prompt_tokens_added, 0,
            "status-only 不该产生任何 token 数字"
        );
        assert_eq!(r.response_choices_restored, 0);
        let row: (i64, i64, i64, Option<String>) = sqlx::query_as(
            "SELECT status_code, prompt_tokens, total_tokens, response_choices FROM request_logs WHERE id='a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, 200, "状态应改判");
        assert_eq!((row.1, row.2), (0, 0), "token 不应被写");
        assert!(row.3.is_none(), "response_choices 不应被写");
        let cancelled: i64 =
            sqlx::query_scalar("SELECT client_cancelled FROM request_logs WHERE id='a'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cancelled, 0);
        pool.close().await;

        // ── usage-only：补 token，但状态必须保持 499 ──
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        seed(&pool).await;
        let r = repair_pool(
            &pool,
            RepairInput {
                apply: Some(true),
                limit: Some(10),
                reclassify: Some(false),
                backfill_usage: Some(true),
            },
        )
        .await
        .unwrap();
        assert!(r.prompt_tokens_added > 0, "usage-only 应补上 prompt");
        let row: (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT status_code, prompt_tokens, error_message FROM request_logs WHERE id='a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, 499, "usage-only 不该改状态");
        assert!(row.1 > 0);
        assert_eq!(
            row.2.as_deref(),
            Some("client_cancelled"),
            "error_message 应保留"
        );
        pool.close().await;
    }

    /// 两个开关同时关闭是误用，必须显式报错而不是静默"处理"掉所有行。
    #[tokio::test]
    async fn empty_scope_is_rejected() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let e = repair_pool(
            &pool,
            RepairInput {
                apply: Some(true),
                limit: Some(10),
                reclassify: Some(false),
                backfill_usage: Some(false),
            },
        )
        .await
        .unwrap_err();
        assert!(e.contains("不能同时为 false"), "{e}");
        pool.close().await;
    }

    #[tokio::test]
    async fn repair_pool_reclassifies_proven_rows_and_is_idempotent() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        async fn insert(
            pool: &SqlitePool,
            seq: i64,
            status: i64,
            prompt: i64,
            total: i64,
            body: Value,
        ) {
            sqlx::query(
                "INSERT INTO request_logs (id, seq, api_key_id, channel_id, model, mode, \
                 status_code, prompt_tokens, completion_tokens, total_tokens, duration_ms, \
                 is_stream, is_retry, created_at, request_body, error_message, risk_level, \
                 risk_score, security_action, sanitized, client_cancelled, stream_committed) \
                 VALUES (?, ?, 'key-1', 'ch-1', 'm', 'chat', ?, ?, 0, ?, 1, 1, 0, \
                         '2026-09-04T00:00:00.000Z', ?, ?, 'clean', 0, 'allow', 0, ?, 1)",
            )
            .bind(format!("id-{seq}"))
            .bind(seq)
            .bind(status)
            .bind(prompt)
            .bind(total)
            .bind(body.to_string())
            .bind((status == 499).then_some("client_cancelled"))
            .bind(if status == 499 { 1 } else { 0 })
            .execute(pool)
            .await
            .unwrap();
        }

        // seq 1：被误记的 499；seq 2：同会话下一轮，历史里多出了 seq 1 那轮的回复
        insert(
            &pool,
            1,
            499,
            0,
            0,
            json!({"messages":[{"role":"user","content":"hi"}]}),
        )
        .await;
        insert(
            &pool,
            2,
            200,
            1000,
            1010,
            json!({"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"hello there"}]}),
        )
        .await;
        // seq 3：499，下一轮（seq 4）不是它的延续（历史前缀对不上）→ 无证据，
        // 保持 499，但上游确实消耗过的 prompt 仍要补记
        insert(
            &pool,
            3,
            499,
            0,
            0,
            json!({"messages":[{"role":"user","content":"other session"}]}),
        )
        .await;
        insert(
            &pool,
            4,
            200,
            900,
            905,
            json!({"messages":[{"role":"user","content":"completely unrelated"}]}),
        )
        .await;

        // ── dry-run：不得改动任何行 ──
        let dry = repair_pool(
            &pool,
            RepairInput {
                apply: Some(false),
                limit: Some(10),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(dry.dry_run);
        assert_eq!(dry.scanned, 2, "两条 499 都应被扫到");
        assert_eq!(dry.reclassified, 1, "只有有证据的那条判定为成功交付");
        assert_eq!(dry.kept_cancelled, 1);
        assert_eq!(dry.response_choices_restored, 1);
        assert!(dry.prompt_tokens_added > 0);
        let still_499: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE status_code=499")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(still_499, 2, "dry-run 不能改状态");
        let marked: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM request_logs WHERE usage_source IS NOT NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(marked, 0, "dry-run 不能打标记");

        // ── apply ──
        let run = repair_pool(
            &pool,
            RepairInput {
                apply: Some(true),
                limit: Some(10),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(!run.dry_run);
        assert_eq!(run.reclassified, 1);
        assert_eq!(run.remaining, 0, "apply 之后必须收敛到 0");

        let repaired: (i64, i64, i64, i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status_code, prompt_tokens, completion_tokens, client_cancelled, \
             error_message, usage_source FROM request_logs WHERE seq = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(repaired.0, 200, "有证据的行应改判 200");
        assert_eq!(repaired.3, 0, "client_cancelled 必须清零");
        assert_eq!(repaired.4, None, "error_message 必须清空");
        assert_eq!(repaired.5.as_deref(), Some("repaired"), "必须留痕");
        // 参照行 prompt=1000、字节比落在窗口内 → 插值；completion 由恢复出的回复算出
        assert!(repaired.1 > 0, "prompt 必须补上");
        assert!(repaired.2 > 0, "completion 必须由恢复出的回复算出");

        let choices: Option<String> =
            sqlx::query_scalar("SELECT response_choices FROM request_logs WHERE seq = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let choices = choices.expect("response_choices 应被恢复");
        assert!(choices.contains("hello there"), "{choices}");

        // 无证据的 seq 3：保持 499，但 prompt 补记、同样留痕
        let kept: (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT status_code, prompt_tokens, usage_source FROM request_logs WHERE seq = 3",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kept.0, 499, "无证据的行不能改判成功");
        assert!(kept.1 > 0, "上游确实消耗了 prompt，必须记上");
        assert_eq!(kept.2.as_deref(), Some("repaired"));

        // 参照行本身不受影响
        let untouched: (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT prompt_tokens, total_tokens, usage_source FROM request_logs WHERE seq = 2",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(untouched.0, 1000);
        assert_eq!(untouched.2, None, "非 499 行不该被碰");

        // ── 幂等：再跑一次什么都不做 ──
        let again = repair_pool(
            &pool,
            RepairInput {
                apply: Some(true),
                limit: Some(10),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(again.scanned, 0);
        assert_eq!(again.remaining, 0);

        pool.close().await;
    }
}
