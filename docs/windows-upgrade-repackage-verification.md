# 0.4.0 行尾兼容修复重新打包与安装验收

日期：2026-10-09。对应排查：[Windows 升级启动失败](windows-0.3.9-to-0.4.0-startup-diagnosis.md)。

## 正常升级安装包

- 文件：`test-results/upgrade-build/release/WaLiAPI_0.4.0_x64-setup_crlf-fix.exe`
- 类型：Windows x64 NSIS 安装包，版本号保留 0.4.0。
- 大小：24,452,309 字节。
- SHA-256：`5E87ED9BF872DC2BDE0A999DA5381822F0CD8A34ECF8BE4C760AAF58CCDD7EBC`
- 保留正式应用标识 `waliapi.xiaofuge.cn`，可用于升级原应用。
- 已包含历史迁移 LF/CRLF 兼容修复和上游 WAL 容错修复，以及 pdfium 资源。
- 本地验收包未做代码签名，也没有生成在线更新签名文件。
- 前端 TypeScript 检查和 Vite 构建通过；Rust release 构建及 NSIS 打包完成，LTO / codegen-units 保持项目原配置。

## 安装验收隔离方式

原 0.3.9 网关正在承载请求，因此以一致性旧库副本验收安装与启动，没有停止或覆盖原网关。

- 单独构建相同源码的 release 验收包，仅改变应用名称、标识、窗口标题和主程序文件名。
- 名称：`WaLiAPI Upgrade Test`。
- 标识：`waliapi.xiaofuge.cn.upgrade-test-20261009`。
- 主程序：`waliapi-upgrade-test.exe`。不同进程名防止 NSIS 安装器关闭原 `waliapi.exe`。
- 数据目录：`%APPDATA%\waliapi.xiaofuge.cn.upgrade-test-20261009`。
- 服务地址：`http://127.0.0.1:18777`。
- 副本内关闭主动健康探测、OTLP 导出，禁用 OAuth 账号自动维护，防止重复刷新真实账号凭证；原账号数据和状态没有改动。
- 副本内关闭“关闭到托盘”，便于验收正常退出和再次启动。
- 网页检查仅使用 Orca 内置浏览器；原生窗口通过 Orca 的无截图辅助功能状态检查。

## 数据基线

对原库开启固定只读事务，通过 SQLite backup API 创建一致性快照，避免持续写入造成分段备份反复重试。快照用时 28 秒，文件为 11,131,953,152 字节，迁移版本 45。

| 表 | 副本启动前数量 |
| --- | ---: |
| channels | 7 |
| api_keys | 1 |
| auth_accounts | 7 |
| request_logs | 2817 |
| kb_chunks | 0 |
| prompt_templates | 8 |

副本中迁移 1～45 的校验值均只能匹配 CRLF 变体；当前编译期 LF 校验全部不匹配，保留了实际故障触发条件。

使用摘要核对渠道、密钥和账号凭证数据保留，不在记录中输出实际密钥或 Token。账号摘要排除为了隔离而设置的 `disabled` 字段。

## 验收结果

安装、真实旧库升级和重复启动均通过。这里验证的是旧库副本；正常升级安装包已生成，原 0.3.9 安装未被替换。

| 验收项 | 实测结果 |
| --- | --- |
| NSIS 安装 | 独立名称的 release 安装包安装成功，安装器退出码为 0 |
| 首次服务可用 | 81.24 秒，监听 `127.0.0.1:18777` |
| 迁移前备份 | `VACUUM INTO` 用时 77.98 秒，生成 3,072,475,136 字节的一致快照；源库因包含空闲页而为约 11.1 GB |
| 历史校验修正 | 原有逻辑修正版本 5、7，新逻辑修正另 43 条 LF/CRLF 差异 |
| 数据库升级 | schema 45 → 46，新增 `usage_stats` 表，WAL 模式正常 |
| 迁移校验 | 1～46 全部匹配当前编译源文件，未知校验差异为 0 |
| 关键数据保留 | 渠道、API Key、账号凭证的 SHA-256 摘要前后一致；账号只排除隔离设置的 `disabled` 字段 |
| 正常退出 | 测试窗口接受关闭请求，进程正常退出 |
| 再次启动 | 1.81 秒，未增加备份文件，日志中没有重复校验修正 |
| 桌面界面 | 可见窗口正常响应，首页显示 `v0.4.0`、服务“运行中”、Base URL `http://127.0.0.1:18777`，统计数据已展示 |
| HTTP 健康检查 | 首次及再次启动均返回 `status: ok`、`running: true`、`version: 0.4.0` |
| 原网关 | 原进程保持运行，8777 健康检查仍返回 `version: 0.3.9`，原库仍为 schema 45 |

首次启动等待主要来自大数据库的备份，期间桌面窗口可能暂时显示“未响应”。此次备份完成后校验修正和迁移顺利完成；约 81 秒的首次耗时不代表持续卡死。再次启动没有备份，约 1.8 秒即恢复服务。

自动升级前备份也已只读核对：schema 45、45 条成功迁移记录，渠道 7、密钥 1、账号 7、请求日志 2817，与迁移前基线一致。验收结束后测试进程正常关闭，独立安装及数据副本保留供复查。

### 表数量与日志保留

| 表 | 启动前 | 启动后 |
| --- | ---: | ---: |
| channels | 7 | 7 |
| api_keys | 1 | 1 |
| auth_accounts | 7 | 7 |
| request_logs | 2817 | 2731 |
| kb_chunks | 0 | 0 |
| prompt_templates | 8 | 8 |

副本沿用原设置 `logs.retention_days=1`。减少的 86 条请求日志全部早于启动时的一天保留截止时间（UTC 时间范围为 2026-10-08 08:58:59～09:41:54）；属于 `audit_log::run_maintenance_loop` 的正常过期清理。用量统计在清理前回填，桌面仍展示累计约 2.8K 请求。渠道、密钥、账号、模板和知识切片数量没有变化。

### 本地证据

运行证据保存在忽略目录 `test-results/upgrade-build/`。以下摘要和界面/健康结果不输出明文凭证；真实旧库快照保存在该目录的 `rollback/data/` 中：

- `isolated-launch.json`、`isolated-restart.json`：首次及再次启动耗时、进程与备份数量。
- `isolated-data-verification.json`：只读数据摘要和过期日志核对。
- `isolated-after-checksums.json`：升级后 46 条迁移校验结果。
- `isolated-repeat-ui.json`：Orca 无截图原生窗口树，包含版本、服务状态和首页内容。
- `isolated-first-health.json`、`isolated-repeat-health.json`、`original-after-health.json`：Orca 内置浏览器健康检查结果。
- `original-and-backup-verification.json`、`isolated-final-close.json`：原库未升级、备份基线核对及测试进程正常关闭结果。
- `build.*.log`、`isolated-build.*.log`：常规包及隔离包的 release 构建记录。

验收没有向真实上游模型发送请求；OAuth 账号在副本中禁用，原网关仍承担已有请求。该结果覆盖安装、数据库升级、前端启动和服务健康；不替代所有业务功能的完整验收。
