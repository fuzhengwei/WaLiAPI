# WaLiAPI 项目搭建完成

## 目标
基于 TypeScript + Rust + Tauri 2 开发本地桌面 LLM API 网关应用，统一转换各供应商 API 为 OpenAI 兼容协议。

## 完成内容

### 技术栈
- **前端**: React 19 + TypeScript + Vite 7 + Tailwind CSS 4 + React Router 7 + Lucide Icons + Zustand
- **后端**: Rust + Tauri 2 + Axum + SQLx (SQLite) + Reqwest
- **构建**: pnpm + cargo，成功生成 macOS .app 和 .dmg 包

### Rust 后端模块
- `src-tauri/src/lib.rs` — Tauri 应用入口，数据库初始化、HTTP 服务启动、托盘菜单
- `src-tauri/src/db/` — 数据库层（models, repository, SQLite 连接与迁移）
- `src-tauri/src/adaptor/` — API 适配器（openai, deepseek, claude, gemini, custom）
- `src-tauri/src/core/` — 核心调度（dispatcher 渠道选择, proxy 请求代理）
- `src-tauri/src/server/` — HTTP 服务器（axum router, handlers, SSE 流式支持）
- `src-tauri/src/commands/` — Tauri Commands（channels, api_keys, logs, settings, server）
- `src-tauri/migrations/001_init.sql` — 数据库初始化（channels, api_keys, request_logs 表）

### 前端页面
- `DashboardPage` — 仪表盘，展示统计数据和图表
- `ChannelsPage` — 渠道管理，增删改查、测试、启用/禁用
- `ApiKeysPage` — 密钥管理，创建、复制、配额管理
- `LogsPage` — 请求/响应日志，分页表格展示
- `SettingsPage` — 服务配置、通用设置、界面、重试
- `ChannelForm` — 渠道编辑表单组件
- `Layout` — 侧边栏 + 主内容区布局

### 适配的渠道类型
OpenAI、DeepSeek、Claude (Anthropic)、Gemini (Google)、Custom（自定义）

### 构建产物
- `/src-tauri/target/release/bundle/macos/waliapi.app`
- `/src-tauri/target/release/bundle/dmg/waliapi_0.1.0_aarch64.dmg`

## 关键决策
1. 使用 Tauri 2 而非 Electron，减小包体积
2. SQLite 通过 sqlx 直接在 Rust 层管理，不依赖 tauri-plugin-sql 的前端迁移
3. HTTP 服务使用 axum，支持 SSE 流式响应
4. 前端通过 Tauri Commands 调用后端，而非直接访问数据库
5. 配置存储使用 tauri-plugin-store (settings.json)

## 编译修复记录
- `settings.rs`: 修复 `store.get()` 返回值生命周期问题，提取辅助函数
- `commands/server.rs`: 修复 `State` 类型为 `Arc<AppState>`
- `db/mod.rs`: 添加 `Manager` trait 导入以使用 `app.path()`
- `adaptors`: `unwrap_or(&"str")` → `.map(|s| s.as_str()).unwrap_or("str")`
- `dispatcher.rs`: 修复 `top_candidates` 被 for 循环 move 后再引用
- `models.rs`: 为 `LogStats` 派生 `sqlx::FromRow`
- `lib.rs`: 修复 `start_server` 参数传递 `Arc` clone
- 清理多处未使用导入

## 后续可扩展
- Skills 和 MCP 扩展点已预留
- 更多渠道适配器（通义千问、文心一言、智谱等）
- 用户认证与多用户支持
- 请求重试与故障转移策略
- 模型映射与价格计算
