# ADR-0027: Untrusted-customer Client lock and owner PIN gate

- Status: Accepted
- Date: 2026-06-28
- Deciders: shop owner (product)
- Refines: ADR-0005 (tamper response and expiry power-off), ADR-0022 (expiry
  grace period), ADR-0024 (web-admin as the sole owner surface).

## Context

The redesigned Client
introduces a three-format window model whose first format is a **Fullscreen
Lock** — a blocking screen shown whenever the customer must not use the PC
(logged-out / expired / out of time / commanded shutdown). The implementation
plan also proposed a system tray with "minimize-to-tray on close" and a
customer-visible "Quit" item.

That combination is contradictory for a Pisonet shop. The person seated at a
Client is the **customer** — the untrusted party. If the customer can minimize
the Lock to the tray, or right-click the tray and Quit, they obtain a free,
unmetered PC, defeating the core business function.

At the same time, AGENTS.md forbids "invasive OS-hardening / anti-cheat" in the
MVP, so we cannot kiosk-lock the operating system itself. And the **owner** (the
trusted party) still legitimately needs to exit and reconfigure the Client for
maintenance — including when the admin server is unreachable.

## Decision

### 1. The customer is untrusted; the Lock genuinely blocks

While the Client is in Fullscreen Lock state:

- The window does **not** minimize to the tray and offers **no
  customer-reachable Quit**.
- Close / Alt-F4 is suppressed or re-asserts the Lock rather than exiting.
- There is **no system tray** (see ADR-0028: under Shell Launcher the Windows
  shell is replaced, so no notification area exists; and the tray's
  customer-facing items are forbidden anyway). Owner actions live behind the
  PIN-gated settings panel, not a tray. Process survival across crashes is the
  watchdog's job (ADR-0005), not a tray.

Originally we accepted that, per the no-OS-hardening rule, a customer with
OS-level access (Task Manager, etc.) could still kill the process, and that *our
own UI* merely never hands the customer an exit. **Superseded by ADR-0028:** the
owner subsequently approved kiosk OS-hardening (Windows Assigned Access / Shell
Launcher v2 + app-level soft hardening) to close that gap. This ADR's decisions
(no customer Quit, no minimize-to-tray, PIN-gated owner access) still stand; only
the "accepted OS-chrome escape gap" is now closed by ADR-0028.

### 2. Owner access is gated by a server-sourced Owner PIN

A single **Owner PIN**, shared across all PCs, gates the Client's backend
settings (server URL, PC ID, View Mode) and Quit. There is **no** separate
owner/admin account role; customer account login on the Client remains purely
the customer's session.

- The PIN is stored **hashed** in the admin `settings` table and set/changed via
  the owner-only `/web-admin/settings`.
- Verification is **server-side**: the Client POSTs the entered PIN to an admin
  endpoint, which compares against the stored hash. The real PIN/hash is not
  shipped to the untrusted machine for the live check.

### 3. Offline fallback via a cached verifier

On each successful server contact the Client caches the `owner_pin` **hash**
locally. When the admin server is unreachable, the owner can still unlock
backend settings / Quit by verifying against the cached hash. A strong hash
keeps the locally cached value low-risk on a machine the owner physically
controls.

### 4. First-configuration bootstrap exception

A never-configured Client has neither an admin server connection nor a cached
Owner PIN verifier. While `configured === false`, the Fullscreen Lock therefore
allows access only to the admin server URL needed to establish that first
connection, without an Owner PIN. This exception ends after the initial Client
configuration is persisted. All later backend settings access, including server
URL changes, View Mode, and Quit, requires the Owner PIN gate.

## Considered options

- **Trusted-context client (minimize-to-tray + Quit for anyone).** Rejected:
  the seated party is the untrusted customer; this is a free-PC exploit.
- **Local-only PIN per Client (`ClientConfig`).** Rejected in favour of a
  server-sourced shared PIN so the owner manages one secret for the whole shop.
- **Owner authenticates against the admin `/web-admin` session instead of a
  PIN.** Rejected for MVP: heavier, and fails closed exactly when the server is
  down and the owner needs in.
- **Strict server-only verification, no offline unlock.** Rejected: would lock
  the owner out during the network/server troubleshooting that most needs an
  exit. The cached-hash fallback (decision 3) was chosen instead.
- **Require an Owner PIN before first configuration.** Rejected: an
  unconfigured Client has no server or cached verifier against which to check
  it. A separate bootstrap secret or mandatory image pre-provisioning adds
  another authority concept solely to solve this one-time dependency cycle.
- **OS-only exit (no in-app owner door).** Rejected: brittle and relies on the
  OS access we otherwise treat as out of scope.

## Consequences

- A numeric PIN is inherently weak / brute-forceable over the LAN. We accept
  this for MVP because the PIN gates a maintenance convenience, not the
  money/time authority boundary (which stays server-authoritative per AGENTS.md).
  The verify endpoint should stay minimal and may warrant rate-limiting later.
- A future reader must not reintroduce a customer-reachable Quit or
  minimize-to-tray while the Client is in Lock state.
- A future reader must not ship the real PIN or a fast/weak hash to the Client
  for local comparison; the live check is server-side and only a strong hash is
  cached for offline fallback.
- New admin work required: an `owner_pin` setting (hashed), a PIN-verify
  endpoint, and web-admin PIN editing alongside `shop_name`.
- The first-configuration exception must remain narrow: it exposes only the
  server URL connection step and must not expose View Mode or Quit.
