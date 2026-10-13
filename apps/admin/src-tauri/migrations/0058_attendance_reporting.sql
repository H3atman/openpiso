-- Attendance Claims are delivered time, not paid Cash Revenue. Keep every
-- existing Free Time Dispensed source unchanged and add only successful claims.
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
  AND c.coin_promo_bonus_seconds > 0
UNION ALL
SELECT
  date(a.created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', a.created_at, 'localtime') AS business_week,
  strftime('%Y-%m', a.created_at, 'localtime') AS business_month,
  a.account_id,
  a.pc_id,
  a.event_type,
  r.coin_promo_bonus_seconds AS seconds_delta,
  a.id AS audit_event_id,
  a.created_at
FROM rejection_credits r
JOIN audit_log a ON a.id = r.audit_event_id
WHERE a.outcome = 'success'
  AND r.coin_promo_bonus_seconds > 0
UNION ALL
SELECT
  date(c.created_at, 'localtime') AS business_date,
  strftime('%Y-W%W', c.created_at, 'localtime') AS business_week,
  strftime('%Y-%m', c.created_at, 'localtime') AS business_month,
  a.account_id,
  c.claim_pc_id AS pc_id,
  a.event_type,
  c.claimed_seconds AS seconds_delta,
  a.id AS audit_event_id,
  c.created_at
FROM attendance_claims c
JOIN audit_log a ON a.id = c.audit_event_id
WHERE a.event_type = 'attendance_claim'
  AND a.outcome = 'success'
  AND c.claimed_seconds > 0;
