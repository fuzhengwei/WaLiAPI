# Codex 重置卡：实现设计与落地记录

**日期**：2026-10-02
**实现基准**：[00-architecture-decisions.md](00-architecture-decisions.md)
**当前状态**：核心链路已落地；以 [07-implementation-review.md](07-implementation-review.md) 的复核结论为准。

## 1. 端到端链路

```text
AccountCard（账号 A）
  └─ authApi.listResetCredits(A)
       └─ auth_list_reset_credits
            └─ AuthService::list_reset_credits(A)
                 └─ CodexProvider::list_reset_credits
                      └─ GET /backend-api/wham/rate-limit-reset-credits

ResetCreditDialog 右侧选择卡 C + 底部确认
  └─ authApi.consumeResetCredit(A, C, operation)
       └─ auth_consume_reset_credit
            └─ AuthService::consume_reset_credit(A, C, operation)
                 ├─ 读取 A 最新账号与 payload
                 ├─ GET 卡列表并校验 C
                 ├─ 写入 auth_reset_operations.pending
                 ├─ CodexProvider::consume_reset_credit
                 │    └─ POST /backend-api/wham/rate-limit-reset-credits/consume
                 ├─ 持久化结果 code
                 ├─ reset/already_redeemed → refresh_quota(A)
                 └─ 返回安全结果 + quota 回读状态
```

## 2. 后端接口设计

### 2.1 Provider DTO

实现位于 `codex_backend.rs` 的 provider 私有解析函数，再转换成服务层安全 DTO：

```rust
CodexResetCredit {
    id: String,
    reset_type: String,
    status: String,
    granted_at: Option<String>,
    expires_at: Option<String>,
    title: Option<String>,
    description: Option<String>,
}

CodexResetCreditsSnapshot {
    available_count: Option<i64>,
    credits: Vec<CodexResetCredit>,
}

CodexResetCreditOutcome {
    code: ResetCreditCode,
    windows_reset: i64,
}
```

wire DTO 允许新增字段但不把未知字段当作成功；服务层只输出安全摘要。`expires_at` 兼容 Unix 秒和 RFC3339 字符串，禁止在 provider 层提前格式化为本地日期。

### 2.2 Provider 方法

`Provider` trait 已增加默认不支持实现：

```rust
async fn list_reset_credits(
    &self,
    account: &AuthAccount,
    payload: &ProviderPayload,
) -> Result<ResetCreditsSnapshot, ProviderError>;

async fn consume_reset_credit(
    &self,
    account: &AuthAccount,
    payload: &ProviderPayload,
    request_id: &str,
    credit_id: &str,
) -> Result<ResetCreditOutcome, ProviderError>;
```

`CodexProvider` 使用现有 `auth_headers`，保留 `Authorization`、`chatgpt-account-id`、`originator` 和 User-Agent；消费接口使用独立的非流式超时。调用方传入的 Authorization 和 actor header 不得被转发。

### 2.3 Service 方法

`AuthService` 已新增：

- `list_reset_credits(account_id)`：能力和账号状态检查后查询并转换安全 DTO；
- `consume_reset_credit(account_id, credit_id, operation_id)`：加账号锁，读取/创建 pending，重新查询卡列表，验证归属和有效期，消费并持久化结果；
- `resume_reset_operation(account_id, operation_id)`：只读取既有幂等键恢复，不自动选卡。

`AuthService::refresh_quota` 保持原签名和行为。消费成功后直接调用它，不复制 `fetch_quota`、`quota_from_usage_payload` 或 repository quota 写入逻辑。

## 3. 数据库设计

迁移 `src-tauri/migrations/045_auth_reset_operations.sql` 已落地：

```sql
CREATE TABLE auth_reset_operations (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL,
  credit_id_hash TEXT NOT NULL,
  redeem_request_id TEXT NOT NULL,
  status TEXT NOT NULL,
  upstream_code TEXT,
  error_class TEXT,
  quota_refresh_status TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE (account_id, redeem_request_id)
);
CREATE INDEX idx_auth_reset_operations_account
  ON auth_reset_operations(account_id, updated_at DESC);
```

