# Gemini Auth 账号设计

状态：已确认（2026-09-18 对话评审）

范围：在现有 Auth Provider 体系中新增 Gemini CLI / Code Assist 账号，使个人 Google 登录额度可以参与 `/v1/chat/completions`、`/v1/messages`、`/v1/responses` 路由。

本文只设计 v1。渠道 Google OpenAI 兼容预设、旧 `gemini_native` override、应用配置里的 Gemini CLI `.env` 写入均不在本期。

---

## 1. 已确认的产品决策

| 决策 | 选择 |
|---|---|
| 能力形态 | Auth 账号，对齐 Codex / Kimi，不是第四个下游协议，也不是渠道预设替换 |
| 登录 | 应用内 Google 浏览器 OAuth（loopback + PKCE） |
| 导入 | 用户显式选择文件：`~/.gemini/oauth_creds.json` 或 CLIProxyAPI 包装格式 |
| 下游协议 | Chat + Messages + Responses 全开 |
| GCP 项目 | 登录/导入后自动 `loadCodeAssist` / `onboardUser`，不读进程 `GOOGLE_CLOUD_PROJECT` |
| 上游 | 只打 Cloud Code Assist，不打 `generativelanguage.googleapis.com` |

### 非目标

- Vertex AI、AI Studio API Key（继续走渠道）
- 下游暴露 `/v1beta/models/{model}:generateContent`
- RFC 8628 设备码、Gemini CLI 粘贴授权码（`NO_BROWSER`）
- 凭据导出、额度面板
- 把转换写进 Provider 或复用 `adaptor/gemini.rs`
- 新数据库 migration（`provider` 无 CHECK，与 Kimi 相同）
- 改渠道表单或应用配置 Gemini CLI 写入

---

## 2. 架构

### 2.1 为什么不能走渠道 Google 预设

渠道预设 `openai:google` 使用：

```text
https://generativelanguage.googleapis.com/v1beta/openai
Authorization: Bearer <API Key>
```

Gemini CLI「Sign in with Google」的 token scope 是 `cloud-platform` + userinfo。该 token 打公开 Gemini / OpenAI 兼容面会 403（ACCESS_TOKEN_SCOPE_INSUFFICIENT）。官方出站是：

```text
POST https://cloudcode-pa.googleapis.com/v1internal:generateContent
POST https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse
```

信封由 Gemini CLI `packages/core/src/code_assist/converter.ts` 定义：`{ model, project?, user_prompt_id?, request: VertexGenerateContentRequest }`。响应为 `{ response?: VertexGenerateContentResponse, traceId? }`。

### 2.2 数据流

```text
登录 / 导入
  -> commands/auth.rs
  -> AuthService -> GeminiProvider
  -> Google OAuth 或 oauth_creds.json
  -> userinfo + loadCodeAssist / onboardUser
  -> auth_accounts（payload=token，attributes=email/project/tier）

下游 Chat / Messages / Responses
  -> security gate
  -> RoutePlan（Gemini 账号进 Conversion 组）
  -> CodecRegistry：下游 -> Chat -> Gemini 内层 GenerateContentRequest
  -> GeminiProvider::outbound 包信封、Bearer、打 v1internal
  -> 401 -> AuthService 强制刷新并只重试同一账号一次
  -> 解码信封 -> Gemini -> Chat -> 下游
  -> request_logs(upstream_type=auth_account)
```

### 2.3 模块边界

| 模块 | 职责 | 不做什么 |
|---|---|---|
| `auth_provider/gemini_login.rs` | OAuth PKCE、导入两种 JSON、刷新 token | 协议转换、路由 |
| `auth_provider/gemini_backend.rs` | `Provider` 实现、onboarding、出站 HTTP、静态模型目录 | 编解码 body |
| `auth_provider/spec.rs` | renderer-safe 能力表 | wire URL |
| `protocol/codec` | Chat ↔ Gemini；Messages/Responses 经 Chat 组合 | 鉴权、project 信封 |
| `core/route_plan.rs` | 固定 Gemini wire profile | 解析 `payload_json` |
| `core/protocol_boundary.rs` | `(Gemini, generate_content)` → `Protocol::Gemini` | |
| `endpoint_executor` | 走现有 Auth Json framing 路径 | 猜测 framing |
| `commands/auth.rs` + Auth 页 | 无秘密 DTO、登录/导入 UI | 读取 token |

`payload_json`：`access_token`、`refresh_token`、`expires_at`（RFC3339）。
`attributes_json`：`email`、`project_id`、`user_tier`、`user_tier_name`。路由层不解析 token。

---

## 3. 登录、导入、onboarding

### 3.1 ProviderSpec

```text
kind                = gemini
display_name        = Gemini
icon_key            = google
login_mode          = browser_callback
login_methods       = [browser_callback]
supports_import     = true
supports_export     = false
supports_quota      = false
```

`ProviderKind::Gemini` 加入 `From<&str>`、`auth_providers_list` 和 `commands/auth.rs` 白名单。

