# ADR-0031: Client configuration persistence and server-side PC identity

- Status: Accepted
- Date: 2026-06-29
- Deciders: shop owner (product)
- Relates to: ADR-0027 (client lock & owner PIN gate), ADR-0028 (kiosk OS
  hardening), ADR-0029 (client window model / `view_mode`), ADR-0024 (web-admin
  sole owner surface), ADR-0011 (admin authority), ADR-0016 (diagnostic logging).

## Context

The OpenPiso Client keeps its `ClientConfig` (`server_url`, `pc_id`,
`view_mode`) **in memory only**: every launch starts from
`ClientConfig::default()` (`http://127.0.0.1:3210` / `pc-01` / `widget`). Adding
launch auto-connect made that gap load-bearing — an owner who points a PC at a
real admin URL loses it on reboot, and on a multi-PC deployment every client
reconnects as `pc-01` and collides on one identity.

Two problems are entangled and decided together here:

1. **The client forgets its own setup.** It needs to remember which server it
   talks to across restarts.
2. **The client asserts its own identity.** Today the heartbeat is keyed on the
   client-claimed `pc_id` in the URL — a device-supplied id, which the durable
   invariants say the server must never trust. On diskless/imaged fleets, per-PC
   `pc_id` setup is also tedious and easy to clone wrong.

Deployment reality that bounds the design: client PCs run on **static IPs**, are
**wired** (onboard Ethernet), and frozen (Deep Freeze) / diskless PCs are
configured by the owner manually (unfreeze → set up → refreeze, or rebuild the
diskless image). So the software does **not** need to defeat freezing — it only
needs durable identity once the owner has done that one-time setup.

Part A (config persistence) and Part B (server-side identity) **ship as one
coupled change**: identity is the whole point, and shipping persistence with an
owner-typed `pc_id` first would build a field Part B immediately removes.

## Decision

### 1. The client persists its configuration to a Rust-owned file

