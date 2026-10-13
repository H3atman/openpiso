PRAGMA foreign_keys = ON;

-- ADR-0038: points become integer centipoints (1/100 point) everywhere.
-- Every points column is renamed to its `_centi` name and backfilled x100;
-- the earning carry becomes centicents (1/100 cent); accrued carry is paid
-- out into the visible balance at the earn rate in effect right now. The
-- whole migration runs in one transaction, so no customer value is created
-- or destroyed partway.

-- Views referencing renamed columns must be dropped before the renames and
-- rebuilt against the new names afterwards.
DROP VIEW report_points_ledger;
DROP VIEW report_points_liability;

ALTER TABLE accounts RENAME COLUMN points_balance TO points_balance_centi;
ALTER TABLE accounts
  RENAME COLUMN point_earn_carry_cents TO point_earn_carry_centicents;
ALTER TABLE audit_log RENAME COLUMN points_delta TO points_delta_centi;
ALTER TABLE coin_transactions
  RENAME COLUMN points_earned TO points_earned_centi;
ALTER TABLE vouchers RENAME COLUMN points_value TO points_value_centi;
ALTER TABLE wheel_spins RENAME COLUMN points_spent TO points_spent_centi;
ALTER TABLE points_transfer_requests RENAME COLUMN points TO points_centi;

-- x100 backfill: whole points -> centipoints, cents -> centicents.
UPDATE accounts SET
  points_balance_centi = points_balance_centi * 100,
  point_earn_carry_centicents = point_earn_carry_centicents * 100;
-- The append-only guard must yield for this one unit re-denomination —
-- values change scale, never meaning. The trigger is restored verbatim below.
DROP TRIGGER audit_log_no_update;
UPDATE audit_log SET points_delta_centi = points_delta_centi * 100
  WHERE points_delta_centi IS NOT NULL;
CREATE TRIGGER audit_log_no_update
BEFORE UPDATE ON audit_log
BEGIN
  SELECT RAISE(ABORT, 'audit_log is append-only');
END;
UPDATE coin_transactions SET points_earned_centi = points_earned_centi * 100;
UPDATE vouchers SET points_value_centi = points_value_centi * 100;
UPDATE wheel_spins SET points_spent_centi = points_spent_centi * 100;
UPDATE points_transfer_requests SET points_centi = points_centi * 100;

-- One-time carry payout (ADR-0038): accrued sub-threshold progress becomes
-- visible fractional balance at the going earn rate. One centipoint costs
-- `earn_cents_per_point` centicents, so the division below yields
-- centipoints exactly; the sub-centipoint remainder is the new carry.
UPDATE accounts SET
  points_balance_centi = points_balance_centi
    + point_earn_carry_centicents
      / (SELECT earn_cents_per_point FROM economy_settings WHERE id = 1),
  point_earn_carry_centicents = point_earn_carry_centicents
    % (SELECT earn_cents_per_point FROM economy_settings WHERE id = 1);

-- Rebuilt reporting views (ADR-0014 semantics unchanged, centipoint units).
CREATE VIEW report_points_ledger AS
SELECT
  date(created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', created_at, 'localtime') AS business_week,
  strftime('%Y-%m', created_at, 'localtime') AS business_month,
  account_id,
  pc_id,
  event_type,
  points_delta_centi,
  id AS audit_event_id,
  created_at
FROM audit_log
WHERE outcome = 'success'
  AND account_id IS NOT NULL
  AND points_delta_centi IS NOT NULL
  AND points_delta_centi <> 0
  AND event_type IN (
    'coin_accepted',
    'points_redeemed',
    'wheel_spin',
    'promo_voucher_redeemed',
    'sold_voucher_redeemed'
  );

CREATE VIEW report_points_liability AS
SELECT COALESCE(SUM(points_balance_centi), 0) AS total_points_liability_centi
FROM accounts
WHERE status = 'active';
