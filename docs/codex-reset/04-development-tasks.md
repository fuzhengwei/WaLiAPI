# Codex 重置卡：开发任务清单

**执行方式**：按阶段顺序推进；同一文件不并行修改。
**任务状态**：`[ ]` 未开始，`[~]` 进行中，`[x]` 已完成，`[!]` 阻塞。
**关联**：[03-implementation-plan.md](03-implementation-plan.md)、[05-acceptance-spec.md](05-acceptance-spec.md)

## 阶段 0：契约和能力

- [x] T00.1 在 `codex_backend.rs` 增加重置卡列表、消费请求和响应 wire DTO；验证 mock 请求命中两个精确路径。
- [x] T00.2 实现认证头 allowlist、独立超时和四类结果 code 映射；验证 Bearer、`chatgpt-account-id`、`originator` 正确且调用方 Authorization 不被转发。
- [x] T00.3 在 `Provider` trait 增加默认 UnsupportedFeatures 能力，在 `ProviderSpec` 增加 `supports_reset_credit`；验证只有 Codex 为 true。
- [x] T00.4 增加接口不可用时的安全 fallback；验证不发送重复消费请求且返回固定 Usage URL。

## 阶段 1：数据和 Repository

- [x] T01.1 新增 `src-tauri/migrations/045_auth_reset_operations.sql`；验证旧数据库升级、内存 SQLite 建表和唯一索引。
- [x] T01.2 在 `db/models.rs` 定义 reset operation 状态、错误分类和安全摘要；验证序列化不含 payload、完整卡 ID 或认证字段。
- [x] T01.3 在 `db/repository.rs` 增加 pending 创建、幂等读取、状态更新和未完成操作查询；验证账号隔离、重复创建和应用重启恢复。
- [x] T01.4 用单向哈希保存卡标识并补齐索引；验证日志和数据库字段无法还原或输出完整卡 ID。

## 阶段 2：AuthService 编排

- [x] T02.1 实现 `list_reset_credits(account_id)`；验证 API Key、其他 provider、失效、停用和不存在账号在发网前失败。
- [x] T02.2 实现服务端二次卡列表校验；验证跨账号卡、过期卡、非 `available` 卡和非 `codex_rate_limits` 卡不会发送 POST。
- [x] T02.3 实现每账号消费锁、逻辑操作 ID 和 UUID `redeem_request_id`；验证同账号重复点击只产生一个上游请求，不同账号互不阻塞。
- [x] T02.4 实现 `consume_reset_credit` 的四类结果持久化和安全提示；验证未知 code 不被当作成功。
- [x] T02.5 实现超时/断连的 pending 或 unknown 恢复；验证恢复只复用原幂等键，不换卡、不生成新键。
- [x] T02.6 在 reset/already_redeemed 后调用现有 `refresh_quota`；验证只更新目标账号，回读失败时旧 `quota_json` 保留。
- [x] T02.7 增加日志脱敏和审计字段；验证 token、Cookie、Authorization、上游正文和完整卡 ID 不出现在日志、错误或事件中。

## 阶段 3：Tauri 与 Web 管理面

- [x] T03.1 在 `commands/auth.rs` 增加 `auth_list_reset_credits`、`auth_consume_reset_credit`、`auth_resume_reset_operation`；验证只返回安全 DTO。
- [x] T03.2 在 `lib.rs` 注册新命令；验证桌面 MockRuntime 可调用，既有 `auth_refresh_quota` 未改变。
- [x] T03.3 在 `server/admin_routes.rs` 增加 invoke 分发；验证 Web 管理会话、CSRF、参数和返回结构与 Tauri 一致。
- [x] T03.4 增加固定官方 Usage fallback；验证 URL 无 query、fragment、邮箱、token 或内部账号 ID。

## 阶段 4：前端闭环

- [x] T04.1 在 `src/types/index.ts` 增加 capability、卡摘要、快照、操作结果和错误类型；验证与 Rust DTO 的字段一致。
- [x] T04.2 在 `src/lib/api.ts` 增加三个 auth API 方法并通过 `runtime.ts` invoke；静态检索验证无裸 fetch/直接 Tauri invoke。
- [x] T04.3 在 `AccountCard.tsx` 为有效 Codex OAuth 账号增加入口；验证 Kimi、Gemini、Grok、API Key、停用和失效账号不显示消费按钮。
- [x] T04.4 按 `docs/codex-reset/assets/reset-card-flow-2.png` 复原右侧选卡弹窗；验证默认选中首张可用卡、按原始时间值过滤卡、显示本地时区、取消不发 POST、重复点击被禁用。后端消费链路不改动。
- [x] T04.5 在 `AuthChannelsPage.tsx` 串接查询、确认、结果、额度刷新和卡列表刷新；验证账号 A 的操作不会改变账号 B。
- [x] T04.6 前端确认时生成并复用当前任务的 `operationId`，未知结果锁定任务状态；后端只读恢复命令保留，不增加历史结果查询。

## 阶段 5：测试和回归

- [x] T05.1 provider mock contract 覆盖 GET/POST 路径、请求体、认证头、四类结果和未知 code；运行 Codex provider 聚焦测试。
- [x] T05.2 service + Repository 聚焦测试覆盖账号隔离、状态校验、消费锁、幂等恢复、043 迁移和 quota 回读失败；运行对应 Rust 测试。
- [~] T05.3 provider command/admin 已接入，前端三态、取消、重复点击、任务状态和 operationId 传递已实现；尚未建立独立的 command/admin 与 React 自动化 smoke test。
- [~] T05.4 `cargo test --no-fail-fast`（1149 个库测试及全部集成测试）、必要 Rust 聚焦测试和 `pnpm build` 已通过；`cargo fmt --check` 仍被基线文件 `src-tauri/src/endpoint_executor/grok_arguments.rs` 的既有格式差异阻塞。
- [x] T05.5 在 `2867406171@qq.com` 执行只读列表 smoke test；存在 3 张可用卡，唯一目标为 UTC 2026-10-04 的 `codex_rate_limits` 卡。
- [x] T05.6 在 `2867406171@qq.com` 从桌面端选择 UTC 2026-10-04 到期卡并执行唯一一次真实消费；上游返回 `reset`，额度回读 `refreshed`，验收结束。

## 阶段 6：必要回归和交付

- [~] T06.1 已核对登录、刷新令牌、模型同步、额度刷新和 `/v1/*` 的调用边界未被重置命令直接改写；完整回归矩阵仍待补跑。
- [x] T06.2 完成官方 Usage fallback 和 pending 恢复演练；验证不会重复消费或清空 quota。
- [x] T06.3 已按当前代码、验证结果和未完成项更新本目录文档。

## 每个任务的交付记录

任务完成时在变更记录中写明：修改文件、接口变化、测试命令与结果、兼容性影响、未决风险。未经 Xerina 明确要求，不提交、推送或发布。
