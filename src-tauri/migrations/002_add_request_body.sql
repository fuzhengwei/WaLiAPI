-- Add request_body column to request_logs for storing the original request payload
ALTER TABLE request_logs ADD COLUMN request_body TEXT;
