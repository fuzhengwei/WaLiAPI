-- 039: 用一条覆盖索引服务全部日志聚合查询，并取代已被判定失效的 029
--
--
-- 编号说明：取 039 而非 037 —— 已核实在飞的 open PR 中，#106 占用 037
-- （037_kb_search_projection.sql）、#109 占用 038（038_api_key_knowledge_access.sql），
-- 两者 base 同为 v0.3.2。sqlx 不允许同一版本出现两份迁移，故跳过 037/038。
-- 若合并顺序变化需要再顺延，改动仅是重命名本文件 + 首行编号。
--
-- 症状：仪表盘每次切回"加载中" 3-4 秒；密钥页切过去 2 秒以上才出内容。
--
-- 根因有两层，都指向同一列 cached_tokens：
--
-- (1) 列位置导致的行溢出。cached_tokens 由迁移 026 以 ALTER TABLE ADD COLUMN 加入，
--     cid 排在 cid=18 的 request_body（Agent 流量下平均约 900 KB）之后，于是它的值
--     落在 overflow 页链上；读它必须走完每一行的 overflow 链，等价于把 request_body
--     的字节总量重读一遍。对照：同一张表上 SUM(prompt_tokens) 只要 11 ms。
--
-- (2) 迁移 029 的索引被 0.3.x 判定失效。029 建的是 (created_at, cached_tokens)。
--     0.3.1 的渠道健康探测给这些聚合查询统一加了 `is_probe = 0` 过滤，而该索引不含
--     is_probe 列 → 不再"覆盖" → SQLite 放弃索引改走 SCAN request_logs（执行计划可证），
--     正好落回 (1) 的溢出代价。也就是说 029 修好的东西被 0.3.1 又弄坏了。
--
-- 实测（2.88 GB 真实库副本，取 3 次最小值）：
--   SUM(cached_tokens) WHERE is_probe=0        2602 ms ->   0.13 ms
--   SUM(cached_tokens) WHERE created_at LIKE   412 ms ->   0.17 ms
--   get_model_stats                           2660 ms ->   0.71 ms
--   get_api_key_stats                         2590 ms ->   0.76 ms
--   get_token_trend                             0.32 ms ->  0.23 ms（不回退）
--   日志列表（摘要，LIMIT 40）                  10.6 ms -> 11.2 ms（噪声级）
--   get_dashboard_stats 内 12 条查询合计       8105 ms ->  4.8 ms
--
-- 为什么是这一组列：is_probe 打头（所有聚合都是等值过滤），其后是各查询的过滤列与
-- 投影列的并集，使它们全部可以 index-only 完成、永不回表。
--
-- 为什么顺带删掉 029：宽索引以 is_probe 打头且包含 created_at，实测"029+宽"与"仅宽"
-- 逐条耗时相同（0.13/0.17/0.71/0.76/0.23 vs 0.13/0.18/0.74/0.80/0.25），029 已无价值，
-- 留着只是白白多一份写放大。
--
-- 代价：新增索引净体积 0.98 MB（占 2.88 GB 库的 0.03%，约 360 字节/行）；建索引一次性
--   约 2.5 秒。插入写放大仍不可测 —— 每行本来就要写约 900 KB 的 request_body，
--   多这 360 字节是噪声级。
--
-- 说明：本迁移只动索引，不改任何查询语义与数据。

CREATE INDEX IF NOT EXISTS idx_logs_stats ON request_logs(
    is_probe,
    created_at,
    cached_tokens,
    prompt_tokens,
    completion_tokens,
    total_tokens,
    duration_ms,
    status_code,
    model,
    api_key_id
);

DROP INDEX IF EXISTS idx_logs_created_cached;
