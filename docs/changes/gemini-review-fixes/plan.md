# Gemini 审核问题修复计划

状态：VERIFIED
日期：2026-09-18

## objective
修复 Gemini Auth/Codec 实现审核发现的六类问题，保持既有 Kimi/Codex 的 fail-closed 转换约定，并保留工作树中与本任务无关的预先改动。

## non-goals
- 不改变 Gemini Code Assist 上游协议、数据库 schema 或渠道 Google 实现。
- 不提交 Git、不修改无关的预先改动。
- 不增加新的认证方式；Gemini 仅保留 browser callback。

## current behavior and evidence
- Responses→Chat 的历史转换会用 `filter_map` 丢弃 Responses 内置工具和非文本输入。
- Chat→Gemini 工具声明会用 `filter_map` 丢弃 malformed/non-function 工具。
- Chat tool result 找不到 tool call 时生成 `unknown` 工具名。
- onboarding 在服务器未返回 project 时会回退到导入文件的 project hint。
- Gemini stream semantic failure 只检查首个 SSE record，decoder 对后续无 candidates 事件直接跳过。
- Gemini ProviderSpec、backend、commands、LoginModal 暴露了规格明确排除的 DeviceCode/粘贴授权码流程。

## approved design decisions
1. Responses→Gemini 在转换前严格检查工具和 input 内容；不可表示能力返回 `UnsupportedFeatures`，不发上游请求。
2. Responses→Chat 历史转换继续服务其他路径，但 Gemini 专用路径不能依赖其静默丢字段行为。
3. Chat 工具逐项严格验证并在任意失败时拒绝整个请求。
4. tool result 必须有非空 `tool_call_id` 且能匹配之前 assistant tool call；不再生成 `unknown`。
5. onboarding 只接受服务端返回的 project；没有服务端 project 就失败，LRO error 也失败。
6. Gemini SSE 的每个 record 都检查 semantic failure；promptFeedback 阻断不得被正常 stop 掩盖。
7. Gemini 只支持 BrowserCallback；删除 Gemini 专用 user-code/paste-code UI/API/实现。通用 Codex DeviceCode 流程保留。

## expected affected files/modules
- `src-tauri/src/protocol/codec/gemini/mod.rs`
- `src-tauri/src/protocol/codec/gemini/encode.rs`
- `src-tauri/src/protocol/codec/gemini/stream.rs`
- `src-tauri/src/auth_provider/gemini_backend.rs`
- `src-tauri/src/auth_provider/gemini_login.rs`
- `src-tauri/src/auth_provider/spec.rs`
- `src-tauri/src/auth_provider/mod.rs`
- `src-tauri/src/commands/auth.rs`
- `src/components/auth/LoginModal.tsx`
- 相关 Gemini 测试文件

## implementation steps
1. 为 Responses→Gemini 增加严格 validator，拒绝内置工具、文件/音频/视频/远程图片和未知 input block。
2. 重写 Chat tools 转换为严格逐项校验；修正 tool result 关联校验。
3. 修正 onboarding project/LRO error 语义并更新测试。
4. 在 executor 流处理路径对每个 Gemini SSE record 做 semantic failure 检查；decoder 不再吞掉 promptFeedback 阻断。
5. 删除 Gemini DeviceCode 分支、粘贴码 session API 及 UI，并更新能力测试。
6. 运行定向及回归验证。

## acceptance criteria
- 不可表示的 Responses/Chat 能力在任何上游调用前返回错误。
- malformed tool、错 tool_call_id、缺 project、LRO error 不会生成成功请求。
- Gemini stream 的任意 record 出现 promptFeedback 阻断时不会发送正常 stop。
- Gemini ProviderSpec 的 login_methods 精确为 `[browser_callback]`；前端不显示 Gemini 粘贴授权码。
- 既有 Kimi/Codex 和 Gemini 正常路径测试通过。

## verification commands
- `cd src-tauri && cargo test gemini -- --nocapture`
- `cd src-tauri && cargo test auth`
- `cd src-tauri && cargo test protocol`
- `cd src-tauri && cargo test route_plan`
- `cd src-tauri && cargo fmt --check`
- `git diff --check HEAD --`
- `pnpm build`（若依赖/网络可用）

## compatibility and rollback
不改数据库和公开上游协议；失败时可按本任务 diff 回滚。保留非本任务工作树改动，不覆盖用户文件。

## unresolved questions
无。前端构建若因当前环境依赖下载失败，记录为未验证项。

## baseline
HEAD：`540dfb8`。工作树在执行前已存在大量已修改文件及 Gemini/设计文档未跟踪文件；执行时只修改本计划列出的 Gemini 相关实现/测试和本任务证据文件。
