-- PC Monitoring Incidents (ADR-0076): one durable episode of degraded PC
-- Monitoring coverage for one PC. All monitored failure kinds share this one
-- model; the kind is typed incident data, never a forked Audit event name.
-- Opening commits with a generic `pc_monitoring_incident_opened` Alert and
-- closure with a generic `pc_monitoring_incident_closed` Notice, each in the
-- same SQLite transaction as the row change.
--
-- v1 (#341) only opens `client_reachability_mismatch`; the other two kinds are
-- reserved so later coverage work needs no schema change.
CREATE TABLE pc_monitoring_incidents (
  id TEXT PRIMARY KEY,
  pc_id TEXT NOT NULL REFERENCES pcs(id),
  kind TEXT NOT NULL CHECK (
    kind IN (
      'client_reachability_mismatch',
      'host_probe_coverage',
      'client_guard_coverage'
    )
  ),
  opened_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  closed_at TEXT,
  -- Why the episode ended: an accepted heartbeat (`client_recovered`), loss of
  -- Host Reachability or of any observable target (`host_lost` — Client
  -- recovery stays unconfirmed), or shop-wide monitoring being disabled
  -- (`monitoring_disabled`). Never "recovered" unless the Client truly came
  -- back.
  close_reason TEXT CHECK (close_reason IN (
    'client_recovered', 'host_lost', 'monitoring_disabled'
  )),
  -- Set once Admin restarts over an incident still open: its wall-clock span
  -- was not continuously observed, so display must not claim otherwise.
  observation_interrupted INTEGER NOT NULL DEFAULT 0
    CHECK (observation_interrupted IN (0, 1)),
  open_audit_id TEXT NOT NULL REFERENCES audit_log(id),
  close_audit_id TEXT REFERENCES audit_log(id),
  -- A closure always carries exactly one reason; an open episode has none.
  CHECK ((closed_at IS NULL) = (close_reason IS NULL))
);

-- One open incident per PC and kind (ADR-0076): repeated ticks stay
-- idempotent because a second open cannot be inserted while one is open.
CREATE UNIQUE INDEX idx_pc_monitoring_incidents_open
  ON pc_monitoring_incidents (pc_id, kind)
  WHERE closed_at IS NULL;

-- Dashboard badge lookups scan for open episodes by PC.
CREATE INDEX idx_pc_monitoring_incidents_pc
  ON pc_monitoring_incidents (pc_id, closed_at);
