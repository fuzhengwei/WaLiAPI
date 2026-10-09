# SQLite FFI 兼容补丁

SQLx 0.8.6 与现有 Tauri SQL 插件要求 `libsqlite3-sys ^0.30.1`，其中内置 SQLite 3.46.0 受 [WAL-reset 缺陷](https://sqlite.org/wal.html#walresetbug) 影响。该系列没有包含修复的补丁版本；仅把应用升级 SQLx 0.9 会与插件的原生 `links=sqlite3` 冲突。

本地 0.30.1 包仅转出官方 `libsqlite3-sys =0.37.0` 的公共 FFI，并转发原特性。实际包包含 SQLite 3.51.3；它是唯一声明 `links=sqlite3` 并编译原生库的包。补丁不保存 SQLite C 源码，不修改 Cargo registry，不删除 Tauri 插件能力。`column_metadata` 显式保留 SQLx 使用的列来源接口，原最低版本特性转发为新包的最低版本要求。运行时在打开 WAL 前要求 SQLite 至少 3.51.3，避免构建环境意外选择未修复的系统库。

来源：[rusqlite 0.39.0 / libsqlite3-sys 0.37.0](https://github.com/rusqlite/rusqlite/tree/v0.39.0/libsqlite3-sys)，官方实际包采用 MIT 协议，版本和发布校验由 Cargo.lock 固定。这里的转出文件没有复制上游实现。兼容验证覆盖现有 SQLx 查询、迁移、FTS 与事务，并分别执行默认特性测试和 Headless 编译检查；项目未使用的 SQLCipher、扩展加载及其他平台交叉编译路径未验证。

当应用与 Tauri SQL 插件都支持同一修复版本的 FFI 后，删除本地 patch 及此目录，统一使用官方依赖。
