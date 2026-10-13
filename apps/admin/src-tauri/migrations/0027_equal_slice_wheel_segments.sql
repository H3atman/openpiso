PRAGMA foreign_keys = ON;

-- Migration 0027: Wheel Segments lose their probability weight (ADR-0047).
--
-- Every segment is now exactly 1/n likely and 1/n of the circle, so the drawn
-- wheel states its own odds. Rarity is expressed by repeating a prize across
-- several segments instead of by an invisible weight.
--
-- Two upgrade cases:
--   * A wheel whose segment ids still match the 0025 seed exactly was never
--     edited by its owner and carries no configuration worth preserving — it is
--     replaced with the new 12-slice equal-size default.
--   * Any edited wheel is flattened in place. Its odds change, and it may lose
--     its house edge; the Wheel tab warns on load and the spin path already
--     refuses a negative-edge wheel, so the owner is told rather than silently
--     printing time.

PRAGMA legacy_alter_table = ON;

-- 1. Rebuild wheel_segments without the weight column

CREATE TABLE wheel_segments_new (
  id TEXT PRIMARY KEY,
  position INTEGER NOT NULL,
  label TEXT NOT NULL,
  seconds INTEGER NOT NULL DEFAULT 0 CHECK (seconds >= 0),
  points_centi INTEGER NOT NULL DEFAULT 0 CHECK (points_centi >= 0),
  free_spin INTEGER NOT NULL DEFAULT 0 CHECK (free_spin IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Flatten every existing segment to an equal slice, order preserved.
INSERT INTO wheel_segments_new (id, position, label, seconds, points_centi, free_spin, created_at)
  SELECT ws.id, ws.position, ws.label, ws.seconds, ws.points_centi, ws.free_spin, ws.created_at
  FROM wheel_segments AS ws;

DROP TABLE wheel_segments;
ALTER TABLE wheel_segments_new RENAME TO wheel_segments;

CREATE INDEX idx_wheel_segments_position ON wheel_segments(position);

-- 2. Replace the wheel only if it is still exactly the 0025 seed.
--
-- The owner editor deletes every row and re-inserts with fresh UUIDs, so an
-- intact set of the five seeded ids proves the wheel was never saved.
--
-- Insert first, delete second, each guarded by a condition that reads the table
-- as it stands at that moment: no temp table, and no dependence on statement
-- ordering surviving the migration runner.

-- The new 12-slice default. Duplicates are interleaved rather than grouped: a
-- contiguous block of five "Try again" slices would read as one huge losing
-- wedge, which is the rigged look this ADR removes (ADR-0047). Positions are
-- offset past the five rows still present; they are renumbered below.
INSERT INTO wheel_segments (id, position, label, seconds, points_centi, free_spin)
  SELECT * FROM (
    SELECT 'wheel-seg-eq-01'  AS id, 101 AS position, 'Try again'  AS label,   0 AS seconds, 0 AS points_centi, 0 AS free_spin
    UNION ALL SELECT 'wheel-seg-eq-02', 102, '5 minutes',  300, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-03', 103, 'Try again',    0, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-04', 104, '10 minutes', 600, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-05', 105, 'Try again',    0, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-06', 106, '5 minutes',  300, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-07', 107, 'Try again',    0, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-08', 108, '15 minutes', 900, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-09', 109, 'Try again',    0, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-10', 110, '5 minutes',  300, 0, 0
    UNION ALL SELECT 'wheel-seg-eq-11', 111, 'Free spin',    0, 0, 1
    UNION ALL SELECT 'wheel-seg-eq-12', 112, '10 minutes', 600, 0, 0
  )
  WHERE (SELECT COUNT(*) FROM wheel_segments) = 5
    AND (SELECT COUNT(*) FROM wheel_segments
         WHERE id IN ('wheel-seg-time-5', 'wheel-seg-time-10', 'wheel-seg-time-15',
                      'wheel-seg-nothing', 'wheel-seg-free')) = 5;

-- Retire the old seed, but only once its replacement is actually in place.
DELETE FROM wheel_segments
  WHERE id IN ('wheel-seg-time-5', 'wheel-seg-time-10', 'wheel-seg-time-15',
               'wheel-seg-nothing', 'wheel-seg-free')
    AND EXISTS (SELECT 1 FROM wheel_segments WHERE id = 'wheel-seg-eq-01');

-- Close the position gap left by the offset above.
UPDATE wheel_segments
  SET position = position - 100
  WHERE position > 100;

PRAGMA legacy_alter_table = OFF;
