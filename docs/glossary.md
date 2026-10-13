# OpenPiso Glossary

Ubiquitous language for the OpenPiso domain. Terms here are authoritative —
use these exact words in code, schema, API, and UI. Update this file when a
term's meaning is decided or changed during design.

> Status legend: **(decided)** = pinned by an ADR. **(open)** = mentioned but
> not yet fully defined.

---

### Theme **(decided)**

The server-stored, shop-wide appearance record: one **Accent**, separate
light/dark modes for web admin, kiosk, and Client, plus an integer version used
for live distribution. It is a cosmetic shop setting, not a per-browser or
per-PC preference. See [ADR-0035](adr/0035-centralized-theme.md).

### Accent **(decided)**

The Theme's single shop-wide lowercase `#rrggbb` input. The shared ramp
generator derives the themed color tokens from it; it is not a per-surface
color and does not include neutral or destructive tokens. See
[ADR-0035](adr/0035-centralized-theme.md).

### PC

One rentable Pisonet computer. Has a stable id (e.g. `pc-01`) and a display
name (e.g. `PC 01`). A PC is a *place a customer sits*, not necessarily the
owner of the time being consumed. See **Source of Truth**.

The PC's `pc_id` is **server-assigned identity that the Client caches**, not
owner intent. The server owns which `pcs` row a machine is; the Client persists
the `pc_id` it was last told (Part A / ADR-0031), but on conflict the
server-resolved identity wins and overwrites the cached value. See **PC
Identity Key**.

### PC Identity Key **(open)**

The durable key the server uses to decide which `pcs` row a booting machine is:
the **MAC of the onboard Ethernet NIC**. Resolved deterministically as the MAC
of the interface that carries the route to the admin server (which, in a wired
shop, *is* the onboard NIC), falling back to the lowest-sorted physical,
non-virtual, non-wireless MAC when the server is unreachable at boot. Read per
boot, never persisted on the Client. It is an identity *hint*, not an auth
secret (spoofable) — it never gates money/time/session authority. The
**source IP** is a one-time bootstrap key used only until a MAC is bound.

Resolution is **pre-registration only** — there is no auto-create. The server
resolves identity (MAC from the `X-OpenPiso-Mac` header, source IP from the
connection peer address — never a body-supplied IP) by: (1) MAC matches an
existing `pcs` row → that PC; (2) no MAC match but the source IP matches an owner
**pre-registered** row **whose `mac_address IS NULL`** → bind that row's MAC; (3)
otherwise **reject** the report (the machine is unregistered). The owner must
pre-register every PC by number/name + static IP before it can connect; there
are no pending/hidden auto-created rows and no "adopt" flow.

The owner-authored static IP is stored as **`registered_ip`**. It is the step-2
match key and the address used to reach that PC for a **Remote Control
Session**. Device traffic cannot change it. An established MAC binding is
**never** overridden by an IP match — a registered PC reporting a *new* MAC
from the same IP (i.e. hardware/NIC replacement) is rejected until the owner
clears the binding (nulls `mac_address` by editing the PC in web-admin), so the
next boot re-binds by IP. See [ADR-0031](adr/0031-client-config-persistence-and-pc-identity.md),
[ADR-0075](adr/0075-web-admin-remote-control.md), and **PC**.

### Guest Time **(decided)**

Time that is bound to a **PC**, not to any customer account. Created when
someone inserts coins at a PC without logging in. Whoever is sitting at that
PC consumes it. It is **not portable** — it stays with the PC. This is the
original MVP model (`pcs.remaining_seconds`).

### Account **(decided)**

A customer identity that can log in and out, authenticated by **username +
password** against the local server (no cloud). Owns a portable **Remaining
Time** balance and a **Points** balance — both money-equivalent. Password
recovery is an owner-performed **reset** (no self-service, no retrieval of old
passwords). Weak security is accepted (neighborhood shop). See
[ADR-0004](adr/0004-account-authentication.md).

### Legacy Account **(decided)**

An Account created by an owner **Account Import** from an external system.
Legacy status is permanent provenance, not a different kind of Account; all
existing Account actions remain available.

### Account Import **(decided)**

An owner-authenticated migration of Accounts, balances, and credentials from a
legacy export. A preview plans the same operation that a confirmed commit
performs; valid rows are committed atomically and unimportable rows are
reported as skipped. A Username previously held by any deleted Account is
eligible for **Account Re-import** and creates a new Account identity; active
Username conflicts remain skipped. The owner-facing label is **Legacy**; the
schema records the neutral source system in `imported_from` (for example,
`martec`).

### Account Re-import **(decided)**

A later Account Import that creates a new Account identity using the Username
of any previously deleted Account. Deletion only makes the Username eligible
for this import path; it does not restore or automatically create an Account.
The deleted Account and its historical records remain preserved. Manual Account
creation and self-service signup cannot reuse it. See
[ADR-0059](adr/0059-account-import-reuses-deleted-usernames.md).

### Last Login **(decided)**

An Account's most recent successful login, derived from its `login_succeeded`
Audit Log events. An Account with no successful login displays **Never**; a
deleted Account retains its historical Last Login.

### Password Change **(decided)**

An authenticated Account replacing its own password after proving knowledge of
the current password. A Password Change preserves the Account Session and
Active Session; it is not a recovery path. If another Password Change replaces
the password first, a request based on the superseded password cannot overwrite
it. See [ADR-0037](adr/0037-contention-safe-password-change.md).

### Password Reset **(decided)**

An owner replacing an Account password without requiring the current password.
This is the Account recovery path and is distinct from a customer-initiated
**Password Change**. See
[ADR-0004](adr/0004-account-authentication.md) and
[ADR-0037](adr/0037-contention-safe-password-change.md).

### Username **(decided)**

An account's local login name. Display spelling is preserved, while identity
and uniqueness use a trimmed, ASCII-lowercase normalized value. Usernames are
case-insensitive: an active Username cannot be reused, while Account Import may
reuse the Username of any deleted Account by creating a new Account identity.
See [ADR-0017](adr/0017-username-identity.md) and
[ADR-0059](adr/0059-account-import-reuses-deleted-usernames.md).

### Guest **(decided)**

The state of a PC session where no account is logged in. Guest is a
*convenience mode*: insert a coin and use the PC immediately, no login
required. The trade-off is that Guest Time is PC-bound and can be absorbed by
the next person who logs in at that PC (see **Claim**). The original expansion plan's "Guest
Accounts" refers to **this mode** — it is **not** a real account entity and
creates no `accounts` row. See [ADR-0010](adr/0010-account-creation.md).

### Claim **(decided)**

The act of a logged-in customer absorbing the PC's current **Guest Time**
into their own account balance. Triggered by an explicit confirmation prompt
("Do you want to claim this time?") — never silent. After a claim, the time
is portable and belongs to the account; the PC's guest balance drops to 0.

### Active Session **(decided)**

The live binding of an **Account** to a **PC** for the duration of a logged-in
visit. An account has **at most one** active session. Logging in elsewhere
auto-kicks the prior PC. The server owns this binding. See
[ADR-0002](adr/0002-account-session-binding.md). (Distinct from the MVP
`sessions` table, whose role is to be re-decided at schema design.)

A session survives a client crash/restart (a crash is not a logout); it ends
only on explicit logout, auto-kick, time = 0, or being taken over by another
account at the same PC. A login wins both contests — it kicks the account off
any other PC (ADR-0002) and ends any other account on that PC
([ADR-0007](adr/0007-pc-level-takeover.md)); displaced accounts always keep
their portable balance. See [ADR-0006](adr/0006-session-lifecycle.md).

### Account Session **(decided)**

The request-guard credential that proves an HTTP caller holds a valid account
token for an **Active Session**. Implemented as the `AccountSession` extractor on
the LAN server (`:3210`), mirroring the owner side's `OwnerSession`: a handler
that mutates an Active Session names `AccountSession` in its signature and cannot
run without a valid token, so the auth check is enforced by the type system
rather than by each author remembering a first line. The token travels in an
`Authorization: Bearer` header (not the request body) and resolves to the bound
`account_id`, `pc_id`, and `session_id`. Distinct from **Active Session** itself:
the Account Session is the *credential/guard*, the Active Session is the
server-owned *binding state* it points at. Denials are intentionally **not**
audited — the common case is a benign stale-token race after logout / auto-kick /
takeover. See [ADR-0032](adr/0032-account-session-extractor.md) and the durable
historical record in [ADR-0062](adr/0062-account-session-history.md).
_Avoid_: using "Active Session" for the request guard, or "account token check".

### Remote Control Session **(decided)**

An owner-initiated live view and input connection to a **PC** for assistance,
maintenance, or monitoring. The owner may start it while the PC is occupied or
idle without notifying its user. It is not an **Active Session** and carries
no time, balance, or account authority.
_Avoid_: Remote Session, VNC session.
See [ADR-0075](adr/0075-web-admin-remote-control.md).

### Remote Control Credentials **(decided)**

The two shop-wide TightVNC passwords known by the owner: one grants view-only
access and one grants keyboard and mouse control. OpenPiso requests them when
needed and never stores them.
_Avoid_: PC password, Client password.
See [ADR-0075](adr/0075-web-admin-remote-control.md).

### Remote Control Settings **(decided)**

The shop-wide configuration that enables Remote Control and defines its allowed
PC IPv4 subnet and TightVNC port. It contains no Remote Control Credentials.
See [ADR-0075](adr/0075-web-admin-remote-control.md).

### Owner Sign-Out **(decided)**

The owner ending a customer's **Active Session** from the Web Admin dashboard
PC row. The lifecycle effect is *identical to a logout*: the balance stays on
the account, the binding is dropped, and the PC falls back to its own Guest
Time. It changes no money and no timer beyond ending the binding, and it does
not require the PC to be reachable — the server owns the binding (ADR-0002),
so the Client drops the stale session on its next heartbeat.

The two records deliberately differ. **Account Session history** records
`end_reason = 'logout'`, because for reporting an Owner Sign-Out *is* a logout
and session history should not fork over who clicked. The **Audit Log** records
a distinct `owner_sign_out` event with the `admin` actor, because attributing
it to the customer would make the log claim the customer left voluntarily —
the one thing it must never do in a dispute. A row in session history reading
`logout` next to an audit event reading `owner_sign_out` is correct, not a bug.

The request is keyed on `session_id`, not on the PC: the owner clicks from a
snapshot that refreshes every few seconds, and a stale click must be a no-op
rather than eject whoever took the seat since. See
[ADR-0068](adr/0068-owner-sign-out.md).
_Avoid_: "force logout" or "kick" — **Auto-Kick** already means the ADR-0002
displacement, and "logout" alone means the customer's own protective act.

### Idle Drain **(decided)**

