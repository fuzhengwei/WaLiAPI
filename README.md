<div align="center">

# WaLiAPI

### 本地 LLM API 网关 · 让每一次 AI 对话都透明可见

[![Version](https://img.shields.io/badge/version-0.1.1-blue.svg)](./src-tauri/tauri.conf.json)
[![License](https://img.shields.io/badge/license-MIT-green.svg)](./LICENSE)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-lightgrey.svg)](#-安装使用)
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri%202-orange.svg)](https://tauri.app)

</div>

> **WaLiAPI** 是一款本地运行的 LLM API 网关桌面软件。它将多个上游模型供应商（OpenAI、Claude、DeepSeek、Gemini……）统一为 OpenAI 兼容协议，配合 [WaLiCode](https://walicode.xiaofuge.cn/)、Codex、Claude Code、QClaw 等 AI IDE 使用，让你清楚知道 AI 对话到底在说什么。

网盘下载：[https://pan.quark.cn/s/b6a134a77efa](https://pan.quark.cn/s/b6a134a77efa)

---

## 📑 目录

- [核心功能](#-核心功能)
- [多协议接入](#-多协议接入)
- [技术栈](#-技术栈)
- [安装使用](#-安装使用)
- [项目结构](#-项目结构)
- [版本历史](#-版本历史)
- [贡献者](#-贡献者)
- [许可证](#-许可证)

---

## 🎯 核心功能

### 🔌 多渠道管理

- 支持 **10 种渠道类型**：OpenAI、DeepSeek、Claude、Gemini、智谱、通义、Moonshot、豆包、Ollama 及自定义渠道
- 优先级 + 权重的负载均衡策略，自动故障切换
- 模型映射（渠道级别 model mapping），下游模型名自动映射到上游实际模型
- 渠道连通性测试，实时显示延迟与错误信息

### 🔑 密钥管理

- 为下游应用生成 `sk-waliapi-*` 格式的本地访问密钥
- 支持配额限制与启用/禁用
- 每个密钥展示调用次数、成功率、Token 消耗、平均延迟

### 📊 仪表盘

- 6 项核心指标一目了然：今日请求、今日 Token、累计请求、累计 Token、活跃渠道、平均延迟
- 服务可用率徽章，颜色分级（绿/黄/红）实时反映健康度
- 运维建议根据当前数据动态生成（延迟超阈值建议排查、渠道不足建议启用等）
- 快速操作入口覆盖所有主要功能页

### 📝 审计日志

- 完整记录每次 API 调用：请求体、响应体、模型参数、工具调用、Token 消耗、状态码
- 支持按关键词、密钥、渠道、模型、日期范围搜索筛选
- 请求/响应 JSON 标签页切换，Trace ID 默认折叠可展开
- 日志编号自增，方便定位与引用

### 🛡️ 安全审计中心

- **风险检测引擎**：自动扫描请求中的敏感信息泄露（API Key、私钥、JWT、Cookie、Bearer Token）、敏感文件路径（`~/.ssh`、`.env`、云凭据）、Unicode 隐写字符（零宽字符、方向控制字符）、可疑工具调用（`curl` 外联、管道上传）、网络风险（公网 IP 探测、Webhook/隧道域名）、追踪像素与风控指纹
- **风险等级**：clean / info / low / medium / high / critical，综合评分 0–100
- **策略模式**：只审计 / 警告 / 脱敏 / 阻断，默认只审计不影响请求
- **规则管理**：内置 25+ 条风险规则 + 自定义黑白名单（域名/工具/路径/关键词）
- **日志展示**：请求列表安全等级 Badge，详情页展示风险摘要、评分、处理动作与脱敏证据

### ⚙️ 设置中心

- Tab 切换式布局：安全审计 / 服务配置 / 通用设置 / 界面设置 / 重试策略
- 深色 / 浅色 / 跟随系统主题切换
- 最小化到托盘、关闭到托盘、开机自启
- 失败自动重试策略配置（默认 2 次）

### 📡 流式响应

- 完整 SSE 流式转发，兼容 ChatBox / NextChat / OpenAI SDK 等下游客户端
- 流式使用量解析（累积 input/output tokens）

---

## 🔗 多协议接入

WaLiAPI 在网关层做协议翻译，入口多协议，出口统一为 OpenAI Chat Completions，上游渠道无感知。

| 协议 | 端点 | 认证方式 | 说明 |
|:---|:---|:---|:---|
| **OpenAI Chat Completions** | `POST /v1/chat/completions` | `Authorization: Bearer sk-waliapi-*` | 标准兼容协议，支持流式 |
| **OpenAI Responses** | `POST /v1/responses` | `Authorization: Bearer sk-waliapi-*` | Responses API 双向转换 |
| **Anthropic Messages** | `POST /v1/messages` | `x-api-key: sk-waliapi-*` | Anthropic 协议，自动头转换 |
| **模型列表** | `GET /v1/models` | `Authorization: Bearer sk-waliapi-*` | 聚合所有启用渠道的模型 |
| **健康检查** | `GET /health` | 无 | 服务存活探针 |

接入示例（以 OpenAI 协议为例）：

```bash
curl http://127.0.0.1:8777/v1/chat/completions \
  -H "Authorization: Bearer sk-waliapi-xxxx" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

接入示例（以 Anthropic 协议为例）：

```bash
curl http://127.0.0.1:8777/v1/messages \
  -H "x-api-key: sk-waliapi-xxxx" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

> 💡 在「接入示例」页面可查看 cURL / Python / Node.js / TypeScript / Rust / Java 共 5 平台 × 3 协议 = 15 套代码示例。

---

## 🏗️ 技术栈

| 层 | 技术 | 版本 |
|:---|:---|:---|
| 前端 | React + TypeScript + Vite + Tailwind CSS | 19 / 5.x / 7 / 4 |
| 后端 | Rust + Tauri 2 + Axum + SQLite (sqlx) + Reqwest | Edition 2021 |
| UI | shadcn/ui 风格 + Lucide Icons + React Router 7 | — |
| 打包 | Tauri bundler（.dmg / .msi / .deb / .AppImage） | 2.x |

---

## 📦 安装使用

### 1. 下载安装包

从网盘下载对应平台安装包：[夸克网盘](https://pan.quark.cn/s/b6a134a77efa)

| 平台 | 格式 | 架构 |
|:---|:---|:---|
| macOS | `.dmg` | ARM64 (Apple Silicon) |
| Windows | `.msi` / `.exe` | x64 |
| Linux | `.deb` / `.AppImage` | x64 |

### 2. 配置渠道

打开 WaLiAPI →「渠道管理」→「新建渠道」→ 填写名称、Base URL、API Key、支持的模型 → 保存。

### 3. 创建密钥

「API 密钥」→「新建密钥」→ 生成 `sk-waliapi-*` 格式的本地访问令牌。

### 4. 下游接入

在 ChatBox / NextChat / OpenAI SDK / WaLiCode 中配置：

- **Base URL**: `http://127.0.0.1:8777/v1`
- **API Key**: 创建的 `sk-waliapi-...` 密钥

---

## 📁 项目结构

```
WaLiAPI/
├── src/                          # 前端源码
│   ├── pages/                    # 页面组件
│   │   ├── DashboardPage.tsx     # 仪表盘
│   │   ├── ChannelsPage.tsx      # 渠道管理
│   │   ├── ApiKeysPage.tsx       # 密钥管理
│   │   ├── LogsPage.tsx          # 审计日志
│   │   ├── UsagePage.tsx         # 接入示例
│   │   └── SettingsPage.tsx      # 设置中心
│   ├── lib/                      # 工具库
│   │   ├── api.ts                # API 接口
│   │   └── constants.ts          # 常量定义
│   └── components/               # 通用组件
├── src-tauri/                    # 后端源码
│   ├── src/
│   │   ├── server/               # HTTP 服务器
│   │   │   ├── router.rs         # 路由定义
│   │   │   └── handlers.rs       # 请求处理器
│   │   ├── adaptor/              # 渠道适配器
│   │   │   ├── mod.rs            # 适配器 Trait + 配置
│   │   │   ├── openai.rs         # OpenAI 适配器
│   │   │   ├── claude.rs         # Claude 适配器
│   │   │   ├── deepseek.rs       # DeepSeek 适配器
│   │   │   ├── gemini.rs         # Gemini 适配器
│   │   │   └── custom.rs         # 自定义适配器
│   │   ├── protocol/             # 协议转换层
│   │   │   ├── mod.rs            # 双向格式转换
│   │   │   ├── anthropic.rs      # Anthropic SSE 流式
│   │   │   └── responses.rs     # Responses SSE 流式
│   │   ├── core/                 # 核心逻辑
│   │   │   └── proxy.rs          # 代理转发 + 重试
│   │   ├── security/             # 安全审计
│   │   ├── commands/             # Tauri Commands
│   │   ├── repository.rs         # 数据访问层
│   │   └── lib.rs                # 入口
│   ├── migrations/               # 数据库迁移
│   └── tauri.conf.json           # Tauri 配置
└── package.json
```

---

## 📌 版本历史

### v0.1.1 (2026-07-21)

- ✨ 多协议网关：支持 OpenAI Chat Completions + Responses API + Anthropic Messages 三协议入口
- ✨ 仪表盘优化：统一 6 卡片指标网格 + 健康度徽章 + 动态运维建议
- ✨ 渠道统计：调用次数、Token 消耗、成功率、平均延迟
- ✨ 密钥统计：每个密钥的调用指标展示
- ✨ 接入示例页：三协议切换 + 15 套代码示例 + 连接测试
- 🐛 修复 Claude 渠道测试 HTTP 400 误判为成功
- 🐛 修复日志 seq 始终为 0（建表 DEFAULT 0 导致回填失效）
- 🐛 修复 Anthropic Base URL 不含 /v1 导致用户无法配置
- 🎨 审计日志术语统一，Trace ID 默认折叠
- 🎨 设置页 Tab 横向布局，去掉宽度限制

### v0.1.0 (2026-07-18)

- 🎉 首个发布版本
- 多渠道管理（10 种渠道类型）+ 优先级/权重负载均衡
- 密钥管理 + 配额限制
- 请求/响应日志 + 全维度搜索筛选
- 安全审计中心（25+ 规则，5 种策略模式）
- 设置中心（主题/托盘/自启/重试）
- SSE 流式响应转发

---

## 👥 贡献者

感谢以下开发者对 WaLiAPI 项目的代码贡献：

| 贡献者 | GitHub | 主要贡献 |
|:---|:---|:---|
| 小傅哥 | [@fuzhengwei](https://github.com/fuzhengwei) | 项目创建者，核心架构、多渠道管理、协议网关、安全审计、仪表盘 |
| mw | [@maowei0427](https://github.com/maowei0427) | 日志模块响应内容记录、Trace ID 追踪、详情页体验优化 |
| lianggq | [@GQingL](https://github.com/GQingL) | 日志日期筛选与渠道删除修复 |

> 欢迎通过 PR / Issue 参与项目共建。

---

## 📄 License

[MIT](./LICENSE)

---

<div align="center">
  <sub>Built with ❤️ by the WaLiAPI community</sub>
</div>
