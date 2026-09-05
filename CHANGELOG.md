# Changelog

## v0.2.8 (2026-09-03)

### Codex 账号切换能力

- ✨ **应用配置 Codex 切换账号**：应用配置页 Codex 卡片支持「切回原账号」操作，检测 `auth.json` 是否处于 API Key 模式（`auth_mode == "apikey"` 或 `OPENAI_API_KEY` 非空且无 ChatGPT 登录态），若卡在 API Key 模式则提示并提供「重置 auth.json 为 ChatGPT 登录模式」命令——备份原 `auth.json` 为 `auth.json.waliapi-backup`，重置为 `chatgpt` 模式后用户运行 `codex login` 重新授权
- ✨ **配置恢复 absent 标记**：写入网关配置前检测原配置是否存在，若不存在则打 `.waliapi-absent` 标记文件，恢复时删除写入的配置而非尝试恢复不存在的备份，避免「恢复原配置」变成「恢复成网关配置」死循环
- 🔧 **Codex 配置状态文案优化**：Codex 卡片已配置状态显示「已切换到网关」，恢复按钮显示「切回原账号」并附 tooltip 说明 `auth.json` 不被改动

### 流式稳定性增强

- 🔧 **流式空闲超时守卫**：SSE 流式转发新增 5 分钟空闲超时（`STREAM_IDLE_TIMEOUT`），上游长时间无数据（半开连接 / 上游静默挂死）时主动断开并向下游发送协议错误事件，不再无限等待
- 🔧 **首帧诊断信息增强**：`buffer_first_record` 返回诊断信息（收到的字节数 + 内容预览），审计日志可区分「空响应」「非 SSE JSON 错误体」「HTML 拦截页」等场景，而非统一显示「stream ended before a valid first SSE record」
- 🔧 **上游 Retry-After 遵从**：解析上游响应的 `Retry-After` 头（支持 delta-seconds 和 RFC 7231 IMF-fixdate），在重试前等待指定时间（上限 5 秒 + ±20% jitter），避免密集重试触发上游限流

### 其他

- ✅ 新增 19 个单元测试覆盖配置恢复、Codex auth.json 检测/重置、流式空闲超时、首帧诊断、Retry-After 解析等场景
- 📝 **README 贡献者数据同步**：新增 2 位贡献者 Jason（@freakojc，PR #62 #63）和 cham（@Cham1229，PR #64），按最新提交记录更新全体贡献者提交数与代码变更统计
- 🔧 **版本号统一升级至 0.2.8**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.2.7 (2026-09-02)

### 仪表盘

- ✨ **服务可用率纳入 Auth 账号**：仪表盘「服务可用率」统计口径由「活跃渠道 / 总渠道」扩展为「活跃上游 / 全部上游」——上游包含 API 渠道（`status = 1`）与 Auth 账号（未禁用且凭证有效）两类，仅接入 Auth 账号时可用率不再虚低；「活跃渠道」卡片升级为「活跃上游」，主值显示合计，副文案拆分展示渠道与账号明细；Tauri 桌面端与 waliapi-web 管理端共用同一统计 DTO，同步生效

### 修复

- 🐛 **RAG/Wiki 设置保存后状态未即时更新**：知识库设置页保存后仍展示旧状态（原实现保存后整表刷新但未同步选中项）；重构为选中态仅存 `selectedKbId`、由列表数据派生选中对象，保存成功后用接口返回值精准更新列表对应项，设置页即时展示最新配置（PR #60）
- 🐛 **Codex Responses 请求 strip `prompt_cache_options`**：Codex 后端 `validate_backend_request` 白名单有 `prompt_cache_key` 却缺配套的 `prompt_cache_options`，WaLiCode 走 Responses 协议必带该字段（值为 `{"mode":"implicit"}`），导致整条 Responses 路径被 `HTTP 400` 拒绝、只能退回 Chat 协议；该字段仅作缓存提示、不携带后端请求语义，归入 STRIPPED 静默丢弃，与 Chat 路径行为对齐（PR #59）

### 其他

- 📝 **README 贡献者数据同步**：按最新提交记录更新贡献者提交数与代码变更统计
- 🔧 **版本号统一升级至 0.2.7**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.2.6 (2026-09-02)

### 缓存命中 Token 统计

