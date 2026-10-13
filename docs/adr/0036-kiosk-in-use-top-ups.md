# ADR-0036: Kiosk availability includes in-use PCs

- Status: Accepted
- Date: 2026-07-17
- Deciders: shop owner (product)
- Supersedes: ADR-0023's offline-selection rule and ADR-0026's online-only availability rule.

## Context

The Central Terminal previously treated only `online` PCs as selectable. That
blocked customers from adding money when a PC already had Guest Time or an
Active Session, despite ADR-0008 already defining authoritative routing for
both states. ADR-0023 also described offline preloading, while the shipped kiosk
rejects offline PCs.

## Decision

A PC is **Available** for kiosk selection when it is reachable and not
owner-locked. Both `online` and `in_use` are selectable; `offline` and `locked`
remain unavailable. In-use PCs retain their amber status tone and explicitly
indicate that top-up is allowed.

This is a frontend availability change only. Terminal Selection and server-side
coin routing remain unchanged: an Active Session receives time and Points;
otherwise the PC receives Guest Time and zero Points.

## Consequences

Customers can top up an in-use PC without interrupting its session. Offline PCs
cannot be preloaded through the kiosk. Existing rejection feedback remains for
offline, locked, missing, and empty selections.
