# WaLiAPI 桌面应用使用指南

## 1. 启动应用

应用启动后会自动打开窗口。如果没打开，可以：
- 从 Finder 进入 `src-tauri/target/release/bundle/macos/waliapi.app` 双击启动
- 或执行 `open /Users/fuzhengwei/coding/gitcode/KnowledgePlanet/x-api/WaLiAPI/src-tauri/target/release/bundle/macos/waliapi.app`
- 也可安装 DMG：`src-tauri/target/release/bundle/dmg/waliapi_0.1.0_aarch64.dmg`

## 2. 配置渠道（最重要！）

应用打开后默认是**空状态**，没有任何渠道就无法对外提供服务。

### 步骤：
1. 左侧导航 → 点击 **"渠道"**
2. 点击右上角 **"+ 新建渠道"**
3. 填写渠道信息：
   - **名称**：随便起（如"我的 OpenAI"）
   - **类型**：下拉选择（OpenAI / DeepSeek / Claude / Gemini / 自定义等）
   - **Base URL**：自动填好默认值，一般不用改
   - **API Key**：你的供应商密钥（如 `sk-...`）
   - **模型列表**：默认填好推荐模型，可修改
   - **优先级/权重**：默认即可
4. 点击 **"保存"**
5. 保存后点击 **"▶ 测试"** 按钮验证连通性

## 3. 创建访问密钥（下游应用使用）

1. 左侧导航 → 点击 **"密钥"**
2. 点击右上角 **"+ 新建密钥"**
3. 输入名称（如"ChatBox 用"），可设置配额
4. 点击 **"创建"**
5. 创建后会自动生成形如 `sk-waliapi-xxxxxxxxxx` 的密钥
6. 点击 **复制按钮** 复制密钥

## 4. 下游应用接入

应用启动时会自动开启一个本地 HTTP 服务（地址显示在左下角"服务状态"区域），例如：
```
http://127.0.0.1:53128
```

在 ChatBox / NextChat / LangChain / OpenAI SDK 中：
- **API Base URL**: `http://127.0.0.1:53128/v1`（注意要加 `/v1`）
- **API Key**: 上面创建的密钥 `sk-waliapi-...`

### 测试命令：
```bash
# 测试连通性
curl http://127.0.0.1:53128/health

# 测试聊天
curl http://127.0.0.1:53128/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-waliapi-xxxx" \
  -d '{
    "model": "gpt-4o-mini",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

## 5. 查看请求/响应日志

1. 左侧导航 → **"日志"**
2. 查看所有 API 请求记录：时间、密钥、渠道、模型、状态码、Token 消耗、耗时

## 6. 仪表盘

1. 左侧导航 → **"仪表盘"**
2. 查看：
   - 今日请求数、今日 Token、活跃渠道、密钥数量
   - 总请求、总 Token
   - 平均延迟

## 7. 端口配置（可选）

1. 左侧导航 → **"设置"**
2. 修改"端口"为固定值（如 3000），点击保存
3. 点击 **"重启服务"** 让新端口生效

## 8. 常见问题

**Q: 应用打开后侧边栏"服务状态"显示"未运行"？**
A: 这是预期行为。本地 HTTP 服务需要等到至少一个渠道被创建才会自动可用。如果创建了渠道仍然"未运行"，去设置页点"重启服务"。

**Q: 测试渠道时显示连接失败？**
A: 检查 API Key 是否正确、是否能访问 base_url（可能需要代理）。

**Q: 端口被占用？**
A: 在设置中改成 0（随机端口）或换一个端口号。

**Q: 数据存在哪里？**
A: SQLite 文件存在 `~/Library/Application Support/waliapi.xiaofuge.cn/waliapi.db`。
