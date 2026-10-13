-- Conservative quiet-gap default for typical pulse coin acceptors.
-- Operators can adjust inter_coin_gap_ms after characterizing their hardware.
-- This migration sets the existing singleton row to the new default.
UPDATE coin_device_settings SET inter_coin_gap_ms = 600 WHERE id = 1;