- ✨ **缓存命中 Token 全链路记录**：请求日志新增 `cached_tokens` 字段（migration 026），适配器层（OpenAI / Claude / DeepSeek / Gemini / Custom）统一提取上游缓存命中用量，兼容 `cached_tokens`、`cache_read_input_tokens`、`prompt_cache_hit_tokens` 等多种上游字段格式
- ✨ **仪表盘缓存统计**：新增今日/累计缓存 Token、Prompt Token 指标，模型统计与 Token 趋势图增加缓存维度，API 密钥统计同步支持缓存 Token
- ✨ **日志页缓存与推理强度展示**：日志列表与详情展示缓存命中 Token 及 `reasoning_effort` 字段（migration 027），流式 SSE 同步累积缓存用量

### Auth 账号错误透传（Kimi 渠道场景）

- ✨ **Auth 账号终态错误透传真实状态码**：Auth 账号（OAuth 登录，如 Kimi Code）的凭证属于用户本人，上游 401/403 不再统一脱敏为 502，保留真实状态码，让调用方知道重新登录即可恢复；渠道 Key 的终态失败仍保持 502 脱敏（渠道凭证问题不暴露给调用方）
- 🔧 **故障转移语义不变**：新增 `failure_from_auth_upstream`，仅调整 Auth 账号的状态码透传，FailureClass 分类不变，组内转移、不跨组的语义与渠道一致
- 🔧 **错误响应增加 `failure_class` 字段**：错误响应 body 新增 `failure_class`，便于客户端区分失败类型并做针对性处理
- ✅ 配套 AttemptFlow 真值测试：Auth 账号 401 透传 / 渠道 502 脱敏两条路径

### 修复

- 🐛 **Responses API 流式内容累积修复**：Anthropic 事件分支的无条件 `continue` 导致 Responses 流式事件累积代码不可达，流式 Responses 请求的响应内容从未被记录；重构为 Anthropic / Responses 统一 match 分发，并补上 `response.function_call_arguments.delta` 工具调用参数累积
- 🐛 **pdfium macOS 打包路径修复**：`bundle.resources` 的 glob 前缀使 pdfium 被打入 `Contents/Resources/resources/pdfium/`，运行时仅搜索 `Contents/Resources/pdfium/` 导致 `OCR_RENDER_FAILED`，补上该落点，不改打包与签名（PR #56）
- 🐛 **Wiki Unicode 文本进程崩溃修复**：`ingest_wiki_source` / `search_wiki` 在 Unicode 文本上按字节下标切片触发 `core::str::slice_error_fail` panic，release `panic=abort` 配置下导致整个 WaLiAPI 进程退出；新增 `utils/text.rs` 字符边界安全切片工具，覆盖 wiki ingest / repository / security scanner 路径（PR #58）

### 其他

- 🔧 **默认窗口尺寸调整**：1280×860 → 1440×900，适配仪表盘新增指标
- 🔧 **版本号统一升级至 0.2.6**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.2.5 (2026-09-01)

### Docker Web 部署

- ✨ **Docker Web 部署完善**：Docker 镜像部署流程优化，README 新增 Web 部署教程章节，涵盖 Docker run / Docker Compose / systemd 三种部署方式

### 核心重试与错误处理统一

- 🔧 **统一上游重试判定决策函数**：抽取各路径分散的重试逻辑为统一决策函数，覆盖全部适配器与 handler 路径，配套真值表测试确保判定准确性
- 🐛 **上游终态错误立即短路**：401/403 等终态错误不再轮询渠道，直接返回客户端，避免无效重试消耗时间
- 🐛 **401/403 下游脱敏**：上游返回 401/403 时，下游响应中脱敏处理错误信息，不泄露上游凭证状态
- 🐛 **数据库故障不再误报 401**：数据库连接异常时不再误返 `401 Invalid API key`，返回正确的 503 服务不可用
- 🐛 **Anthropic 内置工具 400 修复**：Anthropic 内置工具（如 web_search）经 OpenAI Chat 渠道转发时不再整体返回 400

### 知识库 VLM OCR

