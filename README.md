# WaLiAPI

> 本地运行的 LLM API 网关桌面软件，统一转换各供应商 API 为 OpenAI 兼容协议。

## 技术栈

- **前端**: React 19 + TypeScript + Vite 7 + Tailwind CSS 4
- **后端**: Rust + Tauri 2 + Axum + SQLite (sqlx) + Reqwest
- **UI**: shadcn/ui 风格 + Lucide Icons + React Router 7

## 功能

- 🔌 **多渠道管理** — 支持 OpenAI、DeepSeek、Claude、Gemini、智谱、通义、Moonshot、豆包、Ollama、自定义
- 🔑 **密钥管理** — 为下游应用生成 `sk-waliapi-*` 格式的访问密钥，支持配额限制
- 📊 **仪表盘** — 请求统计、Token 消耗、渠道状态
- 📝 **请求日志** — 完整的 API 调用记录
- 🔄 **负载均衡** — 优先级 + 权重的渠道选择
- 📡 **流式响应** — 完整 SSE 流式转发
- 🎨 **精美 UI** — 现代深色/浅色主题

## 快速开始

### 开发环境

```bash
# 安装前端依赖
pnpm install

# 开发模式运行
pnpm tauri dev

# 构建生产包
pnpm tauri build
```

### 使用方法

1. **配置渠道** — 在"渠道"页面添加上游 API 供应商
2. **创建密钥** — 在"密钥"页面生成 `sk-waliapi-*` 格式的本地密钥
3. **下游接入** — 在 ChatBox / NextChat / OpenAI SDK 中配置：
   - Base URL: `http://127.0.0.1:{port}/v1`
   - API Key: 创建的 `sk-waliapi-...` 密钥

## 项目结构

```
WaLiAPI/
├── src/                    # React 前端
│   ├── components/         # 通用组件
│   ├── pages/              # 页面（Dashboard/Channels/ApiKeys/Logs/Settings）
│   ├── lib/                # API 调用封装
│   └── types/              # TypeScript 类型定义
├── src-tauri/              # Rust 后端
│   ├── src/
│   │   ├── adaptor/        # 上游适配器（openai/claude/gemini/deepseek/custom）
│   │   ├── commands/       # Tauri IPC 命令
│   │   ├── core/           # 核心逻辑（分发/代理）
│   │   ├── server/         # 内嵌 HTTP Server (axum)
│   │   └── db/             # SQLite 数据层
│   ├── migrations/         # 数据库迁移
│   └── Cargo.toml
└── package.json
```

## License

MIT
