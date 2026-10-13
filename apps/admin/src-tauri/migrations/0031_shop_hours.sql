CREATE TABLE shop_hours_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
  last_call_lead_minutes INTEGER NOT NULL DEFAULT 15
    CHECK (last_call_lead_minutes >= 0)
);

INSERT INTO shop_hours_settings (id, enabled, last_call_lead_minutes)
VALUES (1, 0, 15);

CREATE TABLE shop_hours_schedule (
  weekday INTEGER PRIMARY KEY CHECK (weekday BETWEEN 0 AND 6),
  mode TEXT NOT NULL CHECK (mode IN ('open_period', 'closed_all_day', 'open_24_hours')),
  open_minute INTEGER,
  close_minute INTEGER,
  CHECK (
    (mode = 'open_period'
      AND open_minute BETWEEN 0 AND 1439
      AND close_minute BETWEEN 0 AND 1439)
    OR (mode IN ('closed_all_day', 'open_24_hours')
      AND open_minute IS NULL
      AND close_minute IS NULL)
  )
);

INSERT INTO shop_hours_schedule (weekday, mode, open_minute, close_minute)
VALUES
  (0, 'open_period', 480, 1320),
  (1, 'open_period', 480, 1320),
  (2, 'open_period', 480, 1320),
  (3, 'open_period', 480, 1320),
  (4, 'open_period', 480, 1320),
  (5, 'open_period', 480, 1320),
  (6, 'open_period', 480, 1320);

CREATE TABLE shop_override (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  mode TEXT NOT NULL CHECK (mode IN ('force_open', 'force_closed')),
  expires_at TEXT NOT NULL
);
