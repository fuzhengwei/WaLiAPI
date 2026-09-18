# Gemini → Antigravity OAuth 迁移上下文

状态：READY_FOR_EXECUTION
日期：2026-09-18
基线：`540dfb8ddf3b02ef786016262b8b14ece70bb91d`

## 目标

把 provider ID 仍为 `gemini` 的认证和 Code Assist 上游接入从已失效的 Gemini CLI OAuth 迁移到 Antigravity OAuth，使浏览器授权回调后能够完成 token exchange、个人免费层 onboarding、账号保存与模型同步。

## 当前行为与证据

- 现有 `gemini_login.rs` 使用 Gemini CLI OAuth client、`/oauth2callback`、PKCE 和三项 scopes。
- 旧 token 调用 `loadCodeAssist` 返回 `ineligibleTiers.reasonCode = UNSUPPORTED_CLIENT`，session 最终进入 failed；UI 因错误消息泛化表现为“一直未完成”。
- callback handler 在只收到 authorization code 时即显示“login complete”，而 token exchange/onboarding 尚未执行，文案误导。
- 本机 Antigravity IDE 2.5.5 的打包源码和 auth 日志确认：
  - authorize：`https://accounts.google.com/o/oauth2/v2/auth`
  - token：`https://oauth2.googleapis.com/token`
  - callback：`http://localhost:<port>/oauth-callback`
  - scopes 增加 `cclog`、`experimentsandconfigs`
  - OAuth client 使用 client secret，不使用 PKCE
  - 个人账号 API base：`https://daily-cloudcode-pa.googleapis.com`
  - metadata：`ideName=antigravity`、`ideType=ANTIGRAVITY`、`ideVersion=2.5.5`
  - `loadCodeAssist`、`onboardUser`、`fetchAvailableModels` 位于 `v1internal:*`
- 已使用本机已登录 Antigravity token 对 `daily-cloudcode` 做脱敏只读探测，`loadCodeAssist` 返回 200、`free-tier` 和服务器确认的 project。

## 工作区保护

工作区在本任务开始前已有大量用户改动，Gemini backend/login/codec 还是未跟踪文件。不得 checkout/reset/覆盖其他改动。

目标文件初始 SHA-256：

- `gemini_backend.rs`: `a00d8cf713699eac823483cd789835677a0bdeb4bc1f665757d485894ee67f4d`
- `gemini_login.rs`: `f9b3071accc1a2bd101d30d304f5095346ed59a08f0fd91786ed960fb78b5f94`
- `commands/auth.rs`: `4da398d59206b97f887214f0fc369a3e6dbf5453ae6c7273f72ff956e6a66657`
- `LoginModal.tsx`: `ebcc340cbaaf5075960704a1a83e5d147261e9266c75342918bbb33c66b39b22`
