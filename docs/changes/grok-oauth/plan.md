# Grok OAuth provider

状态：VERIFIED

## 审核修复范围

首次实现已完成，但 MASTER 审核发现以下 bounded findings，必须在提交前修复：

1. replacement 登录不得直接覆盖新账号身份；必须校验新 token 的 account/subject 与原账号一致，不一致时 fail closed。
2. Grok route profile 只允许 `responses` protocol；未知 protocol 必须不路由。
3. 删除重复的 `#[test]` 属性。
4. `verification_url` 仅允许 HTTPS 且 host 为 `x.ai` 或其子域，避免打开不可信 URL，并补充测试。

## 目标

为 WaLiAPI 新增独立的 `grok` OAuth provider，使用 xAI 官方 Grok CLI 兼容的 OAuth 设备授权流程，让桌面端用户可以授权、刷新令牌、同步模型，并通过现有 auth-account 路由调用 Grok。

本任务不修改现有 Codex、Kimi 或 Antigravity/Gemini 的行为。

## 非目标

- 不实现 xAI API key 登录或导入。
- 不实现 localhost callback；xAI OAuth 使用 RFC 8628 device authorization。
- 不提交 OAuth client secret、用户 token、authorization code 或真实账号信息。
- 不新增数据库 migration；provider 字符串沿用现有扩展方式。
- 不引入与 Grok 无关的通用重构。

## 当前行为与证据

- 基线为 `main` 提交 `540dfb8`；当前工作区存在 12 个预先存在的无关格式化修改，执行器不得覆盖、暂存或提交这些文件。
- `ProviderKind`、`ProviderSpec`、`ProviderRegistry`、`AuthService` 已提供 provider 注册、设备授权登录、令牌刷新、模型同步和出站请求边界。
- Kimi 已提供可复用的 Device Authorization UI/runtime 进度契约；Codex 提供 OAuth token 生命周期和 OpenAI-compatible route 参考。
- 公开 CLIProxyAPI xAI 参考实现确认：issuer 为 `https://auth.x.ai`，通过 `/.well-known/openid-configuration` 发现 device authorization/token endpoint，使用 RFC 8628 device code，scope 包含 `openid profile email offline_access grok-cli:access api:access`，OAuth 请求使用公开 client ID，不需要仓库内 client secret；OAuth 上游默认使用 `https://cli-chat-proxy.grok.com/v1`。

## 已批准设计

1. 新增 `ProviderKind::Grok`，canonical provider string 为 `grok`。
2. 新增 `GrokProvider` 与 `GrokLogin`，遵循现有 `Provider`/`LoginRuntime`/`ProviderPayload` 边界。
3. 登录流程：
   - 从固定的 xAI issuer discovery URL 获取 OAuth endpoint；只接受 HTTPS 且 host 为 `x.ai` 或其子域，防止 discovery 劫持。
   - 请求 device code，展示 verification URL、可选完整 URL 和 user code。
   - 轮询 token endpoint，遵循服务端 interval、expires_in、authorization_pending、slow_down、access_denied、expired 等状态，并支持取消和总超时。
   - 保存 access token、refresh token、可选 id token、过期时间、email/subject 和 token endpoint 等 provider-owned payload；任何 Debug、日志、DTO 和错误消息不得输出 token 或原始 OAuth 响应。
4. 刷新流程使用保存的 token endpoint（缺失时重新 discovery），只接受 xAI endpoint，处理 refresh token rotation 和 retry/unauthorized 分类；旧格式或缺少必要字段必须 fail closed。
5. 出站和模型：
   - OAuth 账号使用固定 `https://cli-chat-proxy.grok.com/v1` 上游，不接受请求方覆盖 base URL、Authorization 或身份头。
   - 复用现有 OpenAI Chat/Responses route/codec；仅在参考协议确实需要时增加最小 Grok wire profile，不扩展为任意 URL 代理。
   - 通过固定上游 `/models` 或参考实现等价的模型接口同步模型，并对模型列表做安全解析。
