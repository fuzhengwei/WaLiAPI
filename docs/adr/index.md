# 架构决策记录（ADR）索引

本目录存放 WaLiAPI 的架构决策记录（Architecture Decision Record）。每份 ADR 记录一个有长期影响的技术决策：背景、决策内容、备选方案与影响。

约定：

- 按 `NNNN-kebab-case-title.md` 命名，序号递增，发布后不再修改内容（决策变更通过新的 ADR 标记旧记录为 superseded）。
- 文档头部使用 [Google Open Knowledge Format（OKF）](https://github.com/GoogleCloudPlatform/knowledge-catalog) 风格的 YAML frontmatter：`type`（必填）、`title`、`description`、`status`、`tags`、`timestamp`。
- 正文使用中文，结构遵循「背景 → 决策 → 备选方案 → 影响 → 验证」。

## 决策列表

| 编号 | 标题 | 状态 | 日期 |
| --- | --- | --- | --- |
| [0001](./0001-tailwind-v4-breakpoint-old-webkit-compat.md) | 用 @custom-variant 重定义 Tailwind v4 断点以兼容旧版 WebKit | accepted | 2026-10-01 |
