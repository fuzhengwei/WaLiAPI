# Changelog

## v0.3.6 (2026-09-23)

### 协议转换（codec）

- 🐛 **Chat response_format 映射为 Responses text.format**：Chat Completions 请求的 `response_format` 正确映射到 Responses 协议的 `text.format`，JSON 输出约束跨协议生效（PR #136，@GululuCopa）
- 🐛 **Messages 顶层 safeguards 按 fail-open 丢弃**：无法识别的顶层 safeguards 字段按 fail-open 丢弃，不再导致请求被上游拒绝（PR #136，@GululuCopa）
- 🐛 **Chat→Responses 兼容常用采样字段**：客户端常用采样参数在 Chat→Responses 转换中正确透传（PR #135，@GululuCopa）
- 🐛 **Responses function_call 条目 id 规范化**：function_call 条目 id 必须是 `fc_` 前缀，修复部分客户端解析失败（#129，@GululuCopa）
- 🐛 **Gemini 请求转换兼容标准 JSON Schema 与 Gemini 3 工具签名**（@GululuCopa）
- 🐛 **保留 Gemini 转换上下文并拒绝未知输入**：跨协议请求边界校验，转换上下文不丢失、未知输入直接拒绝（PR #136，@GululuCopa）

### Auth 账号

- 🐛 **Antigravity OAuth 修复**：修复 v0.3.6 Antigravity OAuth 授权流程（PR #128，@GululuCopa）
- ✨ **Antigravity 模型额度展示与工具调用 ID 保留**：Auth 渠道页展示 Antigravity 模型剩余额度，工具调用 ID 跨请求保留（PR #138，@huangkemingyyds）
- 🐛 **Grok 出站请求对齐上游约束**：出站请求对齐上游工具白名单与加密推理约束；规范化工具参数里的整数值浮点（PR #135，@GululuCopa）
- 🐛 **流式出站改用无总超时的 HTTP 客户端**：避免长流式响应被总超时中断（PR #135，@GululuCopa）
- 🐛 **打开系统浏览器失败单独归类 BrowserOpenFailed**：OAuth 授权时浏览器打开失败返回明确错误类型（PR #135，@GululuCopa）

### Codex

- 🐛 **修复旧会话回放与 GPT-6 模型同步**（PR #137，@huangkemingyyds）

### 客户端与配置生成

- 🐛 **修正 OpenCode / OpenClaw / Hermes 的配置生成**：客户端配置应用改为事务化，同步 `modelPolicy.allow` 避免主模型不可见（PR #135 / #136，@GululuCopa）
- 🧪 **回放用例改用相对时间**：避免测试随 TTL 过期自失败（PR #135，@GululuCopa）

### 其他

- 📝 **OAuth 协议覆盖验证记录**：补充 OAuth 协议覆盖验证文档
- 📝 **README 贡献者数据同步**：新增贡献者 黄科铭（@huangkemingyyds，PR #137 #138），按当前仓库提交记录更新全体贡献者提交数与代码变更统计
- 🔧 **版本号统一升级至 0.3.6**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.3.5 (2026-09-21)

### 新增渠道

- ✨ **新增 StepFun（阶跃星辰）渠道预设**：新增 OpenAI 兼容渠道类型 StepFun（`stepfun`，Base URL `https://api.stepfun.com/v1`），内置 5 条静态模型建议——step-5-preview / step-3.7-flash / step-3.5-flash / step-3.5-flash-2603 / step-1o-turbo-vision（旗舰在前，顺序即预填与连通性探测默认），预设仅声明 Chat Completions 端点与 Bearer 鉴权，模型建议可经「同步上游模型」拉取 `GET /v1/models` 覆盖；渠道导入导出的 v2 身份信任白名单、前后端图标与渠道类型定义同步接入（PR #127，@chyuan）

### 其他

- 📝 **README 贡献者数据同步**：按当前仓库提交记录更新贡献者提交数与代码变更统计
- 🔧 **版本号统一升级至 0.3.5**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.3.4 (2026-09-20)

### Auth 账号

- ✨ **Grok OAuth 登录**：新增 Grok 渠道 OAuth 授权登录（`grok_login` / `grok_backend`），支持 Token 自动刷新与协议感知模型发现，Auth 渠道页可直接登录 Grok 账号（PR #122，@GululuCopa）
- ✨ **Antigravity OAuth（Gemini）登录**：新增 Antigravity 作为 Gemini 渠道的 OAuth 登录方式（`gemini_backend`），Gemini 渠道支持 Antigravity 账号接入（PR #121，@GululuCopa）
- 🐛 **Grok 与 Antigravity namespace 工具兼容**：兼容两者工具调用的 namespace 前缀，修复工具调用在协议转换中的匹配问题（@GululuCopa）

