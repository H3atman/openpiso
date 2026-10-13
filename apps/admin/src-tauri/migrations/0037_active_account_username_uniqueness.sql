-- Deleted Accounts remain historical tombstones, but an active Username can be
-- imported again. Rebuild the table because SQLite cannot remove the original
-- table-level UNIQUE constraint from username_normalized in place.
DROP VIEW report_points_liability;

CREATE TABLE accounts_new (
  id TEXT PRIMARY KEY,
  username TEXT NOT NULL,
  username_normalized TEXT NOT NULL,
  password_hash TEXT NOT NULL,
  remaining_seconds INTEGER NOT NULL DEFAULT 0 CHECK (remaining_seconds >= 0),
  points_balance_centi INTEGER NOT NULL DEFAULT 0 CHECK (points_balance_centi >= 0),
  point_earn_carry_centicents INTEGER NOT NULL DEFAULT 0 CHECK (point_earn_carry_centicents >= 0),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'deleted')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  deleted_at TEXT,
  imported_from TEXT,
  imported_at TEXT
);

INSERT INTO accounts_new (
  id, username, username_normalized, password_hash,
  remaining_seconds, points_balance_centi, point_earn_carry_centicents,
  status, created_at, updated_at, deleted_at, imported_from, imported_at
)
SELECT
  id, username, username_normalized, password_hash,
  remaining_seconds, points_balance_centi, point_earn_carry_centicents,
  status, created_at, updated_at, deleted_at, imported_from, imported_at
FROM accounts;

DROP TABLE accounts;
ALTER TABLE accounts_new RENAME TO accounts;

CREATE UNIQUE INDEX idx_accounts_active_username
  ON accounts(username_normalized)
  WHERE status = 'active';

CREATE VIEW report_points_liability AS
SELECT COALESCE(SUM(points_balance_centi), 0) AS total_points_liability_centi
FROM accounts
WHERE status = 'active';
