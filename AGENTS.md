# AGENTS.md

> 本文件面向 AI 编码代理，介绍 WaLiAPI 项目的架构、构建方式与开发约定。阅读本文件即可上手开发，无需先读其他文档。

## 项目概览

**WaLiAPI** 是一款本地运行的 LLM API 网关（当前版本 0.3.2，MIT 协议）。核心能力：

- **多协议网关**：下游可用 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages 三种协议接入，出口统一转换后转发到上游供应商（OpenAI / Claude / DeepSeek / Gemini / 智谱 / 通义 / Moonshot / 豆包 / Ollama / 自定义）。
- **渠道调度**：优先级 + 权重负载均衡、多 API Key 负载、模型映射、自动故障切换。
- **安全审计**：风险扫描引擎（敏感信息、路径、Unicode 隐写等 25+ 内置规则），策略支持只审计 / 警告 / 脱敏 / 阻断。
- **知识库 RAG**：文档解析（tree-sitter 代码符号感知）→ 智能分块 → 向量化（复用渠道 Embedding）→ HNSW 向量索引 + SQLite FTS5 混合检索 → RAG 问答。
- **Wiki 知识引擎**：Markdown + frontmatter 页面管理、`[[wikilinks]]` 知识图谱、摄入管道。
- **MCP Server**：`/mcp` 端点（Streamable HTTP + SSE），对外暴露 29 个工具（知识库 13 + Wiki 16）。

同一套 Rust 代码库编译出两种产物：

- **桌面端**（默认 feature `desktop-ui`）：Tauri 2 窗口应用，macOS / Windows / Linux。
- **Headless 服务端**（`waliapi-web` 二进制，`--no-default-features --features embed-web`）：无窗口纯 HTTP 服务，用于 Docker / systemd 部署，内嵌 Web 管理面板。

## 技术栈

| 层 | 技术 |
|:---|:---|
| 前端 | React 19 + TypeScript ~5.8 + Vite 7 + Tailwind CSS 4 + React Router 7（无独立状态/服务端数据库，组件内 useState/useEffect + 自封装 runtime 层） |
| 后端 | Rust (edition 2021) + Tauri 2 + Axum 0.8 + sqlx 0.8 (SQLite) + reqwest 0.12 + tokio |
| 知识库 | tree-sitter（7 种语言）+ HNSW + FTS5 + bincode + pdf-extract |
| 包管理 | pnpm（workspace，含 `web/` 子包）+ cargo |
| 打包 | Tauri bundler（.dmg / .msi / .deb / .AppImage）、Docker 多阶段构建 |

## 仓库结构（实际目录）

```
├── src/                      # 前端主源码（桌面端直接用，Web 面板复用）
│   ├── pages/                # 9 个页面：Dashboard / Channels / AuthChannels / ApiKeys / Logs
│   │                         #   / KnowledgeBase / Usage / Settings / AppConfig
│   ├── components/           # ChannelForm、MappingSection、auth/、channel-form/、layout/ 等
│   ├── hooks/                # useModelMappings 等
│   ├── lib/                  # api.ts、runtime.ts（统一命令传输层，见下）、constants.ts
│   └── types/                # TypeScript 类型定义
├── web/                      # Web 管理面板构建（pnpm 子包 waliapi-web）
│   └── src/lib/              # tauri-shim.ts 等：把 @tauri-apps/* API 替换为 HTTP 实现
│                             # web/vite.config.ts 用 @app alias 复用 ../src 全部页面组件
├── src-tauri/                # Rust 后端
│   ├── src/
│   │   ├── main.rs           # 桌面端入口（default-run = "waliapi"）
│   │   ├── bin/waliapi-web.rs # headless 服务端入口
│   │   ├── lib.rs            # 库入口：AppState、系统托盘、服务启动
│   │   ├── web_server.rs     # headless 启动逻辑（数据目录解析、host/port）
│   │   ├── server/           # Axum HTTP 服务：router.rs、handlers.rs、admin*.rs（管理面）、
│   │   │                     #   event_bridge.rs（桌面 Webview / Web SSE 统一事件出口）
│   │   ├── core/             # 核心调度：proxy.rs、dispatcher.rs、route_plan.rs、
│   │   │                     #   plan_executor.rs、attempt.rs、stream_supervisor.rs、
│   │   │                     #   channel_identity.rs、feature_flags.rs、protocol_boundary.rs
│   │   ├── endpoint_executor/ # 端点执行器（请求驱动、SSE 处理、Token 用量估算）
│   │   ├── auth_provider/    # Auth 账号（Codex / Kimi / Antigravity OAuth 登录、Token 刷新、模型发现）
│   │   ├── protocol/         # 协议转换层：codec/（chat / messages / responses_codec /
│   │   │                     #   directions 双向转换）、sse_bridge.rs（字节级 SSE 重组，CJK 安全）
│   │   ├── adaptor/          # 上游渠道适配器：openai / claude / deepseek / gemini / custom
│   │   ├── security/         # 安全审计：scanner.rs、rules.rs、redact.rs
│   │   ├── services/         # 服务层：mod.rs 定义 Service trait + ServiceRegistry，
│   │   │                     #   knowledge/（RAG 全链路，含 ocr/ 扫描版 PDF VLM 识别）、wiki/、mcp/、channel_test.rs
│   │   ├── commands/         # Tauri Commands（channel / api_key / auth / log / settings / ...）
│   │   ├── db/               # Database 初始化、models.rs、repository.rs
│   │   ├── channel_presets.rs # 渠道预设注册表
│   │   └── settings_store.rs # 设置存储抽象（桌面 tauri-plugin-store / headless JSON 文件）
│   ├── migrations/           # SQL 迁移 001–027，启动时经 sqlx::migrate! 自动执行，迁移前自动备份 DB
│   ├── resources/pdfium/     # pdfium 动态库打包目录（VLM OCR 用，库文件不入库，见该目录 README）
│   └── tests/                # 集成测试（见「测试」一节）
├── deploy/                   # caddy/Caddyfile.example、systemd/（unit + env 示例）
├── scripts/                  # fetch-pdfium.sh（打包前下载 OCR 渲染依赖 pdfium 动态库）、Gitcode release 同步脚本
├── docs/                     # 设计文档（渠道协议重构、KB 升级、Wiki 架构等）
└── .github/workflows/        # 6 个发布 workflow（见「发布与部署」）
```

