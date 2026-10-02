# Codex 重置卡：架构决策冻结

**状态**：已落地实现稿（2026-10-02 复核）
**日期**：2026-10-02
**范围**：WaLiAPI 已登录 Codex Auth 账号的重置卡查询、选择、消费、额度回读与官方页面降级。

本文件是本功能开发的架构约束。实现过程中若必须改变任一决策，应先更新本文件和相关验收条件，再修改代码。

## 1. 现有架构基线

| 层 | 现有位置 | 本功能的接入点 |
|---|---|---|
| Codex 上游适配器 | `src-tauri/src/auth_provider/codex_backend.rs` | 固定 ChatGPT backend-api 基址、认证头、重置卡 DTO 与 HTTP 调用 |
| Provider 抽象 | `src-tauri/src/auth_provider/mod.rs` | 增加可选重置卡能力，其他 provider 默认不支持 |
| 账号编排 | `src-tauri/src/auth_provider/service.rs` | 账号校验、并发锁、幂等消费、额度回读 |
| 账号数据 | `src-tauri/src/db/models.rs`、`repository.rs` | 重置操作状态持久化；不把一次性操作写入 `quota_json` |
| Tauri 命令 | `src-tauri/src/commands/auth.rs`、`src-tauri/src/lib.rs` | 查询与消费命令 |
| Web 管理面 | `src-tauri/src/server/admin_routes.rs` | 与 Tauri 相同命令名的 invoke 分发 |
| 前端传输 | `src/lib/runtime.ts`、`src/lib/api.ts` | 统一 IPC / `/admin/api/invoke` 访问 |
| Auth 页面 | `src/pages/AuthChannelsPage.tsx`、`src/components/auth/AccountCard.tsx` | 入口、弹窗、结果和额度刷新 |

现有账号模型沿用 `docs/auth-codex/ADRs.md` 的通用账号列 + `payload_json` provider 载荷设计。不得新增 Codex 专属令牌列，也不得把重置卡状态塞进账号额度 JSON。

## 2. 决策

### ADR-R1：原生接口优先，官方页面降级

生产主路径调用 Codex 官方客户端已经使用的：

```text
GET  https://chatgpt.com/backend-api/wham/rate-limit-reset-credits
POST https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume
```

这些是 ChatGPT backend-api 的客户端契约，不是公开稳定的 OpenAI Platform API。接口返回 404、协议漂移、持续 5xx 或资格不明时，返回固定官方页面：

```text
https://chatgpt.com/codex/settings/usage
```

官方页面只作为用户人工核对和降级入口，不由 WaLiAPI 自动点击，不注入 OAuth token，不读取浏览器 Cookie。

### ADR-R2：能力挂在 Codex provider，不污染通用 provider 语义

在 `Provider` 中增加可选的 `list_reset_credits`、`consume_reset_credit` 能力，默认返回 `UnsupportedFeatures`；`CodexProvider` 实现实际 HTTP。`ProviderSpec` 增加 `supports_reset_credit`，仅 `codex=true`。

这样保持 `ProviderRegistry`、账号路由和后续 Claude/Kimi/Antigravity 扩展的通用边界；命令层不得直接创建 ChatGPT HTTP 请求。

### ADR-R3：账号边界由服务端再次确认

前端只传本地 `account_id` 和用户在该账号列表中选择的卡标识。服务端消费前必须：

1. 读取目标账号最新记录；
2. 校验 provider 为 Codex、账号未删除、未停用、状态有效；
3. 用该账号 OAuth payload 查询最新卡列表；
4. 校验卡属于该账号、`status=available`、`reset_type=codex_rate_limits`、`expires_at` 尚未过期；
5. 再发送消费 POST。

卡标识不得放入 URL、日志或跨账号缓存。错误时要求重新查询，不猜测卡序号。

### ADR-R4：消费是一次性副作用，必须独立幂等

新增 `auth_reset_operations` 表，迁移编号顺延为 `045`。每次确认创建一个逻辑操作和唯一 `redeem_request_id`，保存 `pending` 后才发送上游请求。

唯一键为 `(account_id, redeem_request_id)`。网络超时或应用退出后的恢复只能复用原幂等键；禁止因为结果未知而生成新键或自动换卡。每账号消费使用独立互斥锁，不阻塞其他账号。

### ADR-R5：额度状态仍由现有 quota 链路负责

`reset` 或 `already_redeemed` 确认后，调用现有 `AuthService::refresh_quota` / `GET /backend-api/wham/usage`，使用原有 `QuotaState`、`reset_at` 和持久化语义。消费成功但额度回读失败时：

- 保留已确认的重置操作；
- 保留旧 `quota_json`；
- 返回“消费已确认，额度待刷新”；
- 不通过本地清零伪造重置。

### ADR-R6：安全摘要跨越命令边界

命令和前端 DTO 只包含可用数量、卡摘要、状态、类型、标题、描述、Unix 时间及安全错误类别。绝不返回或记录 `payload_json`、access token、refresh token、id token、Cookie、Authorization header、完整上游响应正文。

卡 ID 属于账号权益标识，短期只在查询结果和同账号消费请求中使用；日志只允许保存单向哈希。

### ADR-R7：直接接入，不设计灰度发布

本功能按现有 Auth 架构直接接入，不新增灰度发布、分批放量或独立 feature flag。能力入口由 provider capability、账号状态和上游响应决定。接口不可用、资格不明或协议漂移时，当前操作安全失败并提供官方 Usage fallback；原有登录、令牌刷新、额度查询、模型同步和 `/v1/*` 路由继续工作。

回滚只需要停止新增消费命令或恢复上一版本代码，不删除 pending 操作，不修改已有 quota 缓存。

### ADR-R8：时间和结果语义

上游 `granted_at`、`expires_at` 当前以字符串传输，兼容 Unix 秒字符串、数字和 RFC3339 字符串。服务端按 UTC 比较原始时间，前端按用户本地时区展示完整日期时间。未知 code 不当作成功；`already_redeemed` 只在同一幂等操作上下文中视为已完成。

## 3. 允许修改的文件边界

### 后端

- `src-tauri/src/auth_provider/codex_backend.rs`
- `src-tauri/src/auth_provider/mod.rs`
- `src-tauri/src/auth_provider/spec.rs`
- `src-tauri/src/auth_provider/service.rs`
- `src-tauri/src/auth_provider/types.rs`（仅安全 DTO / 错误类型需要时）
- `src-tauri/src/db/models.rs`
- `src-tauri/src/db/repository.rs`
- `src-tauri/migrations/045_auth_reset_operations.sql`
- `src-tauri/src/commands/auth.rs`
- `src-tauri/src/server/admin_routes.rs`
- `src-tauri/src/lib.rs`
- 相关 `src-tauri/tests/` 或模块内测试

### 前端

- `src/types/index.ts`
- `src/lib/api.ts`
- `src/pages/AuthChannelsPage.tsx`
- `src/components/auth/AccountCard.tsx`
- 新增 `src/components/auth/ResetCreditDialog.tsx`

不得为本功能创建第二套 HTTP transport、第二套额度解析器或第二套账号凭证存储。

## 4. 完成定义

- 代码路径覆盖桌面 Tauri 和 headless Web 管理面。
- Mock 契约覆盖四类业务结果、认证头、错误、超时和幂等恢复。
- 账号隔离、消费前二次校验和敏感字段脱敏有自动化测试。
- `cargo fmt --check`、聚焦 Rust 测试、`cargo test`、`pnpm build` 通过。
- 验收 Spec 的 provider、服务端幂等和构建门禁已通过；前端未知结果恢复入口与完整回归矩阵仍是未闭合项。
