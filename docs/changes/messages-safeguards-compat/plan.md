# Messages `safeguards` 兼容（Claude Code 2.1.280）

状态：IMPLEMENTED（已提交，端到端验证通过）

## objective

让 Claude Code（2.1.280+）通过网关调用非 Anthropic 上游时，不再因为顶层
`safeguards` 字段被 `Messages→*` 转换拒绝而中断会话。

## 现象与证据

网关日志：

```
400 /v1/messages  model=deepseek-flash
request cannot be converted to openai: request uses feature(s) this codec cannot
preserve: /safeguards (unsupported_feature.unsupported_field)
```

来源认定（不是原以为的 OpenClaw / Hermes，而是 **Claude Code**）：

- WaLiAPI 代码里没有 `safeguards`；
- 本机 Claude Code `2.1.280` 二进制里存在该字段的请求构造：
  `...Iae && sa !== void 0 && { safeguards: [{ type: Jve, classifier_context: sa }] }`
  —— 即"条件满足时在 Messages 顶层发送 `safeguards: [{type, classifier_context}]`"；
- 同一二进制还有 `messages.<n>.content.<m>.safeguards` 的错误路径解析与
  `"safeguards" in e` 的剥离函数，说明它是 Anthropic 侧的安全分类器上下文声明。

失败的请求都落在 `/v1/messages`（Anthropic 协议），模型是 `deepseek-flash`
（OpenAI 渠道 → `Messages→Chat`），另有 `Copazk`/`grok-4.7`（→ `Messages→Responses`）同源。

## approved design decisions

1. `safeguards` 在 Chat / Responses / Gemini 三个方向都没有等价物，且它描述的是
   Anthropic 服务端的分类器上下文；**整段 fail-closed 会让用户直接无法对话**，
   因此按 fail-open **丢弃并记入 `normalized` 审计**，与既有的
   `metadata` / `container` / `context_management*` 同类。
2. 同时覆盖三条方向：
   - `messages/encode.rs`（Messages→Chat）；
   - `directions/messages_to_responses/encode.rs`（Messages→Responses）；
   - `Messages→Gemini` 复用 `messages::encode_messages_to_chat`，自动获得同一行为。
3. 不尝试伪造或映射到其它字段（没有语义等价的落点）。

## expected affected files

- `src-tauri/src/protocol/codec/messages/encode.rs`
- `src-tauri/src/protocol/codec/directions/messages_to_responses/encode.rs`
- `src-tauri/src/protocol/codec/tests/chat_messages/messages_request.rs`
- `src-tauri/src/protocol/codec/directions/messages_to_responses/tests.rs`
- `docs/changes/messages-safeguards-compat/{plan.md,execution.md}`

## acceptance criteria

- 带顶层 `safeguards` 的 Messages 请求在 Messages→Chat 与 Messages→Responses
  下都能正常转换，且 `safeguards` 出现在 `normalized` 审计里。
- 其它未知字段仍然 fail-closed（只有 `safeguards` 加入 fail-open 组）。
- 现有测试无回归。