### 3.2 浏览器 OAuth

OAuth client material 不写入仓库，由部署环境注入：

```text
WALIAPI_ANTIGRAVITY_CLIENT_ID     = 由部署环境提供
WALIAPI_ANTIGRAVITY_CLIENT_SECRET = 由部署环境提供
authorize     = https://accounts.google.com/o/oauth2/v2/auth
token         = https://oauth2.googleapis.com/token
scopes        = cloud-platform, userinfo.email, userinfo.profile
```

流程对齐 Codex，比 CLI web 登录更严：

- 绑定 `127.0.0.1` 随机可用端口，回调 `/oauth2callback`
- PKCE S256、`state`、`access_type=offline`
- `LoginRuntime::open_browser`，5 分钟超时，可取消
- 成功/失败可重定向到 Google 的 `auth_success_gemini` / `auth_failure_gemini`

不实现粘贴授权码。测试注入 mock authorize/token URL，禁止打真实 Google。

### 3.3 导入

只接受用户显式选中的文件，不扫描家目录。

1. Gemini CLI：顶层 `access_token` / `refresh_token` / `expiry_date`（毫秒）或 `expiry`
2. CLIProxyAPI：`token` 对象 + 可选 `email` / `project_id`

没有 refresh token 则拒绝。导入后本机 Gemini CLI 登录态不改，notice 对齐 Codex。

### 3.4 Onboarding

登录和导入共用。**不**读取进程环境变量 `GOOGLE_CLOUD_PROJECT`（个人账号会被劫持）。

1. `GET https://www.googleapis.com/oauth2/v2/userinfo` → email 作为 `account_id`（唯一键 `(provider, account_id)`）
2. `POST .../v1internal:loadCodeAssist`
3. 无 `currentTier` 则 `onboardUser`：FREE 不带 project（官方 CLI：带 project 会 Precondition Failed）；其它 tier 才带
4. 未完成 LRO 按 CLI 间隔（5s）轮询 `getOperation`
5. 将 `cloudaicompanionProject`、tier 写入 attributes

Workspace 拿不到 project：失败并说明需配置项目，不编造 ID。
`VALIDATION_REQUIRED`：失败并带验证链接，v1 不做交互验证器。
导入文件已有 `project_id` 时仍跑 loadCodeAssist；冲突以服务器返回为准，文件值只作 onboard 请求的候选。

### 3.5 刷新

Google token 端点，`grant_type=refresh_token`。新 refresh token 若返回则轮换。过期前懒刷新；出站 401 时 `AuthService` 持账号锁强制刷新并只重试一次。失败则 `Unauthorized`，账号退出路由池并进入 maintenance backoff。

---

## 4. Codec

### 4.1 协议矩阵

在 `protocol/codec::Protocol` 增加 **仅上游** 的 `Gemini`。不新增下游 HTTP 路径。

只新写 **Chat ↔ Gemini**。Messages、Responses 先转到 Chat，再转到 Gemini，组合方式对齐 `messages_to_responses_v2`。

| 下游 | 上游 | Codec |
|---|---|---|
| Chat | Gemini | `chat_to_gemini_v1` |
| Messages | Gemini | Messages→Chat→Gemini |
| Responses | Gemini | Responses→Chat→Gemini |

`CodecRegistry::direction` 与 `prepare_pair` 增加上述 pair。未知 pair 仍 fail closed。

### 4.2 内层 vs 信封

Codec 只编/解 Vertex 内层：

```json
{
  "contents": [{ "role": "user", "parts": [{ "text": "..." }] }],
  "systemInstruction": { "parts": [{ "text": "..." }] },
  "tools": [{ "functionDeclarations": [] }],
  "generationConfig": { "temperature": 0.2, "maxOutputTokens": 1024 }
}
```

Provider 负责信封：`model`、`project`（来自 attributes）、`user_prompt_id`。解码器同时接受 `{ "response": { "candidates": ... } }` 和未包装的 Vertex 响应。

### 4.3 v1 映射

必须转换：

- 文本、system → `systemInstruction`
- `temperature` / `max_tokens`
- Chat tools ↔ `functionDeclarations` / `functionCall` / `functionResponse`
- data URI 图片 ↔ `inlineData`
- Gemini `thought` part ↔ 现有 thinking 字段（对不上则沿用当前 fail-open 规则，不得把未知 finish reason 改成 `stop`）

编码后丢掉 `parts` 为空的 content（`v1internal` 会 400）。

零上游调用拒绝：音频/视频、非 data URI 远程图片、Gemini 出图/语音、无法表示的 Responses 文件/计算机工具。

### 4.4 流式

上游 `streamGenerateContent?alt=sse`。每个 SSE JSON 解成 Gemini chunk，再编成 Chat SSE；Messages / Responses 复用现有 Chat 解码器。非流式 JSON，framing = `Json`。

不复用 `adaptor/gemini.rs`。渠道 Google OpenAI 兼容面继续走 Chat，不经此 codec。