注意：README 中的「项目结构」一节可能滞后于代码（例如 `endpoint_executor/`、`auth_provider/` 现为顶层模块而非 `core/` 子模块），以实际目录为准。

## 构建与开发命令

包管理器用 **pnpm**（CI 使用 pnpm 11 + Node 22）。前端依赖安装：

```bash
pnpm install --frozen-lockfile
```

桌面端开发（前端 vite dev + Tauri 窗口）：

```bash
pnpm tauri dev        # 等价于 ./start.sh（内容为 npm run tauri dev）
```

前端构建（产物 `dist/`）：`pnpm build`（先 `tsc` 类型检查再 `vite build`）
Web 面板构建（产物 `web/dist/`）：`pnpm --filter waliapi-web build`

Rust 后端：

```bash
# 桌面端（默认 feature = desktop-ui，需要系统 GTK/WebKit 依赖）
cargo build --manifest-path src-tauri/Cargo.toml

# headless 服务端（关闭 desktop-ui，内嵌 Web 面板静态资源）
cargo build --release --manifest-path src-tauri/Cargo.toml \
  --bin waliapi-web --no-default-features --features embed-web
```

关键 Cargo feature（`src-tauri/Cargo.toml`）：

- `default = ["desktop-ui"]`：桌面专属能力（托盘、文件对话框、自启、OAuth 打开浏览器）。headless 构建必须 `--no-default-features`，否则会把 GTK/WebKit/DBus 链入二进制。
- `embed-web`：通过 rust-embed 把 `web/dist` 内嵌进 `waliapi-web` 二进制。
- release profile 开启 `lto = "fat"` + `codegen-units = 1`，请勿随意改动（用于剔除 headless 二进制中的桌面端代码）。

## 测试

测试集中在 Rust 后端（约 690 个 `#[test]` / `#[tokio::test]`）：

```bash
cd src-tauri && cargo test          # 必须在 src-tauri 目录下运行
```

- 集成测试在 `src-tauri/tests/`（`channel_migration.rs`、`auth_repository.rs`、`request_log.rs`），以及 `src-tauri/src/` 下的 `auth_integration_tests.rs`、`rollout_integration_tests.rs`（`#[cfg(test)]` 编译）。
- 大量模块内联单测（`protocol/`、`core/`、`auth_provider/` 等均有 `#[cfg(test)] mod tests`）。
- 数据库测试用**内存 SQLite** + `sqlx::migrate!("./migrations")` 跑真实迁移 SQL，因此 cwd 必须是 `src-tauri/`。
- 前端无测试框架，类型检查即 `pnpm build` 中的 `tsc`（`tsconfig.json` 开启 strict + noUnusedLocals + noUnusedParameters）。
- 代码风格：`cargo fmt` 格式化、`cargo clippy` 告警需保持为零（项目历史中有专门的告警清零提交）。

