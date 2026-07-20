-- WaLiAPI database initialization

-- 渠道表
CREATE TABLE IF NOT EXISTS channels (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    type          TEXT NOT NULL,
    base_url      TEXT NOT NULL,
    api_key       TEXT NOT NULL,
    models        TEXT NOT NULL DEFAULT '[]',
    status        INTEGER NOT NULL DEFAULT 1,
    priority      INTEGER NOT NULL DEFAULT 0,
    weight        INTEGER NOT NULL DEFAULT 1,
    config        TEXT NOT NULL DEFAULT '{}',
    model_mapping TEXT NOT NULL DEFAULT '{}',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    last_test_at  TEXT,
    last_test_ok  INTEGER
);

-- API Key 表
CREATE TABLE IF NOT EXISTS api_keys (
    id               TEXT PRIMARY KEY,
    name             TEXT NOT NULL,
    key              TEXT NOT NULL UNIQUE,
    status           INTEGER NOT NULL DEFAULT 1,
    allowed_models   TEXT NOT NULL DEFAULT '[]',
    allowed_channels TEXT NOT NULL DEFAULT '[]',
    quota_limit      INTEGER NOT NULL DEFAULT -1,
    quota_used       INTEGER NOT NULL DEFAULT 0,
    expires_at       TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

-- 请求日志表
CREATE TABLE IF NOT EXISTS request_logs (
    id                TEXT PRIMARY KEY,
    seq               INTEGER NOT NULL DEFAULT 0,   
    api_key_id        TEXT,
    api_key_name      TEXT,
    channel_id        TEXT,
    channel_name      TEXT,
    model             TEXT NOT NULL,
    upstream_model    TEXT,
    mode              TEXT NOT NULL,
    status_code       INTEGER NOT NULL,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens      INTEGER NOT NULL DEFAULT 0,
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    error_message     TEXT,
    is_stream         INTEGER NOT NULL DEFAULT 0,
    is_retry          INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_logs_created ON request_logs(created_at);
CREATE INDEX IF NOT EXISTS idx_logs_channel ON request_logs(channel_id);
CREATE INDEX IF NOT EXISTS idx_logs_api_key ON request_logs(api_key_id);
CREATE INDEX IF NOT EXISTS idx_logs_model ON request_logs(model);
