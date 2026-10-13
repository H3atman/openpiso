CREATE TABLE vouchers (
  id TEXT PRIMARY KEY,
  code TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('sold', 'promo')),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'redeemed')),
  seconds_value INTEGER NOT NULL CHECK (seconds_value >= 0),
  points_value INTEGER NOT NULL CHECK (points_value >= 0),
  sold_amount_cents INTEGER NOT NULL DEFAULT 0 CHECK (sold_amount_cents >= 0),
  created_by TEXT NOT NULL DEFAULT 'admin',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  sold_at TEXT,
  redeemed_by_account_id TEXT REFERENCES accounts(id),
  redeemed_at TEXT,
  expires_at TEXT,
  audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  redeemed_audit_event_id TEXT REFERENCES audit_log(id),
  CHECK (seconds_value > 0 OR points_value > 0),
  CHECK (
    (kind = 'sold' AND sold_amount_cents > 0 AND sold_at IS NOT NULL)
    OR (kind = 'promo' AND sold_amount_cents = 0 AND sold_at IS NULL)
  ),
  CHECK (
    (status = 'active' AND redeemed_by_account_id IS NULL AND redeemed_at IS NULL AND redeemed_audit_event_id IS NULL)
    OR (status = 'redeemed' AND redeemed_by_account_id IS NOT NULL AND redeemed_at IS NOT NULL AND redeemed_audit_event_id IS NOT NULL)
  )
);

CREATE INDEX idx_vouchers_status_created_at
  ON vouchers(status, created_at);

CREATE INDEX idx_vouchers_redeemed_by_account
  ON vouchers(redeemed_by_account_id, redeemed_at);

CREATE INDEX idx_vouchers_kind_created_at
  ON vouchers(kind, created_at);

CREATE UNIQUE INDEX idx_vouchers_audit_event_id
  ON vouchers(audit_event_id);

CREATE UNIQUE INDEX idx_vouchers_redeemed_audit_event_id
  ON vouchers(redeemed_audit_event_id)
  WHERE redeemed_audit_event_id IS NOT NULL;

CREATE INDEX idx_audit_log_voucher_reporting
  ON audit_log(event_type, cash_flag, account_id, created_at);
