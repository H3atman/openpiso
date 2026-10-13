-- ADR-0058: durable derived-notification read state.
-- Baseline Alert rows mean pre-upgrade history is read without claiming an
-- owner acknowledgement or appending notification_acknowledged audit events.
CREATE TABLE notification_read_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  last_seen_at TEXT NOT NULL
);

INSERT INTO notification_read_state (id, last_seen_at)
VALUES (1, CURRENT_TIMESTAMP);

CREATE TABLE notification_alert_ack (
  audit_log_id TEXT PRIMARY KEY NOT NULL REFERENCES audit_log(id),
  acknowledged_at TEXT NOT NULL
);

INSERT INTO notification_alert_ack (audit_log_id, acknowledged_at)
SELECT id, CURRENT_TIMESTAMP
FROM audit_log
WHERE event_type IN (
  'account_deleted',
  'account_points_adjusted',
  'account_time_adjusted',
  'account_transfer_out',
  'kiosk_lock_release_throttled',
  'manual_adjustment',
  'owner_login_throttled',
  'pc_removed',
  'points_transfer_out',
  'restore_applied',
  'tamper_kill',
  'voucher_batch_voided',
  'voucher_voided'
);
