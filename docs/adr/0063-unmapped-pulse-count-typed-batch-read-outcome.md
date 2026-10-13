# ADR-0063: Unmapped pulse count as a typed batch-read outcome

- Status: Superseded by [ADR-0070](0070-credit-the-pulse-sum.md)
- Date: 2026-08-07
- Deciders: shop owner (product)
- Refines: [ADR-0023](0023-serial-coin-input-and-terminal-selection.md), [ADR-0043](0043-coin-ingestion-sampling-and-observability.md)
- Relates to: [ADR-0033](0033-typed-coin-source-trust-and-coin-outcome.md),
  [ADR-0070](0070-credit-the-pulse-sum.md),
  [ADR-0072](0072-credit-the-straddling-train.md)

> **Superseded, 2026-08-13.** The `Unmapped` variant and its fail-closed
> crediting consequence retire. The typed `CoinBatchRead` boundary remains, but
> now distinguishes only `None`, `Batch`, and transport or availability errors.
> Historical `coin_batch_rejected` rows and Rejection Credit behavior remain
> supported, but the live producer no longer exists. Under ADR-0070 every
> positive closed batch credits `pulse_count × 100 cents`, and the Pulse Map is
> severed from automatic Coin Batch crediting. The
> measurement that reversed it: the lookup credits **39.6 %** of inserted money
> across the bench archive, and every merged two-coin batch counts *perfectly*
> and credits ₱0. See
> [`coin-acceptor-characterization.md`](../coin-acceptor-characterization.md) §6.

## Context

A Coin Batch whose observed pulse count matches no entry in the Pulse Map
reported itself through the error channel (`Err(AppError::Conflict)`), so
callers could not tell a rejection apart from a serial read failure. One
consequence was a real coin-loss bug: during the in-flight drain that follows
a Selection Window close, an unmapped batch was treated as a transport error
and broke the drain loop, abandoning every coin still buffered behind it.

The `CoinOutcome` name is already taken in the core crate's pricing module for
the seconds-and-points routing result, so the batch-read outcome needs a
different name.

## Decision

The Coin Source batch read returns a typed `CoinBatchRead` outcome with three
variants:

- `None` — no batch is ready (empty buffer or still inside the inter-coin gap);
- `Batch(CoinBatch)` — a closed batch whose pulse count matched the Pulse Map
  (the only outcome that may credit money, time, or a transaction);
- `Unmapped { pulse_count }` — a closed batch whose pulse count matched no
  Pulse Map entry, carrying the observed count.

The error channel is reserved for transport and availability failures, which
are unchanged. Fail-closed is preserved: an `Unmapped` outcome credits no
money, adds no time, and mints no transaction, and the existing diagnostic
log (observed count + configured Pulse Map) is retained at the close site.

The in-flight drain records an `Unmapped` rejection at the terminal boundary
and continues to the next buffered batch; a genuine transport error still
breaks the drain loop. The standing accept path treats `Unmapped` as
`Ok(None)` — a rejection is not an error, and the next mapped coin still
credits.

## Consequences

- Rejections are distinguishable from transport failures at every caller,
  which is the prefactor the selection layer's rejection reaction (parent
  issue #270) sits on.
- No money path exists for `Unmapped`; tests assert zero transactions and
  zero credited time on the rejection seam.
