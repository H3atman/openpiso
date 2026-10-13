-- The random Installation ID is minted at the first Daily Summary, not at
-- install time. Telemetry tables are excluded from Backups, so a restored
-- Admin starts without an ID or pending summaries.
CREATE TABLE telemetry_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  installation_id TEXT NOT NULL,
  last_summary_period TEXT
);

-- One unsent summary per fixed UTC+08:00 reporting day. Retries keep the
-- same event UUID and payload; the worker selects rows due by UTC instant.
CREATE TABLE telemetry_outbox (
  period_utc8 TEXT PRIMARY KEY,
  event_uuid TEXT NOT NULL UNIQUE,
  payload TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  -- RFC 3339 UTC instant of the next allowed attempt; due when <= now.
  next_attempt_at TEXT NOT NULL,
  last_error TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_telemetry_outbox_due ON telemetry_outbox (next_attempt_at);
