-- Per-PC Monitoring Suppression (ADR-0076 / #342): an owner-authorized,
-- automatically expiring pause on new PC Monitoring incidents for one PC.
--
-- One row per suppressed PC: creating a suppression while one is active
-- replaces it, so the primary key is the PC itself. Suppression never erases
-- or closes an incident already open — it only blocks new openings while the
-- absolute expiry is in the future. The maximum duration (60 minutes) is
-- fixed policy enforced in Rust; the stored value stays a server-computed
-- absolute instant so Admin restarts cannot extend or lose it.
CREATE TABLE pc_monitoring_suppressions (
  pc_id TEXT PRIMARY KEY REFERENCES pcs(id),
  -- Server-computed absolute expiry (RFC 3339 UTC). Display shows this
  -- exact instant; lazily-expired rows simply stop matching "active".
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
