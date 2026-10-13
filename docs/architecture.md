# Architecture

OpenPiso is local-first. The admin Tauri process owns SQLite, pricing, paid
time, workstation status, the Central Terminal kiosk, and the LAN HTTP
servers (`:3210` for the client/device API, `:8080` for the owner web
admin). The Tauri webview is the customer **Central Terminal** kiosk; the
owner surface lives at `http://<host>:8080/admin` and is served as static
assets by the same Rust process. That `:8080` server also owns the authenticated
owner HTTP routes (ADR-0024, M5/M6). ADR-0075 plans an authenticated Remote
Control WebSocket bridge on the same server, with outbound RFB/TCP from Admin to
a PC's TightVNC server; it is not implemented yet.

Admin's outbound internet paths include optional Telegram Alert delivery
(ADR-0076) and always-on Telemetry in token-bearing builds (ADR-0083).
A separate Rust worker snapshots one anonymous Daily Summary per UTC+08:00
reporting day into a SQLite outbox and sends due rows to PostHog Cloud US.
Retries never block startup, Heartbeats, timers, coins, sessions, or shutdown;
PostHog responses cannot change local authority. Builds without the project
token cannot send. The owner cannot disable Telemetry in a token-bearing build;
there are no Telemetry controls. Neither Web Admin nor the Client sends Telemetry.

The client Tauri process stores its connection and countdown state in Rust. It
subscribes to a PC-scoped Server-Sent Events stream for immediate
`pc.updated` notifications. It also sends a heartbeat every five seconds and
reconciles its local display with the authoritative remaining time returned by
the server. The heartbeat remains the recovery and liveness mechanism if the
event stream disconnects. On an intentional disconnect or normal application
shutdown, the client calls the admin disconnect endpoint so the PC is marked
offline immediately. The Svelte client only renders Rust-owned state and
collects the server URL.

PC identity is **server-owned** (ADR-0031): the client reports its
onboard-Ethernet MAC in the `X-OpenPiso-Mac` header and the server
resolves which `pcs` row the machine is by MAC match or — on first
boot — by matching the connection source IP against an owner-preregistered
`registered_ip` with `mac_address IS NULL`. The `{pc_id}` path is
gone from the client identity surface (`/api/identity/{heartbeat,disconnect,events}`);
the owner never types a PC ID, and the client caches the resolved
`pc_id` rather than asserting one. ADR-0075 also uses `registered_ip` as the
server-owned Remote Control target after strict IPv4 and configured-subnet
validation; the browser never supplies a host or port.

The admin has two authoritative timer modes. Guest Time remains in
`pcs.remaining_seconds` and drains only while that PC is online or in use.
When an account is logged in, its `accounts.remaining_seconds` balance drains
once per second even if the client crashes, disconnects, or misses heartbeats.
Every charged account second is committed directly to SQLite; there is no
in-memory balance batch. Remaining Time is clamped at zero. A bound account at
zero Remaining Time is not expired while its PC still holds claimable Guest
Time; the binding survives so the customer can claim it instead of being
returned to the login screen within a tick. Once the Guest Time is claimed or
the PC has none, a zero balance expires the session normally.

Active account-to-PC bindings live only in the admin Rust process. One mutex
protects indexes unique by both account and PC, so login resolves auto-kick and
PC takeover contention as one operation. The binding is never restored from
the legacy `sessions` table. A separate `account_session_history` table records
those bindings as history, including their ends, but is not a live-session
store. Startup closes rows left open by the prior process at the PC's last
known heartbeat as `server_restart`; it does not create end Audit Log events or
restore bindings. Restarting the admin server therefore clears all bindings
and tokens while preserving the last committed account and Guest Time
balances. Restarting only a client does not log the account out: the server
continues charging it, while the restarted client shows the generic login
screen and requires credentials again. See
[ADR-0062](adr/0062-account-session-history.md).

Login, logout, auto-kick, takeover, expiry, claim, Tamper Kill, and Tamper Kick
are canonical audit events. Claim transfers all current Guest Time to the
logged-in account in one SQLite transaction and never creates new time. Logout removes only the
ephemeral binding; the account balance remains in SQLite. Committed session
changes, coin transactions, heartbeat status changes, and timer ticks publish
`pc.updated` only after the authoritative operation completes. The admin
dashboard receives the same updates through an internal Tauri event, while
LAN clients receive them through SSE.

Coin simulator and ESP32-shaped requests enter through the same Rust
transaction path. The device supplies only PC selection, amount, pulse facts,
and source identity. While holding the session binding lock, the server
resolves whether the selected PC currently has an active account session. Guest
coins update `pcs.remaining_seconds` and earn zero points. Logged-in coins
update the account's Remaining Time and centipoint points balance directly
(fractional earning, ADR-0038), with only sub-centipoint remainders carried
in centicents. The
same SQLite transaction records the canonical audit event plus its linked
transaction detail. Pricing, points, and coin routing authority all remain in
the admin Rust process.

