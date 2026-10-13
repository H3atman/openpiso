PRAGMA foreign_keys = ON;

-- Single-row wheel master configuration. Disabled by default; the owner opts in.
CREATE TABLE wheel_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
  free_spin_cap INTEGER NOT NULL DEFAULT 1
    CHECK (free_spin_cap >= 0 AND free_spin_cap <= 10),
  version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO wheel_settings (id, enabled, free_spin_cap, version)
  VALUES (1, 0, 1, 1);

-- Ordered wheel segments. Time segments award positive seconds; nothing and
-- free-spin segments award none. Prizes are never points.
CREATE TABLE wheel_segments (
  id TEXT PRIMARY KEY,
  position INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('time', 'nothing', 'free_spin')),
  label TEXT NOT NULL,
  seconds INTEGER NOT NULL DEFAULT 0 CHECK (seconds >= 0),
  weight INTEGER NOT NULL CHECK (weight > 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (kind = 'time' AND seconds > 0)
    OR (kind IN ('nothing', 'free_spin') AND seconds = 0)
  )
);

CREATE INDEX idx_wheel_segments_position ON wheel_segments(position);

-- Default safe wheel: expected payout ~219s vs 1440s point value at the
-- default redeem rate, i.e. a large positive house edge.
INSERT INTO wheel_segments (id, position, kind, label, seconds, weight) VALUES
  ('wheel-seg-time-5', 1, 'time', '5 minutes', 300, 20),
  ('wheel-seg-time-10', 2, 'time', '10 minutes', 600, 10),
  ('wheel-seg-time-15', 3, 'time', '15 minutes', 900, 5),
  ('wheel-seg-nothing', 4, 'nothing', 'Try again', 0, 40),
  ('wheel-seg-free', 5, 'free_spin', 'Free spin', 0, 5);

-- One detail row per spin, linked one-to-one to its canonical audit event
-- (ADR-0018). Persists enough configuration detail to explain historical
-- results.
CREATE TABLE wheel_spins (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  pc_id TEXT NOT NULL REFERENCES pcs(id),
  points_spent INTEGER NOT NULL DEFAULT 1 CHECK (points_spent > 0),
  seconds_won INTEGER NOT NULL DEFAULT 0 CHECK (seconds_won >= 0),
  free_spins_triggered INTEGER NOT NULL DEFAULT 0
    CHECK (free_spins_triggered >= 0),
  config_version INTEGER NOT NULL,
  outcome_json TEXT NOT NULL,
  audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX idx_wheel_spins_audit_event ON wheel_spins(audit_event_id);
CREATE INDEX idx_wheel_spins_account_created
  ON wheel_spins(account_id, created_at);

-- Covers wheel reporting filtered by event type and account, ordered by date.
CREATE INDEX idx_audit_log_wheel_reporting
  ON audit_log(event_type, account_id, created_at);