While an account is logged in and the PC is on, its balance drains every second
regardless of activity. Logging out is the only way to preserve remaining time.
The UI must teach this. See [ADR-0006](adr/0006-session-lifecycle.md).

### Remaining Time

The integer-seconds balance of unused paid time. Lives on the **PC** for
guests, and on the **Account** for logged-in customers. See **Source of
Truth**.

### Attendance Reward **(decided)**

The separate, Account-only reward program that awards **Attendance Time**
through daily **Attendance Check-Ins**. It stacks with paid time, **Points**,
and **Coin Promo**; none substitutes for another. Disabling Attendance Reward
stops new Check-Ins but does not remove Attendance Time already earned. See
[ADR-0084](adr/0084-attendance-rewards.md).

### Attendance Check-In **(decided)**

One immutable daily Attendance Reward earned by one **Account** for one
eligible Admin-host local calendar date. It awards the configured integer
seconds of **Attendance Time**. An Account can earn at most one Attendance
Check-In for a date.

Qualifying value is cumulative integer cents from accepted Account Coin Events
and Account-targeted **Rejection Credits** assigned to their immutable server
date. Guest value never qualifies, value never carries between dates, and
later value cannot award a second Check-In. The first qualifying contribution
fixes that Account/date's threshold and reward.

A completed date is eligible only when Account existence, enabled Attendance
Reward, and effective **Shop Open** time overlapped. Otherwise it is ineligible.
The server freezes completed-date eligibility; later settings or host timezone
changes do not rewrite it. Admin-host local dates use midnight-inclusive,
next-midnight-exclusive boundaries. See
[ADR-0084](adr/0084-attendance-rewards.md).

### Attendance Time **(decided)**

The Account-bound integer seconds awarded by **Attendance Check-Ins**. It does
not drain during play and cannot transfer. It becomes ordinary **Remaining
Time** only through an explicit, authenticated **Attendance Claim**. Unclaimed
Attendance Time expires with its **Attendance Earning Month** or is forfeited
when the Account is deleted; either outcome preserves its history. See
[ADR-0084](adr/0084-attendance-rewards.md).

### Outstanding Attendance Time **(decided)**

The current integer-seconds liability from Attendance Time that an active
Account has earned but has not claimed, allowed to expire, or forfeited. It
includes both `earning` and `claimable` Attendance Time; claimable time is a
subset, not an additional balance. It is reported as time, never converted to
pesos, and has no PC owner. See [ADR-0084](adr/0084-attendance-rewards.md).

### Attendance Earning Month **(decided)**

One Account's Attendance Check-Ins grouped by Admin-host local calendar month.
It exists only after the Account earns a Check-In in that month. The current
month is `earning`; after month-end it is `claimable` for its configured
following calendar months, then `expired`. A month with no outstanding
Attendance Time is `claimed`; Account deletion makes outstanding time
`forfeited`. These states are derived, not permanent transitions: a late
Check-In can move a `claimed` month back to `claimable` before its deadline. See
[ADR-0084](adr/0084-attendance-rewards.md).

### Attendance Claim Window **(decided)**

The configured following calendar months during which an Attendance Earning
Month may be claimed. It begins at midnight on the first day after the earning
month and expires at midnight on the first day after the final claimable month.
The earning month itself is never claimable. See
[ADR-0084](adr/0084-attendance-rewards.md).

### Attendance Claim **(decided)**

The explicit, authenticated conversion of all currently outstanding
**Attendance Time** from exactly one **Attendance Earning Month** into ordinary
**Remaining Time**. Partial and zero-value claims do not exist. Shop Closed
blocks a new login but does not block an already authenticated Account from
claiming. See [ADR-0084](adr/0084-attendance-rewards.md).

### Supplemental Attendance Claim **(decided)**

A later Attendance Claim for the same Attendance Earning Month after a late
Attendance Check-In reopens it before its deadline. It claims all currently
outstanding Attendance Time for that month; it is not a separate entity. See
[ADR-0084](adr/0084-attendance-rewards.md).

### Daily Reward **(decided)**

Client customer-facing label for **Attendance Reward**. Display-only
vocabulary: code identifiers, schema, API, logs, and Web Admin keep the
Attendance terms above. The Client never shows customers the words
*Attendance* or *check-in*.

| Glossary term | Client copy |
|---|---|
| Attendance Reward | Daily Reward (`Daily Reward paused` when disabled) |
| Attendance Check-In | a day earned (`✓ earned`, `Today earned 1 hour`) |
| Attendance Time | a plain **Duration**; "reward time" only where a noun is unavoidable |
| Outstanding (claimable) Attendance Time | waiting to claim |
| Attendance Claim | Claim / Claimed |
| Attendance Earning Month | the month name (`September 2026`) |
| expired | Expired (forfeited is never shown; the Account is gone) |

Claim dates render the last claimable day inclusive (an exclusive deadline of
`2026-11-01` reads `Claim by Oct 31`), from the Admin-local date without Client
timezone conversion, with the year only when it differs from the current
Admin-local year. `Expired`, `Claimed`, and `Claimable from` use the same shape.

### Duration **(decided)**

The domain quantity is always integer seconds. Fixed-width numeric surfaces
(the live countdown) render it as `hh:mm:ss`, signed with `+`/`-` when it is a
delta; banked amounts such as Attendance Time never use it.

Client prose surfaces **(decided)** drop every zero unit and spell hours but
abbreviate the rest: `25 hours`, `1 hour 30 min`, `45 min`, `30 sec`; zero is
`0 sec`; hours are unbounded. Web Admin prose surfaces **(decided)** use the
same format; signed deltas prefix `+`/`-` (`+1 hour`, `-30 min`), with no sign
on zero.

### Source of Truth (for time) **(decided)**

The single stored number that the authoritative timer decrements each second:
- **Guest session** → the PC row (`pcs.remaining_seconds`).
- **Logged-in session** → the account row (the account balance).

See [ADR-0001](adr/0001-time-ownership.md). The balance is written to SQLite
**every second**; on a crash the in-flight second favors the customer; session
bindings are in-memory only, so a server restart logs everyone out with
balances intact. See [ADR-0013](adr/0013-balance-and-session-durability.md).

### Points **(decided)**

A second currency, separate from time, earned per inserted coin **only while
logged in** (guests earn zero). A pure loyalty bonus layered on top of the
normal time purchase — earning points never reduces time bought. Spent two
ways: choose one guaranteed **Redemption Tier** for time, or bet on the
**Spin the Wheel**. Default earn rate ₱50 = 1 point; the seeded default
Redemption Tier is 10 points = 4 hours. See
[ADR-0003](adr/0003-points-economy.md).

Points are **centipoint-precision**: earning is fractional (₱25 at ₱50/point
earns 0.50 points immediately), and balances display with two decimals.
Redemption options use whole-point **Redemption Tiers** and a wheel spin costs
exactly 1.00 point; all other point amounts (transfers, manual adjustments,
voucher values) allow 0.01 precision. Guest purchases earn no points, and
claiming Guest Time never grants retroactive points. See
[ADR-0038](adr/0038-fractional-points-centipoints.md) and
[ADR-0039](adr/0039-redemption-tiers.md).

### Redemption Tier **(decided)**

One owner-authored guaranteed exchange option: a unique whole Points amount
(at least 5 and a multiple of 5) for a whole number of minutes. A shop stores
1–20 Redemption Tiers, displayed in ascending Points order. An upgrade seeds
one grandfathered tier from the former redeem pair, unchanged even if it does
not meet the authoring rules. See [ADR-0039](adr/0039-redemption-tiers.md).

### Centipoint **(decided)**

The canonical integer sub-unit of Points: **1 centipoint = 1/100 point**.
All point amounts are stored, computed, and transmitted as integer
centipoints (never floats); only display formatting divides by 100.
Sub-centipoint earning remainders carry on the account in **centicents**
(1/100 cent). See [ADR-0038](adr/0038-fractional-points-centipoints.md).

### Credit **(decided)**

Owner/customer-facing synonym for **₱1 of inserted money**, used in the points
context (e.g. the "Credits per point" earn-rate setting: 50 credits = ₱50 =
1 point). Display-only vocabulary — storage, computation, and APIs remain
integer **cents**; a credit is never a stored unit.

### Shop Rate **(decided)**

The owner-configurable paid-time exchange: an amount in integer cents and the
whole number of seconds it buys. For `inserted_cents`, the server computes paid
time as `seconds_added = inserted_cents * rate_seconds / rate_amount_cents`
using integer division, so any fractional second is truncated. There is one
active Shop Rate; changing it appends a new rate and retires the previous one
rather than editing it in place. A committed purchase keeps its computed time
and is never retroactively recomputed under a later Shop Rate. Shop Rate is
not the **Points** earn rate (which maps money to loyalty points for logged-in
customers) or the **Bokya Rate** (which controls the probability of a losing
Spin the Wheel landing). See [ADR-0066](adr/0066-shop-rate.md).

### Effective Bonus % **(decided)**

The real extra value an exchange represents at the current **Points** earn rate
and **Shop Rate**, expressed in integer basis points. For a Redemption Tier it
is ≈ `(tier_seconds / tier_points) / (earn_cents_per_point *
rate_seconds / rate_amount_cents)`; for a
**Coin Promo Tier** it is its bonus seconds against the base seconds that
amount buys. Target ~10%. The admin settings screens show it live for every
row and use the most generous row for a headline warning, because it is the
owner's worst case. The owner's overall worst case is the **account worst case**:
the most generous Coin Promo Tier plus the most generous Redemption Tier, since
a logged-in customer receives both on one coin. It warns; it never blocks.
**Attendance Reward** also shows an Effective Bonus %, measured at exactly its
daily threshold spend; because it is a per-Account-per-day step reward rather
than a per-peso rate, it is informational only, never joins the account worst
case, and is not judged by the unsafe threshold. See
[ADR-0003](adr/0003-points-economy.md),
[ADR-0039](adr/0039-redemption-tiers.md), and
[ADR-0052](adr/0052-coin-promo.md).

### Coin Promo **(decided)**

The owner-configured bonus time granted on a coin routed to an **Account**,
credited immediately on top of the paid time the base rate already buys. It is
account-only and decided at insertion: Guests receive nothing, and a later
**Claim** never back-credits it — the same rule
[ADR-0003](adr/0003-points-economy.md) sets for Points. It applies to every
coin event (serial terminal, simulator, remote device, kiosk in-use top-up) and
to nothing else — never to Vouchers, **Manual Adjustments**, redemptions, or
wheel winnings. It has a shop-wide enable flag and ships disabled with no tiers.
Distinct from the `promo` **Voucher** kind; the qualifier "coin" is required.
See [ADR-0052](adr/0052-coin-promo.md).

### Coin Promo Tier **(decided)**

