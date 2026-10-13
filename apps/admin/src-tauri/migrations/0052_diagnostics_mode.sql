-- Diagnostics Mode (ADR-0081): the shop-wide visibility flag for the
-- Diagnostics page. A single-row table, following this repo's convention of
-- one table per setting, defaulting to off.
--
-- It is visibility only: no capture, verbosity, or retention behavior
-- anywhere in Admin or the Client branches on it, and it is not a security
-- boundary — the flag grants no authority and touches no money, time,
-- Points, or session data. Every change is audited so it stays answerable
-- who revealed the surface and when.

CREATE TABLE diagnostics_mode_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1))
);

INSERT INTO diagnostics_mode_settings (id, enabled) VALUES (1, 0);