Attendance Reward extends the Account coin pipeline without changing coin
routing. When earning is enabled, accepted Account Coin Events add Attendance
progress and create a Check-In only when the daily threshold is crossed, in the
same authoritative SQLite transaction. Account-targeted Rejection Credits use
the original rejection instant and frozen attribution.
Attendance Time is a separate non-draining Account liability. Only an
authenticated full claim for one Earning Month transfers its outstanding
seconds into Account Remaining Time; claims alone enter Free Time Dispensed.
Attendance does not change Cash Revenue, Points, Guest routing, or paid-time
authority.

The `:3210` Account API serves the summary, month calendar, and claim; `:8080`
Owner routes serve settings, preview, and reporting. An additive
`attendance_supported` heartbeat fact gates the Client's customer-facing Daily
Reward strip/calendar and claim surfaces, including the zero-time Fullscreen Lock.
Clients that are Guests or connected to an older Admin show no Attendance
surface. Rust remains authoritative; the Client only renders server-derived
state. See [ADR-0084](adr/0084-attendance-rewards.md), which is still Proposed.

The Attendance response contract lives once in `openpiso-core`
(`attendance_wire.rs`, separate from the authority/projection code in
`attendance.rs`): typed `DateTime`/UUID fields and shared `EarningMonthState`,
consumed directly by Admin and the Client. JSON field names, snake-case enums,
nullable dates, and the optional `latest_claim_on` default are preserved for the
TypeScript consumers. Snapshots, DB rows, calendar, and claiming stay in Admin.

Pre-Migration Backup owns both primary artifacts: `write_pre_migration_backup`
succeeds only when the SQL backup and its `.db` companion both exist, and
removes the SQL file if companion creation fails, so the migration runner makes
one call. Ordering is unchanged: SQL creation, mirror attempt and retention,
companion, migrations, mirror-advisory persistence. Mirror failures stay
advisory. Active Session Bindings keep the account-to-PC and PC-to-account maps
consistent internally; events and account tokens are unchanged.

Vouchers and guaranteed points redemption follow the same authority boundary.
The owner mints `sold` or `promo` vouchers through admin Tauri commands; codes
are random UUID-derived values and are never chosen by the frontend. A sold
voucher records cash revenue at mint/sale time. Redemption is account-only over
the LAN API, consumes an active voucher at most once in a SQLite transaction,
credits account Remaining Time and Points, and appends the canonical audit row.
Guaranteed points redemption spends one server-provided Redemption Tier for
time in the admin Rust process; insufficient-points denials change no balance.

Spin the Wheel is the only gambling path and stays entirely server-side. The
owner edits the prize table, optional Bokya Rate, free-spin cap, and master
on/off switch; the admin process validates the table and refuses any
configuration whose expected payout is not strictly below the point value of
the most generous Redemption Tier, recomputed from the live tier set. A spin
enters over the LAN API with only the account session token: the server deducts
one point, rolls its own RNG, resolves the capped free-spin chain, credits won
time and points, and appends the canonical audit event plus its one-to-one
detail row in a single SQLite transaction. The client receives the
already-decided outcome and only animates it; it can neither choose nor reroll a
result. Each Wheel Segment is an equal-size prize pair that can award time,
points, a Free Spin, or any combination. The draw uses one server-owned
probability distribution: unset Bokya Rate draws uniformly across all segments;
a set rate first chooses Bokya versus non-Bokya, then draws uniformly within the
chosen group. Equal geometry therefore does not state the odds when Bokya Rate
is set ([ADR-0049](adr/0049-bokya-rate.md)). The time-axis guardrail remains
mandatory; points deliberately have no save-time gate, with Points Minted by
Wheel reporting as the compensating control (ADR-0046).