One owner-authored row of a **Coin Promo**: a coin amount mapped to whole bonus
seconds. A **Coin Batch** amount is decomposed into tier amounts largest-first
and their bonuses are summed, because a batch is a whole coin train rather than
one coin ([ADR-0070](adr/0070-credit-the-pulse-sum.md)): ₱10 + ₱1 in one train
earns the ₱10 tier, while ₱10 + ₱10 earns the ₱20 tier when one is authored and
the ₱10 tier twice when none is. There is no
nearest-lower fallback — the remainder no tier covers earns nothing — and a
single coin still resolves to exactly its own tier. The tier is
read at the instant the coin commits, so it carries no menu version (unlike a
**Redemption Tier**, which the customer reads from a menu before committing).
Its bonus is reported as the **coin promo** component of **Free Time
Dispensed**, while the coin's cash remains **Cash Revenue**. See
[ADR-0052](adr/0052-coin-promo.md).

### Spin the Wheel **(decided)**

A gambling mechanic, **1 point = 1 spin**. Prizes are **time**, **points**,
"win nothing," or an **extra free spin**. All segments are equally sized; they
are equally likely when **Bokya Rate** is unset. Free spins chain up to a
configurable **cap**. Spins are
**server-authoritative** (RNG on the server, client animation is cosmetic).
House edge (time axis only) is measured against the point value of the most
generous Redemption Tier, must stay positive, and is enforced both when the
owner saves and on the spin path — a negative-edge wheel refuses to spin.
Points awarded by the wheel are tracked via the **Points Minted by Wheel**
report line — there is no points-axis guardrail, deliberate asymmetry.
Owner can enable/disable it at any time. See
[ADR-0012](adr/0012-spin-the-wheel.md),
[ADR-0046](adr/0046-spin-the-wheel-points-prize.md),
[ADR-0047](adr/0047-equal-slice-wheel-segments.md), and
[ADR-0049](adr/0049-bokya-rate.md).

### Bokya **(decided)**

A Spin the Wheel landing awarding no time, no points, and no Free Spin. Bokya is
derived from the Wheel Segment's prize and is never stored. A slice awarding
only a Free Spin is a win, not Bokya. See
[ADR-0049](adr/0049-bokya-rate.md).

### Bokya Rate **(decided)**

The optional owner-set integer probability, from 0 through 10,000 basis points,
that any single Spin the Wheel landing is **Bokya**. Unset means the server
draws uniformly across all Wheel Segments. When set, it controls the
server-authoritative Bokya/non-Bokya draw without changing the wheel's equal
slice geometry. See
[ADR-0049](adr/0049-bokya-rate.md).

### Wheel Segment **(decided)**

One prize slot on the Spin the Wheel. Each segment is a **prize pair** with a
label, an amount of seconds (non-negative), an amount of centipoints
(non-negative), and a free-spin flag. There is no tagged kind — a single segment
can award time, points, and a free spin simultaneously. "Win nothing" is a
segment awarding neither time nor points and not granting a free spin.

Segments carry **no probability weight** and each occupies exactly `1/n` of the
circle. When Bokya Rate is unset, every segment is also exactly `1/n` likely and
the drawn geometry truthfully states the odds. When Bokya Rate is set, the server
controls Bokya versus non-Bokya landings independently of geometry; repeating
prizes remains the way
to distribute probability among segments within either group. Each slice also
renders a **derived prize line** (`5 min`, `2.00 pts`, `Free spin`, `—`) computed
from its own seconds and centipoints, which the free-text label cannot
contradict. The server resolves the outcome authoritatively. See
[ADR-0047](adr/0047-equal-slice-wheel-segments.md),
[ADR-0049](adr/0049-bokya-rate.md), and
[ADR-0046](adr/0046-spin-the-wheel-points-prize.md).

### Points Minted by Wheel **(decided)**

The total centipoints the Spin the Wheel has awarded across all spins in a
given report period. A cost-line metric tracked alongside **Points Liability**
and **Free Time Dispensed** as the compensating control for permitting points
as a wheel prize. See [ADR-0046](adr/0046-spin-the-wheel-points-prize.md) and
[ADR-0014](adr/0014-reporting-model.md).

### Spin Duration **(decided)**

The single owner-configurable non-negative integer seconds (default 5) for how
long the client animates the wheel before revealing the server-resolved result.
A cosmetic timer only — the outcome is already decided by the server when the
animation starts. See [ADR-0046](adr/0046-spin-the-wheel-points-prize.md).

### Free Spin Chain **(decided)**

A sequence of consecutive free spins triggered when a landed segment awards a
free spin. The chain is capped at an owner-configurable maximum (default 1);
once the cap is reached, the forced stop resolves with the time and/or points
accumulated plus any remaining free-spin segments forfeited. The server
resolves the entire chain in one transaction before the client animation. See
[ADR-0012](adr/0012-spin-the-wheel.md) and
[ADR-0046](adr/0046-spin-the-wheel-points-prize.md).

### Diskless **(decided context)**

The shop's deployment model: each PC boots a RAM-backed OS image from a server
and **discards all changes on reboot**. Consequences baked into the design: a
power-cycle is the privacy reset (wipes the prior customer's sessions), the
client app + watchdog ship in the master image, and customer tampering is
self-healed on reboot. See [ADR-0005](adr/0005-tamper-response-and-expiry-powraoff.md).

### Watchdog **(decided)**

A persistent client-side process (Windows service / scheduled task) baked into
the diskless image that relaunches the client app if it is killed. Bounds the
anti-kill arms race together with the diskless reboot reset. Because the server
is authoritative for time, the watchdog is about friction/UX, not theft.
See [ADR-0005](adr/0005-tamper-response-and-expiry-powraoff.md).

### Autostart **(decided)**

The owner-controlled setting that makes the **Central Terminal** launch
automatically on the Admin host, written to the per-user Windows `Run` key. It
is **logon-scoped, not boot-scoped**: unattended start after a power cycle also
requires owner-provisioned Windows autologon, which OpenPiso documents but
never configures. The registry entry is the sole source of truth — there is no
`settings` row and no mirror, so the owner always sees real host state; absence
means off, and off is the shipped default. The toggle is read-only in debug
builds, and enabling or disabling it is audited on success and on failure.
Distinct from the **Watchdog** and the Client Guard, which are Client-side and
relaunch a killed process rather than start one at logon. Autostart also
requires single-instance enforcement, since a second Central Terminal process
would run a second timer loop against the same database. See
[ADR-0055](adr/0055-central-terminal-autostart.md).

### Auto-Shutdown **(decided)**

At time = 0 the server commands the PC to **power off** (real OS shutdown), not
a lock overlay — so the diskless reboot gives the next customer a clean slate.
Replaces the MVP lock-overlay-only behavior for the expansion phase. The server
streams a `power_state` (`active`/`warning`/`shutdown`) on `pc.updated`; it is a
pure function of the PC's connection status and Remaining Time, so **any**
connected PC at zero is commanded off — including a fresh boot (ADR-0034
supersedes the earlier "positive→zero transition only" rule). See
[ADR-0005](adr/0005-tamper-response-and-expiry-powraoff.md) and
[ADR-0034](adr/0034-connected-zero-time-shutdown-and-channel-consistency.md).

### Shutdown Grace Period **(decided)**

The expiry user experience: a non-blocking low-time warning while Remaining
Time is ≤ 120 s, then, at zero, a blocking **300-second** countdown overlay
before the real power-off. A top-up during either phase cancels the shutdown.
The client runs the countdown and issues the OS shutdown (release builds only;
debug builds log instead); the authority is never exposed to Svelte. A connected
PC sitting at zero (including a fresh boot) runs the countdown too (ADR-0034).
See [ADR-0022](adr/0022-expiry-grace-period.md) and
[ADR-0034](adr/0034-connected-zero-time-shutdown-and-channel-consistency.md).

### Offline Grace **(decided)**

The owner-configured positive-integer duration, default **600 seconds**, that
an offline Client uses for the Offline Shutdown Countdown after losing the
admin server. It is distinct from the **Shutdown Grace Period**, which applies
when Remaining Time expires. The value is stored in shop settings, served by
`GET /api/shop`, and cached in the Client's local config for server outages.

### Offline Shutdown Countdown **(decided)**

The client-side countdown armed when a previously-connected Client loses the
admin server. It immediately returns the PC to the **Fullscreen Lock**, uses the
cached **Offline Grace**, and fires the Rust-owned one-shot OS shutdown at the
deadline (release Windows builds only). Its absolute wall-clock deadline is
atomically persisted so a relaunch resumes it; an already-expired deadline gets
a 10-second re-warning. A successful reconnect cancels and deletes it. The
countdown pauses only while owner settings are PIN-unlocked, and closing settings
extends and persists the deadline; the PIN prompt alone does not pause it. Setup,
connecting, unregistered, and never-connected states never arm it. See
[ADR-0045](adr/0045-offline-shutdown-countdown.md).

### Central Terminal **(decided)**

The single place coins enter the shop — there are no per-PC coin slots. The
customer picks a PC at the terminal; the server routes the coin by that PC's
login state: to the logged-in account (time + points) or to the PC as guest
time (zero points). A PC is **Available** for kiosk selection when it is
reachable and not owner-locked; existing Remaining Time does not block a top-up.
In v1 it is a full-screen Tauri kiosk on the server PC, fed by a coin acceptor
on the server's COM port (not the ESP32/HTTP path — that is deferred). The
serial path uses a **Pulse Sampler** while the 500 ms loop retains selection
and commit policy. See
[ADR-0008](adr/0008-coin-routing-central-terminal.md) and
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Kiosk Lock **(decided)**

The **Central Terminal** window's always-on-top state — and only that; it does
not govern fullscreen, decorations, key blocking, or OS hardening. It is
**ephemeral and boots engaged**: `alwaysOnTop` is declared in `tauri.conf.json`
and re-asserted in Rust setup, nothing is persisted, and every launch starts
locked. The owner releases it by double-clicking the **Shop Name** in the kiosk
header and entering the owner PIN; the gesture is inert while a **Selection
Window** is open, and re-engaging needs no PIN. Rust owns both the PIN check
and the window flag. Distinct from the Client's **Fullscreen Lock**
(a whole window format, ADR-0029) and from the **Set Pin** (the physical coin
boundary), and from **Kiosk Lockdown**, which is OS-level confinement of a
Client PC. See [ADR-0056](adr/0056-kiosk-lock-always-on-top.md).

### Terminal Selection **(decided)**

The single authoritative in-memory record, owned by the admin Rust process, of
which **PC** the customer has currently chosen at the **Central Terminal**.
Because there is exactly one physical acceptor, it is a global singleton: at
most one is active at a time, and a second open request while one is active is
rejected ("terminal in use"). Holds the selected `pc_id`, opened/expiry
timestamps, and a display total. Routing of the coins it gates is still decided
by the PC's login state (ADR-0008), not by the selection. See
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Selection Window **(decided)**

The bounded period a **Terminal Selection** is active and the acceptor is
enabled. A UX/timeout boundary only — coins committed during it are already
final (immediate per-coin commit), so closing it never finalizes a batch. It
closes on inactivity (default 15 s, reset by each accepted coin), an explicit
`Enter`, or a hard cap (default 180 s); all owner-editable. The **Pulse Sampler** continues
observing outside the window. A rising edge while the Set Pin is inhibited is
admitted only when a pulse from that same train is already buffered, allowing a
straddling train to finish during close drain; an edge with an empty buffer is
an **Inhibited Pulse** and cannot join a batch. On the Central
Terminal, the customer opens a window by typing a PC number on the numpad and
pressing `Enter`; the insert-screen `Enter` is context-sensitive — it *cancels*
(closes, no money in) or *confirms* (closes, money committed). See
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md),
[ADR-0025](adr/0025-terminal-selection-commit-flow.md), and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Set Pin **(decided)**

