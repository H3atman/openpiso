# ADR-0030: Account transfers — immediate time, owner-confirmed points

- Status: Accepted
- Date: 2026-06-28
- Deciders: shop owner (product)
- Relates to: ADR-0001 (time ownership), ADR-0003/0012/0019 (points economy,
  wheel, earn/carry), ADR-0015 (audit log), ADR-0021 (manual adjustment),
  ADR-0024 (web-admin sole owner surface).

## Context

The client redesign adds customer-to-customer transfers. Two currencies could
move: **Remaining Time** and **Points**. They carry different risk:

- **Time** is genuinely money-equivalent and already portable per account
  (ADR-0001); moving it between accounts is low-risk and the natural feature.
- **Points** are a loyalty currency with an effective-bonus invariant
  (ADR-0003), a reporting liability line (ADR-0014), and a gambling sink (the
  wheel, ADR-0012). Free transfer enables **funnelling** — many accounts shovel
  points into one to chain big spins — and muddies per-account loyalty
  accounting (total liability is conserved, but the distribution is gamed).

The owner wants points transfers to stay possible but under **owner oversight**.

## Decision

### 1. Time transfers are immediate

`POST /api/accounts/transfer` (token-authenticated) moves `seconds` from the
caller's account to a recipient by normalized **Username** in a single DB
transaction: re-read sender balance inside the txn, validate via core, debit
sender / credit recipient, insert an `account_transfers` row and two audit
events (`account_transfer_out`, `account_transfer_in`), return the refreshed
`AccountAuthState`. Self-transfer and missing/deleted recipients are rejected;
balance never goes negative; **self-drain to zero is allowed** and normal expiry
(ADR-0022) follows.

### 2. Points transfers are owner-confirmed via a 6-digit code

Points do **not** move on customer action alone. Instead:

1. Customer initiates → `POST /api/accounts/transfer-points` creates a **Points
   Transfer Request** (status `pending`) with a numeric **6-digit code**, and
   returns the code to **show on the client**.
2. The customer reads the code to the owner. The owner finds/enters **that code**
   in web-admin (`GET /web-admin/transfer-requests`, `POST
   .../transfer-requests/confirm`) and approves it. The code ties the
   physically-present customer to the correct request and prevents approving the
   wrong one (flow direction A).
3. On approval, points are **re-validated and moved atomically** (the sender may
   have spent or spun in the interim); status → `confirmed`.

The code is **single-use** and **time-boxed** (default ~10 min); on timeout the
request goes `expired`, and the owner may `reject` explicitly.

### 3. Durable, auditable request store

Points Transfer Requests live in a **`points_transfer_requests` DB table**
(`id`, `from_account_id`, `to_account_id`, `points`, `code`, `status`,
`created_at`, `resolved_at`), not in memory, so an admin restart does not drop
pending requests and the workflow is auditable. Audit events:
`points_transfer_requested`, then `points_transfer_out`/`points_transfer_in` on
confirm, or `points_transfer_rejected` / `points_transfer_expired`.

## Considered options

- **Time-only transfer for MVP.** Rejected by the owner; points transfer is
  wanted, just gated.
- **Both currencies immediate (original plan).** Rejected: unsupervised points
  funnelling into the wheel.
- **In-memory pending requests (like Terminal Selection, ADR-0023).** Rejected:
  an admin restart would silently drop pending requests and lose auditability.
- **Owner-generated code read to the customer (direction B) / plain
  approve-reject queue with no typed code (direction C).** Rejected in favour of
  A: the customer-presented code best binds the present customer to the request.

## Consequences

- Migration `0009` grows beyond a single `account_transfers` table to also add
  `points_transfer_requests` (+ indexes on both account ids and on `code`/status
  for the pending lookup).
- Two client commands (`transfer_account` for time, a points-request command),
  plus web-admin list/confirm/reject surfaces — more than the plan's single
  `/api/accounts/transfer`.
- Pure transfer validation (≥0, sender has enough, at least one currency > 0,
  self-transfer rejected) stays in `crates/openpiso-core` with unit tests, reused
  by both the immediate time path and the points-confirm path.
- The 6-digit code is a low-entropy, single-use, short-lived confirmation token,
  not a security boundary on its own — the owner's web-admin session is the real
  authority for releasing points.
