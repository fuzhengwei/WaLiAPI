# Gemini Auth Implementation Plan

> **For agentic workers:** 按任务顺序实施。每个任务先写失败测试，再改实现，再跑该任务列出的验证命令。不要把 Gemini 协议转换写进 Provider；Chat / Messages / Responses 转换必须走 `CodecRegistry`。不要提交 git，除非用户明确要求。

**Goal:** 新增 Gemini CLI / Code Assist Auth 账号：浏览器 OAuth、导入 `oauth_creds.json`、自动 onboarding，并使该账号参与 Chat / Messages / Responses 路由。

**Architecture:** 沿用 `Provider` / `AuthService` / `auth_accounts`。`GeminiProvider` 只负责 OAuth、刷新、Code Assist HTTP 信封。上游固定 `cloudcode-pa.googleapis.com/v1internal:generateContent`。新增 codec `Protocol::Gemini`（仅上游）；Messages / Responses 经 Chat 组合。

**Tech Stack:** Rust、Tokio、Reqwest、oauth2、Axum mock、现有 CodecRegistry、React Auth 页。

**Design:** `docs/plans/2026-09-18-gemini-auth-design.md`

---

## 0. 硬编码必须解开

| 位置 | 现状 | 目标 |
|---|---|---|
| `auth_provider/types.rs` | ProviderKind 只有 Codex/Kimi | 加 Gemini |
| `auth_provider/spec.rs` | REGISTERED = Codex+Kimi | 加 Gemini spec |
| `auth_provider/mod.rs` | registry 只注册两个 | 注册 GeminiProvider |
| `commands/auth.rs` `provider_kind` | 只接受 Codex/Kimi | 接受 Gemini |
| `protocol/codec/types.rs` Protocol | Chat/Messages/Responses | 加 Gemini（仅上游） |
| `protocol/codec/registry.rs` direction | 三协议矩阵 | Chat/Messages/Responses ↔ Gemini |
| `core/route_plan.rs` profile_for_model_state | 只认 codex/kimi | 加 gemini 固定 profile |
| `core/protocol_boundary.rs` | 无 Gemini pair | `(Gemini, generate_content)` |
| `protocol/codec/identity.rs` parse_usage | 三协议 | Gemini 读 usageMetadata |
| Auth UI | Codex/Kimi 硬编码 | supportsImport / icon / 无额度无导出 |

---

## Task 1: ProviderKind + Spec + Registry

**Files:**
- Modify: `src-tauri/src/auth_provider/types.rs`
- Modify: `src-tauri/src/auth_provider/spec.rs`
- Modify: `src-tauri/src/auth_provider/mod.rs`（先 stub GeminiProvider，Task 2/3 填满）
- Modify: `src-tauri/src/commands/auth.rs` `provider_kind`

**Tests:** `types.rs` Gemini round-trip；`spec.rs` 精确值（login_mode=browser_callback，import true，export/quota false）；`registered_specs` 长度为 3。

**Stub:** `gemini_backend.rs` 实现 `Provider`：login/import/refresh/outbound/list_models 先返回 `UnsupportedFeatures` 或固定静态模型，保证 registry 能编译。

验证：`cd src-tauri && cargo test provider_kind -- --nocapture` 与 `cargo test spec::tests`

---

## Task 2: Import + Refresh

**Files:** Create `src-tauri/src/auth_provider/gemini_login.rs`

**行为：**
- 导入 CLI 格式：顶层 `access_token`/`refresh_token`/`expiry_date`（毫秒）或 `expiry`
- 导入 CPA 格式：`token` 对象 + 可选 `email`/`project_id`
- 无 refresh_token → `InvalidPayload`
- 刷新：`POST {token_url}` `grant_type=refresh_token`；401/invalid_grant → Unauthorized；5xx → Retryable
- `expires_at` UTC RFC3339
- 测试用 `GeminiLogin::with_endpoints`

验证：`cargo test --manifest-path src-tauri/Cargo.toml gemini_login`

---

## Task 3: Browser OAuth

复用 Codex loopback 形状，端口随机 `127.0.0.1:0`，回调 `/oauth2callback`，PKCE S256，scopes 三条 Google CLI scope，`access_type=offline`。5 分钟超时，runtime 取消。mock authorize 页把浏览器重定向到 callback。测试禁止打真实 Google。

验证：`cargo test --manifest-path src-tauri/Cargo.toml gemini_login::tests`

---

## Task 4: Onboarding + Outbound + Models

**Files:** `src-tauri/src/auth_provider/gemini_backend.rs`

登录/导入成功后：
1. GET userinfo → email 作 `account_id` + `attributes.email`
2. POST `{base}/v1internal:loadCodeAssist`
3. 无 currentTier → onboardUser（FREE 不带 project）并轮询 LRO
4. 写入 `project_id`、`user_tier`（展示用 `plan_type`）

