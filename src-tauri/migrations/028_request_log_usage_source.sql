-- 028: 标注 request_logs 里 token 用量的来源
--
-- `repair_stream_cancel_logs` 会一次性回填历史上被误记为 499 / client_cancelled
-- 且用量恒为 0 的流式请求。回填出来的数字是"按证据恢复 + 本地估算"得到的，
-- 不是上游实测值，必须能在看板和配额里区分开，否则历史估算会永久冒充实测数据。
--
-- 取值语义：
--   * NULL        — 本列引入之前写入的行，来源未知（不做追溯标注）
--   * 'upstream'  — 上游真实回传的 usage（预留，实时路径暂未写入）
--   * 'estimated' — 上游未回传 usage，实时路径本地 BPE 估算（预留）
--   * 'repaired'  — 由 repair_stream_cancel_logs 回填的行
--
-- 纯新增可空列，不改写、不删除任何既有列与既有行；老查询与日志页照常工作。

ALTER TABLE request_logs ADD COLUMN usage_source TEXT;

CREATE INDEX IF NOT EXISTS idx_logs_usage_source ON request_logs(usage_source);
