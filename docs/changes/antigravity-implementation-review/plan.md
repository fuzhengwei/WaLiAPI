# Antigravity 实现规范审核与注释修正

状态：VERIFIED
日期：2026-09-18
基线：`540dfb8ddf3b02ef786016262b8b14ece70bb91d`

## objective

审核当前 Antigravity OAuth / Gemini 内部协议实现是否符合仓库规范和既有设计，并修正已确认的错误分类、误导命名、过时注释与文档不一致。

## non-goals

- 不改变内部 provider ID `gemini`、数据库 schema、路由协议或 Gemini codec wire behavior。
- 不改变 OAuth scopes、client contract、redirect contract 或 Code Assist 请求结构。
- 不执行发布、安装、commit、push、reset 或 checkout。
- 不修改本任务范围外的既有脏文件。

## current behavior and evidence

- `gemini_backend.rs::complete_login` 会吞掉 userinfo 的 `Unauthorized`、`Retryable`、`Protocol` 错误，并在没有 `email_hint` 时统一降级为 `LoginFailed`；而 Gemini import 已关闭，OAuth token 路径不会提供该 hint。
- 新增 Gemini/Antigravity 文件中的模块注释主要为英文，不符合 `AGENTS.md`“新注释与文档使用中文”的约定。
- OAuth 常量和 token 类型仍使用 `GEMINI_*` / `ImportedTokens` 命名，容易误解为 Gemini CLI 导入流程。
- onboarding 注释仍描述 file/import project，但当前 project hint 来自重新登录账号的安全属性。
- 迁移任务文档仍称 UI 显示 Gemini，与当前已批准的 Antigravity 展示名不一致。

## approved design decisions

- UI 与 OAuth client 品牌使用 `Antigravity`；内部 provider/protocol/codec 继续使用 `gemini` 以保持兼容。
- userinfo 错误保持原始 typed error，不再降级为泛化登录失败。
- 删除无用 `email_hint`；将局部 token 类型改名为 `OAuthTokens`。
- OAuth endpoint 常量以 Google OAuth 命名，client material 与超时以 Antigravity 命名；只改标识符，不改值和网络行为。
- 新增或本任务相关注释使用中文，重点记录安全边界、兼容原因和 fail-closed 语义。

## expected affected files or modules

- `src-tauri/src/auth_provider/gemini_login.rs`
- `src-tauri/src/auth_provider/gemini_backend.rs`
- `src-tauri/src/auth_provider/types.rs`
- `src-tauri/src/commands/auth.rs`
- `src-tauri/src/protocol/codec/gemini/{mod,encode,decode,stream}.rs`
- `AGENTS.md`
- `docs/changes/gemini-antigravity-oauth/{tasks,execution}.md`
- 本目录 `execution.md`

## implementation steps

1. 增加 userinfo 失败分类回归测试，再修正 `complete_login` 的错误传播。
2. 删除 `email_hint`，重命名 token 类型与 OAuth 常量，保持值和请求参数不变。
3. 将新增模块注释及关键安全设计注释调整为中文，并修正过时 onboarding 注释。
4. 同步任务文档与仓库结构说明中的 Antigravity 展示名。
5. 运行定向回归、格式、diff 和前端构建检查，审核精确 diff。

## acceptance criteria

- userinfo 401 保持 `ProviderError::Unauthorized`；服务端错误保持 `ProviderError::Retryable`，不再变成 `LoginFailed`。
- 不再存在 `ImportedTokens`、`email_hint` 和误导性的 Gemini OAuth client 常量名。
- 注释明确 UI/OAuth 为 Antigravity、内部 provider/protocol 为 `gemini`，并说明 callback/state、PKCE、legacy payload、模型目录及 header 白名单等关键边界。
- 文档与当前 Antigravity 展示名一致。
- 相关测试与构建检查通过，且未引入 OAuth wire、数据库或凭据变化。

## verification commands

```bash
cd src-tauri && cargo test gemini -- --nocapture
cd src-tauri && cargo test auth -- --nocapture
cd src-tauri && cargo test protocol -- --nocapture
cd src-tauri && cargo test route_plan -- --nocapture
cargo fmt --check --manifest-path src-tauri/Cargo.toml
git diff --check HEAD --
pnpm build
```

## compatibility requirements

旧账号兼容策略、v2/antigravity marker、provider ID、API 错误码、Code Assist payload、模型协议与 UI command contract 均保持不变。

## rollback or failure considerations

本任务仅做局部错误传播、内部标识符和注释/文档调整，可按本任务精确 diff 回退；不得覆盖工作区其他用户改动。若验证暴露需要改变 OAuth/API/schema 的问题，应停止并重新审批范围。

## unresolved questions

无。

## pre-existing dirty files

执行前工作区已有大量已修改文件，以及 Antigravity/Gemini 实现和设计文档未跟踪文件。以 `git status --short` 的执行前输出为快照；本任务仅修改上列文件，其他改动保持原样。