`outbound` 只允许 `(gemini, generate_content)`。把 codec 内层包成：

```json
{ "model": "<upstream_model>", "project": "<project_id>", "request": <encoded_body> }
```

`is_stream=false` → `POST /v1internal:generateContent`
`is_stream=true` → `POST /v1internal:streamGenerateContent?alt=sse`
`Authorization: Bearer`

`list_models` 返回静态目录（protocol=`gemini`）：`gemini-2.5-flash`、`gemini-2.5-pro`、`gemini-3-flash-preview`、`gemini-3.6-flash`。

`with_endpoints(code_assist_base, login)` 供 mock。

验证：`cargo test --manifest-path src-tauri/Cargo.toml gemini_backend`

---

## Task 5: Chat ↔ Gemini codec

**Files:**
- Create `src-tauri/src/protocol/codec/gemini/{mod,encode,decode,stream,tests}.rs`
- Modify `types.rs` Protocol + CodecId
- Modify `identity.rs` usageMetadata
- Modify `registry.rs` Chat↔Gemini
- Modify `protocol/codec/mod.rs` 导出

**encode Chat→Gemini 内层（无 project 信封）：**
- system → systemInstruction.parts[].text
- user/assistant → contents role user/model
- tool role → functionResponse
- assistant tool_calls → functionCall
- tools → functionDeclarations
- temperature / max_tokens → generationConfig
- data URI image → inlineData
- 丢掉空 parts
- 音频/远程 URL 图片 → UnsupportedFeatures

**decode：** 同时认 `{response:{candidates}}` 与未包装 candidates。text + functionCall → Chat message。finishReason：STOP→stop，MAX_TOKENS→length，SAFETY 不降级为 stop。usageMetadata → prompt/completion tokens。

**stream：** SSE JSON chunk → Chat SSE delta；finish 后 `[DONE]`。

验证：`cargo test --manifest-path src-tauri/Cargo.toml codec::gemini`

---

## Task 6: Messages / Responses 组合

**Files:** `protocol/codec/gemini/compose.rs` 或 registry 内 FnDirection

- Messages→Gemini：`encode_messages_to_chat` 再 `encode_chat_to_gemini`；解码 Gemini→Chat→Messages
- Responses→Gemini：`responses_to_openai` 再 Chat→Gemini；解码 Gemini→Chat→`openai_to_responses`

CodecId：`MessagesToGeminiV1`、`ResponsesToGeminiV1`。

验证：`cargo test --manifest-path src-tauri/Cargo.toml prepare_pair` 相关 + gemini compose 测试

---

## Task 7: RoutePlan + protocol_boundary

**Files:**
- `src-tauri/src/core/route_plan.rs`：`UpstreamProtocol::Gemini`；`profile_for_model_state("gemini", _)`
- `src-tauri/src/core/protocol_boundary.rs`

profile：

```text
native_base_url     = https://cloudcode-pa.googleapis.com
upstream_protocol   = gemini
upstream_endpoint   = generate_content
non_stream_framing  = json
```

三下游端点都进 Conversion 组。空快照不可路由。

验证：`cargo test --manifest-path src-tauri/Cargo.toml route_plan` 中 gemini 用例；`cargo test protocol_boundary`

---

## Task 8: Executor 接线

Json framing 已覆盖 Kimi。补：
- `semantic_failure` 识别 Gemini promptFeedback 阻断
- executor 集成测试：mock Code Assist，Chat 非流/流式经 auth account
- 401 刷新一次（复用 AuthService）

验证：`cargo test --manifest-path src-tauri/Cargo.toml endpoint_executor gemini`

---

## Task 9: Commands + UI

**Files:**
- `commands/auth.rs`：白名单 + DTO 增加可选 `project_id`（从 attributes）
- `src/types/index.ts` AuthProviderId
- `ProviderPills.tsx` google 图标
- `LoginModal.tsx` Gemini 用浏览器六步
- `AuthChannelsPage.tsx` fallback provider 信息；导入格式 `gemini`
- `AccountCard.tsx`：Gemini 隐藏额度与导出；展示 project（可用 plan_type/tier）
- `AccountList.tsx` 若有 provider 硬编码一并改

验证：`pnpm build`（tsc）

---

## Task 10: 回归

```bash
cd src-tauri && cargo test auth
cd src-tauri && cargo test protocol
cd src-tauri && cargo test route_plan
pnpm build
```

Codex / Kimi 测试必须绿。禁止真实 Google 网络。

---

## 完成定义

- 浏览器登录与两种 JSON 导入可创建 `provider=gemini` 账号
- 账号可被 Chat / Messages / Responses 路由（Conversion）
- 出站只打 Code Assist `v1internal`
- 渠道 Google OpenAI 预设与 legacy `gemini_native` 不变
- 无新 migration