6. UI/能力表显示名称为 `Grok`，登录方式为 `device_code`，不提供 API-key import/export；没有可靠官方 quota endpoint 时 `supports_quota=false`。
7. OAuth client ID 属于公开 device-flow client material，可以按公开参考实现保留；禁止新增 client secret。所有测试 credential、token、账号信息均为占位值。
8. 添加本地 mock 测试覆盖 discovery endpoint 校验、device code、poll pending/slow_down/success/deny/expiry/cancel、refresh rotation、payload redaction/fail-closed、registry/spec、模型同步和出站 allowlist。

## 预期受影响文件

- `src-tauri/src/auth_provider/grok_login.rs`（新增）
- `src-tauri/src/auth_provider/grok_backend.rs`（新增）
- `src-tauri/src/auth_provider/mod.rs`
- `src-tauri/src/auth_provider/spec.rs`
- `src-tauri/src/auth_provider/types.rs`
- `src-tauri/src/auth_provider/service.rs` 或必要的 provider 路由接线
- `src-tauri/src/core/route_plan.rs`、协议注册或模型同步接线（仅必要范围）
- `src-tauri/src/commands/auth.rs`、`src/lib/api.ts`（仅必要的 provider 白名单/DTO 接线）
- `src/components/auth/*`、`src/pages/AuthChannelsPage.tsx`、`src/types/index.ts`
- `docs/changes/grok-oauth/execution.md`

不要修改当前工作区中预先存在的 12 个格式化脏文件。

## 实施步骤

1. 阅读仓库 instructions 和本任务包，确认基线与脏文件边界。
2. 对照 Kimi device OAuth、Codex token 生命周期和现有 route/codec，新增 Grok login/backend。
3. 完成 provider kind/spec/registry、命令白名单、模型同步、UI 展示和类型接线。
4. 为所有 OAuth HTTP 交互编写本地 mock 测试；不得访问生产 xAI OAuth 或 API。
5. 运行下方验证命令，记录到 `execution.md`，报告失败和未验证项。
6. 不执行 commit、push、rebase、checkout 或 destructive Git 操作；提交和 PR 发布由 MASTER 完成。

## 验收标准

- `grok` provider 能在 renderer-safe provider 列表中显示为 Grok。
- 桌面端可以通过 device code 完成登录；取消、超时、拒绝、错误 state/endpoint 都安全失败。
- token refresh 能处理 rotation，旧凭据和非 xAI endpoint fail closed。
- Grok OAuth 账号能同步模型，并仅使用固定可信上游完成 Chat/Responses 请求。
- token、secret、device code、authorization code 不出现在日志、Debug、DTO、错误文本或提交内容中。
- 现有 Codex/Kimi/Antigravity 相关测试与行为不回归。
- 无数据库 migration，未引入任意上游 URL 代理能力。

## 验证命令

```bash
cargo test grok -- --nocapture
cargo test auth_provider -- --nocapture
cargo test auth -- --nocapture
cargo fmt --check --manifest-path src-tauri/Cargo.toml
git diff --check HEAD --
pnpm build
```

## 兼容性与失败处理

- 保持 provider payload 私有和可演进；不读取旧 provider payload 作为 Grok 凭据。
- xAI discovery、OAuth endpoint、token response 或模型 response 结构不符合安全约束时 fail closed。
- 如果公开 xAI OAuth 参考实现与本设计的固定上游/协议不一致，执行器必须停止并报告，不得自行扩大 scope。
- 失败登录不得写入数据库；刷新失败沿用现有 unauthorized/retryable 状态语义。

## 基线与未决问题

- 基线提交：`540dfb8` (`main` / `origin/main`)。
- 预先存在的 dirty files：见 `git status --short`，共 12 个 Rust 文件；不得纳入本任务。
- 未决问题：无阻塞性问题。若 xAI 官方实际端点/响应与公开参考不一致，按“停止并报告”处理。

## 最终审核结论

四项审核发现均已修复；目标测试、变更 Rust 文件格式检查、前端构建和 diff 检查均通过。13 个预先存在的无关 dirty 文件未纳入变更。
