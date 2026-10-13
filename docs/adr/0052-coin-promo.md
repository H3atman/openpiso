# ADR-0052: Coin Promo — account-only bonus time per coin

- Status: Accepted
- Date: 2026-07-29
- Deciders: shop owner (product)
- Relates to: [ADR-0003](0003-points-economy.md) (earn is account-only, the
  Effective Bonus % invariant), [ADR-0008](0008-coin-routing-central-terminal.md)
  (routing decides the target), [ADR-0014](0014-reporting-model.md) (cash vs
  giveaway split), [ADR-0033](0033-typed-coin-source-trust-and-coin-outcome.md)
  (Coin Outcome is one pure value), [ADR-0039](0039-redemption-tiers.md)
  (owner-authored exchange rows)

## Context

Points are the only account-only incentive the shop has, and they pay out
later — the customer must accumulate, then redeem. The owner wants an
incentive that lands **on the coin**, immediately and visibly: insert ₱5 while
logged in and get extra minutes right now.

ADR-0003 already establishes the principle this rides on: points require an
account, guests earn zero, and a later **Claim** never back-credits. That is a
permanent, intentional incentive to log in *before* paying. A per-coin bonus is
the same incentive expressed in time instead of points.

Nothing in the model currently distinguishes a coin's *paid* time from *given*
time. ADR-0014 mandates that time given away without cash is reported
separately from cash taken in. A bonus attached to a cash coin makes a single
coin event straddle both lines for the first time.

## Decision

### Coin Promo is additive bonus time, never a replacement rate

A **Coin Promo Tier** maps an exact coin amount to whole bonus seconds. The
base rate is untouched: a ₱5 coin at ₱1 = 5 min still buys 25 minutes, and a
`₱5 → +10 min` tier credits 35 minutes total. Paid time and bonus time are
computed separately and always remain separable.

Representing the promo as a replacement price was rejected: it makes it
possible to price a logged-in customer *worse* than a guest with one careless
row, and it destroys the paid/given distinction ADR-0014 requires.

### Exact amount match only

A coin gets a bonus only when its amount equals a Coin Promo Tier's amount
exactly. There is no nearest-lower fallback and no greedy multiplication. A
denomination with no tier gets no bonus.

Coin denominations are a small fixed set fixed by the pulse map, so a missing
amount is a configuration gap the owner closes by authoring a row — not a case
the pricing rule should guess at. Fallback matching would let a larger coin
earn the same bonus as a smaller one; multiples would let one generous row
compound into a payout the owner never pictured.

### Accounts only, decided at insertion

Coin Promo applies only when the coin's routing target is an **Account**. The
target is the one already resolved under the session binding lock (ADR-0008,
ADR-0033); Coin Promo introduces no new notion of who a coin belongs to.

Guest coins receive no bonus, and **claiming Guest Time never back-credits a
Coin Promo** — the identical rule ADR-0003 sets for points. One rule governs
both currencies, and a settled coin transaction is never re-priced.

### Coin events only

Every path that records a coin event is covered: the local serial terminal, the
simulator, a remote HTTP coin device, and a kiosk in-use top-up. Vouchers,
Manual Adjustments, points redemptions, and wheel winnings are excluded. A
`sold` Voucher already carries an owner-authored time value set at mint;
layering a promo on it would price the same value twice in two places. The
other paths are already free time by definition.

### Points are unaffected

Points earning is a function of `amount_cents`, not of seconds credited. Bonus
seconds therefore never inflate points earned or the earn carry. The two
incentives stack on the same coin, but neither is computed from the other.

### The Coin Outcome carries the split

The **Coin Outcome** gains bonus seconds as a field. `seconds_added` remains
the authoritative **total** credited, so timers, balances, and audit continue
to read one number.

The matching tier is looked up by the database layer inside the same immediate
transaction that commits the coin, and the resolved bonus is passed into the
pure outcome computation, which forces it to zero for a Guest target. Core
stays free of SQL, and the outcome remains one pure value decided from an
already-resolved target (ADR-0033).

There is no menu version. Unlike a Redemption Tier, which the customer reads
from a menu and then commits against, a Coin Promo Tier is read at the instant
of commit — there is no window in which an edit could change an exchange the
customer has already seen.

### Reporting keeps cash and giveaway separate

Cash Revenue is unchanged: it remains the coin's `amount_cents`. Bonus seconds
are recorded on the coin transaction and reported as a **coin promo**
component of **Free Time Dispensed**. The owner can always answer "what did
the promo cost me?" — which is the entire purpose of ADR-0014's split.

### Safety math is extended, and remains advisory

Each Coin Promo Tier has an **Effective Bonus %** in integer basis points: its
bonus seconds against the base seconds that amount buys. The owner-facing
headline is the **account worst case** — the most generous Coin Promo Tier plus
the most generous Redemption Tier — because that is what one logged-in customer
actually receives on one coin.

It is measured against the existing unsafe-bonus threshold and, consistent with
how that threshold already behaves for tiers, it **warns and never blocks**.
Coin Promo is a deliberate loss-leader; the owner decides its size, but never
by accident.

### Off by default

The feature has an owner-controlled enable flag so a seasonal promo can be
paused without destroying its rows. It ships disabled with no tiers authored,
so upgrading an existing shop changes its economy by exactly nothing.

## Rejected alternative

### A single percentage multiplier