- ✨ **扫描版 PDF VLM OCR**：知识库支持扫描版 PDF 文档的 VLM（视觉语言模型）OCR 识别，自动检测扫描页面并调用 VLM 进行文字提取
- ✨ **OCR 页级混合识别**：逐页检测是否为扫描页，扫描页走 VLM OCR、文本页走常规提取，混合模式兼顾精度与速度
- ✨ **OCR/Embedding 模型下拉按用途过滤**：知识库配置中 OCR 和 Embedding 模型下拉框按模型用途分类过滤，避免选错模型类型
- 🐛 **Claude 渠道协议适配修复**：修复 Claude 渠道在 OCR 场景下的协议适配问题

### 协议 Codec 加固

- 🐛 **Codex 工具调用参数一次性下发**：修复部分客户端在 Codex 工具调用流式传输中截断参数的问题，改为一次性下发完整参数
- 🐛 **Chat-to-Responses store 字段归一化**：Chat 请求转 Responses 格式时归一化 `store` 字段，避免字段缺失或不一致导致的兼容性问题

### UI 优化

- 🔧 **边框样式优化**：优化界面边框视觉样式

- 🔧 **版本号统一升级至 0.2.5**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.2.4 (2026-08-28)

- ✨ **Auth 账号多格式导入**：支持 Codex、sub2api、CPA 三种格式批量导入，导入下拉抽取为共享组件，空状态卡片复用
- ✨ **sub2api 格式兼容**：兼容 `chatgpt_account_id` 键名映射
- ✨ **模型列表增加 Auth 类型**：`/v1/models` 接口返回结果新增 Auth 账号类型模型
- ✨ **Codex 卡片信息增强**：卡片同时显示 5H 与周限额信息，操作按钮收为一行
- 🔧 **版本号统一升级至 0.2.4**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.2.3 (2026-08-26)

- 🐛 **`/v1/models` 补全 Auth 账号模型**：模型列表接口此前仅聚合启用渠道（Channel）的 `models` 与 `model_mapping`，未包含 `auth_accounts` 登录账号同步的模型，导致「能路由却列不出」。现合并 auth 账号模型快照中 `available` 且未 `unavailable` 的条目及其 `model_mapping` 源别名，与渠道模型统一去重（渠道优先，`owned_by` 归属渠道；账号模型 `owned_by` 为 provider），OpenAI / Anthropic 两种响应格式均生效
- 📝 **README 文档完善**：更新代码贡献者信息表，补齐 v0.2.2 Docker / Web 管理面板贡献者 Fla1337，同步各贡献者最新提交量与代码变更统计
- 🔧 **版本号统一升级至 0.2.3**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.2.2 (2026-08-26)

### Web 管理面板（Docker / headless 部署）

- ✨ **Linux headless 服务器部署**：新增 `waliapi-web` 二进制（无桌面窗口），支持 Docker 和 systemd 两种部署方式，适合放在 Linux 服务器上长期运行
- ✨ **Web 管理面板**：浏览器访问完整管理界面，与桌面版业务能力一致——仪表盘、渠道管理、密钥管理、日志审计、安全规则、知识库、Wiki、MCP、导入导出、应用配置等
- ✨ **多阶段 Docker 构建**：Node/pnpm 编译前端 → Rust 编译 `waliapi-server` → 运行时使用非 root 用户，SQLite 数据持久化到 `/data`
- ✨ **GitHub Actions 发布**：推送 `web-v*` 标签自动创建 Release、上传二进制包、发布 Docker 镜像到 GHCR
- ✨ **systemd 部署支持**：提供 systemd unit 文件和环境变量配置示例，适合不用 Docker 的场景
- ✨ **Web 管理面板用户设置**：支持修改管理员用户名和密码
- 🔧 **桌面版自动启动内嵌服务**：移除"随应用启动内嵌服务"开关，桌面版启动后自动运行 HTTP 服务
- 🔧 **后端重构分离桌面版与 Web 服务**：同一 Rust 代码库编译出桌面版（Tauri 窗口）和 headless 版（纯 HTTP 服务）

### Web 适配层修复

