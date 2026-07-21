## WaLiAPI v0.1.0 — 本地 LLM API 网关

> 本地运行的 LLM API 网关桌面软件，统一转换各供应商 API 为 OpenAI 兼容协议。可配合 [WaLiCode](https://walicode.xiaofuge.cn/)、Codex、Claude Code、QClaw 等 AI IDE，让你知道 AI 对话，到底在说啥。

网盘下载：[https://pan.quark.cn/s/b6a134a77efa](https://pan.quark.cn/s/b6a134a77efa)

### 🎯 核心功能

**🔌 多渠道管理**

- 支持 OpenAI、DeepSeek、Claude、Gemini、智谱、通义、Moonshot、豆包、Ollama 及自定义渠道
- 优先级 + 权重的负载均衡策略
- 模型映射（渠道级别 model mapping）

**🔑 密钥管理**

- 为下游应用生成 `sk-waliapi-*` 格式的本地访问密钥
- 支持配额限制与启用/禁用
- 自定义删除确认弹窗，避免误操作

**📊 仪表盘**

- 请求统计、Token 消耗一览
- 渠道状态与服务可用率
- 平均延迟监控
- 快捷操作入口与配置引导

**📝 请求/响应日志**

- 完整记录每次 API 调用的请求体、响应体、模型参数、工具调用、Token 消耗与响应状态
- 支持按关键词、密钥、渠道、模型、日期范围搜索筛选
- 日志详情展示对话构成、工具标签、请求参数、网关路由与原始 JSON

**🛡️ 安全审计中心**

- **风险检测引擎**：自动扫描请求中的敏感信息泄露（API Key、私钥、JWT、Cookie、Bearer Token）、敏感文件路径（`~/.ssh`、`.env`、云凭据）、Unicode 隐写字符（零宽字符、方向控制字符）、可疑工具调用（`curl` 外联、管道上传）、网络风险（公网 IP 探测、webhook/隧道域名）、追踪像素与风控指纹上下文
- **风险等级**：clean / info / low / medium / high / critical，综合评分 0-100
- **策略模式**：只审计 / 警告 / 脱敏 / 阻断，默认只审计不影响请求
- **规则管理**：内置 25 条风险规则 + 自定义黑白名单（域名/工具/路径/关键词）
- **日志展示**：请求列表新增安全等级 Badge，详情页展示风险摘要、评分、处理动作与脱敏证据
- **配置面板**：独立开关控制 Unicode 隐写检测、工具/命令风险检测、外联/追踪风险检测、严重风险强制阻断

**⚙️ 设置中心**

- Tab 切换式布局：安全审计 / 服务配置 / 通用设置 / 界面设置 / 重试策略
- 深色 / 浅色 / 跟随系统主题切换
- 最小化到托盘、关闭到托盘、开机自启
- 失败自动重试策略配置

**📡 流式响应**

- 完整 SSE 流式转发，兼容 ChatBox / NextChat / OpenAI SDK 等下游客户端

### 🏗️ 技术栈

| 层   | 技术                                            |
| :--- | :---------------------------------------------- |
| 前端 | React 19 + TypeScript + Vite 7 + Tailwind CSS 4 |
| 后端 | Rust + Tauri 2 + Axum + SQLite (sqlx) + Reqwest |
| UI   | shadcn/ui 风格 + Lucide Icons + React Router 7  |

### 📦 安装使用

1. **下载安装包** — 从 Release 页面下载对应平台安装包
2. **配置渠道** — 在「渠道」页面添加上游 API 供应商
3. **创建密钥** — 在「密钥」页面生成 `sk-waliapi-*` 格式的本地密钥
4. **下游接入** — 在 ChatBox / NextChat / OpenAI SDK 中配置：

- Base URL: `http://127.0.0.1:{port}/v1`
- API Key: 创建的 `sk-waliapi-...` 密钥

### 📄 License

MIT
