# ADR-0033: Typed coin-source trust and a pure Coin Outcome

- Status: Accepted (implementation pending — two behaviour-preserving commits;
  see §7)
- Date: 2026-06-30
- Deciders: shop owner (product)
- Relates to: ADR-0008 (coin routing — account vs guest), ADR-0023 (serial coin
  input, terminal selection, single-transaction commit), ADR-0019 (points earn
  carry), ADR-0003 (points economy), ADR-0015 (audit log).

## Context

The coin-event commit is the most authority-dense path in the shop: it
authenticates the coin source, routes the credit to an account or a guest PC,
prices time and points, and writes the audit row — all inside one SQLite
transaction (ADR-0023). Two shapes have accreted in that path.

**1. Device trust is a `bool` threaded through three layers, and a typed enum is
thrown away to produce it.** A coin's trust starts life as a typed
`CoinSourceKind` (`Simulator` / `Serial`) in `terminal_selection::commit_batch`.
It is then degraded to a `device_id` string plus a `trusted_local_serial: bool`,
threaded through `sessions::record_coin_event_inner`, and degraded again to a
second `bool` into `db::coins::record_coin_event_inner`, where it is finally
*re-expanded* by string-matching `device_id` against `LOCAL_SERIAL_DEVICE_ID` /
`"dev-simulator"` to decide the source label and run the secret check. There are
**twin wrapper pairs at two layers** (`record_coin_event` /
`record_trusted_terminal_coin_event` in both `sessions` and `db/coins`) whose
only job is to flip the flag, and the security-critical "who is this device and
may it use this id" logic — including the spoof-guard that an HTTP caller may
never claim `LOCAL_SERIAL_DEVICE_ID` — is interleaved with the financial commit,
so neither can be read or tested without driving a full ~240-line money path. The
boolean is blind: `record_coin_event_inner(pool, input, target, false)` — what is
`false`?

**2. The routing *consequence* has no home.** ADR-0008's rule — logged-in coins
earn time + points + carry; guest coins earn zero points — is split: `sessions`
decides *who* the coin is for (the routing target, account vs guest, resolved
under the in-memory binding lock), and `db/coins` independently decides the
*consequence* (whether points accrue, how carry rolls) inline inside the
transaction. `openpiso-core::pricing` owns only the arithmetic, not the policy.
The "guest earns zero points" rule is therefore exercised only through full
integration tests, never as an isolated unit.

A recent change (splitting the former `db.rs` god-module into `db/coins.rs`,
`db/vouchers.rs`, etc.) gave the coin logic a **file** home but did **not**
consolidate either shape: it was a pure move-and-regroup. The boolean still
threads through three layers and the routing consequence is still emergent. This
ADR records the decision to fix both, having confirmed against the current code
that the file split alone did not.

## Decision

### 1. A typed `CoinSourceClaim` subsumes device identity

```rust
enum CoinSourceClaim {
    SerialTerminal,                          // canonical id LOCAL_SERIAL_DEVICE_ID
    Simulator,                               // canonical id "dev-simulator"
    RemoteDevice { device_id: String, secret: String },
}
```

The claim owns *who attests the coin*. `CoinEventInput` shrinks to the **payment
facts** (`pc_id`, `amount_cents`, `pulse_count`, `timestamp`); `device_id` and
`signature` move into the claim. There is then exactly **one** source of device
identity, and the persisted `coin_transactions.device_id` and `source` label are
**derived** from the resolved claim rather than re-parsed from a free string
inside the commit. The `if device_id == … else if … else` ladder in
`record_coin_event_inner` collapses into a single match the resolver owns.

### 2. The claim is constructed once at each edge and flows down (no bools)

`CoinSourceClaim` is built at the request edge:

- `commit_batch` maps `CoinSourceKind::Simulator → Simulator` and
  `CoinSourceKind::Serial → SerialTerminal` (local sources, trivially trusted, no
  secret).
- the HTTP coin handler (`server.rs`) builds `RemoteDevice { device_id, secret }`
  from the body.

It threads through `sessions` **unchanged** and is consumed by `db/coins`. Both
`trusted_local_serial` bools and **both** twin wrapper pairs are deleted. Trust
becomes a value that flows, not a flag each layer re-derives.

### 3. Authentication happens **inside** the coin-event transaction

`db/coins` gains an extracted resolver:

```rust
async fn resolve_coin_source(
    tx: &mut SqliteConnection,
    claim: &CoinSourceClaim,
) -> AppResult<ResolvedSource>;
```

It runs the device lookup (`coin_devices.secret`/`active`), the secret
comparison, the simulator-enabled gate (`settings.simulator_enabled`), and the
spoof-guard backstop, returning an authenticated `ResolvedSource` (canonical
device id + `source` label). It is called right after `begin()`, so
authentication and the money mutation stay in **one transaction** — a device
deactivated or a secret rotated between auth and commit cannot be honoured. The
commit then reduces to **validate → resolve-source → route → apply-outcome →
audit**.

Authentication is deliberately **not** hoisted to the request edge. Doing so
would read `coin_devices.active` on a separate connection from the money commit,
opening a time-of-check/time-of-use window on a money path — rejected (see
Considered options).

### 4. The spoof-guard becomes a type-level guarantee plus an in-tx backstop

The HTTP edge can construct **only** `RemoteDevice` — it can never express
`SerialTerminal`. So "HTTP may not claim the local serial acceptor" is enforced
by the type system at the wire, where the threat is, instead of by a string
check buried in the commit. The in-transaction check (reject a `SerialTerminal`
claim whose canonical id is not `LOCAL_SERIAL_DEVICE_ID`) is **kept** as
defence-in-depth.

