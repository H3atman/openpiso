PRAGMA foreign_keys = ON;

-- ADR-0039: seed the current guaranteed redeem promise exactly as stored.
-- Deliberately no authoring-rule CHECK constraints: an existing pair is
-- grandfathered even when it violates the new rules. Rust validates all
-- subsequent complete-set writes.
CREATE TABLE redemption_tiers (
  points INTEGER PRIMARY KEY,
  seconds INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO redemption_tiers (points, seconds)
SELECT redeem_points, redeem_seconds
FROM economy_settings
WHERE id = 1;