状态由服务层写入：`pending`、`reset`、`already_redeemed`、`nothing_to_reset`、`no_credit`、`unknown`、`failed`。数据库不保存完整卡 ID；恢复命令只读取已有操作，不重放消费请求。

## 4. 命令与 Web 管理面

新增 Tauri commands：

| 命令 | 输入 | 返回 |
|---|---|---|
| `auth_list_reset_credits` | `{ id }` | 卡列表安全摘要、available count、fallback 状态 |
| `auth_consume_reset_credit` | `{ id, creditId, operationId? }` | 操作结果、quota refresh 状态、卡列表刷新提示 |
| `auth_resume_reset_operation` | `{ id, operationId }` | 原操作当前状态与可继续动作 |

命令在 `src-tauri/src/lib.rs` 注册，并在 `admin_routes.rs` 的 invoke 分发中使用相同命令名。所有 Web 请求继续使用现有管理会话和 CSRF 保护。

## 5. 前端设计

### 5.1 类型与 API

在 `src/types/index.ts` 增加 `AuthResetCredit`、`AuthResetCreditsSnapshot`、`AuthResetOperationResult`、`AuthResetCapability`。在 `src/lib/api.ts` 增加：

```ts
listResetCredits(id: string)
consumeResetCredit(id: string, creditId: string, operationId?: string)
resumeResetOperation(id: string, operationId: string)
```

只能调用 `runtime.ts` 的 `invoke`，不得直接访问 ChatGPT URL。

### 5.2 账号卡与弹窗

`AccountCard` 只负责展示入口和回调；新增 `ResetCreditDialog` 负责：

1. 加载并显示目标账号标签；
2. 展示标题、描述、获得时间、过期时间、状态和可用数量；
3. 用原始时间值过滤可提交卡；
4. 二次确认并显示不可逆提示；
5. 消费期间禁用按钮；
6. 显示四类结果、待确认状态和 fallback；
7. 成功后刷新账号卡片、额度和重置卡列表。

视觉复原基准为本目录的 [产品图 2](assets/reset-card-flow-2.png)。实际 React 组件使用项目现有 Tailwind、lucide-react 和样式令牌实现右侧大弹窗、背景列表、卡片单选、底部提示和确认按钮；后端接口、消费前二次查询、幂等和额度回读保持不变。

其他 provider、API Key、无效/停用账号不渲染入口。账号 A 的弹窗状态不能由账号 B 的刷新结果覆盖。

## 6. 分阶段开发

### 阶段 0：契约冻结

- 新增 provider DTO、结果枚举和 capability。
- 固定请求路径、认证头 allowlist、超时和未知 code 处理。
- 完成 provider mock contract 后再进入数据库实现。

### 阶段 1：数据与服务

- 新增 043 迁移、models 和 Repository 操作。
- 实现账号校验、消费锁、幂等 pending、结果持久化。
- 接通消费后的现有 quota 回读。

### 阶段 2：命令和管理面

- 注册 Tauri commands。
- 增加 Web admin invoke 分发。
- 完成安全 DTO 和错误分类。

### 阶段 3：前端闭环

- 增加 TypeScript 类型和 API 封装。
- 完成 AccountCard 入口、ResetCreditDialog 和结果状态。
- 接入官方 Usage fallback 与手动刷新额度。

### 阶段 4：必要测试和回归

- 完成 Rust provider/service/repository/command 测试和前端构建。
- 执行必要的 provider/service/command 和前端回归，确认原功能不受影响。
- 指定账号按验收 Spec 先完成卡列表核对，再只做一次明确卡消费 smoke test。
- 测试通过后直接进入开发版本验收，不设计灰度发布流程。

## 7. 回滚

1. 停止新增消费命令或恢复上一版本代码。
2. 保留 `auth_reset_operations` 记录，禁止重复消费。
3. 入口显示官方 Usage 链接。
4. 保持 `auth_refresh_quota`、模型同步、令牌刷新和路由不变。
5. 消费已确认但额度未回读的操作不回滚远端结果，只允许稍后使用原账号刷新。