## 运行时架构

- HTTP 服务默认监听 `127.0.0.1:8777`（桌面）/ `0.0.0.0:8777`（Docker）。端口可在设置中修改（`server.port`）。
- 数据面端点：`POST /v1/chat/completions`、`POST /v1/responses`、`POST /v1/messages`（Anthropic）、`POST /v1/embeddings`、`GET /v1/models`、`GET /health`；服务端点：`/mcp`、`/api/kb/*`、`/api/wiki/*`。
- 前端与后端通信走统一适配层 `src/lib/runtime.ts`：桌面端用 Tauri IPC `invoke`；浏览器端把同样的命令名和参数 POST 到 `/admin/api/invoke`。**新增前后端交互时必须走该层，不要直接调用 `@tauri-apps/api` 或裸 fetch**。
- `AppState`（`lib.rs`）是全局状态容器：`db`、`auth_service`、`server_port/running/handle`、`admin_sessions`、`events`（事件总线）、`settings`、`data_dir` 等。
- headless 模式通过 `tauri::test::MockRuntime` 获得 `State<'static, Arc<AppState>>` 供命令分发，桌面专属代码一律用 `#[cfg(feature = "desktop-ui")]` 门控。
- 流式与非流式请求使用不同的 reqwest client：流式只设 `connect_timeout`（10s），非流式另有总超时 `timeout_secs`（渠道级配置，默认 60s）。修改超时逻辑时注意区分两者。
- 数据目录解析优先级：显式参数 > `WALIAPI_DATA_DIR` > `XDG_DATA_HOME` > 平台应用数据目录。SQLite 不支持多实例写同一数据目录，部署时保持单实例。

## 认证体系（三个互不通用的凭证域）

| 用途 | 凭证 |
|:---|:---|
| 数据面 `/v1/*` | 后台创建的 `sk-waliapi-*` 密钥 |
| Web 管理面 + KB/Wiki REST | `WALIAPI_ADMIN_TOKEN`（≥32 字符） |
| MCP 端点 | `WALIAPI_MCP_TOKEN`（≥32 字符，须与 ADMIN 不同） |

管理路由带会话鉴权 + CSRF 防护，且不附带宽松 CORS（CORS 仅作用于 API Key 鉴权的网关/服务路由）。反向代理只负责 TLS 与转发，不得移除认证头。

## 数据库迁移

- 迁移文件位于 `src-tauri/migrations/`，按 `NNN_name.sql` 三位序号命名，新增迁移顺延编号。
- 启动时自动执行，迁移前自动备份数据库（保留最近 3 份）。
- 修改 schema 必须新增迁移文件，不要改动已发布的迁移。

## 发布与部署

GitHub Actions 按标签触发（`.github/workflows/`）：

- `macos-arm64-v*` → macOS ARM64 桌面安装包
- `all-win-v*` → Windows 安装包
- `linux-v*` → Linux 安装包
- `v*` → Docker 桌面镜像发布
- `web-v*` → Web 产物（Linux 二进制包 + GHCR 镜像 `ghcr.io/<owner>/<repo>:<version>`）

**版本号必须四处同步升级**：`package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.lock`。

Docker 部署：`docker compose up -d --build`（多阶段构建，运行时非 root 用户，数据卷 `/data`，需注入 `WALIAPI_ADMIN_TOKEN` / `WALIAPI_MCP_TOKEN`）。Compose 默认只绑定 `127.0.0.1`，公网部署用 Caddy/Nginx 终止 HTTPS 反代。systemd 部署示例见 `deploy/systemd/`。

## 开发约定

- **注释与文档使用中文**（部分历史代码注释为英文，新代码跟随所在文件的主流语言）。
- Rust：`anyhow` / `thiserror` 处理错误，`tracing` 记日志；异步运行时 tokio。
- 前端：函数组件 + Hooks，组件内状态用 useState/useEffect（无 Zustand/TanStack——依赖已确认为死依赖并移除），样式用 Tailwind CSS 4（`@tailwindcss/vite` 插件），图标用 lucide-react。
- 渠道相关改动常涉及多处的模型映射与 Key 选择逻辑（`core/proxy.rs` 与 `endpoint_executor/` 两条转发路径都要覆盖）。
- 桌面端窗口配置中 `dragDropEnabled: false` 是有意为之（Tauri v2 会吞掉 HTML5 drop 事件），不要改回。
- 仓库根目录的 `waliapi_build_complete_20260718.md`、`waliapi_usage_guide_20260718.md` 是早期搭建记录，内容可能过时，参考 `README.md` 和 `docs/` 下的设计文档为准。