The COM-header control line the server *drives* to enable or inhibit the coin
acceptor (the green pin). It is **fail-closed**: deasserted (inhibit) at
process start before serial init, whenever no **Selection Window** is open, on
crash/restart, and on shutdown; asserted only while a window is open. This is
the lock that makes orphan coins impossible in normal operation. The **Pulse
Sampler** refuses a rising edge while this pin is deasserted only when the
shared buffer is empty; that refused edge is an **Inhibited Pulse**. A train
already buffered may finish across inhibition and remains one **Coin Batch**.
See [ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Coin Pin **(decided)**

The COM-header status line the server *reads* (the blue pin); the acceptor
emits pulses on it per inserted coin. The serial **Pulse Sampler** currently
polls it and detects rising edges; batch assembly remains outside the sampler.
See
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Coin Batch **(decided)**

One pulse train, closed by a quiet inter-coin gap. A train can contain one coin
or multiple coins with no observable seam. A
positive pulse count is automatically valued by the fixed Rust authority of
100 cents per pulse, producing exactly one Coin Transaction through the
existing authoritative coin path. The runtime Coin Source never reads the
persisted Pulse Map. A **Coin Trace Id** is minted when the batch closes and
remains with it across commit retries; the field-informed default gap is 600 ms
and still requires bench validation. See
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Implausible Timing **(decided S2 boundary)**

An observation that the inter-pulse timing in a closed **Coin Batch** departs
from the measured acceptor timing. It is reported as a rate and never prevents
or changes credit. S1 reports timing as unavailable; S2 implements the detector.
See [ADR-0070](adr/0070-credit-the-pulse-sum.md).

### Pulse Map **(decided)**

The pulse-count → denomination table retained for calibration, reporting, and
historical **Rejection Credit**. It cannot prevent or change automatic **Coin
Batch** credit. The accepted owner-surface redesign makes it read-only. See
[ADR-0070](adr/0070-credit-the-pulse-sum.md) and
[ADR-0073](adr/0073-owner-coin-surface-is-a-report.md).

### Coin Batch Rejection **(decided historical boundary)**

A historical Audit Log row from the former live path where a closed **Coin
Batch** matched no **Pulse Map** entry. Current automatic acceptance has no
Pulse Map mismatch outcome and produces no new Coin Batch Rejection rows. The
historical row remains readable and the owner may repair it with a
**Rejection Credit**. See [ADR-0064](adr/0064-owner-visible-coin-loss-notices.md)
and [ADR-0070](adr/0070-credit-the-pulse-sum.md).

### Rejection Credit **(decided)**

An owner-initiated re-credit linked to a historical `coin_batch_rejected` Audit
Log event. There is no free-standing credit and no free-text reason. Automatic
**Coin Batch** credit does not create this event for a **Pulse Map** mismatch.

The target is **frozen** at the rejection's recorded subjects: the recorded
**Account** if there was one, otherwise that **PC**'s **Guest Time** — never the
PC's current occupant. A recorded Account that has since been deleted causes the
credit to be refused.

The owner picks a denomination from the current **Pulse Map** and a coin count;
the amount is never free text and never prefilled. The credited value is
computed exactly as an accepted coin's would be: **Shop Rate** seconds, **Points**
with the centicent carry, and any matching **Coin Promo** bonus for an Account
target; base Shop Rate seconds only for a Guest target.

It is **Cash Revenue** (`cash_flag = 1`) because the money is in the coin box,
with any promo component reported under **Free Time Dispensed**. It writes a
canonical `rejection_credit` **Alert** event plus a `rejection_credits` detail
row — never a coin transaction. Each rejection is creditable for 30 days, up to
its recorded coin count, and a Rejection Credit cannot be reversed.

_Avoid_: "refund". A refund returns cash; a Rejection Credit moves value in and
never opens the till. _Avoid_ also **Credit** alone, which is display-only
vocabulary for ₱1 of inserted money.

See [ADR-0069](adr/0069-rejection-credit.md).

### Pulse Sampler **(decided)**

The serial-only worker that polls the **Coin Pin** at approximately 30 ms and
performs rising-edge detection into the shared pulse buffer. Its state is
unseeded at initial open and after reset or port recovery: the first normalized
sample establishes previous-level state and cannot create an edge or pulse.
Only a later low-to-high normalized transition is observed, for either
supported polarity. It has no gap, pricing, selection, or commit policy; the
500 ms selection loop drains its buffer. The simulator injects pulses without
a sampler thread. See
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md), refining
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md).

### Inhibited Pulse **(decided)**

A rising edge observed while the **Set Pin** is not asserted and the shared
pulse buffer is empty. It is refused, recorded, and rate-limited as an anomaly;
it never enters a **Coin Batch** and can never credit a customer. A rising edge
while inhibited with a non-empty buffer is admitted as the tail of the already
started train and is not an **Inhibited Pulse**. When the sampler is healthy,
software cannot distinguish an idle insertion from an inhibit wiring or
acceptor fault without electrical feedback; the Coin Debug Bundle retains the
observable reason and timestamp for field investigation. See
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md), refining
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md).

### Coin Trace Id **(decided)**

The identifier minted when a **Coin Batch** closes to represent one physical
coin across commit attempts. It is carried by the batch and stored in the
existing **Audit Log** event metadata JSON, joining diagnostic records to the
linked transaction without a new column. See
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md), refining
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md).

### Coin Debug Bundle **(decided)**

The bounded, owner-authenticated Markdown export of coin-path configuration,
computed findings, joined coin timelines, anomalies, and a whitelisted JSONL
appendix. Coverage is limited to the current app run and the most recent two
hours, with a 50-physical-coin timeline cap and a UTF-8 byte ceiling. Its
coverage starts at the **Coin Debug Window** start. Device secrets are
structurally excluded and account usernames are pseudonymised to stable
per-bundle labels. See
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md), refining
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md).

### Coin Debug Window **(decided)**

The time window the **Coin Debug Bundle** covers. Its movable start (the "debug
window start") is in-memory runtime state, initialised to the admin process
start and advanced to "now" when Coin Device settings are saved or the owner
resets it from the Diagnostics Coin tab. It only ever narrows coverage — the
existing two-hour cap still clamps it — and an app restart re-initialises it to
the process start. Read/advanced via `GET`/`POST
/web-admin/coin-device/debug-window`.

### Acceptor Health **(decided S1)**

The owner-facing read-only health contract for the acceptor. It reports the
current **Coin Debug Window** start, the timing status
`unavailable_until_filtered`, and a `null` **Implausible Timing** rate until
filtered inter-pulse timing lands in S2. It is a status object, not a list of
Coin Batches or historical mismatch rows. See
[ADR-0070](adr/0070-credit-the-pulse-sum.md).

### Coin Calibration Capture **(decided)**

The owner-only, local-serial workflow that records physical pulse counts for
an intended denomination without creating a Coin Transaction. It uses the live
Pulse Sampler under the Terminal Selection lock, requires five consistent
individual samples, and changes the pulse map only after explicit owner Apply.
It is bounded, fail-closed, and never awards Guest Time, Points, Coin Promo, or
a Coin Trace Id. See [ADR-0060](adr/0060-coin-20-calibration-capture.md).

### Detected Serial Port **(decided)**

A serial port reported by the Admin host's read-only operating-system scan.
It carries an owner-facing name, a descriptive label, and a `detected` flag.
The saved COM Port remains in the choice list when absent from the scan and is
then marked undetected; a scan failure is distinct from a successful empty
scan. The Simulator is not a Detected Serial Port — it is a Web Admin choice
for an empty COM Port value.

### Coin Source **(decided)**

The abstraction that produces **Coin Batches**: a real implementation (the
serial acceptor on the COM port) and a simulator implementation for
hardware-free testing. Both feed the same **Terminal Selection** path, so the
simulator credits the active selection's PC rather than an arbitrary `pc_id`.
If the real source cannot open or drops, the terminal shows "out of order" and
refuses to open a window (fail-closed). This is the **hardware** sense (the
producing abstraction); the **wire-trust** sense an individual coin event
carries is a **Coin Source Claim**. See
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md) and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Coin Source Claim **(decided)**

The trust attestation a coin event carries — *who* says this coin is real and
may use this device id. Distinct from **Coin Source** (the hardware
abstraction): a claim also covers the remote HTTP coin device, which has no
polling loop. Three variants — the trusted local **serial terminal**, the
**simulator** (gated by the simulator-enabled setting), and a **remote device**
bearing a device id + secret. The claim is built at the request edge and
**authenticated inside the coin-event transaction**, so authentication and the
money mutation stay atomic. The HTTP edge can construct only the remote-device
variant — it can never claim the local serial acceptor — so a network caller
cannot impersonate it (spoof-guard). Its secret is redacted by debug
formatting and must never reach diagnostic logs or a **Coin Debug Bundle**. See
[ADR-0033](adr/0033-typed-coin-source-trust-and-coin-outcome.md),
[ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md), and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Coin Outcome **(decided)**

The routed *consequence* of a coin event, computed as one pure value from the
routing target (account vs guest), amount, prior carry, and economy: **seconds
added** (always, the total credited) plus **points earned**, **new carry**, and
**Coin Promo** bonus seconds (accounts only — guests earn zero,
ADR-0008/ADR-0019/ADR-0052). The database layer persists an
already-decided outcome; deciding *who* the coin is for (the routing target) is
resolved separately in the session layer under the binding lock, because the
active session can change between read and commit. See
[ADR-0033](adr/0033-typed-coin-source-trust-and-coin-outcome.md),
[ADR-0008](adr/0008-coin-routing-central-terminal.md), and
[ADR-0019](adr/0019-points-earn-carry.md).

### Audit Log **(decided)**

