-- Security audit summary for request logs
ALTER TABLE request_logs ADD COLUMN risk_level TEXT NOT NULL DEFAULT 'clean';
ALTER TABLE request_logs ADD COLUMN risk_score INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_logs ADD COLUMN risk_summary TEXT;
ALTER TABLE request_logs ADD COLUMN security_action TEXT NOT NULL DEFAULT 'allow';
ALTER TABLE request_logs ADD COLUMN sanitized INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request_logs ADD COLUMN blocked_reason TEXT;

CREATE TABLE IF NOT EXISTS request_security_findings (
    id              TEXT PRIMARY KEY,
    log_id          TEXT NOT NULL,
    phase           TEXT NOT NULL,
    category        TEXT NOT NULL,
    rule_id         TEXT NOT NULL,
    severity        TEXT NOT NULL,
    title           TEXT NOT NULL,
    description     TEXT,
    location        TEXT,
    evidence_masked TEXT,
    evidence_hash   TEXT,
    action          TEXT,
    created_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_logs_risk_level ON request_logs(risk_level);
CREATE INDEX IF NOT EXISTS idx_logs_risk_score ON request_logs(risk_score);
CREATE INDEX IF NOT EXISTS idx_findings_log ON request_security_findings(log_id);
CREATE INDEX IF NOT EXISTS idx_findings_rule ON request_security_findings(rule_id);
