CREATE TABLE backup_schedule_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  frequency TEXT NOT NULL DEFAULT 'daily'
    CHECK (frequency IN ('daily', 'weekly', 'monthly', 'interval')),
  minute_of_day INTEGER NOT NULL DEFAULT 240
    CHECK (minute_of_day BETWEEN 0 AND 1439),
  weekday INTEGER NOT NULL DEFAULT 0 CHECK (weekday BETWEEN 0 AND 6),
  day_of_month INTEGER NOT NULL DEFAULT 1 CHECK (day_of_month BETWEEN 1 AND 31),
  interval_seconds INTEGER NOT NULL DEFAULT 60
    CHECK (interval_seconds BETWEEN 60 AND 2592000),
  backup_sets TEXT NOT NULL DEFAULT '["Accounts","Configuration","Ledger","Vouchers","Terminals","Runtime State","Credentials"]',
  retention_count INTEGER NOT NULL DEFAULT 14 CHECK (retention_count BETWEEN 1 AND 365),
  mirror_directory TEXT
);

INSERT INTO backup_schedule_settings (
  id, enabled, frequency, minute_of_day, weekday, day_of_month,
  interval_seconds, backup_sets, retention_count, mirror_directory
) VALUES (
  1, 1, 'daily', 240, 0, 1, 60,
  '["Accounts","Configuration","Ledger","Vouchers","Terminals","Runtime State","Credentials"]',
  14, NULL
);
