# Codex 重置卡开发文档

**状态**：生产代码已接入；服务端契约、幂等和构建门禁已通过，前端未知结果恢复入口仍待补齐。
**日期**：2026-10-02
**适用项目**：WaLiAPI

本目录直接遵循项目现有的 Auth/Codex 和分阶段任务文档约定，保留架构决策、实现计划、开发任务、验收 Spec、复核记录和产品构件。

## 文档索引

| 文件 | 内容 |
|---|---|
| [00-architecture-decisions.md](00-architecture-decisions.md) | 现有代码接入点、架构决策、文件边界、回滚原则和完成定义 |
| [03-implementation-plan.md](03-implementation-plan.md) | Provider/Service/Repository/Command/前端实现设计与阶段计划 |
| [04-development-tasks.md](04-development-tasks.md) | 可按 T00–T06 执行的开发任务、验证方式和交付记录要求 |
| [05-acceptance-spec.md](05-acceptance-spec.md) | 功能、安全、接口、数据、必要测试、真实测试和交付判定 |
| [07-implementation-review.md](07-implementation-review.md) | 当前分支架构、代理复用、验证结果和未闭合项 |
| [prototype.html](prototype.html) | 按产品图复原的可交互前端原型：选择、确认、成功三态 |
| [assets/reset-card-flow-original.png](assets/reset-card-flow-original.png) | Xerina 提供的产品图原图：账号列表与右侧选卡弹窗 |
| [assets/reset-card-flow-2.png](assets/reset-card-flow-2.png) | 产品图 2 实现基准（与原图同一视觉稿） |
| [assets/reset-card-flow.png](assets/reset-card-flow.png) | 第一版三态流程视觉参考 |

## 已确认的官方接口

```text
GET  /backend-api/wham/rate-limit-reset-credits
POST /backend-api/wham/rate-limit-reset-credits/consume
```

额度回读继续使用现有：

```text
GET /backend-api/wham/usage
```

## 当前实现状态

- Codex OAuth 登录、token 刷新、额度查询、`reset_at` 解析已经存在。
- Codex provider 已接入重置卡列表和消费接口，使用账号级锁、持久化幂等键和安全 DTO。
- 迁移 043 已记录操作状态和卡 ID 哈希；消费成功后复用既有 `/wham/usage` 额度回读。
- Tauri 与 Web 管理命令、列表/卡片视图重置入口、右侧选卡弹窗和成功/失败结果均已接入；不设计灰度发布。后端接口、账号校验、消费幂等和额度回读原理保持不变。
- provider mock、全量 Rust 测试和 `pnpm build` 已通过；`cargo fmt --check` 仍受未改动基线文件的格式差异影响。
- 指定账号的真实验收记录保留在验收 Spec；桌面链路排查中的临时过程记录已清理。

## 不可变更的安全规则

- 不读取或注入浏览器 Cookie。
- 不把 OAuth token、Authorization header 或完整卡 ID写入日志、URL、错误和前端持久状态。
- 不通过清空本地 `quota_json` 伪造额度重置。
- 消费结果未知时沿用原幂等键，禁止换键重扣或自动换卡。
- 接口漂移时停止新增消费请求，保留现有额度刷新和官方 Usage fallback。

## 当前复核结论

- 重置卡请求复用 `global_proxy_url()`、`blocking_client` 和现有 Codex `auth_headers`，没有新增代理或凭证存储。
- 额度回读复用 `AuthService::refresh_quota`，没有清空或伪造本地 `quota_json`。
- 未知结果会持久化为 `unknown`；后端恢复命令只读已有操作，不会生成新幂等键。前端尚未展示该操作 ID，因此未知结果后的人工恢复入口仍待补齐。
