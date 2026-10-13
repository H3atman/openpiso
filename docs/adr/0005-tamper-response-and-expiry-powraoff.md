# ADR-0005: Client tamper response & expiry power-off (diskless)

- Status: Accepted
- Date: 2026-06-21
- Deciders: shop owner (product)
- Supersedes: the MVP "do not build OS-hardening / invasive anti-cheat"
  constraint, **for the expansion phase only and only as scoped below.**
- Superseded in part by [ADR-0034](0034-connected-zero-time-shutdown-and-channel-consistency.md):
  the boot-loop guard (only command shutdown on an observed positive→zero
  transition) is dropped — **any** connected PC at zero now runs the grace
  countdown and powers off, including a fresh boot. The expiry power-off decision
  itself stands.

## Context

The MVP is shipped and in production. This ADR is part of the **expansion
phase**, which deliberately relaxes the MVP rule against OS-hardening.

Two concerns from the original expansion plan: a customer killing the client app to get free
time, and "PC Auto-Shutdown with solid anti-kill." Grilling established:

- **The money is already safe.** The authoritative timer runs on the server
  (ADR-0001). Killing the client does not stop the customer's time from
  counting down server-side; it just freezes their own screen while their paid
  time burns, and shows the PC going dark on the dashboard.
- "Solid anti-kill" of a user-space app on a machine the customer physically
  controls is impossible by itself. But the shop is **diskless**: PCs boot a
  RAM-backed image from a server and discard all changes on reboot.

## Decision

### Tamper response (client killed)
- Ship a **persistent client-side watchdog** (Windows service / scheduled
  task), **baked into the diskless master image**. If the client app is killed,
  the watchdog relaunches it.
- The watchdog regress (customer kills the watchdog too) is bounded by the
  diskless reset: any tampering is **wiped on the next reboot**. The arms race
  ends at the power button, which the owner controls. This is accepted as
  "good enough," not absolute.
- Because money is server-authoritative, the restart is about **friction and
  UX**, not theft prevention.

### Expiry (time hits 0)
- At 0, the **server commands the PC to power off** (real OS shutdown), not a
  lock overlay.
- **Justification (diskless):** power-off → reboot = clean slate that wipes the
  previous customer's browser logins, social/email sessions, and personal data.
  A lock overlay leaves the OS running and the prior session accessible behind
  the lock — an unacceptable privacy risk in a shared diskless shop. This is a
  requirement a lock cannot meet.

### Accepted scope expansions (beyond MVP)
- The client gains **OS shutdown privilege**, and a **persistent service/
  scheduled task** is installed via the master image. Both are intentional,
  documented departures from the minimal-capability MVP, justified by the
  diskless privacy requirement and tamper-resistance.

## Accepted trade-offs

- The next customer waits for a cold boot (acceptable; diskless boot is fast
  and the privacy reset is worth it).
- Topping up requires the PC to be powered back on and the customer to log in
  again (acceptable in a diskless shop).

## Consequences / still open

- A **grace period / warning** before power-off (e.g. a countdown overlay so
  customers can save work and top up) is recommended but not yet decided.
- When the watchdog restarts a killed client mid-session, the server session
  is still live and the account is still burning. Whether the restarted client
  **auto-resumes** that PC's active session or **forces re-login** (costing the
  customer seconds while they retype credentials) is not yet decided — see next
  ADR.
- Exact shutdown mechanism (watchdog vs client issuing the OS shutdown) is an
  implementation detail.
