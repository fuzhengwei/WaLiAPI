# Gemini → Antigravity OAuth 迁移设计

状态：READY_FOR_EXECUTION
日期：2026-09-18

## 已批准设计

1. **兼容标识不变**：provider ID 保持 `gemini`，不改数据库 schema、路由协议和 Gemini codec。
2. **凭据域升级**：新 payload 写入 `version: 2`、`oauth_client: "antigravity"`。refresh 和 outbound 对旧/无标识 payload fail closed，要求重新登录，绝不把旧 Gemini CLI refresh token 交给 Antigravity client。
3. **OAuth 与官方实现对齐**：使用 Antigravity client ID/secret、五项 scopes、`localhost` + `/oauth-callback`、`access_type=offline`、`prompt=consent`；保留 WaLiAPI 自己的严格 state 校验；移除 PKCE 参数。
4. **回调语义准确**：callback 页面只说明“授权已收到，正在应用内完成登录”，不提前宣称账号登录完成；错误 callback 返回明确失败状态。
5. **个人账号 onboarding**：调用 `loadCodeAssist`，复用服务器返回 project；无 project 时仅在 `free-tier` 明确可用时调用 `onboardUser("free-tier")`；解析 ineligible tiers，验证 URL 单独呈现，其他不符合条件返回稳定、脱敏错误。
6. **Antigravity wire profile**：base URL 改为 `daily-cloudcode-pa.googleapis.com`，统一 Antigravity metadata 与 User-Agent。保留已验证的 generate/stream envelope。
7. **模型发现**：优先调用 `fetchAvailableModels` 并解析服务器返回模型；上游成功但响应无模型视为协议错误，不伪造静态可用模型。
8. **关闭旧导入**：`supports_import=false`，移除 Gemini CLI `oauth_creds.json` UI 入口；底层 Gemini import 返回 `ImportFailed`，避免产生无法刷新或被上游拒绝的账号。
9. **错误与秘密**：不记录 token、code、state、邮箱、project ID、client secret；renderer 只收到稳定错误码/安全文案。

## 非目标

- 不实现企业/GCP TOS project picker。
- 不读取或依赖 Antigravity IDE 本地 state 数据库。
- 不改变 Gemini 请求/响应 codec，不新增数据库迁移。
- 不提交或 push Git。

## 兼容与回滚

- 旧账号保留在数据库，但会被明确要求重新登录；重新登录可原位替换账号。
- 若回滚，仅回退本任务涉及的 Gemini auth/backend/spec/UI 文件；无 schema/data rollback。
- Antigravity 上游协议若变更，动态模型同步应失败并保持账号已保存但暂不参与路由的既有 warning 语义。
