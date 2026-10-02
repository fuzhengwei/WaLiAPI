# Codex 重置卡：验收 Spec

**版本**：v1.0
**日期**：2026-09-30
**验收对象**：WaLiAPI Codex Auth 账号重置卡能力

## 1. 验收前置条件

- 数据库已执行迁移 043。
- 自动化测试使用 mock fixture；本次真实验收账号按 Xerina 最新指定为 `2867406171@qq.com`。
- 真实验收目标卡固定为原始 `expires_at` 对应 2026-10-04 的 `available`、`codex_rate_limits` 卡。
- 测试记录不得包含 token、Cookie、Authorization header、完整卡 ID 或完整上游响应正文。

## 2. 功能验收

| ID | Given / When | Then | 验证方式 |
|---|---|---|---|
| AC-R01 | 有效 Codex 账号 A 打开入口 | 只查询账号 A，显示 available count 和安全卡摘要 | provider mock + UI 测试 |
| AC-R02 | 页面同时有 A、B 和其他 provider | A 的卡不出现在 B；非 Codex 不显示入口 | service/组件测试 |
| AC-R03 | 卡状态非 available、类型错误或已过期 | 卡不可提交；服务端 POST 前拒绝 | service mock 断言 POST=0 |
| AC-R04 | 用户未确认或取消弹窗 | 不发送消费 POST，不改变 quota 和操作状态 | component test |
| AC-R05 | 用户选择 A 的卡并确认 | POST 精确路径，body 含非空 redeem_request_id 和目标 credit_id，并带 A 的认证头 | HTTP contract test |
| AC-R06 | 上游返回 reset | 显示成功，持久化操作，刷新 A 的 quota 和卡列表 | service + UI integration |
| AC-R07 | 上游返回 already_redeemed | 沿用同一幂等操作回读，不生成新消费请求 | idempotency test |
| AC-R08 | 上游返回 nothing_to_reset | 显示无可重置窗口，不显示成功 | result mapping test |
| AC-R09 | 上游返回 no_credit | 显示无可用卡并刷新列表，不显示成功 | result mapping test |
| AC-R10 | POST 超时或连接中断 | 操作进入 pending/unknown，恢复只能沿用原幂等键 | restart/retry test |
| AC-R11 | quota 回读失败 | 保留已确认消费和旧 quota_json，提示稍后刷新 | service integration |
| AC-R12 | 查询或消费接口 404/协议漂移/持续 5xx | 提供固定官方 Usage URL，现有额度刷新仍可用 | command/UI test |
| AC-R13 | 按产品图 2 操作 | 账号列表重置入口、右侧选择卡弹窗、单选卡片、底部提示、确认使用和结果状态与 `assets/reset-card-flow-2.png` 的布局和文案语义一致 | 浏览器手动 smoke test |

## 3. 安全验收

| ID | 条件 | 必须满足 |
|---|---|---|
| SEC-R01 | 非 Codex、停用、失效、删除账号 | 发网前返回 capability/account error |
| SEC-R02 | 前端提交任意 account ID + credit ID | 服务端重新拉取卡列表并校验归属，跨账号消费失败 |
| SEC-R03 | 重复点击或并发请求 | 同账号只有一个上游消费逻辑，不同账号互不阻塞 |
| SEC-R04 | 日志、事件、错误、DTO、URL | 不出现 token、Cookie、Authorization header、完整卡 ID、邮箱或上游响应正文 |
| SEC-R05 | 官方 fallback | 必须精确为 `https://chatgpt.com/codex/settings/usage`，无 query 和 fragment |
| SEC-R06 | Unix 时间处理 | 过期判断使用原始秒；本地化仅用于显示 |

## 4. 接口契约验收

### 查询

```text
GET https://chatgpt.com/backend-api/wham/rate-limit-reset-credits
```

必须验证：

- `Authorization: Bearer <account access token>`；
- `chatgpt-account-id: <account.account_id>`；
- Codex `originator` 和 User-Agent；
- 不转发调用方 Authorization、Cookie 或任意客户端 actor header；
- 解析 `available_count`、`credits[]` 和 Unix 时间。

### 消费

