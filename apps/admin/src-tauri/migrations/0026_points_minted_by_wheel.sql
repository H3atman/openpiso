PRAGMA foreign_keys = ON;

-- Migration 0026: Points Minted by Wheel reporting view (ADR-0046).
--
-- Gross points won through the wheel, for the Points Minted by Wheel
-- report line. Only spins that won points are counted (points_won_centi > 0).
-- This is gross, not net of spin costs — a spin where the customer won
-- nothing is excluded.

CREATE VIEW report_points_minted_by_wheel AS
SELECT
  date(created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', created_at, 'localtime') AS business_week,
  strftime('%Y-%m', created_at, 'localtime') AS business_month,
  account_id,
  pc_id,
  points_won_centi,
  id AS spin_id,
  created_at
FROM wheel_spins
WHERE points_won_centi > 0;
