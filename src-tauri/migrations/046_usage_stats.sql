-- 用量统计聚合表:与审计日志解耦,清理日志不影响已统计的调用量与 Token。
-- 四维主键 (hour, model, channel_id, api_key_id):hour 为 UTC 小时桶,
-- channel_id/api_key_id 为空字符串表示无渠道(Auth 账号等)/无 Key。
-- 增长规模为 O(组合数 × 小时)而非 O(请求数),长期保留可接受。
CREATE TABLE IF NOT EXISTS usage_stats (
    hour              TEXT    NOT NULL,  -- YYYY-MM-DDTHH:00:00.000Z(UTC 小时桶)
    model             TEXT    NOT NULL,
    channel_id        TEXT    NOT NULL DEFAULT '',
    api_key_id        TEXT    NOT NULL DEFAULT '',
    request_count     INTEGER NOT NULL DEFAULT 0,
    success_count     INTEGER NOT NULL DEFAULT 0,   -- status_code 2xx
    fail_count        INTEGER NOT NULL DEFAULT 0,   -- 非 2xx
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens      INTEGER NOT NULL DEFAULT 0,
    cached_tokens     INTEGER NOT NULL DEFAULT 0,
    total_duration_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (hour, model, channel_id, api_key_id)
);

-- 小时前缀范围查询(今日/趋势/按天统计)走此索引。
CREATE INDEX IF NOT EXISTS idx_usage_stats_hour ON usage_stats(hour);