```text
POST https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume
Content-Type: application/json

{
  "redeem_request_id": "<uuid>",
  "credit_id": "<selected-credit-id>"
}
```

必须验证：

- `redeem_request_id` 非空、持久化且重试不变；
- `credit_id` 来自目标账号最新列表并通过状态、类型、有效期校验；
- 四类 code 精确映射；未知 code 安全失败；
- 401/403/429/5xx/超时不会清空额度或误报成功。

## 5. 数据和回滚验收

- 迁移 043 可在旧数据库上执行，重复启动不重复建表。
- `auth_reset_operations` 能恢复 pending/unknown 操作，唯一约束阻止同账号同幂等键重复创建。
- 消费操作不写入 `quota_json`；只有现有额度查询成功才更新 quota。
- 接口异常时不发送新的重复消费请求，历史操作记录保留，官方 fallback 可用。
- 回滚演练不删除账号、不清空 quota、不生成新幂等键、不重复消费。

## 6. 自动化门禁

以下必要门禁必须通过：

```bash
cd src-tauri
cargo fmt --check
cargo test auth_provider::codex_backend::tests
cargo test auth_provider::service::tests
cd ..
pnpm build
```

新增测试至少包括：

- `codex_backend` mock contract；
- `auth_provider::service` 并发、幂等、账号隔离、043 迁移和 quota 回读；
- Tauri command/Web admin 与 React 三态弹窗的必要 smoke test。

## 7. 真实账号 smoke test

本次唯一真实验收账号为 `2867406171@qq.com`。图片原型中的其他账号、卡号和日期都是视觉示例，不得替代该账号或扩大测试范围。

真实验收前置锁定：

- 目标账号：`2867406171@qq.com`；
- 目标卡：原始 `expires_at` 按 UTC 对应 2026-10-04；
- 消费次数：整个验收流程最多一次真实 POST；
- 禁止行为：重试、换卡、并行调用、再次验证消费、使用其他账号消费。

执行步骤：

1. 只读查询并记录可用数量和脱敏时间；
2. 确认目标列表中存在且仅选择 2026-10-04 到期的可用卡；日期不匹配或无法唯一确认时停止；
3. 确认一次真实消费，并在发送前记录“本次验收已消耗唯一机会”；
4. 验证上游返回 `reset`、可用卡数量减少；
5. 验证 `auth_refresh_quota` 能读到新的额度窗口；
6. 验证同一账号同一操作不能再次发送重复消费请求。

真实验收的 POST 如果超时或结果未知，必须停止后续消费，记录为验收失败/待人工核对，不得新建幂等键重试。

真实测试结果必须与 mock 测试分开记录。若接口返回未知结果、超时或回读失败，发布判定为未通过，不得以 UI 显示“成功”替代远端证据。

## 8. 开发验收判定

只有同时满足以下条件才能认定开发完成：

- AC-R01 至 AC-R13 全部通过；
- SEC-R01 至 SEC-R06 全部通过；
- 自动化门禁全部通过；
- 真实账号 smoke test 通过；
- 必要回归完成并能恢复官方 Usage fallback。

当前实现按本次范围只保证当前任务的状态和幂等：前端会生成并传递 `operationId`，未知结果后锁定任务且不自动重试；不增加历史结果查询或历史操作页面。完整回归矩阵仍待补跑。

## 9. 当前验收记录

- 自动化门禁：通过 `cargo test --no-fail-fast`（1149 个库测试及全部集成测试）、Codex provider reset contract、AuthService 聚焦测试和 `pnpm build`。
- `cargo fmt --check`：被未改动的基线文件 `src-tauri/src/endpoint_executor/grok_arguments.rs` 格式差异阻塞。
- `2867406171@qq.com` 只读列表：通过；共 3 张可用卡，唯一目标卡为 UTC 2026-10-04 到期的 `codex_rate_limits` 卡。
- 2026-10-02 修正代理连接池后，桌面端 R 入口只读查询成功列出 3 张卡，唯一一次确认使用 UTC 2026-10-04 到期卡。
- 上游返回 `reset`，额度回读状态为 `refreshed`，前端显示“额度已重置”；未重试、未换卡、未再次消费。
