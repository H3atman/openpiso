PRAGMA foreign_keys = ON;

-- Migration 0025: Wheel Segment becomes a prize pair (ADR-0046).
--
-- Rebuilds the wheel_segments table — the old kind-based constraint cannot be
-- dropped in place. Existing rows are preserved in meaning:
--   - time     → seconds preserved, points_centi = 0, free_spin = 0
--   - nothing  → seconds = 0, points_centi = 0, free_spin = 0
--   - free_spin → seconds = 0, points_centi = 0, free_spin = 1
--
-- Also adds the Spin Duration setting to wheel_settings (default 5 seconds,
-- 1-120 validated range). Consumed later by the client game animation.
-- Rebuilds wheel_spins with the new points_won_centi column and renamed
-- points_spent -> points_spent_centi, preserving existing history.

-- 1. Rebuild wheel_segments with the new columns, preserving existing rows

-- Temporarily disable FK checking during the table swap
PRAGMA legacy_alter_table = ON;

CREATE TABLE wheel_segments_new (
  id TEXT PRIMARY KEY,
  position INTEGER NOT NULL,
  label TEXT NOT NULL,
  seconds INTEGER NOT NULL DEFAULT 0 CHECK (seconds >= 0),
  points_centi INTEGER NOT NULL DEFAULT 0 CHECK (points_centi >= 0),
  free_spin INTEGER NOT NULL DEFAULT 0 CHECK (free_spin IN (0, 1)),
  weight INTEGER NOT NULL CHECK (weight > 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Preserve existing rows: kind -> columns mapping
INSERT INTO wheel_segments_new (id, position, label, seconds, points_centi, free_spin, weight, created_at)
  SELECT ws.id, ws.position, ws.label,
    CASE WHEN ws.kind = 'time' THEN ws.seconds ELSE 0 END,
    0,
    CASE WHEN ws.kind = 'free_spin' THEN 1 ELSE 0 END,
    ws.weight, ws.created_at
  FROM wheel_segments AS ws;

-- If old table had no rows (fresh install edge case), seed defaults
INSERT INTO wheel_segments_new (id, position, label, seconds, points_centi, free_spin, weight)
  SELECT 'wheel-seg-time-5',  1, '5 minutes',  300, 0, 0, 20
  WHERE (SELECT COUNT(*) FROM wheel_segments_new) = 0;
INSERT INTO wheel_segments_new (id, position, label, seconds, points_centi, free_spin, weight)
  SELECT 'wheel-seg-time-10', 2, '10 minutes', 600, 0, 0, 10
  WHERE (SELECT COUNT(*) FROM wheel_segments_new) <= 1;
INSERT INTO wheel_segments_new (id, position, label, seconds, points_centi, free_spin, weight)
  SELECT 'wheel-seg-time-15', 3, '15 minutes', 900, 0, 0,  5
  WHERE (SELECT COUNT(*) FROM wheel_segments_new) <= 2;
INSERT INTO wheel_segments_new (id, position, label, seconds, points_centi, free_spin, weight)
  SELECT 'wheel-seg-nothing',  4, 'Try again',    0, 0, 0, 40
  WHERE (SELECT COUNT(*) FROM wheel_segments_new) <= 3;
INSERT INTO wheel_segments_new (id, position, label, seconds, points_centi, free_spin, weight)
  SELECT 'wheel-seg-free',     5, 'Free spin',     0, 0, 1,  5
  WHERE (SELECT COUNT(*) FROM wheel_segments_new) <= 4;

DROP TABLE wheel_segments;
ALTER TABLE wheel_segments_new RENAME TO wheel_segments;

CREATE INDEX idx_wheel_segments_position ON wheel_segments(position);

-- 2. Add spin_duration to wheel_settings
ALTER TABLE wheel_settings ADD COLUMN spin_duration INTEGER NOT NULL DEFAULT 5
  CHECK (spin_duration >= 1 AND spin_duration <= 120);

-- 3. Rebuild wheel_spins with points_won_centi column, preserving history
CREATE TABLE wheel_spins_new (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  pc_id TEXT NOT NULL REFERENCES pcs(id),
  points_spent_centi INTEGER NOT NULL DEFAULT 100 CHECK (points_spent_centi > 0),
  seconds_won INTEGER NOT NULL DEFAULT 0 CHECK (seconds_won >= 0),
  points_won_centi INTEGER NOT NULL DEFAULT 0 CHECK (points_won_centi >= 0),
  free_spins_triggered INTEGER NOT NULL DEFAULT 0
    CHECK (free_spins_triggered >= 0),
  config_version INTEGER NOT NULL,
  outcome_json TEXT NOT NULL,
  audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Copy existing history; old spins had no points so points_won_centi = 0
INSERT INTO wheel_spins_new (
  id, account_id, pc_id, points_spent_centi, seconds_won,
  points_won_centi, free_spins_triggered, config_version, outcome_json,
  audit_event_id, created_at
)
  SELECT ws.id, ws.account_id, ws.pc_id, ws.points_spent_centi, ws.seconds_won,
    0, ws.free_spins_triggered, ws.config_version, ws.outcome_json,
    ws.audit_event_id, ws.created_at
  FROM wheel_spins AS ws;

DROP TABLE wheel_spins;
ALTER TABLE wheel_spins_new RENAME TO wheel_spins;

CREATE UNIQUE INDEX idx_wheel_spins_audit_event ON wheel_spins(audit_event_id);
CREATE INDEX idx_wheel_spins_account_created
  ON wheel_spins(account_id, created_at);

PRAGMA legacy_alter_table = OFF;
