-- Seed disabled Attendance and Shop Hours; baseline a still-live Shop Override at the same UTC instant.
CREATE TABLE attendance_settings_versions (
  version INTEGER PRIMARY KEY CHECK (version > 0),
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  daily_threshold_cents INTEGER NOT NULL CHECK (daily_threshold_cents > 0),
  reward_seconds INTEGER NOT NULL CHECK (reward_seconds > 0),
  claim_window_months INTEGER NOT NULL CHECK (claim_window_months BETWEEN 1 AND 120),
  effective_at TEXT NOT NULL CHECK (julianday(effective_at) IS NOT NULL AND effective_at LIKE '%Z'),
  audit_event_id TEXT UNIQUE REFERENCES audit_log(id)
);
INSERT INTO attendance_settings_versions VALUES (1, 0, 1000, 3600, 1, strftime('%Y-%m-%dT%H:%M:%fZ','now'), NULL);
CREATE INDEX idx_attendance_settings_effective ON attendance_settings_versions(effective_at, version);

CREATE TABLE shop_hours_versions (
  id INTEGER PRIMARY KEY,
  enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
  last_call_lead_minutes INTEGER NOT NULL CHECK (last_call_lead_minutes >= 0),
  effective_at TEXT NOT NULL CHECK (julianday(effective_at) IS NOT NULL AND effective_at LIKE '%Z'),
  audit_event_id TEXT UNIQUE REFERENCES audit_log(id)
);
INSERT INTO shop_hours_versions(id, enabled, last_call_lead_minutes, effective_at)
SELECT 1, enabled, last_call_lead_minutes, (SELECT effective_at FROM attendance_settings_versions WHERE version = 1)
FROM shop_hours_settings WHERE id = 1;
CREATE INDEX idx_shop_hours_versions_effective ON shop_hours_versions(effective_at, id);
CREATE TABLE shop_hours_version_days (
  shop_hours_version_id INTEGER NOT NULL REFERENCES shop_hours_versions(id),
  weekday INTEGER NOT NULL CHECK (weekday BETWEEN 0 AND 6),
  mode TEXT NOT NULL CHECK (mode IN ('open_period', 'closed_all_day', 'open_24_hours')),
  open_minute INTEGER, close_minute INTEGER,
  PRIMARY KEY(shop_hours_version_id, weekday),
  CHECK ((mode = 'open_period' AND open_minute BETWEEN 0 AND 1439 AND close_minute BETWEEN 0 AND 1439)
    OR (mode != 'open_period' AND open_minute IS NULL AND close_minute IS NULL))
);
INSERT INTO shop_hours_version_days SELECT 1, weekday, mode, open_minute, close_minute FROM shop_hours_schedule;
CREATE TABLE shop_override_history (
  id INTEGER PRIMARY KEY,
  mode TEXT CHECK (mode IN ('force_open', 'force_closed')),
  effective_at TEXT NOT NULL CHECK (julianday(effective_at) IS NOT NULL AND effective_at LIKE '%Z'),
  expires_at TEXT CHECK (expires_at IS NULL OR (julianday(expires_at) IS NOT NULL AND expires_at LIKE '%Z')),
  audit_event_id TEXT UNIQUE REFERENCES audit_log(id),
  CHECK ((mode IS NULL AND expires_at IS NULL) OR (mode IS NOT NULL AND julianday(expires_at) > julianday(effective_at)))
);
INSERT INTO shop_override_history(id, mode, effective_at, expires_at)
SELECT 1, mode, (SELECT effective_at FROM attendance_settings_versions WHERE version = 1),
  strftime('%Y-%m-%dT%H:%M:%fZ', expires_at)
FROM shop_override WHERE id = 1 AND julianday(expires_at) > julianday((SELECT effective_at FROM attendance_settings_versions WHERE version = 1));

CREATE TABLE attendance_clock_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  utc_floor TEXT NOT NULL CHECK (julianday(utc_floor) IS NOT NULL AND utc_floor LIKE '%Z'),
  open_local_date TEXT NOT NULL CHECK (open_local_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]')
);
INSERT INTO attendance_clock_state VALUES (1, (SELECT effective_at FROM attendance_settings_versions WHERE version = 1), date('now','localtime'));
CREATE TABLE attendance_calendar_dates (
  earning_date TEXT PRIMARY KEY CHECK (earning_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  finalized_at TEXT NOT NULL CHECK (julianday(finalized_at) IS NOT NULL AND finalized_at LIKE '%Z'),
  restart_observed_utc_offset INTEGER NOT NULL
);
CREATE TABLE attendance_eligible_intervals (
  id INTEGER PRIMARY KEY,
  earning_date TEXT NOT NULL REFERENCES attendance_calendar_dates(earning_date),
  starts_at TEXT NOT NULL CHECK (julianday(starts_at) IS NOT NULL AND starts_at LIKE '%Z'),
  ends_at TEXT NOT NULL CHECK (julianday(ends_at) IS NOT NULL AND ends_at LIKE '%Z'),
  CHECK (julianday(ends_at) > julianday(starts_at))
);
CREATE INDEX idx_attendance_intervals_date_start ON attendance_eligible_intervals(earning_date, starts_at);

-- Frozen configuration and eligibility evidence cannot be rewritten.
CREATE TRIGGER attendance_settings_versions_no_update BEFORE UPDATE ON attendance_settings_versions BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_settings_versions_no_delete BEFORE DELETE ON attendance_settings_versions BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER shop_hours_versions_no_update BEFORE UPDATE ON shop_hours_versions BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER shop_hours_versions_no_delete BEFORE DELETE ON shop_hours_versions BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER shop_hours_version_days_no_update BEFORE UPDATE ON shop_hours_version_days BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER shop_hours_version_days_no_delete BEFORE DELETE ON shop_hours_version_days BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER shop_override_history_no_update BEFORE UPDATE ON shop_override_history BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER shop_override_history_no_delete BEFORE DELETE ON shop_override_history BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_calendar_dates_no_update BEFORE UPDATE ON attendance_calendar_dates BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_calendar_dates_no_delete BEFORE DELETE ON attendance_calendar_dates BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_eligible_intervals_no_update BEFORE UPDATE ON attendance_eligible_intervals BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
CREATE TRIGGER attendance_eligible_intervals_no_delete BEFORE DELETE ON attendance_eligible_intervals BEGIN SELECT RAISE(ABORT, 'immutable attendance evidence'); END;
