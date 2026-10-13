-- M6 Reporting and Audit Review (ADR-0014, ADR-0015).
-- Adds reporting views over canonical audit_log, covering indexes for audit
-- review dimensions, and a manual-adjustments reporting index. No new tables;
-- manual adjustments remain PC-bound per ADR-0021.

-- Cash Revenue: cash that entered the shop (coins + sold vouchers).
-- Per ADR-0014: never blended with free time given away. Sold voucher
-- redemption is excluded because the cash was already recognized at mint.
-- `seconds_delta` is exposed so per-PC and per-account "seconds sold" totals
-- can be derived from the same view (coins carry seconds sold; sold
-- vouchers carry the prepaid seconds value but no pc_id, so PC-scoped sums
-- stay coins-only).
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
  AND event_type IN ('coin_accepted', 'voucher_sold');

-- Free Time Dispensed: time given away without cash (ADR-0014).
-- Promo voucher redemptions, guaranteed points redemptions, wheel winnings,
-- and owner manual add-time. Sold voucher redemption is intentionally
-- excluded: it credits prepaid time already paid for at mint.
CREATE VIEW report_free_time_dispensed AS
SELECT
  date(created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', created_at, 'localtime') AS business_week,
  strftime('%Y-%m', created_at, 'localtime') AS business_month,
  account_id,
  pc_id,
  event_type,
  seconds_delta,
  id AS audit_event_id,
  created_at
FROM audit_log
WHERE outcome = 'success'
  AND seconds_delta > 0
  AND event_type IN (
    'promo_voucher_redeemed',
    'points_redeemed',
    'wheel_spin',
    'manual_adjustment'
  );

-- Points ledger: earn, redeem, wheel-bet, and voucher points movements
-- (ADR-0014). One row per canonical event that changed an account's points.
CREATE VIEW report_points_ledger AS
SELECT
  date(created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', created_at, 'localtime') AS business_week,
  strftime('%Y-%m', created_at, 'localtime') AS business_month,
  account_id,
  pc_id,
  event_type,
  points_delta,
  id AS audit_event_id,
  created_at
FROM audit_log
WHERE outcome = 'success'
  AND account_id IS NOT NULL
  AND points_delta IS NOT NULL
  AND points_delta <> 0
  AND event_type IN (
    'coin_accepted',
    'points_redeemed',
    'wheel_spin',
    'promo_voucher_redeemed',
    'sold_voucher_redeemed'
  );

-- Points Liability: total outstanding unredeemed points across active
-- accounts (ADR-0014). Current snapshot, not a historical event sum.
CREATE VIEW report_points_liability AS
SELECT COALESCE(SUM(points_balance), 0) AS total_points_liability
FROM accounts
WHERE status = 'active';

-- Cover audit viewer filtering by outcome and date range (success/failure/
-- denied), which the existing event/actor/account/pc indexes do not cover.
CREATE INDEX idx_audit_log_outcome_created
  ON audit_log(outcome, created_at);

-- Cover audit viewer and report queries that filter by event type and read
-- reverse-chronologically. The existing idx_audit_log_event_type is a single
-- column; this one adds created_at so date-range scans stay index-only.
CREATE INDEX idx_audit_log_event_type_created
  ON audit_log(event_type, created_at);

-- Cover manual-adjustment reporting by PC and date (ADR-0021). The legacy
-- idx_manual_adjustments_pc_id is a single-column index from the MVP.
CREATE INDEX idx_manual_adjustments_pc_created_at
  ON manual_adjustments(pc_id, created_at);