`ClientConfig` is written to `client-config.json` in `app.path().app_config_dir()`
(`%APPDATA%\org.openpiso.client\` on Windows) as pretty `serde_json`. It holds
`server_url`, `view_mode`, and the **server-assigned, cached** `pc_id` (see §2).
It is:

- **read at startup** (`.setup()`) **before** auto-connect, so the first
  connection uses the persisted server, falling back to defaults if the file is
  missing/corrupt (logged at `warn`, never blocks startup);
- **rewritten** on every committed `set_client_config`, on `view_mode` changes,
  and when the client adopts a newly server-resolved `pc_id` — each guarded
  against no-op writes (only saved when the field actually changed), via an
  atomic temp-write + rename. Every save serializes a snapshot of the
  authoritative in-memory config taken under the config write lock, so the
  command path and the connection task (which both write the file) cannot
  clobber each other's fields.

All disk I/O lives in a Rust `config_store` module called only from
commands/setup/connection task. The **frontend never reads or writes the file**.
No new plugin and **no new capability** (Rust-side `std::fs` + `app.path()` are
not ACL-gated); validation stays at the Rust boundary before anything is
persisted. No secrets are stored: the owner PIN hash stays memory-only
(ADR-0027 §3); auth/session tokens stay memory-only.

**Save-failure policy: soft-fail with a visible warning.** A failed save does
not block the operation (the in-memory config is already authoritative for the
session), but `set_client_config` returns a non-fatal warning the settings UI
shows ("Settings applied, but couldn't be saved — they may not survive a
reboot"), so the silent-revert failure mode this ADR exists to fix cannot recur
unnoticed.

### 2. The server owns PC identity, keyed on MAC with IP bootstrap

`pc_id` is **server-assigned identity that the client caches**, not owner intent.
There is **no owner-editable PC ID field** on the Client — the owner pre-registers
PCs server-side (web-admin); the client only edits `server_url` (and the customer
sets `view_mode`). The client receives its `pc_id` from the server, caches it
(§1), and shows it in the footer for diagnostics.

**Identity key = the MAC of the onboard Ethernet NIC**, resolved deterministically
as the MAC of the interface that carries the route to the admin server (in a
wired shop, that *is* the onboard NIC), falling back to the lowest-sorted
physical, non-virtual, non-wireless MAC when the server is unreachable at boot.
It is read per boot, **never persisted** on the client.

**Client identity endpoints are MAC-keyed, not path-keyed.** `heartbeat`,
`events`, and `disconnect` carry the MAC in an `X-OpenPiso-Mac` header (uniform
across the SSE GET and the POSTs); the server reads the **source IP from the
connection peer address** (`ConnectInfo<SocketAddr>`, requiring
`into_make_service_with_connect_info`) and never trusts a body-supplied IP. The
`{pc_id}` path parameter survives only on owner/web-admin routes. On each report
the server resolves identity in this order:

1. **MAC matches an existing PC row** → that is the identity. Steady-state path
   on every reboot — the PC is auto-recognized with no re-setup.
2. **No MAC match, but the source IP matches an owner-preregistered row whose
   `mac_address IS NULL`** → bind that row's `mac_address`. One-time onboarding
   bind; static IPs make it dependable. An already-bound row is **never**
   re-bound by IP.
3. **No MAC and no IP match** → **reject** the report; the machine is
   unregistered.

The server returns the authoritative `Pc`; the client adopts the returned
`pc_id` and persists it (§1). The pure `(mac, source_ip, rows) → decision`
logic is a free function for unit tests.

The client distinguishes three conditions the current single error string
conflates: **not yet identified** (server reachable, resolution in flight — Lock
shows "Registering this PC…"), **unregistered** (server returned the typed
rejection — Lock shows "ask the owner to register this PC", retries quietly),
and **unreachable** (transport failure). `ClientConfig.pc_id` and
`TimerState.pc_id` become `Option<String>` (no misleading `pc-01` default), and
`connection_status` gains an `"unregistered"` value.

**MAC is an identity hint, not an auth secret.** It is spoofable, so it never
gates money/time/session authority — that remains the server's job, and (when
enabled) the HMAC device secret per AGENTS.md. MAC only answers "which PC row is
this," and IP only bootstraps that mapping once.

### 3. Hardware replacement

A diskless re-image / Deep Freeze rebuild does **not** change the onboard NIC's
MAC, so step 1 keeps working across re-imaging untouched. The only case where a
registered PC reports a *new* MAC from the same IP is actual hardware/NIC
replacement; it is rejected (step 2 binds NULL-MAC rows only) until the owner
clears the binding — editing the PC in web-admin to null its `mac_address`, so
the next boot re-binds by IP.

## Considered options

- **`tauri-plugin-store` for persistence.** Rejected: a production dependency,
  plugin registration, **and** a JS capability for one small file — more surface
  than the local-first, minimal-capability stance wants. Revisit only if many UI
  preferences later need storage.
- **Keep client-asserted `pc_id` as the identity (status quo).** Rejected:
  trusts a device-supplied id, and clones/`pc-01` collisions are exactly the bug.
- **IP as the durable identity key.** Rejected: even with static IPs, an owner
  re-planning addresses or re-using an IP after a re-image would silently move a
  PC's session/time history to a different machine. MAC is durable; IP only
  bootstraps.
- **Auto-create a pending/hidden PC for unknown identity.** Rejected: the
  identity/heartbeat endpoint is unauthenticated, so auto-create lets any LAN
  client spawn unbounded junk rows, and it adds a whole pending/adopt UI. The
  owner sets PCs up deliberately, so **reject unknown and pre-register only** is
  both safer and simpler.
- **Keep path-keyed client endpoints, re-key the SSE stream after resolution.**
  Rejected in favour of MAC-keyed endpoints: keeping a client-claimed `pc_id` in
  the path means the untrusted claim is also the URL key, with a first-boot
  window where the SSE stream is opened on the wrong id and the client rejects
  its own events. MAC-keying is the cleaner end state.
- **Observed `last_ip` column.** Dropped from v1: diagnostic-only and
  functionally inert. Only `registered_ip` (the match key) is needed. Add
  `last_ip` later if IP-drift visibility is ever actually wanted.
- **Dedicated "Reset NIC Binding" button.** Deferred: hardware swaps are rare;
  the owner clears the binding via the existing PC edit. Add a one-click action
  only if it becomes a real annoyance.
- **Route-based MAC selection.** Kept (≈10 lines, robust); a plain
  "first non-loopback non-virtual Ethernet interface" filter is an acceptable
  fallback. Elaborate adapter heuristics are out of scope.

## Consequences

- New migration `0010` adds `mac_address` and `registered_ip` to `pcs` (both
  nullable), each with a `UNIQUE` partial index `WHERE … IS NOT NULL`.
- The identity endpoints become MAC-keyed (`X-OpenPiso-Mac` header + peer-IP);
  `:3210` is mounted with `into_make_service_with_connect_info`. `db::heartbeat`
  (and the SSE filter) gain the three-step resolution and return the
  server-resolved `Pc`; unknown identity is rejected with a typed body the
  client recognizes.
- The client gains per-boot MAC collection (Rust-side, **not** persisted),
  adopts the server-resolved `pc_id` into its persisted config, and renders the
  three identity states. `set_client_config` no longer takes a `pc_id`.
- Web-admin gains **pre-register a PC** (number/name + static IP). There is no
  pending/adopt surface. Clearing a NIC binding is done via the existing PC edit.
- The client config file holds only a LAN URL, a cached PC id, and a UI
  preference — no secrets. A corrupt file degrades to defaults and is
  overwritten on the next successful save.
- Pure encode/decode of `ClientConfig` and the MAC/IP resolution decision are
  factored as free functions so they unit-test without a Tauri `AppHandle` or a
  live socket.