### 渠道管理

- ✨ **模型映射支持开启/关闭**（迁移 041）：每条模型映射可单独停用，`model_mapping_disabled` 记录被关闭的映射对；路由匹配、上游模型解析、`/v1/models` 聚合均跳过被关闭的映射；映射行点击开关即时切换，导入导出同步兼容该字段
- ✨ **从 curl 导入渠道**：新建渠道表单支持粘贴任意 OpenAI / Anthropic / Ollama 兼容的 curl 命令，自动解析并一键填充协议、Base URL、API Key 与模型（支持 `\` 续行与各类引号转义）
- ✨ **复制测试 curl**：渠道列表新增「复制测试 curl」，按渠道协议 / URL / 模型生成可直接执行的 curl 命令（含真实 API Key），粘贴到终端即可验证渠道连通性

## v0.3.3 (2026-09-16)

### 日志

- ✨ **新增「简要」日志级别**：在「基本 / 详情」之外新增「简要」级别——请求消息列表只保留最新 3 条，长对话场景下可显著降低日志存储占用（PR #119）

### RAG 检索回归修复

- 🐛 **管理搜索接口按模式和权重执行检索**：管理端搜索不再忽略检索模式与权重配置，与实际问答链路行为一致（PR #118）
- 🐛 **失败文档不再阻止重新导入**：导入失败的文档允许直接重试相同内容，无需先清理残留数据（PR #118）
- 🐛 **索引落后时回退完整检索**：向量索引落后于切片数据时自动回退到完整检索，避免漏召回（PR #118）
- 🐛 **向量响应校验与索引对齐**：校验 Embedding 响应并按索引匹配输入文本，防止向量错位导致的检索结果异常（PR #118）

### 修复

- 🐛 **Token 配额标签澄清**：API Key 的 Token 配额标签文案更明确，避免与知识库权限混淆（PR #117）
- 🧪 **补齐 `request_headers` 测试字段**：修复 lib test 目标编译失败问题（PR #119）

### 其他

- 📝 **README 贡献者数据同步**：按当前仓库提交记录更新贡献者提交数与代码变更统计，README 历史版本改为折叠展示
- 🔧 **版本号统一升级至 0.3.3**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.3.2 (2026-09-13)

### 知识库检索与数据安全

- 🐛 **中文 PDF 与检索兼容性修复**：修复 PDF 部首字形、中文索引与向量检索漏召回问题，提升中文知识库检索稳定性
- 🔒 **知识库访问授权**：API Key 支持配置知识库授权，REST 与 MCP 查询按授权范围开放，避免跨知识库读取
- 🛡️ **RAG 来源与索引一致性修复**：来源列表只保留实际使用的上下文；模型变更时使旧向量缓存和索引失效；索引读改写串行化并原子保存；重建失败保留旧切片并原子替换；删除文档时保留导入源文件
- 🧪 **RAG 授权与计费测试补强**：覆盖查询授权和模型调用计费等关键场景

### 渠道、日志与 Auth

- ✨ **知识库权限管理界面**：新增知识库权限配置与连接检查入口，API Key 可按需授权知识库
- ⚡ **日志统计性能优化**：使用覆盖索引服务日志聚合查询，替代已判定失效的旧索引方案
- 🔇 **探测日志降噪**：审计日志仅记录探测状态翻转，恢复状态就地更新，不再为每次探测新增日志
- 🔐 **Claude Code 网关鉴权初始化**：补充网关鉴权 bootstrap 流程，并使 Codex Auth 写入逻辑跨平台

### 其他

- 📝 **README 贡献者数据同步**：按当前仓库提交记录更新贡献者提交数与代码变更统计
- 🔧 **版本号统一升级至 0.3.2**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.3.1 (2026-09-10)

### 渠道与配额

- ✨ **渠道主动健康探测**：后台周期性探测上游可用性，异常渠道在候选排序中自动沉底，恢复后自动回归；探测流量与业务统计口径隔离（迁移 033，PR #102）
- ✨ **配额记账强化**：两轨配额记账口径合一并递增封顶；配额 429 错误体按端点协议返回并携带 `used/limit` 字段（PR #92）

### 流式与可观测性

- ✨ **流式内容段持久化**：SSE 流式生成内容逐段落库，连接中断后已生成内容仍可查看（迁移 032，PR #95）
- ✨ **Responses 断线续传回放**：Responses 协议支持逐帧持久化 + offset 回放 + incomplete 收尾，客户端断线后可从中断点续传
- ✨ **数据面 X-Request-Id 标准化**：统一采纳/生成/回显请求 ID 并落库 trace_id，链路追踪闭环（PR #94）
- ✨ **OTLP/HTTP JSON 导出器**：request_log 增量导出为 OTLP span，可对接外部可观测平台（PR #94）

### 知识库

- ✨ **文档级增量索引**：chunk 内容哈希比对 + 未变块 embedding 复用 + HNSW 单点插入与墓碑摘除，文档更新只重算变更部分（PR #93）
- ✨ **多轮对话查询改写**：指代型问题在检索前先做查询改写，提升多轮 RAG 命中率（默认关，`kb.query_rewrite` 开关）
- ✨ **混合检索增强**：RRF 融合默认开启，可选 LLM listwise 重排进一步提升召回质量
- ✨ **Prompt 模板版本化**：模板支持版本管理页与种子兼容硬保证（迁移 034）

### 语义缓存

- ✨ **语义缓存 exact+semantic 两层**：精确命中 + 向量语义命中两级缓存，默认关闭；清空逻辑下沉 `semantic_cache::clear` 并补按模型/全清测试（迁移 035）

### Auth 账号

- ✨ **Codex 设备码登录**：支持 Device Authorization 流程（`codex login --device-auth`），并处理 pending 授权状态轮询（PR #77）
- ✨ **Auth 账号列表视图**：Auth 渠道页新增账号列表视图，多账号一目了然（PR #103）

### 修复

- 🐛 **Anthropic 容量错误提交前识别**：容量/过载类错误在响应提交前检测并触发跨协议故障切换，避免错误透传给下游（PR #101）

### 其他

- 📝 **README 贡献者数据同步**：按最新提交记录更新全体贡献者提交数与代码变更统计
- 🔧 **版本号统一升级至 0.3.1**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

## v0.3.0 (2026-09-09)

### 审计日志策略优化

- ✨ **审计日志存储与加载优化**：请求日志新增策略化存储与分级加载能力——`request_logs` 表增加 `detail_level`（明细级别）与 `started_at`（开始时间）字段并建立索引，日志页与设置页同步接入策略配置，大数据量场景下日志查询与加载更高效（PR #75）

### 修复

- 🐛 **sub2api 导入兼容修复**：sub2api 导出数据缺少 account id 时回退使用 `chatgpt_user_id`，避免导入失败或账号无法识别（PR #73）
- 🐛 **sub2api 导入账号数即时刷新**：导入完成后前端立即刷新账号计数，无需手动刷新页面（PR #73）
- 🐛 **Auth 账号操作后滚动位置保持**：Auth 渠道页执行账号操作后不再跳回顶部，保持当前滚动位置（PR #73）

## v0.2.9 (2026-09-07)

### Codex 账号与额度

- ✨ **Auth 账号额度改为剩余展示**：Auth 渠道卡片从“已用额度”切换为“剩余额度”视角，并补上前端展示工具与测试，用户能更直观看到当前还能使用多少额度（PR #67）
- ✨ **支持主动刷新 Codex 额度**：新增手动刷新 Codex 配额能力，前后端都接入刷新入口，Auth 渠道页可直接触发额度同步，减少等待后台轮询的时间（PR #68）
- 🐛 **修复 Codex 大响应 SSE 帧异常**：放宽大体积 `response.created` 事件的处理，避免 Codex 大响应场景因 SSE 帧创建阶段异常导致 502 或流式中断（PR #66）

### 全量审计修复合流

- 🔒 **合入 v0.2.7→v0.2.8 全量审计修复**：当前 `v0.2.9` 分支已并入 PR #70，包含 KB/Wiki/MCP 端点鉴权恢复、KB 上传与导入边界收紧、URL SSRF 防护、管理面认证加固、API Key 掩码展示与按需取全量等修复
- 🔧 **核心稳定性与安全扫描补强**：同步并入流式首帧/空闲超时、流式落账修复、多 Key 加权选择边界修正、404 故障切换语义统一、原生 Anthropic 路径落账对齐，以及响应侧安全扫描接入全部转发路径
- 🖥️ **管理端体验与运维诊断增强**：全局 ErrorBoundary、日志安全解析、Wiki 渲染 sanitize、会话过期统一跳登录、日志页与知识库页竞态治理，以及 `/health` 版本号与日志目录等可观测性增强均已随 PR #70 合流

### 其他

- 🐛 **Usage 连接测试请求头兼容性修复**：连接测试不再写入中文占位密钥，改为 ASCII 安全占位值，避免无效请求头被浏览器或运行时拒绝（PR #71）
- 📝 **README 贡献者数据同步**：新增 2 位贡献者 yuanqixun 和 zjx，并按最新提交记录更新全体贡献者提交数与代码变更统计
- 🔧 **版本号统一升级至 0.2.9**（package.json / Cargo.toml / tauri.conf.json / Cargo.lock）

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
