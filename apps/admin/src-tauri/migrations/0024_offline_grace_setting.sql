-- Owner-editable Offline Grace duration for Clients that lose the admin server.
-- Distinct from shutdown_grace_seconds, which controls expiry shutdown.
INSERT INTO settings (key, value, updated_at)
VALUES ('offline_grace_seconds', '120', CURRENT_TIMESTAMP)
ON CONFLICT(key) DO NOTHING;
