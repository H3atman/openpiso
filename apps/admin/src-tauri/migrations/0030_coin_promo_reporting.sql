-- Report the giveaway portion of account Coin Promo coins separately from their
-- unchanged full Cash Revenue (ADR-0014, ADR-0052).
DROP VIEW report_free_time_dispensed;

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
  )
UNION ALL
SELECT
  date(a.created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', a.created_at, 'localtime') AS business_week,
  strftime('%Y-%m', a.created_at, 'localtime') AS business_month,
  a.account_id,
  a.pc_id,
  a.event_type,
  c.coin_promo_bonus_seconds AS seconds_delta,
  a.id AS audit_event_id,
  a.created_at
FROM coin_transactions c
JOIN audit_log a ON a.id = c.audit_event_id
WHERE a.outcome = 'success'
  AND c.coin_promo_bonus_seconds > 0;
