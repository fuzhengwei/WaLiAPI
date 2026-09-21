# Antigravity OAuth 方案 A

状态：READY_FOR_EXECUTION

## 目标

基于上游最新 `v0.3.4`（基线 `69c19cd`）修复 Antigravity OAuth 登录流程：默认使用公开 client material，支持 PKCE，并保留环境变量覆盖，使用户无需预先配置客户端 secret 即可通过浏览器完成授权。

## 非目标

- 不修改 Codex/Kimi OAuth。
- 不修改 OAuth payload 版本、localhost callback、state 校验和 refresh 行为的既有语义。
- 不修改数据库 schema 或 migration（上游基线已包含 migration 041）。
- 不读取、验证、打印或提交用户本地 secret。
- 不纳入主工作区其他未提交修改。

## 当前行为与证据

上游 `v0.3.4` 的 `src-tauri/src/auth_provider/gemini_login.rs`：

- client ID 有默认值，但 client secret 只从 `WALIAPI_ANTIGRAVITY_CLIENT_SECRET` 读取。
- 授权 URL 未携带 PKCE 参数。
- authorization-code token exchange 未携带 `code_verifier`。
- refresh 请求会按现有配置携带 client material。

## 批准设计

1. 默认使用仓库中已有的公开 Antigravity client material。
2. `WALIAPI_ANTIGRAVITY_CLIENT_ID` 与 `WALIAPI_ANTIGRAVITY_CLIENT_SECRET` 作为显式覆盖；覆盖值只在运行时读取，不进入日志、文档或提交。
3. 每次登录生成独立 PKCE `code_verifier`，授权 URL 增加 `code_challenge` 与 `code_challenge_method=S256`，token exchange 增加 `code_verifier`。
4. 保留 localhost callback、state 校验、payload 版本和 refresh 行为。

## 预期文件

- `src-tauri/src/auth_provider/gemini_login.rs`
- `docs/changes/antigravity-oauth-solution-a/plan.md`
- `docs/changes/antigravity-oauth-solution-a/execution.md`

## 实施步骤

1. 从基线文件移植方案 A 的最小代码变更。
2. 检查 diff，确认无 migration、secret、无关文件变化。
3. 运行 Rust 目标测试、格式检查、前端构建和 diff 检查。
4. 记录执行证据，提交并创建目标为 `v0.3.4` 的 PR。

## 验收标准

- `GeminiLogin::new()` 默认使用内置 client material，环境变量可覆盖。
- authorization URL 包含 PKCE challenge 和 S256 method。
- authorization-code exchange 使用对应 verifier。
- refresh 请求继续使用 client material。
- token、secret、code、verifier 不写入日志。
- 不修改 Codex/Kimi，不修改 migration 041。
- 目标测试和构建检查通过或明确记录无法完成的检查。

## 验证命令

```bash
cargo test --manifest-path src-tauri/Cargo.toml gemini -- --nocapture
cargo test --manifest-path src-tauri/Cargo.toml auth -- --nocapture
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
git diff --check HEAD --
pnpm build
```

## 兼容性与回滚

- 保持现有 provider ID、payload 格式、localhost callback 和 refresh 兼容。
- 回滚方式为还原本分支提交；不涉及数据库迁移。

## 基线与工作区

- 基线：`origin/v0.3.4`，`69c19cd`。
- 主工作区存在用户未提交修改；本任务使用 `/private/tmp/waliapi-antigravity-solution-a` 独立 worktree，主工作区修改不应进入本分支。

## 未解决问题

无。PR 目标分支为 `v0.3.4`；PR #122 已合并，本变更独立基于最新 `v0.3.4`。
