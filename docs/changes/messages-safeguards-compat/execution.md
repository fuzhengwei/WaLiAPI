# 执行记录

## result
IMPLEMENTED — 已提交，端到端复现验证通过。

## changed files

- `src-tauri/src/protocol/codec/messages/encode.rs`
  - `Messages→Chat` 的 fail-open 组（`metadata` / `container` /
    `context_management` / `context_management_config`）加入 `safeguards`，
    附中文注释说明来源（Claude Code 2.1.280）与取舍。
- `src-tauri/src/protocol/codec/directions/messages_to_responses/encode.rs`
  - `Messages→Responses` 同样把 `safeguards` 归入"丢弃 + 记录"。
- `src-tauri/src/protocol/codec/tests/chat_messages/messages_request.rs`
  - 新增 `messages_request_safeguards_dropped_fail_open`。
- `src-tauri/src/protocol/codec/directions/messages_to_responses/tests.rs`
  - 新增 `request_drops_safeguards_fail_open`。
- `Messages→Gemini` 复用 `messages::encode_messages_to_chat`，随之覆盖。

## verification

- `cargo test protocol::codec` → 173 passed（含 2 个新增）。
- `cargo test` 全量 → **1109 passed / 0 failed**。
- 端到端（临时 headless 实例 + 真实上游，`/v1/messages` + 顶层
  `safeguards:[{type,classifier_context}]`，即原始失败形状）：
  - `deepseek-flash`（Messages→Chat，原先 400 的那条路径）→ **200**
  - `grok-4.7`（Messages→Responses）→ **200**

## 其它同时段的 4xx（供对照，均非本次改动范围）

- `response_format` 的 400（18:04Z）：已在
  `docs/changes/chat-response-format-mapping/` 修复并安装。
- `stream interrupted ... operation timed out` 的 502（17:08–17:18Z）：
  已在 `docs/changes/provider-stream-timeout/` 修复并安装。
- `503 No available upstream candidate for model: wali`（16:41–16:43Z）：
  旧 OpenClaw 配置遗留，见 `docs/changes/openclaw-model-policy-sync/`。

## limitation

- `safeguards` 描述的是 Anthropic 服务端的分类器上下文，被丢弃意味着该声明在
  非 Anthropic 上游不生效（这些上游本就没有对应机制）。
- 运行中的桌面实例需要重新构建安装后才带上本改动。

## security
未涉及凭据；验证脚本与临时数据副本已清理。
