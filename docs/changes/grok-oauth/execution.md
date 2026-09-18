# Grok OAuth provider — execution

状态：IMPLEMENTED（FIX_REVIEW）

执行仓库：`WaLiAPI`
分支：`feat/grok-oauth`
HEAD：`0b24811`（`docs: plan Grok OAuth provider`，父提交为 packet 基线 `540dfb8`）
未执行 commit / push / rebase / checkout / reset / clean。

## 实现摘要

新增独立 `grok` OAuth provider，协议对齐公开 CLIProxyAPI `internal/auth/xai`：

- OIDC discovery：`https://auth.x.ai/.well-known/openid-configuration`
- RFC 8628 device authorization；公开 client ID，无 client secret
- 发现到的 OAuth 端点仅接受 HTTPS 且 host 为 `x.ai` 或其子域
- OAuth 出站固定 `https://cli-chat-proxy.grok.com/v1`，复用现有 Responses codec / `ForcedResponsesSse`
- 登录方式 `device_code`；不提供 API-key 导入/导出、localhost callback、任意上游 URL

参考实现的 HTTP chat 路径为 `POST {cli-chat-proxy}/responses`，与 packet「固定 cli-chat-proxy 上游 + 复用 OpenAI Chat/Responses codec」一致，未扩大 scope。

## FIX_REVIEW（四个 MASTER findings）

1. **Replacement 身份 fail-closed**：不再把新 token 的 `account_id` 覆盖成旧账号 id。新 token 的 subject/account_id 必须与 `replacement.provider_account_id` 一致，否则 `InvalidPayload`。测试：`replacement_login_keeps_matching_account_id`、`replacement_login_fails_closed_on_account_id_mismatch`。
2. **Grok route profile 仅 `responses`**：`profile_for_model_state` 从 `("grok", _)` 改为 `("grok", "responses")`。未知/空 protocol 不路由。测试：`grok_unknown_protocol_fails_closed_no_candidate`。
3. **删除重复 `#[test]`**：`grok_profile_is_responses_with_forced_sse_and_fixed_upstream` 上多余的属性已去掉。
4. **`verification_url` 必须是 HTTPS 且 host 为 `x.ai` 或其子域**：展示/打开前走与 discovery 相同的 endpoint 校验。测试：accept/reject 单测 + `login_rejects_non_xai_verification_url`。

## 变更文件

新增：

- `src-tauri/src/auth_provider/grok_login.rs`
- `src-tauri/src/auth_provider/grok_backend.rs`
- `docs/changes/grok-oauth/execution.md`

修改：

- `src-tauri/src/auth_provider/mod.rs`
- `src-tauri/src/auth_provider/spec.rs`
- `src-tauri/src/auth_provider/types.rs`
- `src-tauri/src/commands/auth.rs`
- `src-tauri/src/core/route_plan.rs`
- `src-tauri/src/core/attempt.rs`（注释）
- `src-tauri/src/security/redact.rs`
- `src/types/index.ts`
- `src/components/auth/ProviderPills.tsx`
- `src/components/auth/LoginModal.tsx`
- `src/components/auth/AccountCard.tsx`
- `src/components/auth/AccountList.tsx`
- `src/pages/AuthChannelsPage.tsx`

无数据库 migration。

## 预先存在的 dirty files

packet 写「12 个」；工作区实测 **13** 个预先存在的无关 Rust 修改。SHA-256 在执行前后一致，未暂存、未改写：

- `src-tauri/src/bin/waliapi-web.rs`
- `src-tauri/src/commands/app_config.rs`
- `src-tauri/src/db/repository.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/src/protocol/legacy/responses_decode.rs`
- `src-tauri/src/rollout_integration_tests.rs`
- `src-tauri/src/server/router.rs`
- `src-tauri/src/services/channel_test.rs`
- `src-tauri/src/services/knowledge/repository.rs`
- `src-tauri/src/services/mcp/handlers.rs`
- `src-tauri/tests/auth_repository.rs`
- `src-tauri/tests/kb_ocr.rs`
- `src-tauri/tests/request_log.rs`

## 验证命令与结果

均在 `WaLiAPI` 下执行。Rust 测试 cwd 为 `src-tauri/`。

| 命令 | 首次实现 | FIX_REVIEW |
| --- | --- | --- |
| `cargo test grok -- --nocapture` | 43 passed | 47 passed, 997 filtered out |
| `cargo test auth_provider -- --nocapture` | 155 passed | 159 passed, 885 filtered out |
| `cargo test auth -- --nocapture` | 237 passed | 241 passed, 803 filtered out |
| 变更 Rust 文件 `rustfmt --check` | 通过 | 通过（`grok_login.rs` / `grok_backend.rs` / `route_plan.rs`） |
| `cargo fmt --check --manifest-path src-tauri/Cargo.toml` | **失败（预先存在）** `endpoint_executor/mod.rs:1013` | FIX_REVIEW 未再跑全树 fmt；该预先差异未改 |
| `git diff --check HEAD --` | 通过 | 通过（exit 0） |
| `pnpm build` | 通过 | 通过：`tsc && vite build` |

FIX_REVIEW 额外 mock 覆盖：replacement 身份匹配/不匹配、Grok 未知 protocol 不路由、verification URL 非 xAI/非 HTTPS 拒绝。测试未访问生产 xAI。13 个预先 dirty 文件 SHA-256 仍一致。

## 偏差

无。FIX_REVIEW 仅改四个 findings，未实现 localhost callback、API-key 导入、任意上游 URL。OAuth client ID 为公开 device-flow 材料；测试 token / email / subject 均为占位值。

## 失败

- `cargo fmt --check`：预先存在的 `endpoint_executor/mod.rs` 格式差异。本任务实现文件已格式化。

## 未验证

- 真实 xAI OIDC / device 授权页 / cli-chat-proxy 生产 HTTP（packet 禁止访问生产 OAuth/API）
- 桌面端真实浏览器打开与 headless Web 面板的端到端人工登录
- `pnpm --filter waliapi-web build`（packet 未要求；Web 面板通过 `@app` 复用已 `tsc` 的 `src/`）
