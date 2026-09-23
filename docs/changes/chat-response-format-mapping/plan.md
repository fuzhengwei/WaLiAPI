# Chat `response_format` → Responses `text.format`

状态：IMPLEMENTED（已提交，端到端验证通过）

## objective

让下游 Chat 客户端（Hermes 的辅助标题生成、OpenCode 等）通过网关调用 Responses
上游（Grok 账号 / Codex 账号）时，`response_format` 不再让整段请求 400。

## 现象与证据

Hermes 日志：

```
⚠ Auxiliary title generation failed: HTTP 400: request cannot be converted to responses:
request uses feature(s) this codec cannot preserve: /response_format
(unsupported_feature.structured_output): Chat field "response_format" has no Responses
backend representation
```

网关请求日志与之对应：`/v1/chat/completions`、`model=grok-4.7`、400、
`failure_class=caller_terminal`。

原因是 `Chat→Responses` 对 `response_format` 一律 fail-closed（`StructuredOutput`），
而 Responses 侧其实有等价能力：**`text.format`**。

上游能力实测（本机真实账号）：

- Grok OAuth 账号：`text.format=json_object` / `json_schema` 均 200；
- Codex 账号（`gpt-6-astra`）：两种形态同样 200。

## approved design decisions

1. `Chat→Responses` 把 `response_format` 映射为 Responses 的 `text.format`：
   - `{"type":"json_object"}` → `{"text":{"format":{"type":"json_object"}}}`
   - `{"type":"json_schema","json_schema":{...}}` → `{"text":{"format":{...}}}`：
     **把 Chat 的嵌套 `json_schema` 平铺**到 format（`name` / `schema` / `strict` /
     `description`），与 Responses 的形状一致；
   - `{"type":"text"}` 表示无结构约束，等价于不发送（记入 `normalized` 审计）；
   - 其它 `type` 或缺失 `json_schema` / `name` / `schema` → 仍然明确报错
     （`StructuredOutput`），不静默降级。
2. 两个上游都实测接受 `text.format`，因此无需按上游分流。
3. 与既有的 `verbosity` 处理不冲突：`verbosity` 仍是丢弃 + 记录，不会产生 `text`。

## expected affected files

- `src-tauri/src/protocol/codec/responses_codec/encode_chat.rs`
- `src-tauri/src/protocol/codec/responses_codec/tests.rs`
- `docs/changes/chat-response-format-mapping/{plan.md,execution.md}`

## verification commands

```bash
cd src-tauri && cargo test responses_codec
cd src-tauri && cargo test
```

## acceptance criteria

- Chat 的 `json_object` / `json_schema` 经 Responses 上游可用，且请求体里出现
  正确的 `text.format`。
- `{"type":"text"}` 不产生 `text` 字段，且留下审计痕迹。
- 畸形 `response_format` 仍以明确错误拒绝。
- 现有测试无回归。