The single **append-only** event stream that is the source of truth for
who-did-what *and* for financial reporting (the ADR-0014 reports are a typed
view over it). Strongly typed columns (event_type, **actor**, account/PC,
money/time/points deltas, cash flag) plus metadata. Coin metadata may carry a
**Coin Trace Id** that joins the audit row to diagnostic records and the linked
coin transaction. Records successes **and**
failures/denials, with honest distinct session-end reasons (logout vs auto_kick
vs taken_over vs expired). No app-side edit/delete; retained forever; CSV
export. See [ADR-0015](adr/0015-audit-log.md) and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Actor **(decided)**

Who/what initiated an audit event: a customer **account**, the **admin**, the
**coin device**, or **system** (timer/auto-kick/takeover/expiry). The field that
keeps "login/logout" honest. See [ADR-0015](adr/0015-audit-log.md).

### Actor Account **(decided)**

The **Account** that authenticated an action. It is distinct from the target
**Account** where the action landed. The Audit Log's `actor_account_id` field
backs this relationship and is nullable for admin, system, and coin-device
Actors, or when historical attribution is unavailable. In audit summaries, the
relationship is surfaced only when the Actor Account differs from the Target
Account. A deleted Actor Account still renders by username; a recorded but
unresolvable Actor Account renders as **(unknown account)** rather than being
omitted. An Actor Account is never substituted with the target Account.

### Target Account **(decided)**

The **Account** where an action landed, which may differ from the Actor
Account. The Audit Log's `account_id` field backs this relationship and is
nullable when an action has no Account target.

### Diagnostic Log **(decided)**