M1 adds local customer accounts and a single owner credential. Passwords are
Argon2 hashes in SQLite and are never returned through Tauri or HTTP APIs.
Owner and customer authentication tokens are process-memory capabilities
invalidated when Admin restarts. Owner Sessions additionally
(`OwnerTokens` in `auth.rs`) implement ADR-0074's HttpOnly cookie, session CSRF
token, exact-Origin mutation check, and 30-minute idle / 12-hour absolute expiry
with an explicit human-activity path; the owner token never appears in JSON. See
[api.md](api.md#owner-session-contract-adr-0074-implemented). Remote Control,
step-up token rotation, and long-lived-connection cancellation are not
implemented. Customer authentication tokens are unchanged. The desktop Svelte
owner dashboard is no longer shipped; mutations are reached over the
authenticated `:8080/admin` HTTP API (M4) and the browser web app (M5). Customer login/sign-up crosses
the LAN boundary through validated Rust HTTP handlers on `:3210`.

### Central Terminal & Web Admin (M1 — serial coin pipeline foundation)

[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) moves the
coin entry point from the deferred ESP32/HTTP path to a serial coin acceptor
wired directly to the server's COM port. In v1 the **admin Rust process
reads the acceptor itself**; the device supplies only pulses, never a
`pc_id`. The `CoinSource` boundary in `apps/admin/src-tauri/src/coin_source.rs`
defines the contract that the real hardware and the headless simulator both
implement:

- `try_next_batch_at(now)` / `try_next_batch()` → a `CoinBatchRead`
  outcome: `Batch(CoinBatch { coin_trace_id, pulse_count, pulse_timings_us,
  span_us, gap_us, attempt })` for every closed positive-count batch, or
  `None` while no batch is ready. A Coin Batch contains observations and retry
  evidence only; authority derives checked `pulse_count × 100 cents` at the
  commit boundary. The `*_at(Instant)` form takes an explicit clock so the M2
  selection state machine can drive the gap check without sleeping; the
  default `try_next_batch` delegates to real time. The serial source's
  dedicated **Pulse Sampler** supplies the shared pulse buffer at approximately
  30 ms; the selection loop does not read the pin.
- `assert_set_pin` / `inhibit_set_pin` / `is_set_pin_asserted` drive and
  read the COM-header control line the acceptor inhibits on (RTS by default).
- `SerialCoinSource` opens the port, inhibits the set pin **before** the
  source is ever observed, and uses the shared sampler to seed previous-level
  state from the first normalized Coin Pin sample (no edge or pulse). Later
  rising edges (Carrier Detect by default) are observed, and the source
  re-inhibits on `Drop`. After 20 consecutive Coin Pin read
  failures the sampler drops the handle and retries reopening with a bounded
  1–30 second backoff; a reopened handle is inhibited before sampling resumes
  and never reasserts the Set Pin.
- `ConfiguredCoinSource` selects `SimulatorCoinSource` when no COM port is
  configured and `SerialCoinSource` otherwise. M4 loads the line mapping, gap,
  and Selection Window timers from SQLite. The persisted Pulse Map is not part
  of runtime source configuration; it remains for calibration and historical
  Rejection Credit denomination choices. Authenticated settings updates
  replace the inhibited source live while the terminal is idle.

Money is integer cents and automatic Coin Batch authority is fixed at 100 cents
per observed pulse. Every positive pulse count is accepted after the quiet gap;
there is no `Unmapped` outcome and no live Pulse Map mismatch rejection. Read
`None` and transport/availability errors remain typed separately. The map, the
gap, the COM port name, and which control/status line is the set/coin pin are
owner-editable settings persisted in SQLite by migrations
`0007_coin_device_settings.sql` and `0008_terminal_selection_timers.sql`; see
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md).

### Central Terminal & Web Admin (M2 — Terminal Selection state, lock, and routing)

[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) specifies a
server-owned `TerminalSelection` singleton. M2 implements it in
`apps/admin/src-tauri/src/terminal_selection.rs`:

- **Singleton, in-memory only.** The selection is a `tokio::Mutex<Option<ActiveSelection>>`
  inside the `AppState`. It is never persisted: a server restart clears the
  selection, so the set pin starts inhibited and the next window must be
  opened by a fresh kiosk interaction. This matches ADR-0013: session and
  selection bindings are memory-only.
- **Lock.** `open(pc_id)` rejects with `AppError::Conflict("terminal in use")`
  when one is already active. The set pin is asserted **only** after the
  lock check passes; the wire call happens before the selection becomes
  visible, so a failed assert leaves the post-`close` invariant intact.
- **Fail-closed on every transition.** The set pin is inhibited by:
  construction (defence-in-depth re-assert after the source's own
  constructor inhibit), `Drop` of the source, every `close` path
  (`Enter`, `Inactivity`, `HardCap`, `Manual`, `Shutdown`). `Drop` provides a
  best-effort graceful-shutdown inhibit. After abnormal termination the OS
  closes the handle; restart explicitly inhibits the configured set-pin line
  before the source becomes available.
- **Window timers.** A Selection Window is a UX/timeout boundary only.
  Inactivity (15 s, reset per accepted coin) and hard cap (180 s) are
  enforced by a 500 ms background poll task in `lib.rs` that calls
  `tick()` and `try_accept_next_batch()`. The auto-close path inhibits
  the pin, drains in-flight batches, and broadcasts the closed snapshot.
- **Per-coin immediate commit.** Each closed `CoinBatch` is committed
  through the existing `sessions.record_coin_event(input)` path with
  `input.pc_id = active.pc_id`. Routing (account vs guest, points earn,
  carry) stays in `sessions`/`db` per ADR-0008 — M2 does not change it.
  The lock is held across the source read **and** the DB commit so a
  concurrent `close` cannot drop the coin between "I read a batch" and
  "I credited it".
- **In-flight at close.** After the Set Pin is inhibited, the source's
  gap-bounded batch buffer is drained for the configured gap plus scheduling
  headroom, while the singleton remains occupied. The Pulse Sampler may still
  observe the tail of a train already buffered; those admitted straddling
  pulses remain one Coin Batch and are credited to the just-selected PC using
  the fixed pulse-sum authority. An edge observed with an empty buffer remains
  refused as an Inhibited Pulse. There is no live rejection branch in the
  drain; genuine transport errors retain the pending batch and stop draining.
  This is the ADR-0023 rule that "a coin already in flight when a window closes
  still credits the just-selected PC".
- **Authority over `pc_id`.** The selection is the only source of
  `pc_id` for an accepted coin. The device payload (real serial or
  simulator) never carries one. The M2 tests assert this end-to-end:
  the simulator can inject only pulses, and the credit lands on the
  selected PC.

The Tauri command surface is intentionally Tauri-local: the selection is
the customer-facing Central Terminal flow (M3 kiosk) and is never exposed
on the `:3210` LAN HTTP API.

