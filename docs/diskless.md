# Diskless Deployment, Expiry Shutdown, and Recovery

This document covers the M8 diskless enforcement model: the expiry power-off,
the client watchdog, and the diskless master-image setup, recovery, and security
notes. It implements [ADR-0005](adr/0005-tamper-response-and-expiry-powraoff.md),
[ADR-0022](adr/0022-expiry-grace-period.md),
[ADR-0034](adr/0034-connected-zero-time-shutdown-and-channel-consistency.md),
and [ADR-0045](adr/0045-offline-shutdown-countdown.md).

## Why diskless

Each rentable PC boots a RAM-backed OS image from a server and **discards all
changes on reboot**. A power-cycle is therefore the privacy reset: it wipes the
previous customer's browser logins, social/email sessions, and personal data.
This is the property a lock overlay cannot provide, and it is why expiry powers
the PC **off** instead of locking the screen.

The money is always safe regardless of the client: the authoritative timer runs
on the admin server (ADR-0001). Killing the client only freezes the customer's
own screen while their paid time keeps burning server-side.

## Expiry shutdown flow

Authority for the *decision* stays on the server; the client only runs the grace
countdown and issues the OS shutdown.

1. **Warning** — while `0 < Remaining Time ≤ 120 s`, the server streams
   `power_state = "warning"` on `pc.updated`. The client shows a non-blocking
   low-time banner. Nothing is locked.
2. **Grace** — whenever a *connected* PC's authoritative Remaining Time is at
   zero, the server commands `power_state = "shutdown"`. The client shows a
   blocking full-screen countdown for a fixed **30-second** grace window. A
   top-up that raises Remaining Time above zero cancels the shutdown (the server
   flips `power_state` back to `active`/`warning`).
3. **Power-off** — if the grace window elapses while the server still commands
   shutdown, the client process issues a real Windows shutdown
   (`shutdown /s /t 0`).

`power_state` is a pure function of the PC's connection status and Remaining Time
(ADR-0034): **any** connected PC at zero is commanded to shut down — including a
fresh boot that never held paid time. (The earlier rule commanded shutdown only
on an observed positive→zero transition, to dodge a boot loop; that exception is
dropped. A plain `shutdown /s` leaves the machine off with no auto-wake, so there
is no literal loop — an idle or being-prepared PC left connected at zero simply
powers off after the grace window, which is the intended behavior; the owner
powers it back on, or preloads Guest Time at the Central Terminal.) An `offline`
PC is never commanded — there is nothing connected to act on it.

The command is stamped once at the broadcast chokepoint and reused by every
delivery channel — the heartbeat HTTP response, the disconnect response, and the
SSE initial snapshot and updates — so the 5-second heartbeat can never disagree
with the SSE stream and reset the client's grace deadline (ADR-0034).

A restarted client (after a crash, kill, or watchdog relaunch) shows the generic
login screen while the live server session keeps burning; it does not auto-resume
the account (ADR-0006, ADR-0007).

## Offline Shutdown Countdown

When a previously-connected Client loses the admin server, it immediately
leaves the Widget or Mini Bar and shows the Fullscreen Lock's **Offline Shutdown
Countdown**. The cached **Offline Grace** duration is used because the server
cannot be queried. The Lock explains that the server is unreachable and keeps
the PIN-gated Settings entry available for owner recovery.

A successful heartbeat or SSE response cancels the offline deadline, deletes
its persisted copy, and restores normal server-driven routing. Setup, connecting,
unregistered, wrong-URL, and never-connected states do not arm it, and a
disconnected Client is never routable to a customer session surface. The
absolute wall-clock deadline is atomically persisted in the Client config
directory, so a relaunch resumes it; an already-expired deadline gets a
10-second re-warning. The countdown pauses only while the owner-PIN-unlocked
settings panel is open, and closing it extends and persists the deadline. The
PIN prompt alone does not pause it.

If Offline Grace elapses while the Client remains disconnected, Rust invokes the
same one-shot OS shutdown path as expiry. Release Windows builds power off;
debug and non-Windows builds only log the intent. See [ADR-0045](adr/0045-offline-shutdown-countdown.md).

### Build profiles and safety

Shutdown authority lives in the client's Rust process and is never exposed to the
Svelte frontend. To protect developers, **debug builds do not power off**: they
log the intent and forward a diagnostic. Only **release builds** (the diskless
image) perform the real shutdown. Non-Windows builds also only log.

## Client watchdog

`scripts/windows/openpiso-watchdog.ps1` is a persistent per-logon Scheduled Task
baked into the master image. It relaunches the client if a customer kills it and
forwards watchdog events (relaunch, failure) to the admin
`POST /api/forwarded-diagnostics` endpoint so they survive the diskless reboot
(ADR-0016, ADR-0078).

The watchdog is **not** a security boundary. If a customer also kills the
watchdog, the diskless reboot undoes it. The arms race ends at the power button,
which the owner controls. This is accepted as "good enough."

## Master-image setup

Perform these steps once while preparing the master image, then seal it:

1. Install the release build of the OpenPiso client (e.g. into
   `C:\Program Files\OpenPiso Client\`).
2. Copy `openpiso-watchdog.ps1` next to the client executable.
3. Configure the client's admin server URL and `pc_id`. The `pc_id` must match a
   row in the admin `pcs` table; **each PC needs a distinct id** so expiry cannot
   target the wrong machine.
4. In an elevated PowerShell, register the watchdog task:

   ```powershell
   .\scripts\windows\install-watchdog.ps1 `
     -WatchdogScript "C:\Program Files\OpenPiso Client\openpiso-watchdog.ps1" `
     -ServerUrl "http://192.168.1.10:3210" -PcId "pc-01"
   ```

5. Grant the kiosk user the right to shut down the machine (default on Windows
   client SKUs). No other elevated privilege is required.
6. Seal/snapshot the image. Every diskless boot now starts the client and the
   watchdog automatically.

## Recovery

- **A PC will not power off at expiry.** Check the admin diagnostic viewer for a
  forwarded `OS shutdown command failed` event. Confirm the kiosk user can shut
  down the machine and that a release (not debug) build is deployed. The owner
  can always power the PC off manually.
- **A PC powers off unexpectedly while idle or being prepared.** Expected: a
  connected PC at zero Remaining Time runs the grace countdown and powers off
  (ADR-0034). A plain `shutdown /s` leaves it off — there is no auto-wake loop. To
  keep a PC up without a customer, preload Guest Time at the Central Terminal
  (ADR-0023) or leave it powered off until needed.
- **The client keeps disappearing.** The watchdog relaunches it within a few
  seconds; repeated relaunches appear as forwarded diagnostics. Persistent
  failure usually means the client path in the watchdog task is wrong.
- **Full reset.** Reboot the PC: the diskless image discards all local changes
  and returns to the clean sealed state.

## Security notes

- Expiry cannot shut down the wrong PC: control flows over the PC-scoped SSE
  stream and the client ignores any state whose `pc_id` is not its own.
- Shutdown authority is in Rust, not Svelte; the client window keeps the minimal
  `core:default` capability and gains no shell or broad filesystem permission.
- The watchdog and OS-shutdown privilege are the only intentional departures from
  the minimal-capability MVP, justified by the diskless privacy requirement and
  documented in ADR-0005.
- Keep the admin LAN private. Do not expose admin mutation endpoints to the
  public internet.