- 🐛 **`api.ts` 绕过 runtime 适配层**：`api.ts` 直接用 `@tauri-apps/api/core` 的 `invoke`，浏览器环境无 Tauri IPC 全部失败，改为统一走 `runtime.ts` 适配层
- 🐛 **`runtime.ts` 请求路径和格式不匹配后端**：修正 fetch 路径（`/api/admin/invoke` → `/admin/api/invoke`）、body 字段名（`command` → `cmd`）、响应解析逻辑、补齐 CSRF 头（`X-Requested-With`）、SSE 路径同步修正
- 🐛 **`default-run` 缺失导致 `cargo run` 报错**：`Cargo.toml` 有两个 binary（`waliapi` + `waliapi-web`），未设 `default-run`，补上 `default-run = "waliapi"`

### 流式请求超时修复（502 问题）

- 🐛 **流式请求被总超时掐断**：`reqwest` 的 `.timeout()` 是整个请求总超时（含 SSE 传输），大量对话时 LLM 生成时间超过 `timeout_secs`（默认 60s）连接被掐断，客户端收到 502
- 🔧 **分离流式/非流式超时策略**：新增 `streaming_client()`（仅 `connect_timeout` 10s，不设总超时）和 `blocking_client()`（`connect_timeout` + 总超时 `timeout_secs`），流式请求不再受总超时限制
- 🔧 **全链路覆盖**：5 个 adaptor（openai/claude/deepseek/gemini/custom）的 `forward_stream` + `endpoint_executor` + `handlers.rs` 的 `openai_messages_request` / `native_anthropic_request` + embeddings 全部切换到对应 client

### 模型映射编辑修复

- 🐛 **模型映射编辑输入丢失**：`useModelMappings` 的 `useEffect([initial])` 在每次 prop 变化时重置内部状态，`pairsToMapping` 丢弃 from/to 为空的不完整行后，`onChange → 父组件更新 → prop 变化 → useEffect 重置` 的循环把用户正在输入的数据吃掉。引入 `skipNextSyncRef` + `markSynced()` 跳过内部变更的 round-trip

### Codec 加固

- 🔧 **Chat store/stream_options 归一化**：归一化 Chat 请求的 `store` 和 `stream_options` 字段，合批 Responses 工具调用与 easy input
- 🐛 **thinking none/off 映射修复**：thinking 设为 none/off 时映射为 adaptive + low effort，不再报错
- 🐛 **`--help` 参数路由修复**：`--help` 在参数路由前拦截，恢复正常帮助文本和退出码 0

### Docker 构建修复

- 🐛 **Rust 基础镜像升级**：rust 1.88 → 1.96，notify-rust@4.18 要求 rustc ≥ 1.89
- 🐛 **Dockerfile.tp 兼容国内镜像**：新增国内镜像源构建变体，去掉 syntax 指令（tp 网络到不了 auth.docker.io）
- 🔧 **tauri.conf.json 显式指定 mainBinaryName**：修复构建时 binary 名称不确定的问题

### 其他

- 版本号统一升级至 0.2.2（package.json / Cargo.toml / tauri.conf.json）
- Cargo.toml 添加 `default-run = "waliapi"`

---

## v0.2.1 (2026-08-18)

### 协议转换层结构化重构

- 🔧 **protocol 模块目录化**：将 protocol 根转换逻辑拆分为独立子模块——codec/chat、codec/messages、codec/responses_codec、directions（messages_to_responses / responses_to_messages），每个方向独立 encode/decode/stream/test，消除 1500 行巨型文件
- 🔧 **死代码清理与 API 收敛**：清理 protocol 模块遗留 API 和死代码，clippy 告警归零，完成模块结构与 re-export 审计
- 🔧 **codec 加固**：移植 tool-call 回放保留空 reasoning_content 兼容性优化，修复测试编译问题，全仓 cargo fmt 格式化

### Kimi Code Auth 账号接入

- ✨ **Kimi 设备 OAuth 登录**：实现 Kimi 设备授权流程（device code → 授权 → token），支持 token 自动刷新
- ✨ **Provider 中立认证框架**：新增 provider metadata + model protocol snapshot，支持多登录方式扩展
- ✨ **认证路由集成**：model-level auth profiles 传入 prepared attempts，executor 注册 Kimi 认证尝试
- ✨ **登录会话管理**：provider-neutral login sessions and commands，通用 login context 与 locked replacement 持久化
- ✨ **协议感知模型发现**：Kimi 后端协议感知的模型发现与注册
- ✨ **前端 Auth 面板**：Kimi auth login UI + provider-aware accounts 页面
- 🐛 **402 订阅无效终态处理**：402 订阅无效分为终态，不再 12h 死循环重试
- 🐛 **令牌失效原因记录**：invalidation_reason 记录并透出到 DTO，失效账号卡片显示具体失效原因
- 🐛 **渠道页账号过滤修复**：渠道页按 provider 过滤账号卡片，不再混显
- ✅ **测试覆盖**：Kimi routing replacement refresh 与协议流程测试