### 5. `CoinSourceClaim` is distinct from `CoinSourceKind`

`CoinSourceKind` (in `coin_source.rs`) stays "which **local hardware** is polling
pulses" — `Simulator` / `Serial`, tied to the `CoinSource` trait (`try_next_batch`,
set-pin, availability). `CoinSourceClaim` is "what **trust** does this coin event
carry on the wire" and **must** include the remote-HTTP-device case, which has no
polling loop. They answer different questions; merging them would force a
secret-bearing, never-polled `RemoteDevice` variant into every `match` in
`ConfiguredCoinSource`. Two honest types, not one overloaded one.

### 6. The routing *consequence* becomes a pure `CoinOutcome`; the *who* stays put

A pure `openpiso-core` function maps the already-decided routing target, amount,
prior carry, and economy to one value:

```rust
// (target, amount_cents, prior_carry_cents, economy, rate) -> CoinOutcome
struct CoinOutcome { seconds_added: i64, points: i64, new_carry: i64 }
```

"Guest earns zero points" and "account earns + carries" become **one** decision
the db layer merely persists — symmetric with §3's "persist an already-
authenticated source." The routing **target** (account vs guest) is **not**
moved: it must be resolved in `sessions` under the binding lock, because the
active session can change between read and commit (ADR-0002/0013). Only the
*consequence* is extracted. This reconciles ADR-0008: the routing rule is
unchanged; it simply gains a single owned home for its arithmetic-plus-policy
instead of being emergent across two modules.

### 7. Sequenced as two behaviour-preserving commits

- **Commit 1 (trust):** introduce `CoinSourceClaim` + `resolve_coin_source`,
  delete both bools and both wrapper pairs.
- **Commit 2 (outcome):** lift route→price into the pure `CoinOutcome`.

Each is behaviour-preserving and kept green on `cargo test --workspace` before
the next. They are split because they preserve *different* behaviours (trust
resolution vs outcome computation) and the coin path is already the widest diff;
two focused commits keep review legible. Test surface (keep-and-add): the
existing security integration tests (`real_device_event_requires_matching_secret`,
`simulation_is_rejected_when_disabled`,
`local_serial_device_is_reserved_for_trusted_terminal_path`) stay **unchanged**
as the behaviour-preserving net; new unit tests are added for `resolve_coin_source`
(secret match/mismatch, simulator gate, in-tx spoof backstop), for the HTTP-edge
claim constructor (cannot yield `SerialTerminal`), and for `CoinOutcome` in core
(guest→0 points, account→points+carry, carry rollover).

## Considered options

- **Keep the `bool` + twin wrappers (status quo).** Rejected: boolean-blindness,
  two wrapper pairs that only flip a flag, and security logic untestable except
  through a full money commit.
- **Type only the db boundary, leave `sessions` resolving its own bool.**
  Rejected: leaves identical boolean-blindness one frame up and keeps one of the
  two wrapper pairs; trust still re-derived per layer rather than flowing.
- **Authenticate at the request edge (claim arrives pre-authenticated).**
  Rejected: reads `coin_devices.active` on a separate connection from the money
  commit, introducing a TOCTOU window on a money path to save one in-transaction
  function call — a bad trade in the one subsystem where atomicity is the point.
- **Claim alongside an unchanged `CoinEventInput` (keep `device_id` on the
  input).** Rejected: two device-identity sources in the same call with nothing
  guaranteeing they agree — a latent invariant ("`input.device_id` must match the
  claim") of exactly the remembered-discipline kind this change deletes.
- **Extend `CoinSourceKind` with a `RemoteDevice` variant.** Rejected: puts a
  secret-bearing, never-polled variant next to a trait that assumes a serial
  port; every `ConfiguredCoinSource` match grows a meaningless `unreachable!()`
  arm. See §5.
- **Fold `CoinOutcome` into the trust commit (one pass).** Rejected: bundles two
  distinct concerns into one reviewable unit. See §7.
- **Shelve the routing-consequence extraction (treat it as not worth it).**
  Rejected: it is the one piece of the old "routing authority split" candidate
  that genuinely improves test coverage (guest-zero-points as a pure unit), and
  commit 1 leaves the function in exactly the shape that makes it a small lift.

## Consequences

- New domain terms: **Coin Source Claim** and **Coin Outcome**
  (`docs/glossary.md`). The existing **Coin Source** term keeps its hardware
  sense; the claim is the new wire-trust sense.
- `db/coins`: the two `record_*` wrappers and the `trusted_local_serial` bool are
  deleted; `record_coin_event_inner` takes a `CoinSourceClaim`, calls
  `resolve_coin_source`, and applies a `CoinOutcome`. `sessions`: its own wrapper
  pair and bool are deleted; it forwards the claim it is handed.
- `CoinEventInput` loses `device_id`/`signature` (they move into the claim);
  construction sites (`server.rs`, `terminal_selection.rs`, `reports.rs`
  fixtures, the `simulate_coin` test helper) are updated. No wire change for the
  HTTP coin payload's *fields* — `device_id` + `signature` are still read from
  the body, but at the edge into the claim, not deep in the commit.
- The spoof-guard is strengthened: an HTTP body cannot express the trusted
  serial variant at all (type-level), with the in-tx check retained.
- Atomicity (ADR-0023) and the routing rule (ADR-0008) are preserved; the latter
  gains a single owned home for its consequence arithmetic.
- The security and routing logic become unit-testable in isolation while the
  existing integration tests remain as regression guards (keep-and-add).
