-- S2 earning: immutable source facts, the capped daily projection, Check-Ins,
-- Earning Months, and the nullable Attendance audit delta. Only daily progress
-- is mutable; Contributions and Check-Ins are append-only.
ALTER TABLE audit_log ADD COLUMN attendance_seconds_delta INTEGER
  CHECK (attendance_seconds_delta IS NULL OR attendance_seconds_delta != 0);

CREATE TABLE attendance_contributions (
  admission_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  source_audit_event_id TEXT NOT NULL UNIQUE REFERENCES audit_log(id),
  source_kind TEXT NOT NULL CHECK (source_kind IN ('coin_accepted', 'rejection_credit')),
  configuration_version INTEGER NOT NULL REFERENCES attendance_settings_versions(version),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  earning_date TEXT NOT NULL CHECK (earning_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  cents INTEGER NOT NULL CHECK (cents > 0),
  source_at TEXT NOT NULL CHECK (julianday(source_at) IS NOT NULL AND source_at LIKE '%Z'),
  original_rejection_audit_event_id TEXT REFERENCES audit_log(id),
  CHECK ((source_kind = 'rejection_credit') = (original_rejection_audit_event_id IS NOT NULL))
);
CREATE INDEX idx_attendance_contributions_account_date
  ON attendance_contributions(account_id, earning_date);

CREATE TABLE attendance_daily_progress (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  earning_date TEXT NOT NULL CHECK (earning_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  configuration_version INTEGER NOT NULL REFERENCES attendance_settings_versions(version),
  daily_threshold_cents INTEGER NOT NULL CHECK (daily_threshold_cents > 0),
  reward_seconds INTEGER NOT NULL CHECK (reward_seconds > 0),
  first_source_audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  capped_cents INTEGER NOT NULL CHECK (capped_cents BETWEEN 1 AND daily_threshold_cents),
  PRIMARY KEY (account_id, earning_date)
);

CREATE TABLE attendance_earning_months (
  id INTEGER PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  earning_month TEXT NOT NULL CHECK (earning_month GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]'),
  claim_window_configuration_version INTEGER NOT NULL REFERENCES attendance_settings_versions(version),
  claim_window_months INTEGER NOT NULL CHECK (claim_window_months BETWEEN 1 AND 120),
  claim_starts_at TEXT NOT NULL CHECK (julianday(claim_starts_at) IS NOT NULL AND claim_starts_at LIKE '%Z'),
  deadline_at_exclusive TEXT NOT NULL CHECK (julianday(deadline_at_exclusive) IS NOT NULL AND deadline_at_exclusive LIKE '%Z'
    AND julianday(deadline_at_exclusive) > julianday(claim_starts_at)),
  claim_local_dates TEXT NOT NULL CHECK (length(claim_local_dates) > 0),
  UNIQUE (account_id, earning_month)
);
CREATE INDEX idx_attendance_earning_months_deadline
  ON attendance_earning_months(deadline_at_exclusive);

CREATE TABLE attendance_check_ins (
  audit_event_id TEXT NOT NULL UNIQUE REFERENCES audit_log(id),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  earning_date TEXT NOT NULL CHECK (earning_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  earning_month_id INTEGER NOT NULL REFERENCES attendance_earning_months(id),
  configuration_version INTEGER NOT NULL REFERENCES attendance_settings_versions(version),
  daily_threshold_cents INTEGER NOT NULL CHECK (daily_threshold_cents > 0),
  reward_seconds INTEGER NOT NULL CHECK (reward_seconds > 0),
  threshold_crossing_source_audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  trigger_pc_id TEXT REFERENCES pcs(id),
  original_earning_at TEXT NOT NULL CHECK (julianday(original_earning_at) IS NOT NULL AND original_earning_at LIKE '%Z'),
  UNIQUE (account_id, earning_date)
);
CREATE INDEX idx_attendance_check_ins_date ON attendance_check_ins(earning_date);
CREATE INDEX idx_attendance_check_ins_account_month
  ON attendance_check_ins(account_id, earning_month_id);

-- Linked events must carry the successful source types; a Rejection Credit
-- links its original coin_batch_rejected evidence.
CREATE TRIGGER attendance_contributions_type_guard BEFORE INSERT ON attendance_contributions
BEGIN
  SELECT RAISE(ABORT, 'attendance contribution source event mismatch')
  WHERE NOT EXISTS (
    SELECT 1 FROM audit_log
    WHERE id = NEW.source_audit_event_id
      AND event_type = NEW.source_kind
      AND outcome = 'success'
      AND account_id = NEW.account_id
      AND amount_cents = NEW.cents
  );
  SELECT RAISE(ABORT, 'attendance rejection source event mismatch')
  WHERE NEW.original_rejection_audit_event_id IS NOT NULL
    AND NOT EXISTS (
      SELECT 1 FROM audit_log
      WHERE id = NEW.original_rejection_audit_event_id
        AND event_type = 'coin_batch_rejected'
        AND outcome = 'failure'
        AND account_id = NEW.account_id
    );
END;

CREATE TRIGGER attendance_check_ins_type_guard BEFORE INSERT ON attendance_check_ins
BEGIN
  SELECT RAISE(ABORT, 'attendance check-in event mismatch')
  WHERE NOT EXISTS (
    SELECT 1 FROM audit_log
    WHERE id = NEW.audit_event_id
      AND event_type = 'attendance_check_in'
      AND outcome = 'success'
      AND account_id = NEW.account_id
      AND seconds_delta IS NULL
      AND attendance_seconds_delta = NEW.reward_seconds
  );
  SELECT RAISE(ABORT, 'attendance check-in source mismatch')
  WHERE NOT EXISTS (
    SELECT 1 FROM attendance_contributions c
    JOIN audit_log a ON a.id = c.source_audit_event_id
    WHERE c.source_audit_event_id = NEW.threshold_crossing_source_audit_event_id
      AND c.account_id = NEW.account_id
      AND c.earning_date = NEW.earning_date
      AND a.outcome = 'success'
  );
END;

-- Immutable source facts and Check-Ins cannot be rewritten.
CREATE TRIGGER attendance_contributions_no_update BEFORE UPDATE ON attendance_contributions
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_contributions_no_delete BEFORE DELETE ON attendance_contributions
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_check_ins_no_update BEFORE UPDATE ON attendance_check_ins
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_check_ins_no_delete BEFORE DELETE ON attendance_check_ins
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_earning_months_no_update BEFORE UPDATE ON attendance_earning_months
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_earning_months_no_delete BEFORE DELETE ON attendance_earning_months
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