Developer-facing, high-volume, **disposable** logs via `tracing` to rotating
files (~7-day cap) — kept *separate* from the Audit Log; the interleaved
"show developer logs" viewer ADR-0016 proposed was superseded by the
**Support Bundle** before it was built. Coin-path edges, batches, port
health, Set Pin transitions, and commit rejections are structured records;
raw pin samples remain DEBUG. The owner can publish a bounded **Coin Debug
Bundle** from them. On diskless clients, meaningful
diagnostics are forwarded to the server as **Forwarded Diagnostics** (local
files evaporate on reboot). See
[ADR-0016](adr/0016-diagnostic-logging.md),
[ADR-0078](adr/0078-support-bundle.md), and
[ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

### Cash Revenue **(decided)**

The reporting top line: **cash that entered the shop** — coins through the
terminal plus `sold` vouchers. Never blended with free time given away. See
[ADR-0014](adr/0014-reporting-model.md).

### Free Time Dispensed **(decided)**

A separate reporting cost line: time given away without cash — `promo` vouchers,
points redemptions, wheel winnings, manual admin add-time, and the **Coin
Promo** bonus seconds granted on an otherwise-cash coin. See
[ADR-0014](adr/0014-reporting-model.md) and
[ADR-0052](adr/0052-coin-promo.md).

### Points Liability **(decided)**

Total unredeemed points across all accounts — the future free-time the shop has
already promised. Tracked on the dashboard as an early-warning number. See
[ADR-0014](adr/0014-reporting-model.md).

### Voucher **(decided)**

An owner-minted, **single-use** code that credits a logged-in account with a
set time and points value. First-to-redeem wins; redemption is atomic and pays
out at most once. Has a **kind** — `sold` (prepaid, counts as revenue) or
`promo` (giveaway, counts as cost) — because reporting must treat them
differently. Single-mint kind is inferred from its cash amount. Its code uses the same
owner-chosen prefix and suffix rules as a Voucher Batch, defaulting to prefix
`OP` and suffix length 8; only the resulting code is persisted. An active
Voucher may be **voided** (retained history, no redemption, no automatic
revenue reversal). Guests cannot redeem (no account). Expiry is represented as
a nullable timestamp and omitted by default. See [ADR-0009](adr/0009-vouchers.md),
[ADR-0020](adr/0020-voucher-recognition-and-expiry.md),
[ADR-0040](adr/0040-voucher-batch.md), and
[ADR-0042](adr/0042-single-voucher-inference-and-void.md).

### Voucher Batch **(decided)**

A first-class owner-minted group of 1–1000 Vouchers generated in one action.
Each has an immutable, globally unique server-generated **Batch Name** of five
case-sensitive Base62 characters for owner-facing identification, while its
owner-chosen prefix remains the voucher-code prefix. Vouchers share that prefix,
uniform time/points/cash values, and optional expiry. Kind is
inferred: per-card cash amount > 0 → `sold`, 0 → `promo`. Batch codes are
`PREFIX-` plus an owner-chosen number (6–16) of random chars from a 32-char
unambiguous set, dash-grouped every 4; prefix is 2–8 chars A–Z/0–9. Unlike
single-mint sold vouchers, a batch-sold card's cash is recognized **at
redemption**, never at mint. A batch is reopenable for re-print, CSV re-export
(UTF-8 BOM), and redemption progress, and can be **voided** — all still-active
cards become unredeemable in one audited action; redeemed cards are untouched.
Minting audits once per batch, not per card. See
[ADR-0040](adr/0040-voucher-batch.md).

### Voucher Template **(decided)**

The single shop-wide owner-authored HTML+CSS per-card fragment (no JS in v1)
that the print view repeats per Voucher, substituting placeholder tokens
(code, time, points, price, shop name, expiry, prefix). Stored in settings,
edited in web-admin with live preview and reset-to-default. Rendering happens
only in the owner's sandboxed browser frame; no customer surface executes it.
See [ADR-0041](adr/0041-voucher-template.md).

### Owner **(decided context)**

The shop operator — the **trusted** party. Administers the shop through the
web-admin surface and, on a Client, through the PIN-gated backend settings.
Distinct from a **Customer**, who is the untrusted party physically seated at a
PC. There is **no** owner/admin *account* role; owner authority on a Client is
the **Owner PIN**, not an account login.

### Owner PIN **(decided)**

A single secret shared across all PCs that gates a Client's backend settings
(server URL, PC ID, View Mode) and Quit. Stored **hashed** in the admin
`settings` table and edited via web-admin. Verified **server-side**; the
verifying hash is cached locally so the owner is not locked out when the admin
server is unreachable. A weak/brute-forceable PIN is accepted because it gates a
maintenance convenience, not the money/time authority boundary. See
[ADR-0027](adr/0027-client-lock-and-owner-pin-gate.md).

A never-configured Client has one narrow bootstrap exception: before its first
configuration is persisted, only the admin server URL needed to establish the
initial connection is accessible without the Owner PIN. The exception does not
expose View Mode or Quit; all later backend settings access is PIN-gated.

This is **not** the **Web Admin PIN**, which gates the `:8080/admin` owner web
surface. The Owner PIN gates Clients; the Web Admin PIN gates the owner console.
First-run setup seeds the Owner PIN from the Web Admin PIN; subsequent changes
to either PIN are independent.

### Account Login Throttle **(decided)**

The in-memory brute-force protection for customer **Account** login. It is keyed
by the normalized **Username**, not the PC or source IP: ten consecutive failed
logins cause a flat sixty-second lockout, and a successful login clears the
state. Unknown Usernames are never bucketed. This is distinct from the **Web
Admin PIN** throttle, which is keyed per source IP, starts after five failures,
and uses escalating backoff windows. See [ADR-0061](adr/0061-account-login-throttle.md).

### Web Admin PIN **(decided)**

The credential (`owner_credentials`) that gates login to the owner web admin
surface at `:8080/admin` (ADR-0024). Stored **hashed**; set on first run via
`POST /web-admin/setup`, which refuses to overwrite an existing PIN
(`Conflict`). An authenticated owner can rotate it via `POST
/web-admin/credential` by proving the current PIN; the caller's session survives
while every other owner session is revoked (ADR-0050). A forgotten PIN still
requires wiping the credential and repeating first-run setup. Login (`POST
/web-admin/login`) is per-IP rate-limited with exponential backoff so the secret
cannot be brute-forced over the LAN.

This is **not** the **Owner PIN**: the Owner PIN (`settings.owner_pin`, ADR-0027)
gates a Client's backend settings and is shared across PCs; the Web Admin PIN
gates only the owner web console and lives in a different table. First-run setup seeds the Owner PIN from the Web Admin PIN; subsequent
changes to either PIN are independent.

### Owner Session **(decided)**

The authenticated browser session that carries owner authority across the Web
Admin. It expires after 30 minutes without human input or after 12 hours
absolute; polling and framebuffer traffic do not count as human input.
See [ADR-0074](adr/0074-owner-session-hardening.md).

### Shop Hours **(decided)**

The whole shop-hours feature: the master enabled flag, seven day schedule rows,
and configurable **Last Call** lead time. It determines the scheduled **Shop
Open** and **Shop Closed** state, but the computed state is **Shop Status** and
a temporary force is a **Shop Override**. See [ADR-0053](adr/0053-shop-hours.md).

### Open Period **(decided)**

One day's `open time → close time` interval in **Shop Hours**. It may cross
midnight; that day owns its tail into the next calendar day. A day marked
closed all day has no Open Period starting on it, but it does not truncate a
prior day's tail. This is deliberately not a "window": **Selection Window** is
reserved for the coin-terminal concept in ADR-0025. The bare verbs *open* and
*close* belong to Selection Window operations, not this domain term. See
[ADR-0053](adr/0053-shop-hours.md).

### Shop Override **(decided)**

A temporary owner-set force-open or force-closed state with an absolute expiry
instant. It wins over the schedule until expiry, then the schedule resumes.
It is not sticky and does not expire merely at the next scheduled boundary. See
[ADR-0053](adr/0053-shop-hours.md).

### Shop Status **(decided)**

The computed `{ open, next_change_at, reason }` value every surface renders.
It combines the enabled schedule, **Open Period**, **Last Call** context, and
any active **Shop Override**. See [ADR-0053](adr/0053-shop-hours.md).

### Last Call **(decided)**

The configurable period before a scheduled shop close during which buying still
works while customer surfaces warn that the shop will close soon. Last Call
does not end an **Active Session** or change a balance. See
[ADR-0053](adr/0053-shop-hours.md).

### Shop Open / Shop Closed **(decided)**

The customer-facing states of a shop under **Shop Hours**. Always write them
with the **shop** qualifier; never use bare *open* or *closed* for shop state.
The bare verbs *open* and *close* belong to **Selection Window** operations,
not Shop Hours vocabulary. See [ADR-0053](adr/0053-shop-hours.md).

### Shop Name **(decided)**

The owner-configurable display name of the shop, stored in the admin `settings`
table and edited via web-admin. Clients read it over the public read-only
endpoint (`GET /api/shop`); the Central Terminal reads it in-process from the
admin-owned database. Separate from PC/timer state.

### Fullscreen Lock **(decided)**

The blocking, undecorated, always-on-top Client screen shown whenever the
customer must not use the PC (logged-out / expired / no usable time /
`power_state = shutdown`). Generalizes the earlier expired-overlay concept into
the primary logged-out screen. It is **genuinely blocking**: no
customer-reachable Quit and no minimize-to-tray, and it re-asserts on focus loss.
Usable Guest Time normally auto-enters the persisted **View Mode**; **Account**
authentication lives here, and an explicit Guest **Log in** route can return here
without consuming Guest Time. See
[ADR-0027](adr/0027-client-lock-and-owner-pin-gate.md) and
[ADR-0029](adr/0029-client-window-model.md).
_Avoid_: Overlay, expired screen.

### Widget **(decided)**

The small draggable floating Client window shown once the customer has entered a
session with usable time. Hosts account self-service (claim, voucher, points,
transfer, change password, spin) plus a "Log in" shortcut back to the Lock for a
guest who wants to upgrade. See
[ADR-0029](adr/0029-client-window-model.md).

### Mini Bar **(decided)**

The smallest Client window — remaining time only, top-center of the current
monitor, dim until hover. A shrunk, mutually-exclusive form of the **Widget**.
See [ADR-0029](adr/0029-client-window-model.md).

### View Mode **(decided)**

The customer's persisted choice between **Widget** and **Mini Bar** while time
is active (default Widget). Persisted per Client in `ClientConfig`.

### Transfer **(decided)**

A customer-initiated move of value from their own **Account** to another Account
by **Username**. Split by currency:

- **Time Transfer** — Remaining Time (`seconds`). **Immediate**: atomic,
  server-authoritative, no owner step. Recorded in `account_transfers`.
- **Points Transfer** — **Points**. **Owner-confirmed**: creates a **Points
  Transfer Request** that the owner must approve before points move.

Self-transfer and transfers to deleted/missing accounts are rejected
server-side; balances never go negative; self-drain to zero is allowed (normal
expiry follows). Both currencies may move in one customer action, but the points
portion only settles on owner confirmation. See
[ADR-0030](adr/0030-account-transfers.md).

### Points Transfer Request **(decided)**

A pending, owner-confirmed points transfer. Holds sender, recipient, points, a
**single-use, time-boxed 6-digit code**, and a status (`pending → confirmed |
rejected | expired`), persisted in a `points_transfer_requests` table. The
customer initiates it and reads the code off their screen; the owner approves
*that code* in web-admin, at which point points are re-validated and moved
atomically. The code ties the physically-present customer to the right request.
See [ADR-0030](adr/0030-account-transfers.md).

### Kiosk Lockdown **(decided)**

The OS-level confinement of a Client PC to the Client app, overriding the prior
MVP "no OS-hardening" stance (with explicit owner approval). Delegated to
Windows **Assigned Access / Shell Launcher v2** (owner machine provisioning),
with app-level soft hardening layered on top: keyboard-hook suppression of
Win/Alt-Tab/Alt-Esc/Ctrl-Esc under the Lock, re-assert-on-blur, and
**window-level prevention** — no taskbar button (`skipTaskbar`), a refused
`WM_CLOSE` unless a legitimate exit is already in flight, and Alt+F4 blocked in
every view mode while the Client is the foreground window. Ordinary window
levers therefore cannot close the Client in an installed release build; Task
Manager still can, and that is the **Client Guard**'s job. The **Owner PIN**
must remain a working escape even with lockdown fully on. See
[ADR-0028](adr/0028-client-kiosk-os-hardening.md).

### Client Guard **(decided)**

The Windows service that keeps the Client running on a kiosk PC. Runs as SYSTEM
so the seated customer (a standard user) cannot stop it, is registered and
started by the Client's own installer, and holds a named pipe the Client keeps
open for its lifetime — a dropped pipe is process death. It relaunches the
Client, classifies the death, and never permanently gives up. A PIN-gated owner
quit stands it down until the Client next runs or the machine reboots. See
[ADR-0048](adr/0048-client-guard-tamper-kill.md).

### Client Guard Coverage **(decided)**

The diagnostic fact that an installed release Windows Client currently holds
its connection to **Client Guard**. Missing coverage means protection is
degraded; it does not prove tampering and grants no authority to the Client.
See [ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Tamper Kill, Client Reachability

### Kill Report Age **(decided)**

The non-negative whole-millisecond duration between a **Tamper Kill** and the
Client sending its pending heartbeat report, derived from two readings of the
Client clock. The Admin subtracts it from report receipt time to derive the
kill instant; `0` anchors at receipt. It has a 24-hour sanity limit, not a
freshness limit. It is sent only with a pending report and is never persisted,
so every retry and post-reboot report derives a fresh age. It is a protocol-only
diagnostic, explicitly exempt from integer-second authority durations, and
never changes balances or timers. See
[ADR-0048](adr/0048-client-guard-tamper-kill.md).

### Local Tamper Kill handoff **(decided)**

The durable local boundary that lets a launch of the Client that will not
become the runtime (a losing duplicate under startup arbitration, or the
explicit sender-only mode) hand a validated **Tamper Kill** report to the
already-running Client. The sender validates positive integer arguments at
the Rust boundary, atomically writes one identity-named file into the
**handoff inbox**, and exits with a truthful code; the running Client drains
the inbox, persists the merged pending report before removing inbox files,
and delivers it through the existing Heartbeat acknowledgement path — a
local accept means durably recoverable, never Admin-acknowledged. Repeated
delivery of one report is recognized by the **Delivery ledger**, so it never
inflates the attempt count. Arguments and inbox files are hints, never
authority; attribution stays the server's.
See [ADR-0079](adr/0079-client-local-tamper-kill-handoff.md).

### Delivery ledger **(decided)**

The bounded record — at most 64 identities, stored in the Client's
authoritative `tamper-kill-report.json` alongside the pending report — of the
**Local Tamper Kill handoff** deliveries already folded into that report. A
delivery's identity is the timestamp/attempt pair the Guard handed over, not
the coalesced report the Client holds, so the ledger recognizes a retry even
after distinct reports coalesced, after a restart, and after Admin
acknowledged the report — the acknowledgement clears the report and keeps the
identities. Genuinely distinct reports still coalesce under the existing
oldest-timestamp-wins rule. See
[ADR-0079](adr/0079-client-local-tamper-kill-handoff.md).

### Startup arbitration **(decided)**

The one-time, atomic decision at the top of the Client launch boundary —
before the startup placeholder, keyboard hooks, Guard connection, Tauri
runtime, state writes, or networking — that admits exactly one **Client
runtime** per shop PC. Implemented as a machine-wide, lifetime-scoped
ownership object (`Global\OpenPiso-Client-Runtime`) whose handle is
non-inheritable and is released by the OS at owner-process death. Three
distinct outcomes: **admitted owner**, **handled duplicate** (inert, prompt
exit; hands off any carried report through the Local Tamper Kill handoff
first), and **coordination failure** (denied access, incompatible object,
uncertain wait — exit nonzero, never silent admission). Enforced only in
release Windows builds; debug and non-Windows builds stay inert. Supplements
the Client Guard's exact-child liveness gate; a losing launch never touches
the Guard pipe. See
[ADR-0080](adr/0080-client-startup-arbitration.md).

### Handoff Inbox **(decided)**

The bounded directory (`tamper-handoff-inbox/` inside the Client config
directory, at most 32 files of at most 4 KiB) holding validated but not yet
ingested **Local Tamper Kill handoff** reports. One file per delivery, named
by the report identity so a retried delivery rewrites the same file; the
running Client removes a file only after the merged report is persisted, so
a crash at any point leaves the report recoverable. Senders serialize their
capacity check and publish on one interprocess admission lock inside the
directory, so concurrent senders cannot exceed the cap; a sender that cannot
determine capacity fails truthfully instead of admitting a report. See
[ADR-0079](adr/0079-client-local-tamper-kill-handoff.md).

### Tamper Kill Subject **(decided)**

The server-resolved nature of the session present when a **Tamper Kill**
happened. The four values are `account` (an Account session), `guest` (a
Guest session), `none` (no session was active), and `unknown` (attribution
could not be resolved or trusted). The Admin derives the kill instant from
report receipt time minus **Kill Report Age**; the device timestamp is only the
report name and duplicate-detection key. A missing age or an age beyond 24
hours resolves to `unknown`. The Client treats `none` exactly like `unknown`;
the distinction is for the owner's Alert. An `unknown` row also records one
diagnostic reason in `metadata_json`: missing or invalid **Kill Report Age**, or
the refusal to attribute a current occupant when no historical Account Session
covered the kill instant. See [ADR-0048](adr/0048-client-guard-tamper-kill.md).

### Tamper Kill **(decided)**

A Client process death that the **Client Guard** did not receive a clean-exit
message for and that is not an OS shutdown or Windows sign-out (see
**End-Session Grace**). Task Manager "End task" is the
motivating case; an application crash is deliberately classified the same way,
because the Guard cannot distinguish them and a reliably-crashable Client would
otherwise be a free unmetered PC. Recorded as the canonical `tamper_kill` audit
event with a **Tamper Kill Subject** of `account` | `guest` | `none` |
`unknown`. Account attribution is resolved **server-side against Account
Session history at the server-derived kill instant**: report receipt time minus
**Kill Report Age**. A missing age or an age beyond 24 hours resolves to
`unknown`; the device timestamp is only a report name and never an authority.
A valid age with no Account or Guest session resolves to `none`. See
[ADR-0048](adr/0048-client-guard-tamper-kill.md) and
[ADR-0062](adr/0062-account-session-history.md).

### End-Session Grace **(decided)**

The few seconds (about 3) the **Client Guard** waits after an unexplained
Client pipe drop for Windows to report pre-shutdown or session logoff before it
classifies the death. Windows ends the user's processes before telling
services, so the notice trails the death. A notice within the grace makes the
death an OS shutdown, not a **Tamper Kill**. Only the Service Control Manager
delivers the notice; the Client cannot claim one. See
[ADR-0048](adr/0048-client-guard-tamper-kill.md).
_Avoid_: Shutdown Grace Period, Offline Grace

### Tamper Kick **(decided)**

The session end caused by a **Tamper Kill**: the account is logged out and the
PC returns to the Fullscreen Lock, with guests required to explicitly continue.
Balance and points are **untouched** — this is friction, not confiscation.
Recorded as its own `tamper_kick` event type alongside `logout`, `auto_kick`,
`taken_over`, and `expired`, never folded into any of them (see **Audit Log**).
See [ADR-0048](adr/0048-client-guard-tamper-kill.md).

### Manual Adjustment **(decided)**

An owner-initiated change to a PC's Guest Time that is not a direct coin
event. Targets PC Guest Time only; account balance corrections flow through
the typed mutation paths of their own (coin events, vouchers, points
redemption, wheel, soft deletion, the account time and points adjustments, and
a **Rejection Credit**). The applied delta is clamped so Remaining Time never
goes below zero; the canonical `manual_adjustment` audit event records the
actual applied delta for exact report reconciliation, while the requested
delta is kept in metadata. A negative delta is a compensating event, never an
audit-history edit. See [ADR-0069](adr/0069-rejection-credit.md), which
supersedes [ADR-0021](adr/0021-manual-adjustment-target.md).

### Backup **(decided)**

One readable, self-describing `.sql` artifact containing a consistent dump of
selected **Backup Sets**, including schema and migration history. Its header
identifies its **Backup Kind**, contents, versions, row counts, checksum, and
warns that selected credentials and device secrets are included verbatim. A
**Pre-Migration Backup** always includes every **Backup Set**. Its migration
operation must also write a byte-exact database companion; that companion is not
a second Backup or a separate Backup Kind. There is no separate "save" concept:
an owner-triggered save is a Backup of Kind `manual`. Avoid *snapshot* for this
feature because **DashboardSnapshot** already owns
that word, and avoid *window* because **Selection Window** is reserved by
[ADR-0025](adr/0025-terminal-selection-commit-flow.md). See
[ADR-0054](adr/0054-backup-and-restore.md).

### Pre-Migration Backup **(decided)**

The mandatory Backup created immediately before pending migrations are applied.
It has Backup Kind `pre_migration`, covers every **Backup Set**, and also writes
a byte-exact database companion. If either artifact cannot be written, migration
is refused with no bypass: this is the fail-closed rollback boundary for a bad
app update. See [ADR-0054](adr/0054-backup-and-restore.md).

### Backup Set **(decided)**

A named group of database tables that shapes what a Backup dumps: Accounts,
Configuration, Ledger, Vouchers, Terminals, Runtime State, or Credentials.
Selection is an export boundary only and **never applies to restore**. Restore
requires a Backup covering every **Backup Set** and always replaces the whole
database; there is no selective restore. See [ADR-0054](adr/0054-backup-and-restore.md).

### Backup Kind **(decided)**

The reason a Backup was created. The only values are `manual`, `scheduled`,
`pre_migration`, and `pre_restore`. An owner-triggered save is therefore a
Backup with Kind `manual`, not a separate save operation. See
[ADR-0054](adr/0054-backup-and-restore.md).

### Backup Schedule **(decided)**

The owner-configured unattended Backup policy: enabled state, Daily/Weekly/
Monthly frequency, local run time and applicable day, selected Backup Sets, and
scheduled-retention count. Retention is independent by Backup Kind: scheduled
uses this count, Pre-Migration and Pre-Restore keep 10 each, and manual Backups
are never pruned automatically. It is a routine safety net, not the migration
safety boundary; the **Pre-Migration Backup** remains mandatory and independent
of its Backup Set selection. See [ADR-0054](adr/0054-backup-and-restore.md).

### Backup Catalog **(decided)**

The owner-facing listing of canonical `.sql` Backups derived by scanning the
primary Backup filesystem directory and parsing each file's header. It is not a
database table and the filesystem, not SQLite metadata, is its source of truth. See
[ADR-0054](adr/0054-backup-and-restore.md).

### Mirror Directory **(decided)**

An optional secondary filesystem destination to which a successfully written
Backup is copied. A failed mirror copy never invalidates the primary Backup or
the Pre-Migration safety boundary; the owner receives a standing advisory with
the filesystem error. See [ADR-0054](adr/0054-backup-and-restore.md).

### Staged Restore **(decided)**

A candidate replacement database created by validating a Backup, replaying its
SQL into a temporary file, forward-migrating it to the current schema, and
verifying integrity while the live database remains untouched. A Staged Restore
is not live until the next launch applies it. See
[ADR-0054](adr/0054-backup-and-restore.md).

### Pending Restore **(decided)**

The durable marker for a verified **Staged Restore** that will be applied on the
next launch. Applying it first sets the outgoing database aside as a Backup of
Kind `pre_restore`, then swaps in the staged database and clears the marker. A
Pending Restore may be cancelled before launch. See
[ADR-0054](adr/0054-backup-and-restore.md).

### Notification **(decided)**

An owner-facing, 30-day derived view of an **Audit Log** row whose fixed
classifier returns **Alert** or **Notice**. The audit row remains the only event;
a Notification is not a separate event or table. The fixed classifier remains
the sole authority; this taxonomy rule is not a runtime view check: a recurring
transition whose current value is already visible in a live Web Admin view is
**Silent** when it only echoes that state. Notifications carry discrete events,
not state. PC Reachability Events are **Silent** because current reachability is
already visible in the PC grid, while recurring `shop_opened` and `shop_closed`
events remain Notices as discrete shop-hours events. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md).
_Avoid_: action toast, OS notification

### Alert **(decided)**

A Notification for money, abuse, or tamper that remains active until its Audit
Log row is explicitly acknowledged. Acknowledgement records review only; it
performs no remediation and changes no money, time, or Points. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md).