One knob — "accounts get +40% time" — needs no tier table and no matching rule.
It was rejected because it cannot express the non-linear ladder the owner
wants, where a larger coin earns a disproportionately better bonus. That
escalation is the point: it pushes the customer toward the bigger coin.

## Consequences

- Schema, core, API, and UI use the terms **Coin Promo** and **Coin Promo
  Tier** exactly as defined in the glossary. "Promo" alone remains the Voucher
  kind and must not be used for this feature.
- A coin transaction now records both cash taken and time given, and reporting
  must read the bonus separately rather than inferring it from `seconds_added`.
- **Effective Bonus %** is no longer a property of Redemption Tiers alone; the
  owner's worst case is the combination of both giveaways.
- Guest coins and claimed Guest Time remain entirely outside the promo, so
  logging in before paying stays strictly better for the customer.
- Time-windowed promos (happy hour, date ranges) are deliberately **not**
  decided here. They require server-clock, timezone, and
  insertion-versus-commit-time semantics and need their own ADR.

## Amendment (2026-08-03): the Client ladder is a collapsed disclosure

The Widget's Time tab rendered the full Coin Promo ladder inline and always
expanded. At 320×480 that grid was one of the reasons the tab overflowed its
panel (see the ADR-0029 amendment of the same date), and the space it took was
charged to every customer in every state, including those with no coin to
insert.

Decision — presentation only; nothing about how a bonus is earned changes:

- **The ladder is an inline `<details>`, collapsed by default.** It stays
  inline: a Coin Promo ladder is informational, and `AGENTS.md` forbids
  informational content in the Widget from rendering as a `fixed` overlay.
- **Its summary leads with the largest bonus and its coin amount** —
  `Coin Promo — ₱50 → +60 min`. A customer who never expands the ladder still
  sees a concrete offer, and the bonus is never quoted without the coin that
  buys it. Note this headline picks the row with the largest raw bonus, which
  is not necessarily the row with the highest **Effective Bonus %**; on the
  escalating ladder this ADR argues for, they coincide.
- **The ladder body is its own scroll container.** The owner decides how many
  Coin Promo Tiers exist, so an overlong ladder must scroll within itself
  rather than push the Time tab into scrolling.

## Amendment (2026-08-11): Rejection Credit joins the applies-to list

"Coin events only" above listed the local serial terminal, the simulator, a
remote HTTP coin device, and a kiosk in-use top-up, and excluded everything
else. [ADR-0069](0069-rejection-credit.md) adds one entry: a **Rejection
Credit** also receives the matching Coin Promo bonus.

This is not a relaxation of the rule. A Rejection Credit re-credits a coin the
acceptor physically took but could not value — the customer inserted the exact
coin the tier was written for, and only a Pulse Map calibration gap stopped the
bonus landing. Withholding it would penalize the customer for the machine's
fault.

The account-only boundary is unchanged: a **guest** Rejection Credit receives
base **Shop Rate** seconds with no bonus, exactly as an accepted guest coin
does. Vouchers, Manual Adjustments, points redemptions, and wheel winnings
remain excluded. The bonus is recorded on the `rejection_credits` detail row and
reported as a coin promo component of **Free Time Dispensed**, the same split
this ADR already requires.

## Amendment (2026-08-20): the batch is a coin train, so matching decomposes

"Exact amount match only" above assumed one coin event equals one coin. That
stopped being true with [ADR-0070](0070-credit-the-pulse-sum.md): a **Coin
Batch** credits its pulse sum, and consecutive coins concatenate into one train
with no seam at all (characterization N2). ADR-0070 did not revisit this ADR,
and the exact-match rule silently became a *bug*: a customer dropping ₱10 then
₱1 gets one 1100-cent event, which matches no tier, so the ₱10 coin's bonus
vanishes. ₱20 + ₱10 loses both. Only a coin inserted alone, with a quiet gap on
each side, still received its promo.

Decision — matching **decomposes** the batch amount into tier amounts,
largest tier first, and sums the bonuses:

- **The largest covering tier wins; a smaller tier repeats only below it.**
  Two ₱10 coins reach ₱20, so an authored ₱20 tier applies once and the
  escalating ladder this ADR argues for pays out as intended; the ₱10 tier
  repeats only when no ₱20 tier exists. Repetition is not the "greedy
  multiplication" this ADR rejected — that rejection protected against one coin
  compounding a row; here each application corresponds to a coin the customer
  actually inserted.
- **Greedy, not value-maximising.** If the owner authors a ₱20 tier *stingier*
  than two ₱10 tiers, the ₱20 tier still wins. Searching for the best-paying
  decomposition was rejected: it would quietly overrule an owner who priced the
  larger coin deliberately, and an inverted ladder is a configuration mistake to
  surface, not for pricing to paper over.
- **The remainder earns nothing.** There is still no nearest-lower fallback: ₱11
  against a lone ₱10 tier earns the ₱10 bonus and nothing for the ₱1. A larger
  coin never inherits a smaller tier's bonus.
- **The single-coin case is unchanged.** One coin decomposes to exactly its own
  tier, so a shop whose customers insert one coin at a time sees no difference.

The safety math is unchanged and needs no new headline. Every peso the
decomposition covers is priced at some tier's bonus rate and the remainder is
priced at zero, so a batch can never exceed the most generous tier's
**Effective Bonus %** — the **account worst case** this ADR already publishes
remains the true worst case.

This follows ADR-0070's governing principle: the customer's coin is the ground
truth, and the machine's inability to tell two adjacent coins apart is the
machine's problem, not the customer's.
