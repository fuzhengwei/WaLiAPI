# 执行记录

状态：IMPLEMENTED

## 基线

- 上游：`origin/v0.3.4`
- 基线提交：`69c19cd`
- 执行 worktree：`/private/tmp/waliapi-antigravity-solution-a`
- 主工作区的既有未提交修改未纳入本次分支。

## 改动文件

- `src-tauri/src/auth_provider/gemini_login.rs`
  - 默认使用公开 Antigravity client material。
  - 保留 `WALIAPI_ANTIGRAVITY_CLIENT_ID` 与 `WALIAPI_ANTIGRAVITY_CLIENT_SECRET` 的运行时覆盖。
  - 增加每次登录独立生成的 PKCE verifier/challenge。
  - 授权地址增加 `code_challenge` / `code_challenge_method=S256`。
  - authorization-code exchange 增加对应 `code_verifier`。
  - 保留 localhost callback、state 校验、payload marker 和 refresh 逻辑。
  - 未记录任何运行时 token、授权码或本地敏感配置值。
- `docs/changes/antigravity-oauth-solution-a/plan.md`
  - 记录目标、范围、验收标准和基线。

## 验证结果

通过：

```text
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo test --manifest-path src-tauri/Cargo.toml auth_provider::gemini_login::tests -- --nocapture
  14 passed, 0 failed
cargo test --manifest-path src-tauri/Cargo.toml auth -- --nocapture
  277 passed, 0 failed
pnpm build
  tsc 通过；Vite production build 通过
 git diff --check HEAD --
```

补充结果：

- `cargo test --manifest-path src-tauri/Cargo.toml gemini -- --nocapture` 中认证相关测试均通过，但该过滤集合还包含一个与本次 OAuth 改动无关的既有图片 codec 测试失败：`responses_data_uri_images_are_preserved_for_gemini`，断言 `mime_type` 为 `Null` 而非预期值。本次未修改该 codec 代码。
- 构建输出仅有项目既有未使用代码和 chunk size warning，没有新增错误。

## 限制与安全说明

- 本次不修改 Codex/Kimi，不修改 migration 041。
- 公开 client material 仅作为应用级 OAuth client 配置使用；用户个人 token、授权码和本地运行时覆盖值不进入提交、日志或文档。
