-- Built-in security rules (seeded by app, editable by user)
CREATE TABLE IF NOT EXISTS security_builtin_rules (
    id          TEXT PRIMARY KEY,
    rule_id     TEXT NOT NULL UNIQUE,
    category    TEXT NOT NULL,
    severity    TEXT NOT NULL DEFAULT 'medium',
    title       TEXT NOT NULL,
    description TEXT,
    toggle_key  TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL
);

-- Custom security rules (user-defined blacklist/whitelist entries)
CREATE TABLE IF NOT EXISTS security_custom_rules (
    id          TEXT PRIMARY KEY,
    rule_type   TEXT NOT NULL,          -- 'blacklist' | 'whitelist'
    category    TEXT NOT NULL,          -- 'domain' | 'tool' | 'path' | 'keyword'
    pattern     TEXT NOT NULL,
    severity    TEXT NOT NULL DEFAULT 'medium',
    action      TEXT NOT NULL DEFAULT 'warn',
    enabled     INTEGER NOT NULL DEFAULT 1,
    description TEXT,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_builtin_rules_category ON security_builtin_rules(category);
CREATE INDEX IF NOT EXISTS idx_builtin_rules_enabled ON security_builtin_rules(enabled);
CREATE INDEX IF NOT EXISTS idx_custom_rules_type ON security_custom_rules(rule_type);
CREATE INDEX IF NOT EXISTS idx_custom_rules_category ON security_custom_rules(category);
CREATE INDEX IF NOT EXISTS idx_custom_rules_enabled ON security_custom_rules(enabled);
