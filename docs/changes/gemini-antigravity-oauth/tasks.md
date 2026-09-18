# Gemini → Antigravity OAuth 执行任务

状态：VERIFIED
日期：2026-09-18

## objective

完成 Antigravity OAuth、个人账号 onboarding、动态模型发现和 UI 能力迁移，修复浏览器授权回调后登录状态无法完成的问题。

## expected affected files

- `src-tauri/src/auth_provider/gemini_login.rs`
- `src-tauri/src/auth_provider/gemini_backend.rs`
- `src-tauri/src/auth_provider/spec.rs`
- `src-tauri/src/auth_provider/types.rs`
- `src-tauri/src/auth_provider/service.rs`
- `src-tauri/src/commands/auth.rs`（仅 Gemini import/error 文案所需）
- `src-tauri/src/endpoint_executor/integration_tests.rs`
- `src/lib/api.ts`
- `src/pages/AuthChannelsPage.tsx`
- `src/components/auth/LoginModal.tsx`
- 本目录 `execution.md`

## implementation steps

1. RED→GREEN：OAuth URL、callback、token exchange/refresh、payload marker 与旧 payload 拒绝测试。
2. RED→GREEN：Antigravity base URL、metadata/header、free-tier onboarding/ineligible tiers 测试。
3. RED→GREEN：`fetchAvailableModels` 请求与响应解析测试。
4. 关闭 Gemini CLI import capability 和 UI 入口，更新展示文案为 Antigravity OAuth。
5. 执行精确 diff review，记录执行证据。

## acceptance criteria

- 浏览器回调被消费后 session 从 waiting 继续进入 exchanging/saving/syncing，最终 done 或返回可操作的明确错误。
- authorize URL 使用 Antigravity client/scopes/callback，严格验证 state，不带 PKCE。
- exchange/refresh 使用匹配的 Antigravity client credentials。
- 新凭据带 v2/antigravity marker；旧凭据 refresh/outbound 被拒绝并提示重新登录。
- `loadCodeAssist`/`onboardUser`/generate/stream 使用 daily-cloudcode 与 Antigravity metadata/User-Agent。
- 免费层不可用时不盲目 onboarding；validation URL 可安全显示。
- 模型同步使用 `fetchAvailableModels` 的服务器结果。
- UI 不再提供 Gemini CLI 凭据导入，显示名称为 Antigravity，并说明通过 Antigravity OAuth 登录；内部 provider ID 仍为 `gemini`。

## verification commands

```bash
cd src-tauri && cargo test gemini -- --nocapture
cd src-tauri && cargo test auth -- --nocapture
cd src-tauri && cargo test protocol -- --nocapture
cd src-tauri && cargo test route_plan -- --nocapture
cargo fmt --check --manifest-path src-tauri/Cargo.toml
git diff --check HEAD --
pnpm build
pnpm tauri build
```

## unresolved questions

无。企业/GCP TOS 登录明确不在本次范围；个人账号是验收路径。
