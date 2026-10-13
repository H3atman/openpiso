# ADR-0053: Shop Hours — scheduled soft close and temporary overrides

- Status: Accepted
- Date: 2026-07-29
- Deciders: shop owner (product)
- Relates to: [ADR-0001](0001-time-ownership.md) (time ownership and
  balances), [ADR-0008](0008-coin-routing-central-terminal.md) (coin routing),
  [ADR-0013](0013-balance-and-session-durability.md) (balance durability),
  [ADR-0015](0015-audit-log.md) (owner and customer actions),
  [ADR-0023](0023-serial-coin-input-and-terminal-selection.md) (coin
  acceptor), [ADR-0024](0024-web-admin-sole-owner-surface.md) (owner surface),
  [ADR-0025](0025-terminal-selection-commit-flow.md) (Selection Window),
  [ADR-0033](0033-typed-coin-source-trust-and-coin-outcome.md) (authoritative
  coin outcomes)

## Context

The shop needs owner-configurable hours without turning the close of business
into a destructive timer event. A schedule must express a normal open and
close time for each day, including an overnight period, while an owner must be
able to force the shop open or closed temporarily. Every surface needs the
same answer about whether the shop is available, why, and when that answer will
change.

Closing also needs a precise boundary. It must stop new play and new value from
entering the system, but a customer who is already playing has already paid
for the value in their **Active Session**. Ending that session at the scheduled
close would confiscate paid time, and the system has no **cash** refund path for
a cash business. (A **Rejection Credit** moves value *in* for a coin the
acceptor took but never valued — see [ADR-0069](0069-rejection-credit.md) — and
never hands cash back.)

The coin acceptor already has a physical safety boundary: the Set Pin is
asserted only while a **Selection Window** is open (ADR-0025). Shop Hours must
use that boundary rather than introduce a second hardware inhibit mechanism.

## Decision

### Shop Hours vocabulary and schedule

**Shop Hours** is the whole feature: one master enabled flag, seven day rows,
and a configurable **Last Call** lead time. Each configured day has one
**Open Period**, an `open time → close time` interval. An Open Period may cross
midnight; the day on which it starts owns its tail into the next calendar day.
A day marked closed all day means that no Open Period starts on that day. It
does not truncate the prior day's overnight tail.

This feature deliberately uses **Open Period**, not "window": **Selection
Window** is already reserved for the terminal concept by ADR-0025. Shop state
is always written as **Shop Open** or **Shop Closed**; bare open/close are the
Selection Window verbs, not shop-state vocabulary.

The server computes one **Shop Status** value for every surface:
`{ open, next_change_at, reason, last_call_active }`. `last_call_active` is true
only during a scheduled Last Call period; it is false for a zero lead, Shop
Closed, and every Shop Override. The Admin host's local clock is the clock for
this computation; there is no configured shop timezone.

### Overrides

A **Shop Override** is an owner-set force-open or force-closed state with an
absolute expiry instant. While it has not expired, it wins over the schedule.
After expiry, the normal schedule is evaluated again. An override is never
sticky and does not defer expiry to a scheduled boundary.

### Last Call

**Last Call** is the configurable period before a scheduled shop close. During
Last Call, buying still works, but customer-facing surfaces warn that the shop
will close soon. Last Call is advisory; it does not end a session or alter a
balance.

### Soft close and the gated set

A **Shop Closed** status is a soft close. It gates entry and new value only:

- Block opening a **Selection Window** and acceptor use.
- Block coins through every coin path, including top-ups.
- Block login, signup, and Voucher redemption.

It allows actions that use value already inside the system or do not start new
play:

- **Claim**, points redemption, **Spin the Wheel**, and points transfer.
- Logout.
- All owner surfaces and owner actions.

A Shop Closed status never ends an **Active Session**, touches a balance,
triggers a shutdown, or changes the existing time-to-zero auto-shutdown
behaviour. Active sessions run until their Remaining Time reaches zero, and
existing Auto-Shutdown then empties the shop normally.

### Coin enforcement

The Set Pin remains asserted only while a Selection Window is open. When Shop
Hours refuses a new Selection Window, the acceptor is never energised and an
inserted coin is physically returned. No new hardware inhibit path is added.

## Considered options

### Hard stop at scheduled close

Rejected. Ending Active Sessions at close would confiscate paid time. Adding a
cash refund path would be a new money operation with no existing cash-business
support. A soft close preserves the customer's purchased value and lets
Auto-Shutdown empty the shop naturally.

### Allow top-ups while Shop Closed

Rejected. New value would keep entering the shop, so the shop could never
empty. Per-coin legality also cannot be reliably enforced at the acceptor once
it is energised.

### Block every customer action while Shop Closed

Rejected. Claim, points redemption, Spin the Wheel, points transfer, and logout
use value already inside the system or end an existing visit. Blocking them
would trap customer value or prevent a customer from ending an Active Session.
The gated set blocks only entry and new value.

### Add a separate hardware inhibit path

Rejected. The existing fail-closed Set Pin already prevents the acceptor from
being energised outside a Selection Window. A second hardware path would add
complexity without closing a gap.

### Truncate an overnight tail when the next day is closed all day

Rejected. The Open Period belongs to the day on which it starts. A closed-all-
day row means no period starts on that day; it cannot retroactively shorten the
prior day's declared period.

### Expire an override at the next scheduled boundary

Rejected. A force-open override set at 23:00 against an 08:00–22:00 schedule
would otherwise keep the shop open through the following day. An absolute
instant makes the owner's intended duration unambiguous.

### Make overrides sticky with no expiry

Rejected. A permanent force would silently defeat the schedule and create a
state the owner might forget. Every override must have an absolute expiry.

### Configure a shop timezone

Rejected for this feature. The Admin host's local time is sufficient for a
single local shop and avoids a second clock whose DST and configuration rules
would need to govern money and access decisions.

## Consequences

- The glossary terms **Shop Hours**, **Open Period**, **Shop Override**, **Shop
  Status**, **Last Call**, **Shop Open**, and **Shop Closed** are authoritative;
  **Selection Window** remains the terminal term and bare open/close remain its
  verbs.
- All customer-facing surfaces can render one server-computed Shop Status and
  its reason and next change.
- A scheduled close is non-destructive: Active Sessions, balances, and the
  existing Auto-Shutdown path are unchanged.
- Closed-shop enforcement must cover every new-play or new-value boundary,
  including the acceptor, login, signup, and Voucher redemption, while keeping
  owner surfaces and already-held value usable.
- Overnight schedule semantics are stable even when the following calendar day
  is closed all day.
- Override expiry and schedule evaluation use the Admin host's local clock;
  no shop timezone is persisted.
