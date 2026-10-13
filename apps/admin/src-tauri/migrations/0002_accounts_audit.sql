PRAGMA foreign_keys = ON;

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  username TEXT NOT NULL,
  username_normalized TEXT NOT NULL UNIQUE,
  password_hash TEXT NOT NULL,
  remaining_seconds INTEGER NOT NULL DEFAULT 0 CHECK (remaining_seconds >= 0),
  points_balance INTEGER NOT NULL DEFAULT 0 CHECK (points_balance >= 0),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'deleted')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  deleted_at TEXT
);

CREATE TABLE owner_credentials (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  password_hash TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE audit_log (
  id TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK (outcome IN ('success', 'failure', 'denied')),
  actor TEXT NOT NULL CHECK (actor IN ('account', 'admin', 'coin_device', 'system')),
  actor_account_id TEXT,
  account_id TEXT,
  pc_id TEXT,
  amount_cents INTEGER,
  seconds_delta INTEGER,
  points_delta INTEGER,
  cash_flag INTEGER NOT NULL DEFAULT 0 CHECK (cash_flag IN (0, 1)),
  summary TEXT NOT NULL,
  metadata_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (actor_account_id) REFERENCES accounts(id),
  FOREIGN KEY (account_id) REFERENCES accounts(id),
  FOREIGN KEY (pc_id) REFERENCES pcs(id)
);

CREATE INDEX idx_audit_log_created_at ON audit_log(created_at);
CREATE INDEX idx_audit_log_event_type ON audit_log(event_type);
CREATE INDEX idx_audit_log_actor ON audit_log(actor, created_at);
CREATE INDEX idx_audit_log_account_id ON audit_log(account_id, created_at);
CREATE INDEX idx_audit_log_pc_id ON audit_log(pc_id, created_at);

INSERT INTO audit_log (
  id, event_type, outcome, actor, pc_id, amount_cents, seconds_delta,
  cash_flag, summary, metadata_json, created_at
)
SELECT
  'm1-coin-' || id, 'coin_accepted', 'success', 'coin_device', pc_id,
  amount_cents, seconds_added, 1, 'Accepted legacy coin transaction',
  json_object(
    'legacy_table', 'coin_transactions', 'legacy_id', id,
    'device_id', device_id, 'pulse_count', pulse_count, 'source', source
  ),
  created_at
FROM coin_transactions;

INSERT INTO audit_log (
  id, event_type, outcome, actor, pc_id, seconds_delta, cash_flag, summary,
  metadata_json, created_at
)
SELECT
  'm1-adjustment-' || id, 'manual_adjustment', 'success', 'admin', pc_id,
  seconds_delta, 0, 'Applied legacy manual time adjustment',
  json_object(
    'legacy_table', 'manual_adjustments', 'legacy_id', id, 'reason', reason
  ),
  created_at
FROM manual_adjustments;

INSERT INTO audit_log (
  id, event_type, outcome, actor, cash_flag, summary, metadata_json, created_at
)
SELECT
  id, 'legacy_event', 'success', 'system', 0, 'Migrated legacy audit event',
  json_object(
    'legacy_action', action, 'legacy_entity_type', entity_type,
    'legacy_entity_id', entity_id, 'legacy_metadata_json', metadata_json
  ),
  created_at
FROM audit_logs
WHERE NOT (
  action = 'coin.accepted'
  AND entity_type = 'coin_transaction'
  AND EXISTS (
    SELECT 1 FROM coin_transactions WHERE coin_transactions.id = audit_logs.entity_id
  )
);

ALTER TABLE coin_transactions ADD COLUMN audit_event_id TEXT REFERENCES audit_log(id);
UPDATE coin_transactions SET audit_event_id = 'm1-coin-' || id;

ALTER TABLE manual_adjustments ADD COLUMN audit_event_id TEXT REFERENCES audit_log(id);
UPDATE manual_adjustments SET audit_event_id = 'm1-adjustment-' || id;

CREATE UNIQUE INDEX idx_coin_transactions_audit_event_id
  ON coin_transactions(audit_event_id);
CREATE UNIQUE INDEX idx_manual_adjustments_audit_event_id
  ON manual_adjustments(audit_event_id);

CREATE TRIGGER coin_transactions_require_audit_event_insert
BEFORE INSERT ON coin_transactions
WHEN NEW.audit_event_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'coin transaction requires canonical audit event');
END;

CREATE TRIGGER coin_transactions_require_audit_event_update
BEFORE UPDATE OF audit_event_id ON coin_transactions
WHEN NEW.audit_event_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'coin transaction requires canonical audit event');
END;

CREATE TRIGGER manual_adjustments_require_audit_event_insert
BEFORE INSERT ON manual_adjustments
WHEN NEW.audit_event_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'manual adjustment requires canonical audit event');
END;

CREATE TRIGGER manual_adjustments_require_audit_event_update
BEFORE UPDATE OF audit_event_id ON manual_adjustments
WHEN NEW.audit_event_id IS NULL
BEGIN
  SELECT RAISE(ABORT, 'manual adjustment requires canonical audit event');
END;

DROP TABLE audit_logs;

CREATE TRIGGER audit_log_no_update
BEFORE UPDATE ON audit_log
BEGIN
  SELECT RAISE(ABORT, 'audit_log is append-only');
END;

CREATE TRIGGER audit_log_no_delete
BEFORE DELETE ON audit_log
BEGIN
  SELECT RAISE(ABORT, 'audit_log is append-only');
END;
