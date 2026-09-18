# Gemini 审核问题修复执行记录

状态：VERIFIED（MASTER 最终验收）
日期：2026-09-18
基线：HEAD `540dfb8`；执行前工作树已有大量与 Gemini 无关的修改，均保留。

## 修改内容

- Responses → Gemini 增加 fail-closed validator：拒绝内置工具、malformed function tool、文件/未知输入块、远程图片和无法安全保留的 reasoning；允许字符串输入及 data URI 图片。
- Responses 历史转换保留 `input_image`，由 Gemini 专用 validator 决定是否可表示，避免静默丢失。
- Chat → Gemini 工具声明和 assistant tool call/tool result 改为逐项严格校验；缺失或错误 `tool_call_id` 不再生成 `unknown`。
- Gemini Code Assist onboarding 只接受服务端确认的 project；导入文件的 project hint 不再冒充服务端结果；LRO 非空 error 直接失败。
- Gemini SSE decoder 对每个 record 检查 `promptFeedback.blockReason`，非 unspecified 的阻断返回协议错误，不生成正常 stop。
- Gemini 登录能力限定为 `browser_callback`；删除 Gemini 专用粘贴授权码 API/UI/实现；恢复并保留 Codex/Kimi 原有 Device Code 能力。
- 增加 Responses/Chat/tool/image/stream 回归测试。

## 验证

全部通过：

- `cd src-tauri && cargo test gemini -- --nocapture`：47 passed
- `cd src-tauri && cargo test auth -- --nocapture`：219 passed
- `cd src-tauri && cargo test protocol -- --nocapture`：229 passed
- `cd src-tauri && cargo test route_plan -- --nocapture`：53 passed
- `cd src-tauri && cargo fmt --check`：通过
- `git diff --check HEAD --`：通过
- `pnpm build`：通过（`tsc && vite build`）

前端构建仅有既有的 Vite chunk size/dynamic import warning，无构建错误。

## 备注

- 未执行 commit、push、reset、checkout。
- 工作树中与本任务无关的预先修改仍存在，未纳入本次修复判断。