### 审计日志流式响应修复

- 🐛 **流式响应内容记录修复**：流式请求的审计日志中 `response_choices` 字段此前始终为空，现已正确记录响应内容（content / reasoning_content / tool_calls），与非流式路径行为一致
- 🔧 **多协议流式累积**：新增 SSE 事件解析器，支持三种流式协议的响应内容累积
- 🔧 **StreamPumpCore 扩展**：新增 `accumulated_reasoning`、`response_role`、`finish_reason`、`tool_calls_map` 字段

### 其他

- 版本号统一升级至 0.2.1（package.json / Cargo.toml / tauri.conf.json）
- 121 个文件变更，+22,616 / -14,462 行代码

---

## v0.1.9 (2026-08-13)

- ✨ 渠道多 Key 负载均衡：单个渠道配置多个 API Key，按权重随机选择，分散并发压力
- ✨ 渠道复制快捷配置：一键复制现有渠道配置，快速创建相似渠道
- ✨ 审计日志自动刷新：页面可见时每 5 秒静默轮询，新日志自动出现，无需手动刷新
- ✨ 自动更新 Release Notes 动态化：从 CHANGELOG.md 自动提取版本说明

---

## v0.1.8 (2026-08-12)

- ✨ API 密钥黑白名单：密钥级别渠道+模型访问控制
- ✨ Auth 账号模型映射：`auth_accounts` 新增 `model_mapping_json` 列
- ✨ API Key 编辑功能：支持编辑密钥名称、配额、白/黑名单规则
- 🐛 路由优先级修复：关闭 `prefer_auth_accounts` 与 `prefer_same_protocol`
- ✨ Usage 密钥过滤：选中 API Key 后 MODEL 列表自动按白/黑名单过滤

---

## v0.1.7 (2026-08-09)

- ✨ Wiki 知识引擎：项目/页面/源文件三表结构，文档摄入管道，知识图谱，标签体系
- ✨ MCP Server 扩展：新增 16 个 Wiki MCP 工具，总数 13 → 29 个
- 🐛 SSE 字节级重组：修复 CJK 多字节边界帧泄漏问题
- 🐛 Responses 流式修复：handler 路径 SSE 帧重组 + reasoning 归属修复

---

## v0.1.6 (2026-08-08)

- ✨ 渠道协议大重构（T01–T14）：Provider preset registry、严格 codec、SSRF 防护、Provider 下拉组件等
- ✨ 渠道表单 URL 预览：端点下方实时展示实际请求 URL
- ✨ /v1/models 接口：聚合所有启用渠道的模型列表
- ✨ 数据库迁移备份：迁移前自动备份数据库，保留最近 3 份

---

## v0.1.5 (2026-08-03)

- ✨ 模型映射一对多：支持单目标→多目标数组映射
- 🐛 proxy.rs P0 修复：429/5xx 误返客户端，新增 failover 检查
- ✨ 渠道超时配置：`timeout_secs` 字段（默认 60s）
- 🐛 IME composing 修复、拖拽排序修复

---

## v0.1.4 (2026-07-30)

- ✨ 知识库引擎：文档解析 → tree-sitter 代码符号感知 → 智能分块 → 向量化 → HNSW 索引
- ✨ 混合检索：HNSW + FTS5 加权融合
- ✨ RAG 问答引擎 + MCP Server（13 个工具）
- ✨ 应用配置：一键写入 8 款 AI 编程工具
- ✨ 导入导出 + 应用更新检查

---

## v0.1.1 (2026-07-21)

- ✨ 多协议网关：OpenAI Chat + Responses + Anthropic Messages
- ✨ 仪表盘优化 + 渠道统计 + 接入示例页

---

## v0.1.0 (2026-07-18)

- 🎉 首发版本：多渠道管理 + 密钥管理 + 日志审计 + 安全审计 + SSE 流式
