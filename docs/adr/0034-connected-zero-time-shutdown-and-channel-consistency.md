# ADR-0034: Connected zero-time PCs run the grace countdown; consistent power-state delivery

- Status: Accepted
- Date: 2026-06-30
- Deciders: shop owner (product)
- Supersedes: the "only an observed positive→zero transition commands shutdown"
  rule in [ADR-0022](0022-expiry-grace-period.md) and the boot-loop guard framing
  in [ADR-0005](0005-tamper-response-and-expiry-powraoff.md)

## Context

ADR-0005 powers a diskless PC off at expiry; ADR-0022 added a non-blocking
warning then a blocking grace countdown, with `power_state` (`active` /
`warning` / `shutdown`) streamed on `pc.updated`. To avoid a diskless boot loop,
the server commanded `shutdown` **only on an observed `> 0 → 0` transition**: a
PC that connected already at zero (a fresh boot that never held paid time) was
left `active`.

Two problems surfaced:

1. **Wrong default surface.** A configured, connected PC with zero time — the
   common case of a fresh boot at the start of a rental — fell through the
   client's `selectLockSurface` to the **INSERT COIN** surface instead of the
   **shutdown countdown**. The owner wants such a PC to show "TIME'S UP /
   shutting down in N s" and to actually power off after the grace window, so the
   machine cannot be used for free; the customer tops up (coins at the terminal,
   or logging into an account that has time) to cancel it. The grace window gives
   them ample time to do so.

2. **A latent delivery bug made the countdown unreliable even when it did
   appear.** The client runs *two* concurrent channels into `apply_server_state`
   — a 5-second HTTP heartbeat **and** an SSE stream — both of which call
   `manage_shutdown_deadline`, which cancels the grace deadline whenever
   `power_state != "shutdown"`. `power_state` was computed only inside the SSE
   broadcast; the heartbeat HTTP response returned the un-stamped `Pc` (default
   `power_state = active`). So every 5 seconds the heartbeat's `active` fought the
   SSE's `shutdown`, re-arming the grace deadline. On a release build the
   countdown could never complete (the existing expiry power-off was affected
   too); debug builds never power off, so it went unnoticed.

The boot-loop concern that motivated the positive→zero rule is weaker than it
looked: a plain `shutdown /s` leaves the machine **off** with no auto-wake, so
there is no literal infinite loop. The real cost of removing the guard is that an
idle or being-prepared PC left connected at zero powers off after the grace
window — acceptable, since that is exactly the desired behavior for a rentable
PC, and the owner can power it back on.

## Decision

### Power state is a pure function of connection status and Remaining Time

Replace the stateful `ShutdownCoordinator` (per-PC `HashMap` tracking the last
observed time) with a pure free function:

```
power_state(status, remaining):
    offline            -> active   (nothing connected to act on the command)
    remaining <= 0     -> shutdown
    remaining <= 120 s -> warning
    otherwise          -> active
```

Any **connected** PC (`status != "offline"`) at zero Remaining Time is commanded
to shut down — including a fresh boot that never held positive time. There is no
longer a "freshly booted at zero is left alone" exception.

### One chokepoint, consistent across every delivery channel

`EventHub::publish_pc_updated` stamps the authoritative `power_state` and now
**returns** the stamped `Pc`. Every channel reuses that one value:

- the **heartbeat** HTTP response returns the stamped `Pc` (was: returned the
  un-stamped clone);
- the **disconnect** HTTP response likewise;
- the **SSE initial snapshot** is stamped with the same function before the first
  send, so a reconnect at zero shows the countdown immediately.

The heartbeat and the SSE stream can no longer disagree, so the heartbeat can no
longer reset the client's grace deadline.

### Client is unchanged

`selectLockSurface` already renders the shutdown surface (with the auth form /
account panel still available, so a top-up can cancel) whenever
`power_state === "shutdown"`. The `insert_coin` branch remains as a defensive
fallback. The whole change is server-side: emit `shutdown` for connected
zero-time PCs and deliver it consistently.

## Accepted trade-offs

- An idle or being-prepared PC left connected at zero will run the grace
  countdown and power off. This is intended; the owner powers it back on (and can
  preload Guest Time via the Central Terminal for an offline PC, ADR-0023).
- Removing the per-PC tracking removes the only in-memory shutdown state; the
  decision is now recomputed from authoritative inputs on every publish, which is
  simpler and immune to the stale-clone class of bug above.

## Consequences

- `power_state` delivery is consistent across heartbeat, disconnect, and SSE;
  the grace countdown and the real power-off (release builds) now complete.
- No schema migration; `power_state` is still derived in-memory.
- ADR-0005's boot-loop guard and ADR-0022's positive→zero transition rule are
  superseded by this ADR; the glossary and `docs/diskless.md` are updated to
  match.
