# ADR-0032: Unify LAN account auth behind a header-bearer AccountSession extractor

- Status: Implemented
- Date: 2026-06-30
- Deciders: shop owner (product)
- Relates to: ADR-0002 (account/session binding), ADR-0024 (web-admin sole
  owner surface — source of the `OwnerSession` extractor pattern), ADR-0030
  (account transfers), ADR-0015 (audit log).

## Context

The admin process runs two HTTP servers with two different ways to prove
identity:

- **Owner surface (`:8080`, `web_admin.rs`).** Identity is an axum
  **extractor**: `OwnerSession: FromRequestParts` reads the owner cookie,
  rejects with `401` *before* the handler body runs, and audits the denial. A
  handler that needs an authenticated owner simply names `OwnerSession` in its
  signature and cannot run without one.
- **LAN client/device surface (`:3210`, `server.rs`).** Identity is a
  **hand-called helper**: every account-mutating handler takes a body struct
  carrying `token` + `pc_id`, then calls `require_account_token(&state, &input)`
  as its first line. The token rides in the **JSON body**, and the helper also
  re-checks `token.pc_id == input.pc_id`.

The asymmetry is a latent hazard, not just a style inconsistency. The account
endpoints are **money paths** (claim, voucher redeem, points redeem, wheel spin,
time transfer, points-transfer request, change password, logout). The auth
check is enforced only because each author remembered to write line 1. A new
session-mutating endpoint that forgets the call is an **unauthenticated money
path** that still compiles and passes a happy-path test.

The account token is held in an in-memory `AccountTokens` map and already
carries the full binding — `account_id`, `pc_id`, `session_id` — so the body's
`pc_id` is a redundant restatement of data the token already encodes, not an
independent input.

A bounding fact discovered while scoping this: the **only** caller of these
endpoints is the Client's **Rust** backend (`apps/client/src-tauri/src/commands.rs`),
using `reqwest` with ~8 typed request structs. The Client *frontend* never makes
these calls directly — it goes through Tauri `invoke()`. So the token's transport
is a Rust-to-Rust contract between two apps the owner deploys together, not a
browser/JS surface. "Moving the token touches the client" is true but cheap.

## Decision

Replace `require_account_token` with an `AccountSession` extractor that mirrors
`OwnerSession`, and move the account token from the request body to an
`Authorization` header so the extractor can resolve it in `FromRequestParts`
(which runs before the body is read).

### 1. The account token travels in an `Authorization: Bearer` header

`FromRequestParts` only sees `Parts` (headers), not the body. To gate on the
token there, it must arrive as a header. The account token becomes
`Authorization: Bearer <token>`, parsed by a hand-rolled `bearer_token(&parts.headers)`
helper that mirrors the existing `cookie_token` — **no new dependency**
(no `axum-extra`/`TypedHeader`). The Client's `reqwest` calls set the header and
drop `token` from their request structs.

### 2. The body `pc_id` cross-check is dropped

The extractor resolves the token to the full `AccountToken { account_id, pc_id,
session_id }`; handlers read `session.pc_id` wherever they previously read
`input.pc_id`. The old `token.pc_id == input.pc_id` assertion is removed: the
token is a bearer credential, so any holder can trivially echo the matching
`pc_id`, and the binding the token already carries is authoritative. The check
only ever caught *client-side state drift*, not an attacker. `pc_id` (and the
now-empty `SessionMutationInput`) leave the request bodies on both sides; the
operation structs keep only their payload fields (e.g. `VoucherRedeemInput`
keeps `code`; `account_claim`/`account_logout`/`wheel_spin` take no body).

### 3. `AccountSession` is the extractor; it carries the resolved token

```rust
pub struct AccountSession(pub AccountToken);
```

