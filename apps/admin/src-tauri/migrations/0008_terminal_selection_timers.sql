-- Follow-up to M4: persist the ADR-0023 owner-editable Selection Window timers
-- without modifying the already-released 0007 migration.
ALTER TABLE coin_device_settings
  ADD COLUMN inactivity_timeout_seconds INTEGER NOT NULL DEFAULT 15
    CHECK (inactivity_timeout_seconds > 0);

ALTER TABLE coin_device_settings
  ADD COLUMN hard_cap_seconds INTEGER NOT NULL DEFAULT 180
    CHECK (hard_cap_seconds > 0);

ALTER TABLE pcs
  ADD COLUMN visible INTEGER NOT NULL DEFAULT 1 CHECK (visible IN (0, 1));