- `open_selection(pc_id)` → asserts the set pin, returns the broadcast
  `SelectionState` or rejects with `Conflict("terminal in use")`.
- `close_selection(reason?)` → inhibits the set pin, drains in-flight,
  returns the closed `SelectionState` or `None` if no window was open.
  `reason` is one of `"enter"`, `"inactivity"`, `"hard_cap"`, `"manual"`,
  or `"shutdown"` (defaults to `"manual"`).
- `get_selection()` → the current `SelectionState` snapshot, or `None`.
  The frontend calls this on mount for the initial state and listens to
  the `selection-state` Tauri event for live updates.
- `selection-state` Tauri event → `Some(SelectionState)` on open/per-coin
  updates and `None` on close. Mirrors the `pc-updated` pattern in
  `events::EventHub`.

The selection is intentionally **not** persisted, **not** exposed on the
LAN HTTP API, and **not** authenticated — the kiosk is a local Tauri
window, the owner endpoints on `:8080` are a separate M4 surface, and the
PIN is the only owner auth (ADR-0024).

### Central Terminal & Web Admin (M6 — retire desktop console, harden ops, finalize)

M6 retires the desktop Tauri owner console. The Tauri webview is now
**exclusively the customer Central Terminal kiosk**; the 27 owner Tauri
commands that used to be invoked by `apps/admin/src` are removed
(`commands.rs` keeps only `open_selection`, `close_selection`,
`get_selection`, `list_pcs_kiosk`, `get_kiosk_status`,
`get_kiosk_config`), and
`AppState` no longer holds `OwnerTokens` — the handle is owned by the
`:8080` web-admin server. The owner surface is reached only over
authenticated HTTP at `http://<host>:8080/admin`, served by the same Rust
process. M6 is the first milestone where the `KioskCommandNames`
constant in `commands.rs` exists solely to pin this invariant; a
regression test (`tauri_kiosk_commands_contains_only_kiosk_entries`)
asserts both the exact set of allowed commands and that every retired
desktop owner command name stays absent.

The Tauri capability is now genuinely minimal: only `core:default`,
`core:event:default`, and `core:window:default` for the single
`kiosk` window. No shell, no process, no filesystem, no owner
permissions — the owner data never reaches the Tauri webview.

**Ops note (ADR-0024, ADR-0074).** Because `:8080` exposes money-moving,
account-deleting, and Remote Control authority, the operator is responsible
for **binding `:8080` to the wired/admin interface or a separate
VLAN** at install time. The default `0.0.0.0:8080` bind is correct
on a private LAN but should not be reachable from a customer Wi-Fi
SSID. See `docs/installation.md` for the recommended network layout.

### Planned Web Admin Remote Control (ADR-0075)

This accepted design is not implemented yet.

The Web Admin embeds noVNC in a dedicated Dashboard dialog. A deep
`remote_control` Rust module inside the Admin process owns the 30-second
single-use grants, server-side target resolution, one-session shop-wide permit,
TCP/WebSocket relay, cancellation, timeouts, and Audit Log writes. The
`web_admin.rs` routes are thin adapters. The production connector opens TCP to
TightVNC; tests use in-memory duplex streams through the same internal seam.

The bridge extends `:8080`; it adds no listener, sidecar, Tauri command, or
capability. The kiosk capability remains limited to core event and window
operations. Rust relays bytes without interpreting RFB, so its audit states
describe bridge connections and requested access, not proven TightVNC
authentication.

Remote Control Settings are disabled by default and stored through typed
repository functions over SQLite's existing settings table. Rust resolves a PC
id to its canonical private `registered_ip`, verifies membership in the allowed
shop CIDR, applies the shop-wide port, and snapshots the target into the grant.
The browser never receives or chooses that target. Owner Sessions, pending
grants, and live Remote Control state remain in memory; the append-only Audit
Log is the only durable session history.

`remote_control_started`, `remote_control_enabled`,
`remote_control_ended`, and crash-recovery `remote_control_interrupted` events
share a Remote Control Session id. They use the Admin category, create no
Notification, and omit target IPs, credentials, framebuffer, clipboard, input,
and byte counts.

**Real-hardware bring-up.** The coin-device settings persisted in
migration `0007` (COM port, set/coin line pair, inter-coin gap, pulse
map) and the Selection Window timers from `0008` are loaded by
`ConfiguredCoinSource::from_settings` at startup, so the M1/M2/M4 code
already handles both the simulator and the real serial source. The
on-site bench test procedure in `docs/hardware.md` is the calibration
checklist; the "test acceptor" endpoint at `POST /web-admin/coin-device/test`
opens the configured port (set pin left inhibited, fail-closed) and
reports availability without accepting coins.

### PC Monitoring Check now — Host Reachability tracer bullet (ADR-0076)

The first PC Monitoring increment ships the non-mutating **Check now**
comparison: Web Admin can ask Admin to probe one visible registered PC or
every eligible PC and see current **Host Reachability** next to
**Client Reachability**. It opens no incidents, writes no Audit rows or
Notifications, and never touches sessions, money, Remaining Time, or Points.

