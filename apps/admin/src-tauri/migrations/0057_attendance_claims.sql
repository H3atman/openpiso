-- S3 claims and deletion forfeitures are immutable Attendance ledger facts.
CREATE TABLE attendance_claims (
  audit_event_id TEXT NOT NULL UNIQUE REFERENCES audit_log(id),
  request_id TEXT NOT NULL UNIQUE
    CHECK (length(request_id) = 36
      AND request_id GLOB '????????-????-????-????-????????????'
      AND lower(request_id) = request_id
      AND request_id NOT GLOB '*[^0-9a-f-]*'),
  account_id TEXT NOT NULL REFERENCES accounts(id),
  earning_month_id INTEGER NOT NULL REFERENCES attendance_earning_months(id),
  claim_pc_id TEXT REFERENCES pcs(id),
  claimed_seconds INTEGER NOT NULL CHECK (claimed_seconds > 0),
  created_at TEXT NOT NULL CHECK (julianday(created_at) IS NOT NULL AND created_at LIKE '%Z')
);
CREATE INDEX idx_attendance_claims_month_created
  ON attendance_claims(earning_month_id, created_at);
CREATE INDEX idx_attendance_claims_account_created
  ON attendance_claims(account_id, created_at);

CREATE TABLE attendance_forfeitures (
  account_id TEXT NOT NULL REFERENCES accounts(id),
  earning_month_id INTEGER NOT NULL REFERENCES attendance_earning_months(id),
  deletion_audit_event_id TEXT NOT NULL REFERENCES audit_log(id),
  forfeited_seconds INTEGER NOT NULL CHECK (forfeited_seconds > 0),
  deleted_at TEXT NOT NULL CHECK (julianday(deleted_at) IS NOT NULL AND deleted_at LIKE '%Z'),
  UNIQUE (deletion_audit_event_id, earning_month_id)
);
CREATE INDEX idx_attendance_forfeitures_account_month_deleted
  ON attendance_forfeitures(account_id, earning_month_id, deleted_at);

CREATE TRIGGER attendance_claims_type_guard BEFORE INSERT ON attendance_claims
BEGIN
  SELECT RAISE(ABORT, 'attendance claim event mismatch')
  WHERE NOT EXISTS (
    SELECT 1 FROM audit_log
    WHERE id = NEW.audit_event_id
      AND event_type = 'attendance_claim'
      AND outcome = 'success'
      AND account_id = NEW.account_id
      AND pc_id IS NEW.claim_pc_id
      AND seconds_delta = NEW.claimed_seconds
      AND attendance_seconds_delta = -NEW.claimed_seconds
  );
  SELECT RAISE(ABORT, 'attendance claim month mismatch')
  WHERE NOT EXISTS (
    SELECT 1 FROM attendance_earning_months
    WHERE id = NEW.earning_month_id AND account_id = NEW.account_id
  );
END;

CREATE TRIGGER attendance_forfeitures_type_guard BEFORE INSERT ON attendance_forfeitures
BEGIN
  SELECT RAISE(ABORT, 'attendance forfeiture event mismatch')
  WHERE NOT EXISTS (
    SELECT 1 FROM audit_log
    WHERE id = NEW.deletion_audit_event_id
      AND event_type = 'account_deleted'
      AND outcome = 'success'
      AND account_id = NEW.account_id
      AND attendance_seconds_delta < 0
  );
  SELECT RAISE(ABORT, 'attendance forfeiture month mismatch')
  WHERE NOT EXISTS (
    SELECT 1 FROM attendance_earning_months
    WHERE id = NEW.earning_month_id AND account_id = NEW.account_id
  );
END;

CREATE TRIGGER attendance_claims_no_update BEFORE UPDATE ON attendance_claims
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_claims_no_delete BEFORE DELETE ON attendance_claims
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_forfeitures_no_update BEFORE UPDATE ON attendance_forfeitures
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_forfeitures_no_delete BEFORE DELETE ON attendance_forfeitures
BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
