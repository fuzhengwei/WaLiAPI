-- Codex 重置卡消费操作。只保存卡 ID 单向哈希和幂等键，不保存完整卡 ID 或凭证。
CREATE TABLE IF NOT EXISTS auth_reset_operations (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    credit_id_hash TEXT NOT NULL,
    redeem_request_id TEXT NOT NULL,
    status TEXT NOT NULL,
    upstream_code TEXT,
    error_class TEXT,
    quota_refresh_status TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (account_id, redeem_request_id)
);

CREATE INDEX IF NOT EXISTS idx_auth_reset_operations_account
    ON auth_reset_operations(account_id, updated_at DESC);
