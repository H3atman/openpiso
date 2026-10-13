CREATE TABLE theme_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  accent_color TEXT NOT NULL CHECK (accent_color GLOB '#[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]'),
  mode_web_admin TEXT NOT NULL CHECK (mode_web_admin IN ('light', 'dark')),
  mode_kiosk TEXT NOT NULL CHECK (mode_kiosk IN ('light', 'dark')),
  mode_client TEXT NOT NULL CHECK (mode_client IN ('light', 'dark')),
  version INTEGER NOT NULL CHECK (version > 0),
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO theme_settings (id, accent_color, mode_web_admin, mode_kiosk, mode_client, version)
VALUES (1, '#15803d', 'light', 'light', 'light', 1)
ON CONFLICT DO NOTHING;
