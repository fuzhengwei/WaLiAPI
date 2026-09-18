# Gemini → Antigravity OAuth 执行记录

状态：VERIFIED
日期：2026-09-18
基线：`540dfb8ddf3b02ef786016262b8b14ece70bb91d`

## 实施结果

- 内部 Gemini provider ID 和数据库兼容标识保持不变，用户界面与认证端迁移为 Antigravity OAuth；线协议仍使用 `gemini`。
- OAuth callback 使用 `localhost:<port>/oauth-callback`，严格校验 `state`；回调成功后继续执行 token exchange、账号保存与模型同步，不再提前宣称登录完成。
- 新凭据使用 v2/Antigravity marker；旧 Gemini CLI 凭据在 refresh/outbound 前被拒绝，并要求交互式重新登录。
- Code Assist 上游切换到 daily-cloudcode，使用 Antigravity metadata/User-Agent，并支持个人账号 free-tier onboarding、验证 URL 和明确权限错误。
- 模型同步改为 `fetchAvailableModels` 的服务端目录；无模型的成功响应按协议错误处理。
- Gemini CLI 凭据导入入口关闭；UI 显示 Antigravity，内部仍使用 `gemini`，且仅支持浏览器回调登录。

## 变更文件

- `src-tauri/src/auth_provider/gemini_login.rs`
- `src-tauri/src/auth_provider/gemini_backend.rs`
- `src-tauri/src/auth_provider/spec.rs`
- `src-tauri/src/auth_provider/types.rs`
- `src-tauri/src/auth_provider/service.rs`
- `src-tauri/src/commands/auth.rs`
- `src-tauri/src/endpoint_executor/integration_tests.rs`
- `src/lib/api.ts`
- `src/pages/AuthChannelsPage.tsx`
- `src/components/auth/LoginModal.tsx`
- `docs/changes/gemini-antigravity-oauth/tasks.md`
- `docs/changes/gemini-antigravity-oauth/execution.md`

## 验证证据

- `cargo test gemini -- --nocapture`：50 passed，0 failed。
- `cargo test auth -- --nocapture`：222 passed，0 failed。
- `cargo test protocol -- --nocapture`：228 passed，0 failed。
- `cargo test route_plan -- --nocapture`：53 passed，0 failed。
- `cargo fmt --check --manifest-path src-tauri/Cargo.toml`：通过。
- `git diff --check HEAD --`：通过。
- `pnpm build`：通过；仅有既有 Vite 动态/静态 import 和 chunk size warning。
- `pnpm tauri build`：Rust release 编译成功（10m13s），成功生成 `.app`、`.dmg` 和 updater 压缩包；随后因未配置 `TAURI_SIGNING_PRIVATE_KEY` 在 updater 发布签名阶段返回非零。
- 本地测试用 `.app` 已执行 ad-hoc 签名；`codesign --verify --deep --strict` 通过。
- 已安装到 `/Applications/WaLiAPI.app`，版本 `0.3.3`，bundle ID `waliapi.xiaofuge.cn`，arm64。
- 安装后的 `waliapi` 与 `waliapi-web` 二进制 SHA-256 均与本地已签名构建产物一致。

## 已知限制

- 未执行真实 Google/Antigravity 交互式授权；该步骤需要用户本人在浏览器完成授权。自动化测试已覆盖 callback、state 校验、token exchange/refresh、onboarding、模型目录以及 session 后续状态链路。
- updater 发布签名未生成；这不影响当前 `/Applications/WaLiAPI.app` 的本地测试。正式发布仍需配置 Tauri updater 私钥并重新打包。
- 编译中存在工作区既有 unused import/dead code warning，本次迁移未将其扩大为阻塞错误。

## Review

精确审查未发现未解决的范围外修改或验收阻塞项；未在记录或日志中输出 OAuth client secret、真实 token、authorization code、state、邮箱或 project ID。