### Notice **(decided)**

A Notification worth owner awareness but not explicit per-row action. Notices
become read together when the global high-water mark advances; they are never
individually acknowledged. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md).

### Alert Acknowledgement **(decided)**

The durable, idempotent record that the owner reviewed one Alert, identified by
its Audit Log id. Migration baseline rows make pre-upgrade Alerts read without
claiming an owner action. Every repeated incident needs its own acknowledgement,
so a bulk acknowledgement is only a batch of per-Alert acknowledgements over the
Alerts the owner had listed — never a server-side sweep. There is no note,
grouping, or implied remediation. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md).

### Coin Device Availability Event **(decided)**

An owner-visible Audit Log event at the real Coin Source's availability edge:
`coin_device_unavailable` records one outage and `coin_device_recovered` records
its recovery. Retry attempts are not separate events. Both are Notices; the
simulator has no device-availability events. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md).

### Coin Sampling Anomaly **(decided)**

A rate-limited, aggregate Audit Log Notice for Inhibited Pulses. Its `reason`
identifies one of two observable states: the Coin Source was unavailable or
recovering, or the sampler was healthy while no Selection Window was open and
the Set Pin was inhibited. The healthy state names idle insertion and inhibit
wiring as possibilities rather than claiming a distinction software cannot
observe; its Coin Debug Bundle anomaly retains the reason and timestamp for
field feedback. The summary reports the bounded count as refused pulses,
never coins or pesos. The event carries no PC or Account because the empty
buffer provides no truthful Selection Window attribution for that refused edge,
including during close or Coin Source recovery. Raw pin samples and individual
noisy pulses remain Diagnostic Logs. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md) and
[ADR-0064](adr/0064-owner-visible-coin-loss-notices.md).

### PC Reachability Event **(decided)**

A server-observed transition in **Client Reachability**: `pc_went_offline` records
`online`/`in_use` becoming `offline`, and `pc_came_online` records the next
accepted heartbeat restoring reachability. Both are **Silent** (ADR-0058,
amended 2026-08-09): reachability is already visible live in the PC grid, so
these rows never surface as Notifications, but they are emitted once per
transition and retained as server-visible diagnostic evidence. The Offline
Shutdown Countdown relies on Client-local connection state; Client Guard relies
on its held pipe. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md).

### Host Reachability **(decided)**