The ICMP echo lives in the new `monitoring.rs` module behind a `HostProbe`
seam. Production Windows Admin sends one `IcmpSendEcho` call through the
Windows IP Helper API with a fixed one-second timeout — it never starts ping,
PowerShell, a shell, or a sidecar process, and probes run on blocking threads
so they cannot delay authority timers. Non-Windows Admin gets an explicit
unsupported adapter: results report `unavailable`, never a manufactured
healthy claim.

Targets are selected server-side from each row's `registered_ip`; the owner
HTTP endpoints (`POST /web-admin/pc-monitoring/check/{pc_id}` and
`POST /web-admin/pc-monitoring/check-all`) take no request body, so no caller
can supply a probe address. Hidden, addressless, and unknown PCs are excluded
from check-all or returned with typed unavailable reasons on check-one. The
result contract's Guard Coverage field now reports the Client's last reported
diagnostic (#344): `covered`, `unavailable`, `not_applicable` from debug and
non-Windows builds, or `unsupported` when no Heartbeat has reported the field
yet.

The second increment (#341) makes the comparison continuous. A dedicated
`MonitoringService` task probes every ten seconds — never inside the one-second
money/time timer — while shop-wide monitoring is enabled. Monitoring is off by
default after upgrade (the settings key is simply absent) and enabling requires
an authenticated Owner Session plus the current Owner PIN, audited; a
non-Windows Admin refuses to enable because it can never probe. A Client
Reachability Mismatch opens only after Host-Reachable + Client-Unreachable
coexist continuously for 60 seconds, with the same full grace after Admin
startup, enabling, and each break in host reachability. Episodes are durable
`pc_monitoring_incidents` rows committed in one transaction with generic
`pc_monitoring_incident_opened` (Alert) / `pc_monitoring_incident_closed`
(Notice) Audit events through ADR-0058's fixed classifier. One accepted
heartbeat closes an episode as recovered; lost host evidence closes it as
unconfirmed. Restarts mark open incidents observation-interrupted instead of
duplicating Alerts; disabling closes them as monitoring-disabled. Incident
paths stay read-only toward PC status, sessions, money, Remaining Time, and
Points, and the dashboard renders active-incident badges from durable state,
independent of Notification acknowledgement.

Per-PC **Monitoring Suppression** (#342) completes the loop for authorized
maintenance. The owner creates or cancels it per visible registered PC from
the dashboard rows; both mutations re-prove the Owner PIN like the toggle,
validate the target at the Rust boundary (unknown 404, hidden 400), and audit
one `pc_monitoring_suppression_updated` Notice. Duration is fixed v1 policy —
1–60 minutes — and the row stores Admin's absolute RFC 3339 expiry, so a
restart can neither extend nor lose it; expired rows lazily stop matching.
The gate itself lives in one place inside the monitor's opening path, so every
current and future incident kind is blocked while a suppression is active,
an already-open episode stays open, and expiry or cancellation restarts every
kind's full grace instead of alerting immediately. The dashboard carries the
per-PC expiry, the Client's Owner Quit cannot create suppression (these routes
exist only behind an Owner Session on `:8080`), and the Client's quit guidance
directs owners to Web Admin before maintenance.

**Host Probe Coverage** (#343) closes the loop in the other direction: failed
probes only matter while the heartbeat independently proves the PC is running,
so a powered-off or Client-unreachable PC never opens the incident merely by
not answering ICMP. When probes fail continuously for two minutes on a
Client-reachable PC, one durable incident opens with its own Alert and
dashboard badge (a stored address Admin cannot parse as a probe target counts
as lost coverage too); repeated failures never duplicate it, Monitoring Suppression
gates it through the same shared opening path with a fresh full grace after
expiry, restarts mark it observation-interrupted, and one successful response
closes it as a recovery Notice (`probe_recovered`) while losing the heartbeat
closes it as unconfirmed. Web Admin copy states exactly what missing coverage
means: that PC is unmonitored — not offline and not tampered. Migration 0044
extends the incident table's close-reason CHECK for the new value.

**Client Recovery Escalation** (ADR-0077) is the first monitoring path that
changes anything on a PC. Two halves, and the smaller one carries most of the
value. Locally, the Client Guard's relaunch trigger is now level-triggered as
well as edge-triggered: it periodically checks that a verified Client holds the
pipe and relaunches if none does, which closes the case ADR-0048 named and left
open — a Client that never connected produces no disconnect, so nothing
edge-triggered ever fires. That needs no server at all.

Remotely, the Guard gains one outbound poll to `POST /api/guard/state` on
`:3210`, MAC-identified like the heartbeat, reversing ADR-0048's "no second
network client" on the single ground that no other channel to a Guard exists.
It opens no listener. It learns the Admin URL and its MAC from the Client over
the existing pipe and caches both under `%ProgramData%\OpenPiso`, so a
re-pointed Admin propagates with no reinstall. The poll reports Guard liveness,
whether a Client is connected, whether the Guard is disarmed, and how many
relaunch attempts the current absence has cost — the evidence ADR-0076 could
not get, because it learned Guard presence only from the heartbeat that a
mismatch is defined by missing. That same poll also carries the Guard's
Forwarded Diagnostics (ADR-0078): a bounded batch rides the request body the
Guard already makes — the service gains no second network call — so the Guard
can report, say, a rejected foreign Client image even when no Client ever
connected this boot, and an unprovisioned PC with no cached endpoint simply
forwards nothing.

Admin runs a second clock, independent of the 60-second mismatch grace, over
seven conditions: host reachable, no Client Reachability, Guard polling, no
connected Client, at least one failed relaunch, not suppressed, not disarmed.
Any one going false clears it. After the owner-set Escalation Delay (default
60s, floor 30s) the PC is marked for an **Escalated Power Off**: the Alert,
the mismatch closure as `escalated_power_off`, the Telegram item, the cooldown
stamp, and the session end all commit in one transaction, then the order is
armed for the next poll. Balance, Remaining Time, Guest Time, and Points are
untouched. The feature is off by default, PIN-gated, capped at one power off
per PC per hour, and its clock is in memory so an Admin restart restarts it.
A Guard that is dead or was never installed has no poll and so is never
escalated against; that stays a Client Guard Coverage problem for the owner.

**Client Guard Coverage** (#344) turns the reserved Guard fact into a live
diagnostic. Every Client Heartbeat now carries a typed `guard_coverage`
outcome: a release Windows Client reports whether it currently holds its
Client Guard pipe connection (`covered`/`unavailable`), while debug and
non-Windows builds report `not_applicable` — development can never create a
production protection Alert. An older Client that omits the field leaves the
PC's stored diagnostic unknown (`unsupported`), which can never open an
incident either. Admin stores the latest reported value on the PC row and the
continuous monitor treats two continuous minutes of `unavailable` on a
Client-reachable PC as its own durable incident — one Alert, dashboard badge,
and idempotent episode like every other kind, with Monitoring Suppression and
restart marking shared. One covered report closes it as `guard_recovered`;
losing the heartbeat closes it unconfirmed; `not_applicable` and unknown
states never accumulate grace and never fabricate recovery. The diagnostic is
evidence only: it grants no identity, session, money, time, Points, or routing
authority and stays separate from Client Reachability Mismatch. Web Admin
guidance points to Guard and installation checks on the PC — Admin detects
and never attempts automatic service repair. Migration 0045 adds the
`pcs.last_guard_coverage` column and migration 0046 extends the close-reason
CHECK with `guard_recovered`.

### Central Terminal & Web Admin (M3 — Customer terminal kiosk UI)

M3 turns `apps/admin/src` into the customer-facing Central Terminal
kiosk. The Tauri window is fullscreen, decoration-less, and
non-resizable; the capability grants only `core:default`,
`core:event:default`, and `core:window:default` — no shell, no
process, no filesystem. The backend authority is unchanged from M2:
all money/time mutations still flow through the existing
`record_coin_event` path with `pc_id = selection.pc_id` (ADR-0008).

The kiosk surface lives in `apps/admin/src/`:

- `lib/types.ts` adds `SelectionState` and `KioskStatus` so the
  Svelte components and the Rust commands share one wire format.
- `lib/api.ts` adds `listPcsKiosk`, `getKioskStatus`, `openSelection`,
  `closeSelection`, and `getSelection` — the only Tauri commands the
  kiosk calls.
- `App.svelte` is a single fullscreen component with three screens
  (plus **out of order**): the **PC grid** (live, all-PC status cards;
  the customer types a PC number on the physical numpad, the typed
  number is mirrored in an on-screen display, and `Enter` opens the
  Selection Window for the matching PC), **insert** (live inserted
  total, equivalent-time preview, inactivity / hard-cap meters that
  tick locally between server pushes, a context-sensitive `Enter`
  that cancels when no money is in and confirms when a coin has
  landed, and `Esc` as a release-and-return safety hatch), and
  **success** (auto-returning splash). The earlier four-screen flow
  included a confirm step between grid and insert; ADR-0025 removes
  it because the typed-number + explicit `Enter` is already a
  deliberate commit, and the cancel path now actually closes the
  Selection Window instead of leaving it locked until the
  inactivity timeout. The **out of order** screen renders when the
  M4 serial source reports an unhealthy state; the M2 simulator
  never enters it.

Live updates are wired through the existing Tauri events:

- `pc-updated` (already in M1) is the all-PC live stream; the kiosk
  mounts via `listPcsKiosk` and merges per-PC updates on top.
- `selection-state` (M2) drives the insert screen's countdown and
  inserted total.
- `kiosk-status` (M3) carries every coin-source health change so the
  kiosk can switch between the normal flow and "out of order"
  without polling. The M2 simulator publishes `available` once at
  startup; the M4 serial source is the only producer of
  `out_of_order`.
- `shop-rate-updated` carries the committed `KioskConfig` after an owner
  Shop Rate save. The kiosk subscribes before reading `getKioskConfig` on
  mount, so the read-only equivalent-time preview updates immediately and a
  save during boot is not lost. Its committed `Added` total remains the
  server's recorded value.

The kiosk never mutates `pcs.remaining_seconds`, account balances, or
any other authoritative state. The only mutation it triggers is
`openSelection` / `closeSelection`, both of which use the existing M2
singleton and the same `record_coin_event` path used by the simulator
command. The M2 set-pin fail-closed invariants are unchanged.

`audit_log` is the append-only event and reporting source of truth. Rust
mutation services append the audit event in the same SQLite transaction as a
successful mutation. Failures and denials use a separate append operation
because their attempted mutation is rolled back. SQLite triggers reject update
and delete operations against audit rows. M2 implements active account-to-PC
bindings as ephemeral Rust state. Account tokens are scoped to one binding and
become stale after takeover, auto-kick, expiry, logout, or an admin-server
restart.

M6 adds reporting views over `audit_log` and the audit review surface. Cash
Revenue (coins + sold vouchers) and Free Time Dispensed (promo voucher
redemptions, points redemptions, wheel winnings, manual admin add-time) are
read from typed views so the two never blend and always reconcile exactly with
canonical events (ADR-0014). Points Liability is a current snapshot of active
account balances. The admin dashboard reads reports, searches the audit log,
and exports CSV through typed Tauri commands; the CSV export always respects
the on-screen filters. Manual Guest Time adjustments are owner-only, PC-bound,
transactional, and audited; a negative delta is a compensating event, never an
audit-history edit (ADR-0015, ADR-0021). The audit viewer categorizes events
into Auth/Money/Time/Points/Admin/System groups and shows the actor, outcome,
and typed deltas for each row.

### Diagnostic logging (M7)

Diagnostic (developer) logging uses the `tracing` crate with rotating JSON
files in the admin's app data directory (~7-day retention). The audit viewer
has a "show developer logs" toggle that interleaves diagnostic entries with
audit rows by timestamp — one screen, two stores (ADR-0016). Diagnostic events
never enter `audit_log`.

The admin writes structured `tracing` events with optional `pc_id` and
`account_id` fields. Coin-path edges, batches, Set Pin transitions, port
health, inhibited pulses, and commit rejections use dedicated coin targets;
raw pin samples remain DEBUG. The owner-authenticated Coin Debug Bundle receives the current app-start
boundary from the owner server, covers only that run through generation time
(and at most the previous two hours), filters all evidence before computing
findings, caps the timeline at 50 physical coins and the complete UTF-8
Markdown document at approximately 80 KiB. Its coverage header separately
reports time-window, timeline, appendix, and byte truncation. It uses
successful Coin Batch closure evidence as one physical-coin timeline row,
groups commit retries by Coin Trace Id, joins persisted transactions by that
id, and structurally excludes secrets. The owner-authenticated **Acceptor
Health** contract separately reports the current **Coin Debug Window** start
and the S1 timing status `unavailable_until_filtered`; its **Implausible
Timing** rate is `null` until filtered timing exists in S2. It has no per-batch
unmapped-event list. The client writes to stdout and buffers WARN and ERROR
lines on the device, sending them to the admin's batched
`POST /api/forwarded-diagnostics` endpoint (ADR-0078) so they land in the
dedicated forwarded log for centralized visibility even after a diskless
reboot. See
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Diskless enforcement and expiry shutdown (M8)

Expiry powers a diskless PC **off** rather than locking it, so the reboot wipes
the previous customer's sessions for privacy (ADR-0005). The server stays
authoritative for the *decision*: a single pure function at the broadcast
chokepoint computes each connected PC's `power_state` (`active`, `warning`,
`shutdown`) from connection status and Remaining Time and streams it on
`pc.updated`. Any connected PC at zero, including a freshly booted zero-time PC,
runs the shutdown grace; a top-up cancels it immediately, and no per-PC
transition tracking is needed (ADR-0034).

The client renders a non-blocking low-time warning at `power_state = "warning"`
and a blocking 300-second default grace countdown at `"shutdown"` (ADR-0022). When the
grace elapses while shutdown is still commanded, the client's Rust process issues
the real OS shutdown; the authority is never exposed to Svelte, and debug builds
log instead of powering off. The client ignores any state whose `pc_id` is not
its own, so expiry cannot target the wrong PC.

Separately, a previously-connected Client that loses the admin server immediately
returns to the Fullscreen Lock and arms an **Offline Shutdown Countdown** using
cached Offline Grace (ADR-0045). Disconnected Clients cannot route to Widget or
Mini Bar. The absolute deadline is atomically persisted in the Client config
directory, so a relaunch resumes it; an expired deadline gets a 10-second
re-warning. The owner-unlocked settings panel pauses the countdown and closing
it extends and persists the deadline; the PIN prompt alone does not pause. A
successful reconnect cancels and deletes the offline deadline. If it elapses,
the same Rust-owned one-shot shutdown path is used, with release Windows-only
power-off and debug/non-Windows logging. Setup, connecting, unregistered, and
never-connected states do not arm. A persistent Windows watchdog baked into the
master image relaunches a killed client and forwards its events as diagnostics.
See [diskless.md](diskless.md) for deployment and recovery.

### Client window model and owner gate (Client Shell)

Before any of those views can render, the window spends the WebView2 cold start
and bundle parse with nothing painted — seconds on a cold-booted shop PC. The
window therefore declares a dark `backgroundColor`, and `index.html` carries an
inlined boot splash (no bundle, no webfont, no IPC) that `main.ts` removes once
the Svelte app mounts. Without both, that gap renders as a white fullscreen
flash on the customer's screen. `openpiso_client::startup` log milestones
measure the gap; see [installation.md](installation.md).

The Client is one Tauri window reconfigured into three logical views, driven by
a single source of truth in the client backend (`TimerState` +
`AccountAuthState`) plus the narrow frontend-only Guest authentication-route
intent (ADR-0029):

- **Fullscreen Lock** — borderless-maximized, undecorated, always-on-top. The
  genuinely blocking screen shown for authoritative return-to-Lock triggers:
  no usable Guest Time or Account Session, expiry, `overlay_locked`, or
  `power_state = "shutdown"`. Usable Guest Time normally auto-enters the
  persisted View Mode; Account authentication and the explicit Guest Log in
  route live on the Lock.
- **Widget** (320×480; was 200×300 before bug #12) and **Mini Bar** (100×30) —
  the in-session floats. The customer's
  choice between them is persisted as `ClientConfig.view_mode`; the
  Lock is never a stored value. The Mini Bar anchors to the top-center of the
  *current monitor's* work area, never a hard-coded pixel.

The geometry (sizes, decorations, always-on-top, monitor math) lives in Rust
(`window_mode.rs`); the frontend router decides *which* view belongs and calls
the matching `enter_*_mode` command, so the OS window and the mounted Svelte
view never disagree.

The seated customer is **untrusted** (ADR-0027). Accordingly: the Lock offers
no customer-reachable Quit and no minimize-to-tray, and it **re-asserts on focus
loss** — losing focus while on the Lock re-raises and re-focuses the window so an
Alt-Tab cannot yield an unmetered PC. Backend settings (server URL, PC ID, view
mode) are never rendered on the Lock directly; they sit behind an **Owner PIN
gate**. The PIN is a single shared secret stored *hashed* in the admin
`settings` table, set from web-admin, and verified **server-side**
(`POST /api/owner/verify-pin`) — the real PIN is never compared on the client
while the server is reachable. For the **offline fallback** (ADR-0027 §3) the
admin returns the strong Argon2 hash *only on a successful verify*, so it is
never handed to a caller who has not proven the PIN; the client caches it in
memory and verifies against it with Argon2 when the server is later unreachable
(it must have verified online once in the same run, since the cache is not
persisted). In the Widget, **Settings** is the fourth Account tab and second Guest tab.
It first renders the Owner PIN gate inline, then replaces it with the scrollable
backend Settings panel after verification. Leaving Settings for any customer
tab, minimizing, logging out, entering the Fullscreen Lock, destroying the
Widget, or restarting forgets the unlock; Cancel returns to the customer tab
that opened Settings. The owner exits via the PIN-gated **Quit** in that panel;
there is no customer-facing Quit.

The owner-editable **Shop Name** propagates from web-admin's
`settings.shop_name` write to two consumers. The Client reads it from the
public, read-only `GET /api/shop` (which returns `{ shop_name,
shutdown_grace_seconds, offline_grace_seconds, theme_version }` and nothing
secret) and caches the shop identity so the Lock header renders immediately.
The Central Terminal reads it in-process through `get_shop_name` before mount;
the admin event hub pushes the committed value on `shop-name-updated` after an
owner save. The Client also persists the last-seen `offline_grace_seconds` in
its local config for use during an admin server outage. OS-level kiosk hardening (Windows
Assigned Access / Shell Launcher v2) is provisioning, documented in
[installation.md](installation.md), not an app capability (ADR-0028).

**Widget overlay constraint.** Informational content within the Widget,
including PIN-gated Settings, renders inline, never as a `fixed` overlay.
Transactional dialogs (Claim confirmation, Time Transfer, Points Transfer/code,
and Password Change) retain `fixed inset-0` because they intentionally occlude
the Widget for a focused task. All `fixed` overlays in the Widget view tree must
go through `WidgetOverlay`, the single abstraction point for overlay styling.

### Centralized theme distribution (ADR-0035)

The shop theme (one accent color + per-surface light/dark modes) is a raw
record in the `theme_settings` singleton table; all color math lives in the
pure TypeScript ramp generator in `packages/ui` (`generateTheme`). Each app
injects the generated CSS as a single swapped `<style id="openpiso-theme">`
element overriding the static `theme.css`, drives the `dark` class on `<html>`
from its surface's central mode, and caches the raw record in localStorage
(`openpiso.theme-record`) so a boot with the server unreachable keeps the
last-applied theme.

Per-surface propagation: **web-admin** applies the cached record before mount,
runs one root-layout poller (~15 s) that re-applies when the fetched `version`
differs, and the tab that PUTs applies immediately from its own response; the
browser-local ThemeToggle is deleted and the legacy `openpiso.theme` key is
removed at boot. **Client** Rust compares each heartbeat's `theme_version`,
fetches the public `GET :3210/api/theme` on connect or version change, then
emits the full record to its webview as a `theme-updated` Tauri event. **Kiosk**
loads SQLite authority through the typed `get_theme` Tauri command at startup;
a successful owner `PUT :8080/api/theme` also pushes the committed record
straight to its window as `theme-updated`, without polling. See ADR-0035.
