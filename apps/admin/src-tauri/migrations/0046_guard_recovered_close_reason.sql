-- Client Guard Coverage closure reason (ADR-0076 / #344): an episode opened
-- by two continuous minutes of unavailable Guard coverage closes as
-- `guard_recovered` when the Client reports covered again — positive
-- evidence from the same diagnostic that opened it. SQLite cannot alter a
-- CHECK constraint, so the table is rebuilt with the extended allowed set;
-- every row and index is preserved unchanged.
CREATE TABLE pc_monitoring_incidents_rebuilt (
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
  -- Why the episode ended: an accepted heartbeat (`client_recovered`), one
  -- successful probe after coverage loss (`probe_recovered`), restored Guard
  -- coverage (`guard_recovered`), loss of the observable evidence sustaining
  -- the episode (`host_lost` — recovery stays unconfirmed), or shop-wide
  -- monitoring being disabled (`monitoring_disabled`). Never "recovered"
  -- without positive evidence.
  close_reason TEXT CHECK (close_reason IN (
    'client_recovered', 'probe_recovered', 'guard_recovered', 'host_lost',
    'monitoring_disabled'
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

INSERT INTO pc_monitoring_incidents_rebuilt (
  id, pc_id, kind, opened_at, closed_at, close_reason,
  observation_interrupted, open_audit_id, close_audit_id
)
SELECT
  id, pc_id, kind, opened_at, closed_at, close_reason,
  observation_interrupted, open_audit_id, close_audit_id
FROM pc_monitoring_incidents;

DROP TABLE pc_monitoring_incidents;
ALTER TABLE pc_monitoring_incidents_rebuilt RENAME TO pc_monitoring_incidents;

CREATE UNIQUE INDEX idx_pc_monitoring_incidents_open
  ON pc_monitoring_incidents (pc_id, kind)
  WHERE closed_at IS NULL;

CREATE INDEX idx_pc_monitoring_incidents_pc
  ON pc_monitoring_incidents (pc_id, closed_at);
