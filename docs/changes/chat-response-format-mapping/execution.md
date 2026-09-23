# 执行记录

## result
IMPLEMENTED — 已提交，端到端三种形态验证通过。

## changed files

- `src-tauri/src/protocol/codec/responses_codec/encode_chat.rs`
  - `CHAT_TOP_LEVEL` 增加 `response_format`；移除「未知字段」分支里对
    `response_format` 的 `StructuredOutput` 特判（不再可达）。
  - 新增 `response_format_to_text()`：`text` → 不发送、`json_object` →
    `format.type=json_object`、`json_schema` → 平铺 `name`/`schema`/`strict`/
    `description`；畸形输入仍返回 `StructuredOutput` 错误。
  - 字段循环收集 `text_control`，输出阶段写入 Responses 的 `text`。
- `src-tauri/src/protocol/codec/responses_codec/tests.rs`
  - 新增 3 个测试：映射（含嵌套→平铺断言）、`type=text` 不产生 text、
    畸形 `json_schema` 仍被拒绝。

## verification

- `cargo test responses_codec` → 34 passed（含 3 个新增）。
- `cargo test` 全量 → **1107 passed / 0 failed**。
- 端到端（临时 headless 实例 + 真实 Grok 账号，`/v1/chat/completions` → Responses 上游）：
  | 形态 | 结果 |
  | --- | --- |
  | `response_format={"type":"json_object"}` | 200，返回 `{"title":"hello"}` |
  | `response_format={"type":"json_schema",…}`（Hermes 标题生成形态） | 200，返回 `{"title":"Hello World"}` |
  | `response_format={"type":"text"}` | 200 |
- 上游能力实测：Grok 账号与 Codex 账号（`gpt-6-astra`）对
  `text.format=json_object` / `json_schema` 均返回 200（这是"可无条件映射"的依据）。

## limitation

- 只覆盖 Chat → Responses 方向；Responses → Chat 方向的结构化输出映射此前已存在。
- `strict`、`description` 原样透传，未做额外校验（由上游负责）。
- 运行中的桌面实例需要重新构建安装后才能带上本改动。

## security
未涉及凭据变更；临时实例与数据副本在验证后清理。