---

## 5. RoutePlan 与出站

### 5.1 固定 profile

```text
provider            = gemini
native_base_url     = https://cloudcode-pa.googleapis.com
upstream_protocol   = gemini
upstream_endpoint   = generate_content
non_stream_framing  = json
```

`classify_auth_account`：Gemini 对 Chat/Messages/Responses 都不是 native，一律 Conversion。CountTokens / Embeddings 仍不形成账号组。

`protocol_boundary::upstream_protocol(Gemini, "generate_content")` → `Protocol::Gemini`。其它 endpoint 字符串 fail closed。

### 5.2 Executor

走现有 `dispatch_auth_account_executor` / `dispatch_auth_account_stream_executor` 的 Json 分支（与 Kimi 相同），不走 Codex `ForcedResponsesSse` 聚合。

`GeminiProvider::outbound` 只允许 `(gemini, generate_content)`。按 `is_stream` 选择：

```text
POST {base}/v1internal:generateContent
POST {base}/v1internal:streamGenerateContent?alt=sse
```

`Authorization: Bearer <access_token>`。缺少 `project_id` 时：个人 FREE 已 onboard 仍应有 project；若 attributes 为空则 Protocol 错误，不发请求。User-Agent 使用固定 Gemini CLI 兼容值（常量，测试可覆盖 base URL）。

语义失败（安全拦截、空 candidates 且 promptFeedback 阻断）映射为现有 `FailureClass`，不得把上游 401 伪装成本地 API Key 错误。

### 5.3 模型快照

Code Assist 没有可用的 OpenAI 式 `/models`。`list_models` 写入静态目录（来源：Gemini CLI 模型页 / Google 模型目录），每条 `protocol=gemini`、`status=available`。建议初始集合：

- `gemini-2.5-flash`
- `gemini-2.5-pro`
- `gemini-3-flash-preview`
- `gemini-3.6-flash`

同步失败不覆盖旧快照。空快照不能进路由。别名仍走现有 `model_mapping_json`；混合 profile 在 Gemini 上不存在（只有一种 wire），映射目标必须都在快照中。

---

## 6. UI

只扩展 Auth 渠道页，不引入新状态库。

- `ProviderPills`：后端列表多 Gemini；图标用 `google`
- `LoginModal`：复用 Codex 浏览器六步文案（准备回调 → 打开浏览器 → 等待 → 换 token → 保存 → 同步模型）
- 空状态：`supportsImport` 时显示导入，不再用 `isKimi` 硬编码
- 导入格式增加 `gemini`（oauth_creds.json）；Codex 的 sub2api/cpa 不解析 Gemini 文件
- 账号卡片：email、project_id、tier；隐藏额度块
- 规划中 pills 保持 Claude / Kiro，不把 Gemini 标成规划中

Headless Web 管理面走同一套 `auth_providers_list` / invoke 命令。无桌面 opener 时浏览器登录失败并提示在桌面端完成或改用导入。

---

## 7. 测试与验证

全部 mock，禁止真实 Google。

| 层 | 覆盖 |
|---|---|
| `gemini_login` | PKCE 回调成功/state 不匹配/超时/取消；导入两种 JSON；无 refresh 拒绝 |
| onboarding | FREE onboard 不带 project；Workspace 无 project 失败；VALIDATION_REQUIRED 带链接 |
| codec | Chat 文本/tools/图片 roundtrip；空 parts 被丢掉；不支持媒体 400 且零上游；Messages/Responses 组合 |
| route_plan | Gemini 账号三端点都在 conversion 组；未知 endpoint 丢弃；空模型快照不可路由 |
| executor | Json 非流/流式；信封包装；401 只刷新重试一次 |
| 回归 | Codex / Kimi 登录、导入、路由测试保持绿 |

命令：

```bash
cd src-tauri && cargo test gemini
cd src-tauri && cargo test auth
cd src-tauri && cargo test protocol
pnpm build   # tsc
```

---

## 8. 兼容与回滚

- 不改 schema；未注册 `gemini` 的旧二进制会把该行看成 `ProviderKind::Other` 并拒绝命令，路由因无 profile 丢弃账号（fail closed）
- 渠道 `type=gemini` legacy override 与 `openai:google` 预设行为不变
- 回滚：去掉 registry 条目即停止新登录；已写入的 `provider=gemini` 行不再被路由

---

## 9. 参考

- Gemini CLI OAuth：`packages/core/src/code_assist/oauth2.ts`
- Onboarding：`packages/core/src/code_assist/setup.ts`
- 信封：`packages/core/src/code_assist/converter.ts`
- 端点：`packages/core/src/code_assist/server.ts`（`CODE_ASSIST_ENDPOINT`）
- 本仓库：`docs/superpowers/plans/2026-08-16-kimi-auth.md`、`docs/auth-codex/work/02-design.md`
- 明确不做：OAuth token 打 `generativelanguage.googleapis.com`（CLIProxyAPI #637 同类失败）
