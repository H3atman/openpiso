CREATE TABLE coin_promo_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1))
);

INSERT INTO coin_promo_settings (id, enabled) VALUES (1, 0);

CREATE TABLE coin_promo_tiers (
  amount_cents INTEGER PRIMARY KEY CHECK (amount_cents > 0),
  bonus_seconds INTEGER NOT NULL CHECK (bonus_seconds > 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE coin_transactions
  ADD COLUMN coin_promo_bonus_seconds INTEGER NOT NULL DEFAULT 0
    CHECK (coin_promo_bonus_seconds >= 0);
