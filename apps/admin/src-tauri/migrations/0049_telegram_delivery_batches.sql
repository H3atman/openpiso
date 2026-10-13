-- Telegram delivery refinements (GitHub #347).
-- Keep incident kinds in the durable batch key so a fleet mismatch message
-- cannot be mixed with Host Probe or Client Guard Coverage evidence.
ALTER TABLE telegram_outbox ADD COLUMN incident_kind TEXT CHECK (
  incident_kind IS NULL OR incident_kind IN (
    'client_reachability_mismatch',
    'host_probe_coverage',
    'client_guard_coverage'
  )
);

-- Recover the kind for queued incident rows from the canonical incident table.
-- The NULL kind remains reserved for shop-wide state messages.
UPDATE telegram_outbox
SET incident_kind = (
  SELECT kind FROM pc_monitoring_incidents
  WHERE pc_monitoring_incidents.id = telegram_outbox.incident_id
)
WHERE incident_id IS NOT NULL;

-- Expiring and reconnecting only update delivery state. Local incidents,
-- Audit events, and Notification acknowledgement state remain untouched.
CREATE INDEX idx_telegram_outbox_batch
  ON telegram_outbox (item_class, incident_kind, batch_bucket, created_at, id);