`impl FromRequestParts` resolves `bearer_token` → `AccountTokens::get` → `Ok`,
else `Err`. A handler that mutates an **Active Session** names `AccountSession`
in its signature and cannot run without a valid token. `AccountSession` is the
request-guard credential **for** an Active Session (the server-owned binding,
ADR-0002); the two are distinct — the guard proves the caller holds a token, the
Active Session is the state it points at. Because `account_logout` revokes the
raw token string, the extractor exposes the token string alongside the resolved
`AccountToken` (or revocation keys off `session_id`).

### 4. Rejection mirrors `OwnerSession`: `(StatusCode, String)`

`type Rejection = (StatusCode, String)`, hand-building the `401`. This keeps
`AccountSession` self-contained with **no dependency on `impl IntoResponse for
AppError`** (the separate error-boundary candidate). The two extractors share an
identical rejection style. If the error-boundary change lands later, the
rejection can be migrated to `AppError` then.

### 5. Denials are **not** audited (deliberate divergence from `OwnerSession`)

`OwnerSession` audits each denial because owner logins are rare and a denial is
noteworthy. With the `pc_id` cross-check gone, the only account-side denial is
"token not in the map" — which happens **routinely and benignly** every time a
Client uses a token after the session was logged out, auto-kicked (login
elsewhere, ADR-0002), or PC-taken-over (ADR-0007). Auditing every one would
bury real signal under stale-token races. The extractor leaves a comment that
tamper-response (expansion phase) may add a *targeted* signal for invalid-token
hits on money paths if it ever wants one. The asymmetry is intentional.

### 6. Flag-day migration, no compatibility shim

This is a breaking wire change: an old Client posting `token` in the body to a
new admin would `401`. OpenPiso is local-first and single-owner; admin and
Client are versioned in lock-step and updated together. The admin reads the
token **only** from the header — it does **not** dual-read body-or-header. A shim
would re-introduce the body-token path this ADR deletes and dilute the
single-seam benefit. The breaking change is noted for the release so the owner
updates both apps together.

## Considered options

- **Keep `require_account_token` as a helper (status quo).** Rejected: the auth
  check stays leaked into nine handler bodies, enforced only by author
  discipline; a forgotten call is an unauthenticated money path.
- **Token stays in the body, use `FromRequest` instead of `FromRequestParts`.**
  Rejected: `FromRequest` consumes the body, colliding with the handlers' own
  `Json<T>` extraction, and yields a clumsier two-extractor dance. Header
  transport is the clean enabler.
- **Add `axum-extra` for `TypedHeader<Authorization<Bearer>>`.** Rejected: a new
  production dependency for parsing one header the repo already hand-parses for
  cookies (`cookie_token`). AGENTS.md discourages unrelated prod deps.
- **Reject with `AppError` (pull in `impl IntoResponse`).** Deferred, not
  rejected: it couples this change to the error-boundary candidate. Keeping
  `(StatusCode, String)` lets `AccountSession` ship independently; migrate later.
- **Audit denials for symmetry with `OwnerSession`.** Rejected: benign
  stale-token races would flood the audit log. See §5.
- **Dual-read body-or-header transition shim.** Rejected: see §6.

## Consequences

- New `AccountSession` extractor; `require_account_token` and
  `SessionMutationInput` are deleted. The 9 account handlers in `server.rs` gain
  a `session: AccountSession` parameter, read `session.pc_id`/`session.account_id`,
  and shrink to the actual mutation. The auth invariant moves from "every author
  remembers line 1" to "the type is in the signature."
- API change: account-session endpoints now require `Authorization: Bearer
  <token>` and no longer accept `token`/`pc_id` in the body. Documented in
  `api.md`.
- Client (`commands.rs`): a `reqwest` helper sets the header; `token`/`pc_id`
  leave the ~8 request structs.
- Auth becomes one testable surface: a focused `AccountSession` suite
  (missing / malformed / unknown / valid token) proves the rejection paths once;
  existing handler tests are updated to send the header and stop re-proving auth.
- Both HTTP surfaces now share one mental model of authentication
  (`<Principal>Session: FromRequestParts`), differing only in transport (cookie
  vs bearer) and the deliberate audit asymmetry.
- Breaking change: admin and Client must be updated together (no shim).
