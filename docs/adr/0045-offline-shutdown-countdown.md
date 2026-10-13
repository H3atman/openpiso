# ADR-0045: Offline Shutdown Countdown returns the Client to the Lock

- Status: Accepted
- Date: 2026-07-25
- Deciders: shop owner (product)
- Supersedes in part: [ADR-0034](0034-connected-zero-time-shutdown-and-channel-consistency.md)'s client-side `offline → active` stance
- Relates to: [ADR-0022](0022-expiry-grace-period.md), [ADR-0027](0027-client-lock-and-owner-pin-gate.md), [ADR-0029](0029-client-window-model.md)

## Context

ADR-0029 deliberately kept a Client in the Widget or Mini Bar during a brief
network blip when local Remaining Time was available. That made an admin outage
look like a paused timer: the customer could continue using the PC while the
admin server was unreachable. ADR-0034 made the server's expiry `power_state`
authoritative, but the server cannot issue a new command after it becomes
unreachable.

## Decision

A Client arms an **Offline Shutdown Countdown** only on a
`connected → disconnected` transition during the current app run. It uses the
cached owner-configured **Offline Grace** duration from `ClientConfig` and
immediately returns to the **Fullscreen Lock**. The Lock explains that the
admin server is unreachable and keeps the existing PIN-gated Settings entry
available so the owner can repair the connection.

A successful connected heartbeat or SSE response cancels the countdown, deletes
its persisted deadline, and restores normal server-driven routing. A disconnected
Client is never routable to the Widget or Mini Bar, regardless of locally
displayed Remaining Time. Setup, connecting, unregistered, wrong-URL, and
never-connected states do not arm a countdown and retain their existing Lock
surfaces.

When the countdown arms, the Client atomically writes an absolute wall-clock
deadline beside `client-config.json` in its config directory. A relaunch resumes
the remaining duration while offline. If the deadline passed while the app was
not running, the relaunch replaces it with a 10-second re-warning before
shutdown. The persisted deadline is never acted on while the Client is connected.

The countdown pauses only while the owner-PIN-unlocked settings panel is open;
opening the PIN prompt does not pause it. Closing the unlocked panel extends the
deadline by the pause duration and atomically rewrites the persisted deadline. If
the owner reconnects instead, reconnect cancellation wins and removes the
deadline — it cancels the countdown only and never revokes the owner PIN unlock,
which would otherwise expire mid-session on the next 5-second heartbeat. If the
countdown reaches zero while the Client remains disconnected,
the Client reuses the existing Rust-owned OS shutdown path — forced, and
issued once per attempt rather than once per lifetime, so a power-off that left
the machine running is retried (ADR-0022). Release Windows builds power off;
debug and non-Windows builds only log the intent.

Arming, cancellation, resume, expiry-on-launch, and pause decisions are pure
functions. Async connection, timer, and settings tasks only apply those
decisions and update the Rust-owned timer state; Svelte never receives shutdown
authority.

## Consequences

- An admin outage no longer leaves an occupied PC usable in the Widget or Mini Bar.
- A transient outage can be recovered by reconnecting before Offline Grace ends.
- A Client launched before its first successful connection is not put into an
  unexplained shutdown race.
- A Client killed or restarted resumes the persisted deadline; a deadline that
  expired while it was down gets a 10-second re-warning. Diskless reboot still
  naturally clears local state after the client shuts down.
- Owner-unlocked settings provide an intervention window without allowing the
  customer-facing PIN prompt to pause shutdown.
- Offline shutdown and expiry Shutdown Grace remain separate deadlines and
  surfaces, with distinct cached/configured durations.

## Amendment (2026-09-23)

Onboarding defaults are now 300 s Shutdown Grace and 600 s Offline Grace.
Migration 0054 bumps only untouched old defaults; customized values remain.
