PRAGMA foreign_keys = ON;

ALTER TABLE accounts
  ADD COLUMN point_earn_carry_cents INTEGER NOT NULL DEFAULT 0
    CHECK (point_earn_carry_cents >= 0);

CREATE TABLE economy_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  earn_cents_per_point INTEGER NOT NULL CHECK (earn_cents_per_point > 0),
  redeem_points INTEGER NOT NULL CHECK (redeem_points > 0),
  redeem_seconds INTEGER NOT NULL CHECK (redeem_seconds > 0),
  unsafe_bonus_threshold_basis_points INTEGER NOT NULL DEFAULT 2000
    CHECK (unsafe_bonus_threshold_basis_points > 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO economy_settings (
  id, earn_cents_per_point, redeem_points, redeem_seconds,
  unsafe_bonus_threshold_basis_points
) VALUES (1, 5000, 10, 14400, 2000);

ALTER TABLE coin_transactions
  ADD COLUMN account_id TEXT REFERENCES accounts(id);

ALTER TABLE coin_transactions
  ADD COLUMN points_earned INTEGER NOT NULL DEFAULT 0
    CHECK (points_earned >= 0);

ALTER TABLE coin_transactions
  ADD COLUMN target_kind TEXT NOT NULL DEFAULT 'guest'
    CHECK (target_kind IN ('guest', 'account'));

CREATE INDEX idx_coin_transactions_account_created_at
  ON coin_transactions(account_id, created_at);

CREATE INDEX idx_coin_transactions_pc_created_at
  ON coin_transactions(pc_id, created_at);

CREATE INDEX idx_audit_log_coin_reporting
  ON audit_log(event_type, account_id, pc_id, created_at);
