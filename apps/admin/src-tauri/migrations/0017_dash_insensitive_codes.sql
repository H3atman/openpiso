PRAGMA foreign_keys = ON;

-- ADR-0040 / Issue #87: Dash-insensitive voucher code lookup and
-- batch-sold cash-at-redemption in report_cash_revenue.

-- Store the canonical uppercase, dash-free lookup key. Existing codes were
-- server-generated uppercase values; UPPER also keeps the backfill robust.
ALTER TABLE vouchers ADD COLUMN code_normalized TEXT;

UPDATE vouchers
SET code_normalized = UPPER(REPLACE(TRIM(code), '-', ''));

CREATE UNIQUE INDEX idx_vouchers_code_normalized
  ON vouchers(code_normalized);

-- SQLite cannot add a NOT NULL/CHECK constraint with ALTER COLUMN. These
-- triggers enforce the same invariant for every future insert/update path.
CREATE TRIGGER vouchers_code_normalized_insert_check
BEFORE INSERT ON vouchers
WHEN NEW.code_normalized IS NULL
  OR NEW.code_normalized != UPPER(REPLACE(TRIM(NEW.code), '-', ''))
BEGIN
  SELECT RAISE(ABORT, 'invalid voucher code_normalized');
END;

CREATE TRIGGER vouchers_code_normalized_update_check
BEFORE UPDATE OF code, code_normalized ON vouchers
WHEN NEW.code_normalized IS NULL
  OR NEW.code_normalized != UPPER(REPLACE(TRIM(NEW.code), '-', ''))
BEGIN
  SELECT RAISE(ABORT, 'invalid voucher code_normalized');
END;

-- Batch-sold vouchers recognise cash on their redemption event. Single-mint
-- sold vouchers still book cash via voucher_sold at mint and carry cash_flag =
-- false at redemption, preventing double-counting.
DROP VIEW IF EXISTS report_cash_revenue;

CREATE VIEW report_cash_revenue AS
SELECT
  date(created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', created_at, 'localtime') AS business_week,
  strftime('%Y-%m', created_at, 'localtime') AS business_month,
  account_id,
  pc_id,
  event_type,
  amount_cents,
  seconds_delta,
  id AS audit_event_id,
  created_at
FROM audit_log
WHERE outcome = 'success'
  AND cash_flag = 1
  AND amount_cents > 0
  AND event_type IN ('coin_accepted', 'voucher_sold', 'sold_voucher_redeemed');
