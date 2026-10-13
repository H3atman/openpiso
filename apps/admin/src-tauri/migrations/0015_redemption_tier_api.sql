PRAGMA foreign_keys = ON;

-- Customer redemption needs an opaque tier id plus a menu version so a tier
-- edit cannot silently change an exchange after the customer saw it.
ALTER TABLE redemption_tiers RENAME TO redemption_tiers_legacy;

CREATE TABLE redemption_tiers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  points INTEGER NOT NULL UNIQUE,
  seconds INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO redemption_tiers (points, seconds, created_at)
SELECT points, seconds, created_at FROM redemption_tiers_legacy;

DROP TABLE redemption_tiers_legacy;

CREATE TABLE redemption_tier_menu (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL CHECK (version > 0)
);

INSERT INTO redemption_tier_menu (id, version) VALUES (1, 1);
