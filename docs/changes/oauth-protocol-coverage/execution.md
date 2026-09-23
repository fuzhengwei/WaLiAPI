# 执行记录

状态：IMPLEMENTED

基线提交：`33ddd2898e928265295cba25322ad6f7c48b543c`

## 实现内容

- 在 Messages→Chat、Messages→Responses、Chat→Responses 三条路径统一校验 `stop` 数组元素为字符串；Chat→Responses 的 `n` 只接受整数 `1`。
- 保留 Messages→Chat→Gemini 和 Responses→Chat→Gemini 两阶段转换的 request id、stream 和 normalized 指针。
- Gemini Responses 转换对未知输入 item、缺失 role 的兼容 item 显式返回 `unknown_block`，保留已知内置 item 的 fail-open 行为。
- 配置应用的 `.waliapi-backup` 改为幂等；JSON/YAML 解析、根节点和必要嵌套结构错误会中止写入；备份、恢复和事务清理错误会显式返回。
- 新增非法 stop/n、Gemini 上下文与未知 item、Responses 字符串/无类型 input item、配置重复应用/恢复/异常结构回归用例。

## 修改文件

- `src-tauri/src/commands/app_config.rs`
- `src-tauri/src/protocol/codec/request.rs`
- `src-tauri/src/protocol/codec/messages/encode.rs`
- `src-tauri/src/protocol/codec/directions/messages_to_responses/encode.rs`
- `src-tauri/src/protocol/codec/directions/messages_to_responses/tests.rs`
- `src-tauri/src/protocol/codec/responses_codec/encode_chat.rs`
- `src-tauri/src/protocol/codec/responses_codec/tests.rs`
- `src-tauri/src/protocol/codec/tests/chat_messages/messages_request.rs`
- `src-tauri/src/protocol/codec/gemini/mod.rs`
- `src-tauri/src/protocol/legacy/responses_decode.rs`
- `src-tauri/src/protocol/legacy/tests/responses.rs`

## 验证结果

通过：

- `cargo test --manifest-path src-tauri/Cargo.toml commands::app_config`（29 项）
- `cargo test --manifest-path src-tauri/Cargo.toml protocol::codec`（180 项）
- `cargo test --manifest-path src-tauri/Cargo.toml protocol::legacy`（41 项）
- `cargo test --manifest-path src-tauri/Cargo.toml`（1124 项单元测试及集成测试）
- `pnpm build`
- `pnpm --filter waliapi-web build`
- `git diff --check` 与基线 diff 检查

未通过但确认是基线问题：

- `cargo fmt --check --manifest-path src-tauri/Cargo.toml` 仅报告未修改的 `src-tauri/src/auth_provider/types.rs` 与 `src-tauri/src/endpoint_executor/grok_arguments.rs`。
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --no-deps -- -D warnings` 被既有告警阻断，涉及 `commands/wiki.rs`、`core/proxy.rs`、`endpoint_executor/*`、`server/admin_routes.rs`、`services/wiki/*`、`otlp_exporter.rs`、`adaptor/mod.rs` 等；本次修改文件没有出现在 Clippy 诊断中。

前端构建仅有既有的 Vite chunk size 和动态/静态导入提示，不影响构建成功。

## 限制

工作树中预先存在的 `docs/changes/antigravity-oauth-public-client/` 与 `docs/changes/gemini-oauth-login-fix/` 未修改。由于格式检查和 Clippy 的基线告警尚未清理，本任务不标记为 `VERIFIED`。

## 本地安装验证

- `pnpm tauri build --bundles app` 已生成 `src-tauri/target/release/bundle/macos/WaLiAPI.app`；构建最后因本机未设置 `TAURI_SIGNING_PRIVATE_KEY` 无法生成 updater 签名，`.app` 本身已完成构建。
- 已安装到 `/Applications/WaLiAPI.app`，版本 `0.3.5`，Apple Silicon arm64。
- 启动后 `GET http://127.0.0.1:8777/health` 持续返回 200；未携带 API Key 访问 `/v1/models` 返回预期 401。
