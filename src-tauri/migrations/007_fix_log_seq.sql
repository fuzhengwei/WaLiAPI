-- Fix seq column: backfill existing rows with rowid, change default to NULL
-- The seq column was NOT NULL DEFAULT 0, so the "backfill when NULL" logic never triggered
UPDATE request_logs SET seq = rowid WHERE seq = 0 OR seq IS NULL;
