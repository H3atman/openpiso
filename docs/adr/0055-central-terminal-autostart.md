# ADR-0055: Central Terminal Autostart — registry-only truth and single instance

- Status: Accepted
- Date: 2026-07-30
- Deciders: shop owner (product)
- Relates to: [ADR-0005](0005-tamper-response-and-expiry-powraoff.md) (watchdog
  and tamper response), [ADR-0015](0015-audit-log.md) (owner actions),
  [ADR-0024](0024-web-admin-sole-owner-surface.md) (owner surface),
  [ADR-0028](0028-client-kiosk-os-hardening.md) (OS-level provisioning),
  [ADR-0044](0044-immediate-write-transactions-and-pc-removal.md) (write
  contention), [ADR-0048](0048-client-guard-tamper-kill.md) (Client boot
  persistence)

## Context

The Client already has boot persistence: the Client Guard is a SYSTEM service
registered `start= auto` by the Client installer (ADR-0048), layered on the
Assigned Access / Shell Launcher provisioning of ADR-0028. The **Central
Terminal** has nothing equivalent. It owns SQLite, the timers, the coin
pipeline, `:3210`, and `:8080`, so a server PC that reboots without someone
manually launching it leaves the whole shop dead — no coins, no client
heartbeats, no owner surface.

The owner needs a switch for this, and the owner's only surface is the web
admin (ADR-0024). That is awkward in one specific way: every other owner
setting is shop domain state in SQLite, whereas this one is **operating-system
state on the Admin host**. The registry can be edited, wiped by reimaging, or
removed by another tool without OpenPiso knowing.

Turning the Central Terminal into something that launches by itself also makes
a second instance much more likely — the boot-launched copy plus an owner
double-clicking a shortcut. The admin process has no single-instance guard
today. SQLite in WAL mode will happily give the second process a connection, so
two timer loops would tick the same balances, while the loser of the `:3210`
bind race half-runs. That is a money-and-time correctness hazard, not a
cosmetic one.

## Decision

### Autostart is a logon-scoped, owner-controlled toggle

**Autostart** is an owner-controlled setting that makes the Central Terminal
launch automatically on the Admin host. It is implemented with
`tauri-plugin-autostart`, which on Windows writes the current executable path
to the per-user `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` key. It
therefore needs no elevation and no service.

Autostart is **logon-scoped, not boot-scoped**. The Central Terminal is an
interactive kiosk window and cannot exist before a user session does. Starting
unattended after a power cycle additionally requires Windows autologon on the
Admin host. OpenPiso documents that prerequisite and surfaces it as an inline
advisory; it never configures autologon itself, because doing so requires
elevation and storing a Windows account password in the registry.

Autostart ships **off**. Absence of the registry entry is the off state.

### The registry is the source of truth

There is no `settings` row and no SQLite mirror for Autostart. `GET` reads the
live registry state through the plugin; `PUT` writes it. The owner therefore
always sees real host state, and OpenPiso never claims a persistence guarantee
that the machine no longer has.

This is a deliberate, narrow exception to "SQLite is the source of truth". The
authority OpenPiso owns is money, time, points, sessions, and routing; a Run
key is not one of those, and the operating system is the only component that
can honour or forget it. A stored intent would only manufacture a second value
that can disagree with the machine.

### The API is its own endpoint

`GET`/`PUT /web-admin/autostart` return
`{ enabled, supported, editable, exe_path }`. `supported` is false where the
platform has no Autostart implementation; `editable` is false in debug builds;
`exe_path` is the executable that is (or would be) registered. The endpoint is
separate from the shop settings endpoints so a host-level read that can fail
never breaks an unrelated settings load.

Debug builds are **read-only** by design. `auto-launch` registers
`current_exe()`, and a development build would otherwise leave a Run key
pointing into a build tree that later moves or is deleted.

### Every change is audited, including failures

Flipping Autostart appends an audit event with the web-admin owner as actor and
`{ enabled, exe_path }` metadata, on success **and** on failure. Boot
persistence for the machine that owns the money is tamper-relevant, so a
repeatedly failing write must leave a trace rather than only a toast the owner
has already dismissed.

### Single instance is a precondition, not a follow-up

`tauri-plugin-single-instance` is added in the same change. A second launch
focuses the existing kiosk window and exits. This ships together with Autostart
because Autostart is what makes the collision likely, and the failure mode is
duplicated timer loops mutating balances.

### The host boundary is a port

The Autostart capability is expressed as a small trait in the admin crate. The
production implementation wraps the plugin's manager; tests use an in-memory
fake. The axum handler depends on the trait, not on a Tauri `AppHandle`, so the
HTTP behaviour — including the audited failure path — is testable on any
platform without touching a real registry.

## Considered options

### Store the owner's intent in SQLite and reconcile at every launch

Rejected. It self-heals external removal, but only at the next launch — and the
launch is exactly what fails when the entry is missing. Between boots the
stored intent and the machine can disagree, and the owner would be shown the
intent rather than the truth.

### Mirror the flag in SQLite without reconciling

Rejected outright. It reads back the value OpenPiso wrote, so it silently lies
whenever anything else removed the registry entry.

### A Scheduled Task registered `-AtLogOn`, like the diskless watchdog

Rejected for this change. It matches existing prior art
(`scripts/windows/install-watchdog.ps1`) and adds restart-on-failure, but
registering it needs elevation, and the admin process runs unelevated. It also
still fires at logon, so it does not actually close the boot gap it appears to.

### A Windows service, like the Client Guard

Rejected. The Central Terminal is an interactive fullscreen kiosk window; a
SYSTEM service has no interactive session to draw into. The Guard's service
model works precisely because the Guard has no UI.

### Write the Run key directly instead of adding the plugin

Rejected. The direct write is small, but the plugin is maintained, handles
executable path and argument quoting, and keeps the platform-specific detail
out of OpenPiso.

### Have the toggle configure Windows autologon too

Rejected as a security regression. It requires elevation and persists a Windows
account password in the registry, on the machine that holds the shop's money.

### Enable Autostart automatically on first launch

Rejected. It needs a first-run marker in SQLite — reintroducing exactly the
stored state this ADR removes — and silently installs persistence the owner did
not ask for.

### Defer single instance to its own issue

Rejected. Shipping Autostart first would knowingly ship the window in which two
processes tick the same balances.

## Consequences

- The glossary term **Autostart** is authoritative: owner-controlled Windows
  logon persistence for the **Central Terminal**. It is distinct from the
  **Watchdog** and the **Client Guard**, which are Client-side and relaunch a
  killed process rather than start one at logon.
- The admin app gains two production dependencies (`tauri-plugin-autostart`,
  `tauri-plugin-single-instance`) and the corresponding capability entries.
- Owner-visible truth about Autostart is host state, so it can change without
  an OpenPiso action; the UI reads it fresh rather than trusting a cached value.
- Unattended recovery after a power cycle depends on Windows autologon, which
  remains an owner provisioning step documented in `installation.md`.
- A development machine can never acquire a stale Run entry, at the cost of the
  toggle being read-only in debug builds.
- A second Central Terminal launch is now inert, removing the duplicated-timer
  hazard for manual launches as well as autostarted ones.
- The admin app still has no installer (`bundle.active` remains `false`), so
  the registered path is wherever the executable currently lives. Adding an
  admin installer is out of scope here and stays a separate decision.
