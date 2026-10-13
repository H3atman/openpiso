-- Rejection Credit: the owner re-credits a coin the acceptor physically took
-- but never valued (ADR-0069). The detail row hangs off its canonical
-- `rejection_credit` audit event, exactly as `manual_adjustments` does
-- (ADR-0018). No `coin_transactions` row is written: that table means "a coin
-- was accepted and valued by the coin path".
CREATE TABLE rejection_credits (
  id TEXT PRIMARY KEY,
  -- The `coin_batch_rejected` row this credit repairs. Required: there is no
  -- free-standing credit.
  rejection_audit_id TEXT NOT NULL REFERENCES audit_log(id),
  audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  -- Both subjects are frozen at the rejection, never read from live occupancy.
  pc_id TEXT NOT NULL REFERENCES pcs(id),
  account_id TEXT REFERENCES accounts(id),
  target_kind TEXT NOT NULL CHECK (target_kind IN ('account', 'guest')),
  denomination_cents INTEGER NOT NULL CHECK (denomination_cents > 0),
  coins_credited INTEGER NOT NULL CHECK (coins_credited > 0),
  seconds_added INTEGER NOT NULL CHECK (seconds_added >= 0),
  coin_promo_bonus_seconds INTEGER NOT NULL DEFAULT 0
    CHECK (coin_promo_bonus_seconds >= 0),
  points_earned_centi INTEGER NOT NULL DEFAULT 0
    CHECK (points_earned_centi >= 0),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Serves the in-transaction count cap and the creditable-rejection list. There
-- is deliberately no unique index on rejection_audit_id: a suppression summary
-- is creditable in parts, up to its recorded count.
CREATE INDEX idx_rejection_credits_rejection
  ON rejection_credits (rejection_audit_id);

-- One canonical event can never gain a second detail row.
CREATE UNIQUE INDEX idx_rejection_credits_audit_event
  ON rejection_credits (audit_event_id);

-- A Rejection Credit is Cash Revenue: the coin is physically in the coin box
-- (ADR-0014, ADR-0069). `report_cash_revenue` keeps an explicit event-type
-- allowlist, so `cash_flag = 1` alone is not enough — the view is rebuilt with
-- `rejection_credit` added and nothing else changed.
DROP VIEW report_cash_revenue;

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
  AND event_type IN (
    'coin_accepted',
    'voucher_sold',
    'sold_voucher_redeemed',
    'rejection_credit'
  );

-- Report the giveaway portion of a Rejection Credit's Coin Promo bonus
-- alongside the identical split already applied to accepted coins (ADR-0014,
-- ADR-0052, ADR-0069). The credit's full amount stays Cash Revenue via
-- `cash_flag = 1`, and `rejection_credit` stays out of the base select so its
-- paid seconds are never counted as free time.
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
  AND r.coin_promo_bonus_seconds > 0;
