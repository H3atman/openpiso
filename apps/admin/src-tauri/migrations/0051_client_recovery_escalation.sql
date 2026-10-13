-- Client Recovery Escalation (ADR-0077). A PC whose Guard is polling, reports
-- no connected Client, and reports a failed relaunch is powered off after the
-- owner-set Escalation Delay. Three schema facts follow from that:
--
--   1. the session bound to that PC ends as `escalated_power_off`, a new
--      session-end reason that is not a Tamper Kick and carries no
--      attribution;
--   2. the open Client Reachability Mismatch closes as `escalated_power_off`
--      rather than `host_lost`, because the host stopped answering ICMP for a
--      reason we caused; and
--   3. one cooldown row per PC bounds a diskless machine that reboots straight
--      back into the same broken state to one power off per hour.
--
-- Balance, Remaining Time, Guest Time, and Points are untouched by all of it.
-- SQLite cannot alter a CHECK constraint, so both tables are rebuilt with the
-- extended allowed sets; every row and index is preserved unchanged.

PRAGMA foreign_keys = ON;

-- 1. Session-end reason -------------------------------------------------------

CREATE TABLE account_session_history_rebuilt (
  session_id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL,
  pc_id TEXT NOT NULL,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  end_reason TEXT CHECK (
    end_reason IS NULL OR end_reason IN (
      'logout', 'auto_kick', 'taken_over', 'expired', 'tamper_kick',
      'escalated_power_off', 'server_restart'
    )
  ),
  FOREIGN KEY (account_id) REFERENCES accounts(id),
  FOREIGN KEY (pc_id) REFERENCES pcs(id)
);

INSERT INTO account_session_history_rebuilt (
  session_id, account_id, pc_id, started_at, ended_at, end_reason
)
SELECT session_id, account_id, pc_id, started_at, ended_at, end_reason
FROM account_session_history;

DROP TABLE account_session_history;
ALTER TABLE account_session_history_rebuilt RENAME TO account_session_history;

CREATE INDEX idx_account_session_history_pc_started
  ON account_session_history(pc_id, started_at);
CREATE INDEX idx_account_session_history_account_started
  ON account_session_history(account_id, started_at);
CREATE INDEX idx_account_session_history_open
  ON account_session_history(pc_id, ended_at)
  WHERE ended_at IS NULL;

-- 2. Incident closure reason --------------------------------------------------

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
  -- the episode (`host_lost` — recovery stays unconfirmed), shop-wide
  -- monitoring being disabled (`monitoring_disabled`), or our own Escalated
  -- Power Off (`escalated_power_off`). Never "recovered" without positive
  -- evidence, and never `host_lost` for a host we powered off ourselves.
  close_reason TEXT CHECK (close_reason IN (
    'client_recovered', 'probe_recovered', 'guard_recovered', 'host_lost',
    'monitoring_disabled', 'escalated_power_off'
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

-- 3. Escalation cooldown ------------------------------------------------------

-- One row per PC that has been powered off, holding the instant of the last
-- Escalated Power Off. The cooldown is durable rather than in-memory because
-- an Admin restart must not license a second power off inside the same hour.
CREATE TABLE pc_escalation_cooldowns (
  pc_id TEXT PRIMARY KEY REFERENCES pcs(id) ON DELETE CASCADE,
  last_power_off_at TEXT NOT NULL
);
