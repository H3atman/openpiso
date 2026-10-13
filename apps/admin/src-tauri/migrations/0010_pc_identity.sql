-- Migration 0010: server-side PC identity (ADR-0031 / plan §4.2).
--
-- The admin server now owns which `pcs` row a machine is. Identity is
-- resolved per request by (a) the MAC the client reports in the
-- `X-OpenPiso-Mac` header and (b) the source IP captured from the connection
-- peer address. The MAC is durable; the source IP only bootstraps the
-- mapping once.
--
-- Two new columns land on `pcs`:
--
--   mac_address   — the durable identity hint. Set on the first heartbeat
--                   from a pre-registered row whose `mac_address` is NULL
--                   and whose `registered_ip` matches the source IP. Never
--                   re-bound by IP after this (hardware/NIC replacement is
--                   the only case where the owner clears the binding).
--   registered_ip — the owner-authored static IP used only as the
--                   step-2 bootstrap match key. Immutable by device
--                   traffic; the owner edits it via the web-admin PC edit.
--
-- Both are nullable. Existing seeded PCs ship with NULL on both columns and
-- will be re-bindable by the owner pre-registering a static IP on the PC
-- edit page (or by leaving NULL and pre-registering fresh rows).
--
-- Each column gets a `UNIQUE` partial index `WHERE … IS NOT NULL` so two
-- rows cannot both claim the same MAC or registered IP, and a normal
-- `COUNT(*)` scan is still index-friendly. SQLite forbids partial UNIQUE
-- indexes; the partial index is the documented equivalent for nullable
-- columns (an explicit `NULL` marker would be needed for a true UNIQUE
-- constraint, which we deliberately avoid so pre-registered rows can
-- still be bound).
ALTER TABLE pcs ADD COLUMN mac_address TEXT;
ALTER TABLE pcs ADD COLUMN registered_ip TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_pcs_mac_address_unique
  ON pcs(mac_address) WHERE mac_address IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_pcs_registered_ip_unique
  ON pcs(registered_ip) WHERE registered_ip IS NOT NULL;
