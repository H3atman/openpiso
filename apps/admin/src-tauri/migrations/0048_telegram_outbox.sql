-- Telegram delivery outbox (ADR-0076 / GitHub #346): the durable, restart-safe
-- queue of PC Monitoring incident deliveries for the connected private owner
-- chat. Each row is one redacted delivery item whose final message text is
-- composed at enqueue time inside the same SQLite transaction as its
-- incident row change and Audit event; the delivery worker only ever sends
-- committed rows and writes back delivery state. Local incidents and the
-- Audit Log stay canonical whether or not any message is delivered.
--
-- Like `telegram_config`, this table belongs to the Telegram integration and
-- is listed in `backup::BACKUP_EXCLUDED_TABLES`: a restored Admin starts
-- disconnected, and disconnecting deletes pending deliveries.
--
-- Message text carries only PC display name, incident time, observed duration
-- when known, and closure reason — never IP, MAC, Account, username,
-- customer, balance, Remaining Time, Points, secrets, or raw metadata.
CREATE TABLE telegram_outbox (
  id TEXT PRIMARY KEY,
  -- 'opening' items batch together; every other item (closure, resolved
  -- summary, monitoring-disabled state) batches as 'closure'.
  item_class TEXT NOT NULL CHECK (item_class IN ('opening', 'closure')),
  -- Final redacted text. One line per incident; the worker joins same-batch
  -- lines into one Telegram message.
  message_text TEXT NOT NULL,
  -- The incident this item delivers, when it is one. NULL for the
  -- monitoring-disabled state message.
  incident_id TEXT REFERENCES pc_monitoring_incidents(id),
  created_at TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'abandoned')),
  attempts INTEGER NOT NULL DEFAULT 0,
  -- RFC 3339 UTC instant of the next allowed attempt; due when <= now.
  next_attempt_at TEXT NOT NULL,
  -- Same-instant items share a bucket: unix seconds divided by the fixed
  -- 30-second batch window. The worker sends each (item_class, bucket)
  -- group as one message.
  batch_bucket INTEGER NOT NULL,
  last_error TEXT,
  last_error_at TEXT,
  sent_at TEXT
);

-- Worker selection scan: due, still-pending items.
CREATE INDEX idx_telegram_outbox_due ON telegram_outbox (status, next_attempt_at);
