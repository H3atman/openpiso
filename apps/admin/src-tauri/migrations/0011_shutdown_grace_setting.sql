-- Owner-editable shutdown grace period (ADR-0022 refinement).
--
-- The server still owns the power_state decision and the client Rust process
-- owns the OS shutdown. This setting only controls how long the client waits
-- between receiving power_state = shutdown and issuing the OS shutdown.
INSERT INTO settings (key, value, updated_at)
VALUES ('shutdown_grace_seconds', '30', CURRENT_TIMESTAMP)
ON CONFLICT(key) DO NOTHING;