Evidence that the Admin can reach a registered PC host at its registered
address. It says the host and network path responded, not that the Client is
running or healthy. See [ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Online, Client Reachability

### Host Probe Coverage **(decided)**

The diagnostic fact that Admin can test a PC's **Host Reachability** while that
PC's Client independently proves the host is running. Missing coverage means
future mismatch detection is degraded; it does not mean the host is offline.
See [ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Host offline, Client Reachability Mismatch

### Client Reachability **(decided)**

The server's current knowledge that a registered PC's Client is reporting to
the Admin. It is distinct from **Host Reachability**: either can exist without
the other. See [ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Host Reachability

### Client Reachability Mismatch **(decided)**

The incident raised when a PC remains **Host Reachable** but not **Client
Reachable** beyond the monitoring grace period. It is evidence for owner review,
not proof of bypass, abuse, or a **Tamper Kill**. See
[ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Client Bypass, Tamper Kill

### PC Monitoring **(decided)**

The Admin-owned observation of **Host Reachability**, **Client Reachability**,
**Host Probe Coverage**, and **Client Guard Coverage** for visible registered
PCs. It detects and reports degraded coverage but never changes PC, session,
money, time, or Points authority. See
[ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Tamper prevention, Client authority

### PC Monitoring Incident **(decided)**

One durable episode of degraded **PC Monitoring** coverage for one PC. Its kind
is **Client Reachability Mismatch**, **Host Probe Coverage**, or **Client Guard
Coverage**; opening is an Alert and closure is a Notice. A closure names its
reason: an accepted heartbeat (`Client recovered`), one successful probe after
coverage loss (`probe recovered`), restored Guard coverage (`guard
recovered`), loss of the evidence sustaining the episode (`Client recovery
unconfirmed`), or shop-wide monitoring disabled. An Admin restart over a
still-open episode marks it observation-interrupted rather than claiming
continuous observation. See
[ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Tamper Kill, Notification

### Client Guard Coverage **(decided)**

The diagnostic fact that a release Windows Client currently holds its Client
Guard pipe connection, reported with each Heartbeat as covered or unavailable.
Debug and non-Windows Clients report the check as not applicable; an older
Client that omits the field is unsupported or unknown and never creates an
Alert. Two continuous minutes without coverage opens one durable **PC
Monitoring Incident**; restored coverage closes it with one Notice. The
diagnostic grants no identity, session, money, time, Points, or routing
authority and is never merged with a **Client Reachability Mismatch**. See
[ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Guard authority, Tamper prevention

### Guard Poll **(decided)**

The Client Guard's own outbound report to the Admin, identified by MAC on the
client/device surface. It carries Guard liveness, whether a verified Client
currently holds the Guard pipe, whether the Guard is disarmed for this boot,
and the relaunch attempts spent on the current absence, and it reads back the
desired power state for that PC. It is the only channel from Admin to a Guard;
the Guard opens no listener and accepts no inbound connection. See
[ADR-0077](adr/0077-client-recovery-escalation.md).
_Avoid_: Heartbeat, command channel, remote control

### Client Recovery Escalation **(decided)**

The shop-wide, owner-enabled policy that a PC whose Client cannot be recovered
locally is powered off. It applies only while the PC is **Host Reachable**, not
**Client Reachable**, its Guard is polling, that Guard reports no connected
Client and at least one failed relaunch, and the PC is neither suppressed nor
disarmed. Its Escalation Delay is a second clock independent of the mismatch
grace. Disabled after upgrade and changed only with the current Owner PIN. See
[ADR-0077](adr/0077-client-recovery-escalation.md).
_Avoid_: Tamper response, Shutdown Grace, Offline Shutdown Countdown

### Escalated Power Off **(decided)**

The single action **Client Recovery Escalation** takes: the Guard powers its PC
off, the Admin ends that PC's session under the `escalated_power_off` end type,
and the open **Client Reachability Mismatch** closes with the same reason. It
is an Alert, not an accusation, and it never changes balance, time, or Points.
At most one occurs per PC per hour. See
[ADR-0077](adr/0077-client-recovery-escalation.md).
_Avoid_: Tamper Kick, Tamper Kill, expiry power-off

### Monitoring Suppression **(decided)**

An owner-authorized, automatically expiring pause on new **PC Monitoring**
incidents for one PC. It does not erase or close an incident already open. See
[ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Monitoring disable, incident acknowledgement

### Telegram Alerts **(decided)**

The optional, outbound-only delivery of **PC Monitoring** incidents to one
private owner chat through a BotFather bot. The bot token is protected at rest
with Windows DPAPI under the Admin Windows identity and is never returned,
logged, or backed up; the whole integration is excluded from Backups, so a
restored Admin starts disconnected. Connecting, replacing, or disconnecting
requires an authenticated Owner Session plus the current Owner PIN, and a
replacement becomes active only after a successful test message. There is no
bot command surface, update poller after setup, webhook, inbound port, group
recipient, or second recipient. See
[ADR-0076](adr/0076-client-reachability-mismatch-monitoring.md).
_Avoid_: Remote control, bot commands, chat notifications

### Backup Schedule Event **(decided)**

An owner-visible Audit Log Notice about unattended Backup execution:
`backup_schedule_failed` records a failed due attempt, while
`backup_schedule_caught_up` records a successful startup catch-up for a due time
missed during Admin downtime. The Backup file and its catch-up header remain the
source of truth for filesystem history. See
[ADR-0058](adr/0058-audit-derived-web-admin-notifications.md).

### Support Bundle **(decided)**

The owner-requested, redacted Markdown diagnostic document spanning one PC's
**Client**, **Client Guard**, and **Admin** evidence for one time window or one
anchored **PC Monitoring Incident**. It opens with the stored PC facts, then a
**Verdict** naming the matched failure shape in plain language (or stating
plainly that none matched), a merged timeline, an anomaly list, and a
whitelisted raw appendix; truncation by the size cap and dropped Forwarded
Diagnostics are stated, never silent. Device secrets are structurally excluded
and account identities are pseudonymised to stable per-bundle labels. Distinct
from the **Coin Debug Bundle**, which stays its own term and its own artifact:
the Coin Debug Bundle covers the coin path over the current app run and the
Coin Debug Window, while the Support Bundle covers PC health across Client,
Guard, and Admin. Owner Session only — no PIN step-up, no clipboard copy. See
[ADR-0078](adr/0078-support-bundle.md).
_Avoid_: Coin Debug Bundle, log dump, Logs screen

### Diagnostic Forwarding **(decided)**

The always-on push of meaningful Client and Client Guard log lines to Admin as
they are produced, so the evidence survives the PC rebooting, the Client dying,
or the PC being powered off. Release-only in shipped builds, with the
`OPENPISO_FORWARD_DIAGNOSTICS=1` environment variable as the explicit
development opt-in; the Admin sink and the Support Bundle builder are
unconditional. Lines are ack-gated in a bounded device-side buffer: a batch
that fails to send is retained for the next attempt, and overflow drops oldest
and emits an explicit drop marker. An unprovisioned PC forwards nothing rather
than failing. See [ADR-0078](adr/0078-support-bundle.md).
_Avoid_: telemetry (nothing leaves the LAN), log shipping, uploading whole
retained log sets

### Forwarded Diagnostic **(decided)**

One log line that passed the forwarding allowlist — WARN and ERROR
unconditionally, plus a named set of INFO events (Guard pipe connect and
disconnect, Client Guard Coverage transitions, Heartbeat failure, Client
startup milestones, Offline Shutdown Countdown arm and disarm, Tamper Kill)
under a rate cap — and was accepted by Admin. It is stored tagged with its PC
and a source discriminator (`client` or `guard`) in Admin's dedicated rotating
diagnostic files, never in SQLite. See
[ADR-0078](adr/0078-support-bundle.md), closing
[ADR-0016](adr/0016-diagnostic-logging.md)'s open forwarding consequence.
_Avoid_: Audit Log event (Forwarded Diagnostics never enter it), Diagnostic
Log (that is the local tracing store on the device)

### Diagnostic Request **(decided)**

The single-use, nonce-carried order Admin places on a Heartbeat response
asking a live Client to upload one windowed, byte-capped slice of its full
log. The Client acts exactly once per nonce, and truncation of the slice is
stated in the resulting Support Bundle. It tops up Diagnostic Forwarding when
the allowlisted lines were not enough; it is not a full-log upload and cannot
reach a PC that is off or unreachable. The Guard has no Diagnostic Request —
its forwarding rides the poll it already makes. See
[ADR-0078](adr/0078-support-bundle.md).
_Avoid_: remote log pull as a command channel, Heartbeat order (it rides the
Heartbeat response but carries no session, power, or routing authority)

### Launch Token **(decided)**

The eight-hex-character, per-launch discriminator every meaningful Client
lifecycle line carries alongside the process ID (`process_id=… launch=…`),
so Support Bundle evidence can separate process lifetimes even when Windows
reuses a PID — a reused PID never implies one continuous process. Derived
once per Client launch; diagnostic context only, never identity,
authentication, or routing authority. Lines from before the convention lack
the fields and still render. See [ADR-0078](adr/0078-support-bundle.md).
_Avoid_: session ID, device ID, machine identifier
### Diagnostics **(decided)**

The single owner surface for evidence about the running system: a top-level Web
Admin page holding the **Support Bundle** and **Diagnostic Request** (Client
tab), the **Coin Debug Bundle** and Coin Debug Window (Coin tab), and Admin's
own **Run Records** and panic evidence (Server tab). It replaces the Settings
**Support** section, which retires; `/admin/settings/support` redirects here.
It is one route with tabs, never sub-routes, and it holds no configuration —
the Coin Device settings, Pulse Map, Coin Calibration, and Acceptor Health stay
in Settings. Visible only while **Diagnostics Mode** is on — the route always
resolves, naming Settings → Security when it is off. See
[ADR-0081](adr/0081-diagnostics-surface.md).
_Avoid_: Debugging (the surface is Diagnostics everywhere — page, URL, code,
and logs), Logs screen, developer tools

### Diagnostics Mode **(decided)**

The shop-wide, SQLite-persisted flag that reveals the **Diagnostics** page in
the Web Admin. It is a **visibility flag and nothing else**: Admin and the
Client capture identical evidence whether it is on or off, so an incident is
explainable after the fact without anyone having reproduced it with a switch
flipped. Set in Settings → Security, audited on every change, and surfaced by a
standing inline advisory while on. It is not a security boundary — the
underlying endpoints serve any authenticated Owner Session — and not a capture,
verbose-logging, or developer mode. See
[ADR-0081](adr/0081-diagnostics-surface.md).
_Avoid_: debug mode, verbose logging, developer mode, safe mode

### Run Record **(decided)**

One Admin process lifetime: app version, PID, start time, and — once the
process exits through its own handler — end time and **Exit Reason**. Records
are kept as a fixed-size rolling set of the newest few in the Admin data
directory, outside the rotating log directory, so the ADR-0016 retention sweep
never removes them. They answer "did it restart, and how often" without parsing
logs. Admin only: the Client keeps a panic hook but no Run Records. See
[ADR-0082](adr/0082-admin-run-records.md).
_Avoid_: session (that is a customer's paid time), uptime log, crash report

### Exit Reason **(decided)**

Why one **Run Record** ended: an owner or OS closing the window, an
application-initiated exit, a Rust panic, or — when a record has a start and no
end — **suspected hard exit**, meaning the process died without reaching its
own shutdown path (a WebView2 crash, an external kill, or power loss). Suspected
hard exit is an absence of evidence stated plainly, never a claim about the
cause. See [ADR-0082](adr/0082-admin-run-records.md).
_Avoid_: crash (only the panic and suspected-hard-exit cases are crash-shaped),
close reason (that is the PC power-off vocabulary)

### Telemetry **(decided)**

The always-on sending of one anonymous **Daily Summary** per reporting
day from a token-bearing Admin to the OpenPiso project's PostHog Cloud
(US). It is the only data that leaves the shop for the project, never carries customer, money,
time, session, or identifying shop data, and is unavailable in any build made
without the project token. The owner cannot opt out in a token-bearing build.
PostHog sees the source IP in transit before discarding it; past events cannot
be individually erased. No owner-facing Telemetry controls are offered. See
[ADR-0083](adr/0083-install-telemetry.md).
_Avoid_: analytics, usage tracking, Diagnostic Forwarding (that stays on the
LAN)

### Installation ID **(decided)**

The random identifier Admin generates on its first Daily Summary in a
token-bearing build, sent as the summary's `distinct_id`. It is never derived
from hardware, network, shop, or owner data, is excluded from backups, and is
not owner-resettable. No Reset or Disable controls are offered. See
[ADR-0083](adr/0083-install-telemetry.md).
_Avoid_: device ID, machine ID, shop ID, `pc_id`

### Daily Summary **(decided)**

The single `admin_daily_summary` event Admin sends under **Telemetry** for one
reporting day: a fixed UTC+08:00 (Asia/Manila) calendar day, the same boundary
for every shop regardless of the Admin host's timezone. It is taken at Admin's
first opportunity on that day, counts Clients from recent Heartbeats, and
carries only the closed allowlist in ADR-0083. A day the Admin never runs has
no Daily Summary. See [ADR-0083](adr/0083-install-telemetry.md).
_Avoid_: report (Reports are owner-facing), usage log, heartbeat summary

### Active Installation **(decided)**

An **Installation ID** that sent a **Daily Summary** for a given reporting day.
It means a token-bearing Admin was running that day — not that the owner used
the Web Admin or that anyone used a PC. A reinstall with a fresh database is
a new installation. See
[ADR-0083](adr/0083-install-telemetry.md).
_Avoid_: active shop, active user

### App Update **(decided)**

Installing a newer stable release of the Admin app (Central Terminal, web
admin, and server together) over the installed one, initiated by the Owner.
Only stable releases are offered; there is no release channel choice. Client
installs are not App Updates. See [ADR-0086](adr/0086-admin-app-update.md).
_Avoid_: upgrade, patch, auto-update

### Compatible Rollback **(decided)**

Installing an older stable Admin release whose **Schema Version** equals the
current database's. Releases with a different Schema Version are never offered;
crossing a schema boundary backwards is only possible by restoring a
Pre-Migration Backup. See [ADR-0086](adr/0086-admin-app-update.md).
_Avoid_: downgrade, revert

### Schema Version **(decided)**

The highest database migration number an Admin release knows. A database whose
applied migrations exceed it cannot be opened by that release.
