# OAuth 接入与协议转换回归修正

状态：IMPLEMENTED

## objective

修正近期 Antigravity/Grok OAuth 接入及 Codex、Claude、OpenCode、Hermes、OpenClaw 调用链涉及的配置事务与协议转换边界，并补充回归用例，避免重复应用覆盖原始配置、非法参数静默降级、Gemini 双阶段转换丢失上下文，以及配置结构异常导致“成功”但写入无效。

## non-goals

- 不改变 OAuth 供应商端点、凭证格式或模型路由策略。
- 不修改数据库 schema、前端交互或已存在的第三方配置格式约定。
- 不改动工作树中预先存在的 `docs/changes/antigravity-oauth-public-client/` 与 `docs/changes/gemini-oauth-login-fix/`。

## baseline and evidence

- 基线提交：`33ddd2898e928265295cba25322ad6f7c48b543c`（工作树仅有上述两个预先存在的未跟踪目录）。
- 相关现状证据：`src-tauri/src/commands/app_config.rs` 的 Hermes 备份与统一应用前置备份会重复覆盖；`protocol/codec/{messages,chat,responses_codec}` 的 `stop`/`n` 校验不完整；`protocol/codec/gemini/mod.rs` 的 Responses→Gemini 两阶段编码直接丢弃第一阶段 `ConversionContext`；未知 Responses 输入项被静默跳过；OpenCode/OpenClaw/Hermes 对非对象配置节点可能静默继续；统一备份错误被忽略。

## approved design decisions

1. 备份只在首次应用且目标存在时创建，后续重复应用保留首次原始字节；Hermes 使用同一事务备份，统一入口传播备份/marker 错误，失败时恢复资料与目标文件保持一致。
2. `stop`/`stop_sequences` 数组只接受字符串元素；Chat→Responses 的 `n` 只接受正整数 `1`，其它值返回稳定的 unsupported-field 错误。
3. Responses→Gemini 合并两阶段编码的 `normalized` 指针，并保留请求 ID 与 stream 状态；Gemini→Chat→Responses 解码器使用原始上下文。
4. Responses→Gemini 对已支持的白名单 item 类型正常转换；未知类型返回 `unknown_block`，避免输入被静默丢弃。
5. OpenCode/OpenClaw/Hermes 读取的根/必要节点类型不符时返回错误，不写目标；解析错误和备份失败均不得继续写入。

## expected affected files

- `src-tauri/src/commands/app_config.rs` 及其内联测试
- `src-tauri/src/protocol/codec/messages/encode.rs`
- `src-tauri/src/protocol/codec/directions/messages_to_responses/encode.rs`
- `src-tauri/src/protocol/codec/responses_codec/encode_chat.rs`
- `src-tauri/src/protocol/codec/gemini/mod.rs` 及测试
- 对应协议测试模块

## implementation steps

1. 提取字符串数组校验并在三条编码路径复用；严格校验 `n`。
2. 修正 Gemini 两阶段上下文合并与未知 item 错误策略。
3. 将配置写入前的备份/marker 处理改为可传播错误的幂等事务，并让 OpenCode/OpenClaw/Hermes 结构异常显式失败。
4. 为首次应用、重复应用、恢复、非法协议参数、上下文保留与未知 item 增加回归用例。
5. 运行定向测试、格式检查、全量 Rust 测试、前端构建与 clippy。

## acceptance criteria

- 重复应用任一受影响配置不会覆盖首次备份，恢复得到首次原始字节。
- 所有受影响协议方向拒绝非字符串 stop 数组元素；Chat→Responses 拒绝 `n != 1` 或非正整数。
- Responses→Gemini 返回上下文包含两阶段全部 normalized 指针且 decoder 使用正确 request id/stream。
- Responses→Gemini 遇未知 item 返回 `unsupported_feature.unknown_block`，不生成部分请求。
- OpenCode/OpenClaw/Hermes 必要结构异常、解析错误、备份失败均返回失败且原文件不变。
- 新增回归用例通过，既有测试与构建检查不回归。

## verification commands

- `cd src-tauri && cargo test commands::app_config`
- `cd src-tauri && cargo test protocol::codec`
- `cd src-tauri && cargo test`
- `cargo fmt --check --manifest-path src-tauri/Cargo.toml`
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --no-deps -- -D warnings`
- `pnpm build`
- `pnpm --filter waliapi-web build`
- `git diff --check 33ddd2898e928265295cba25322ad6f7c48b543c`

## compatibility and rollback

配置写入仍使用现有 `.waliapi-backup`/`.waliapi-absent` 恢复协议，协议错误仅收紧此前静默接受的无效输入。回滚实现即可恢复旧行为；不需要迁移或外部数据变更。

## unresolved questions

无。若测试暴露与已批准设计冲突的既有兼容约定，停止并报告具体证据。
