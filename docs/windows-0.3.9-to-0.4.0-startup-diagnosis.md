# Windows 从 0.3.9 升级 0.4.0 后启动失败排查

排查日期：2026-10-09。现场为安装包升级后首次启动白屏、窗口未响应。

## 结论

旧 Windows 安装包按 CRLF 字节编译 SQL 迁移。0.4.0 新增 `.gitattributes` 强制 SQL 文件使用 LF，改变了 SQLx 对全部历史迁移计算的 SHA-384 校验值。旧库结构没有因此改变，但新版启动时的迁移验证会报 `VersionMismatch(1)`，随后 `expect("failed to run database migrations")` 触发 panic。

启动路径在 Tauri `.setup()` 中使用 `tauri::async_runtime::block_on`，同步等待数据库备份和初始化。现场大库备份耗时 8～77 秒，这解释了失败之前的长时间“未响应”。桌面 release 没有控制台，现有 tracing 初始化没有将 Rust panic 接入文件日志，因此日志能停在“迁移前已备份数据库”而不显示最终的迁移校验错误。

上游最新的 WAL 容错修复处理另一种启动失败：文件被占用时 DELETE→WAL 转换失败。它没有处理历史迁移行尾兼容，不能单独解决这份旧库的升级问题。

## 版本和现场证据

| 项目 | 现场结果 |
| --- | --- |
| 上游 0.3.9 分支 | `fd115a549bbbd00fc9078e1d510ebfa41d93da7d` |
| 最初 0.4.0 发布标签 `all-v0.4.0` | `90d56ee8932fc97015be663a31324f4db810547b` |
| 最新 0.4.0 分支 / `all-v0.4.0.1` | `036416ce3bf2a4083ac0649b02aec3c910b80b02`，WAL 容错修复 |
| 行尾规则引入提交 | `85205af7ef130ca7fb12277776198cb792559b58`，2026-10-08 21:35 +08:00 |
| 排查时本地运行程序 | `waliapi.exe`，产品版本 0.3.9，已回退 |
| 本地数据库 | `%APPDATA%\waliapi.xiaofuge.cn\waliapi.db`，约 11.1 GB |
| 数据库迁移版本 | 45，未应用新版唯一新增迁移 046 |
| 旧校验值与当前 LF 文件比较 | 1～45 全部不匹配 |
| 将同一文件仅转为 CRLF 后再比较 | 1～45 全部匹配，无无法识别的内容差异 |
| 现有迁移补偿 | 仅更新 005、007，无法补偿 001 等其余版本 |
| 知识库检索投影待回填 | 0 条 |

初始排查只读取迁移元数据、存储信息和统计数量，没有展示账号凭证、渠道密钥或请求正文。后续安装验收使用本机一致性数据库副本，凭证通过摘要核对；详见[重新打包与安装验收](windows-upgrade-repackage-verification.md)。

本地 `waliapi.log.2026-10-09` 中多次出现以下序列（时间转换为北京时间）：

| 开始备份 | 完成备份 | 耗时 | 结果 |
| --- | --- | --- | --- |
| 15:20:50 | 15:22:06 | 76.73 秒 | schema 45 → 46 备份完成，此后没有服务启动记录 |
| 15:33:52 | 15:34:35 | 43.39 秒 | 同上 |
| 15:36:58 | 15:37:07 | 8.40 秒 | 同上 |
| 16:54:30 | 16:55:19 | 48.33 秒 | 同上 |

15:29、15:36、15:37、16:55 的后续启动则出现 `WaLiAPI server listening on http://127.0.0.1:8777`，与回退后 0.3.9 可正常运行一致。

## 复现与验证

只读现场检查：

```powershell
python scripts/diagnose-upgrade-checksums.py --database "$env:APPDATA\waliapi.xiaofuge.cn\waliapi.db"
```

退出码 1 表示存在校验值不兼容；`line_ending_only_versions` 是能证明仅由 LF/CRLF 变化导致的版本，`unrecognized_versions` 表示应继续拒绝升级、进一步排查的版本。该脚本使用 SQLite `mode=ro` 和 `query_only`，不会执行修复。

回归测试位于 `src-tauri/src/db/mod.rs`，通过真实 SQLx 迁移构建 schema 45 的 CRLF 旧库，写入业务保留标记，再调用真实 `Database::new_with_path` 启动函数。使用隔离临时目录，不连接用户正在使用的数据库。

测试结果：

- 修复前，在已经合入上游 WAL 容错修复的代码上运行升级回归测试，稳定复现 `failed to run database migrations: VersionMismatch(1)`，用例失败。
- 修复后，数据库模块 12 项测试全部通过，包括 CRLF 旧库升级、业务标记保留、重复启动、未知校验值拒绝，以及既有 WAL、备份和恢复测试。
- `cargo fmt --check`、`git diff --check` 和只读诊断脚本运行通过。
- 严格检查 `cargo clippy --locked --lib -- -D warnings` 未通过，当前仓库报 223 项既有 lint 问题（未使用导入 / 死代码 / 冗余解引用等）。结构化诊断中没有指向本次修改的 `src/db/mod.rs` 的错误。这次没有扩展为全仓 lint 清理。
- 未对用户原始数据库执行实际升级。后续已构建 Windows x64 安装包，使用真实旧库副本完成首次启动、重复启动、数据保留、桌面界面和服务健康验收，详见[验收记录](windows-upgrade-repackage-verification.md)。

复现 / 验证命令（必须在 `src-tauri` 中运行）：

```powershell
cargo test --locked --lib db::tests::startup_upgrades_windows_crlf_migration_history -- --nocapture
cargo test --locked --lib db::tests -- --nocapture
```

## 本地修复策略

在迁移前备份完成之后、SQLx 迁移验证之前，仅在旧校验值精确匹配当前 SQL 的 LF 或 CRLF 变体时，将它更新为当前编译期校验值。更新在事务中执行，只处理成功应用的迁移记录；不重复执行旧迁移，不改动已发布 SQL 文件。新增兼容逻辑不放宽未知内容差异的校验；原有 005、007 的历史补偿保持原样。

保留 `.gitattributes` 的 LF 规则以及上游 WAL 容错修复。增加升级、数据保留、重复运行以及未知校验值拒绝的回归验证。

## 后续处理边界

排查和安装验收保留原 0.3.9 服务及其数据库；修复包在独立安装和数据副本上验证。原 0.4.0 或仅带 WAL 修复的 0.4.0.1 安装包不包含本次迁移行尾兼容处理。

修复迁移校验失败后，大库首次升级的同步备份仍可能造成几十秒界面未响应。后续可将桌面初始化移到后台，并提供升级进度与明确的失败提示；这属于另外的启动体验改进。
