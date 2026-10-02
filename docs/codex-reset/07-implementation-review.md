# Codex 重置卡：实现复核

**复核日期**：2026-10-02
**分支**：`add-reset`

## 架构结论

当前实现符合 WaLiAPI 的分层边界：

```text
React Auth 页面
  → src/lib/api.ts / runtime.ts
  → Tauri command 或 /admin/api/invoke
  → AuthService
  → Provider trait
  → CodexProvider
  → ChatGPT backend-api
```

- 重置能力挂在 `Provider` 可选能力上，非 Codex provider 默认返回 `UnsupportedFeatures`。
- 账号状态、provider 能力、卡归属、状态、类型和过期时间均在 `AuthService` 发网前再次校验。
- 一次性消费写入 `auth_reset_operations`，卡标识只保存 SHA-256，`quota_json` 不承担操作状态。
- 消费后的额度回读调用既有 `AuthService::refresh_quota`，没有复制额度解析和持久化逻辑。
- 桌面端与 headless Web 端使用同一组命令和 DTO，前端请求继续经过 `runtime.ts`。

## 代理与客户端复用

重置卡列表、消费和额度回读均在 `CodexProvider` 中调用：

- `crate::adaptor::global_proxy_url()` 读取设置页同步的全局代理；
- `crate::adaptor::blocking_client(30, proxy)` 复用现有非流式连接池；
- Codex Responses 流式请求继续使用 `streaming_client(proxy)`。

因此没有新增重置专用代理、固定端口或第二套 HTTP transport。模型同步和 endpoint executor 同时修正为：渠道未填写 `config.proxy` 时回退全局代理，显式 `direct` 仍保持直连。

## 结果与回归

已验证：

- `cd src-tauri && cargo test reset_credit --no-fail-fast`：3 个 provider contract/parser 测试通过；
- `cd src-tauri && cargo test --no-fail-fast`：1149 个库测试及全部集成测试通过；
- `pnpm build`：TypeScript 检查和 Vite 构建通过；
- `git diff --check origin/main...HEAD`：通过；
- `cargo fmt --check`：仅被未改动的基线文件 `src-tauri/src/endpoint_executor/grok_arguments.rs` 格式差异阻塞。

未发现重置改动直接破坏登录、刷新令牌、模型同步、额度刷新或 `/v1/*` 路由的调用边界。

## 尚未闭合项

前端现在在确认时生成 UUID 格式的 `operationId`，将其传入消费命令，并在当前弹窗中展示任务状态。消费请求异常时锁定当前任务，不生成新幂等键、不自动重试；后端仍负责持久化 `unknown` 并保证同一操作不会重复消费。按本次范围不增加历史操作查询或历史记录页面。
