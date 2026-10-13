-- Migration 0038 records the fresh-install ₱20 candidate mapping.
-- The migration runner applies the row only when migration history was empty;
-- upgrades intentionally leave every existing pulse-map row untouched.
SELECT 1;
