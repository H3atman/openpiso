# ADR-0089: Client UI state machines with pure reducers

- Status: Accepted
- Date: 2026-10-18
- Amended: 2026-10-20 — opt-in Web Admin adoption for Login and Manual Adjustment.
- Deciders: shop owner (product)
- Relates to: [ADR-0005](0005-tamper-response-and-expiry-powraoff.md), [ADR-0088](0088-spin-password-confirmation.md)

## Context

`apps/client` UI flows are driven by ad-hoc `$state` booleans and request counters that guard against stale responses. The same logic is duplicated between `App.svelte` and the Widget, and the combinations of flags allow impossible states.

## Decision

Every new or edited UI flow in `apps/client` is an explicit state machine:

- A discriminated-union `State` type and a discriminated-union `Event` type.
- A pure `reduce(state, event): State` using an exhaustive `switch` with a `never` check. The reducer does no I/O.
- Side effects (Tauri `invoke` and `api.ts` calls, timers) live in a thin runner: a Svelte `$effect` or function that owns a `$state` holding the machine state, reacts to it, and dispatches result events.
- The machine lives in plain `.ts` (for example `src/lib/machines/<flow>.ts`) so it is testable and shared across hosts.

Machines are hand-rolled. The first one is the spin flow, `src/lib/machines/spin.ts`, shared by FullscreenLock and Widget.

### Rejected

- **XState or similar**: a production dependency needing approval, heavier than needed.
- **Elm-style `[state, effects]` reducers**: a pure `State` reducer is simpler.
- **Ad-hoc `$state` booleans and request counters**: the current duplication, stale-response counters, and impossible states.

## Consequences

Scope is `apps/client` only for now. Existing flows migrate when touched; a dedicated follow-up branch converts the rest. Flows gain a little boilerplate (types and a reducer) in exchange for impossible states being unrepresentable and the logic being shared.

## Amendment — 2026-10-20: opt-in Web Admin flows

Web Admin adopts the same pure-reducer State/Event pattern for Login and Manual Adjustment,
with machines under `apps/web-admin/src/lib/machines/` and effects in their components.
Adoption is opt-in per flow, not a blanket Web Admin migration; the Client requirement is unchanged.
