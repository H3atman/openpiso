# Local HTTP API

The admin app binds to `0.0.0.0:3210`. Local development clients normally use
`http://127.0.0.1:3210`; LAN clients use the admin PC's private network address.

## Health

`GET /api/health`

Returns the server status and application version.

## Authentication for account-mutating endpoints

All account-mutating endpoints (claim, logout, transfer, voucher/points redeem,
wheel spin, change-password) require an `Authorization: Bearer <token>`
header instead of carrying `token`/`pc_id` in the request body (ADR-0032).
The token is an ephemeral bearer credential issued on signup/login. Stale tokens
are rejected after auto-kick, takeover, expiry, logout, or admin-server restart.
Denials are not audited (the only rejection is "token not found", which occurs
routinely after logout/auto-kick/takeover).

Endpoints that **do not** carry an account token (`signup`, `login`, shop,
owner-PIN verify, health, identity, coin-events) are unchanged.

## Customer accounts

`POST /api/accounts/signup`

Creates an account and immediately authenticates it. Usernames are
case-insensitive, must be 3-32 ASCII letters/digits or `.`, `_`, `-`, and
remain reserved after deletion for manual Account creation and self-service
signup. Account Import is the deliberate exception: it may reuse a deleted
Username by creating a fresh Account identity. While Shop Closed, signup is refused before
account creation with HTTP 409 and the stable body `shop_closed`.

`POST /api/accounts/login`

Authenticates an active account. Deleted accounts and invalid credentials both
return HTTP 401; the canonical audit row retains the specific denial reason.
While Shop Closed, login is refused before credentials are checked with HTTP
409 and the stable body `shop_closed`; this denial is audited as a `login_failed`
row with `reason: shop_closed` and does not count as a credential failure.

Request for both endpoints:

```json
{
  "username": "Customer_01",
  "password": "local-password",
  "pc_id": "pc-01"
}
```

Successful responses contain an ephemeral token, a public account object, and
the active session:

```json
{
  "token": "ephemeral-token",
  "account": {
    "id": "account-uuid",
    "username": "Customer_01",
    "remaining_seconds": 1200,
    "points_balance_centi": 0,
    "status": "active"
  },
  "session": {
    "id": "session-uuid",
    "account_id": "account-uuid",
    "username": "Customer_01",
    "pc_id": "pc-01",
    "remaining_seconds": 1200,
    "points_balance_centi": 0,
    "claimable_guest_seconds": 300
  }
}
```

Login atomically resolves both contention axes. The newest login auto-kicks
the same account from another PC and takes over the target PC from another
account. Re-login on the same PC resumes the existing session ID. Passwords and
password hashes are never returned.

Account Login is throttled per normalized Username: after ten consecutive
failed logins, the next attempt and every attempt for the following sixty
seconds returns HTTP 429 with `Retry-After` and a body such as `too many
attempts; try again in 60s`. The throttle check runs before Shop Hours and
Password verification, and a successful login clears the state. Unknown
Usernames are never bucketed. This differs from the Web Admin PIN throttle,
which is keyed per source IP, starts after five failures, and uses escalating
backoff windows; Account Login always uses the flat sixty-second window.

`POST /api/accounts/claim`

Moves all current Guest Time on the session's PC into the account exactly once.
Requires `Authorization: Bearer <token>`. Takes no request body. Claim remains
available while Shop Closed.

## Daily Reward

The Client Attendance surface is served on `:3210` and is AccountSession-only.
The bearer token determines the Account; no route accepts an Account ID, PC ID,
progress, date, amount, or deadline from the Client. Read projections use one
Admin-local clock snapshot and the durable Attendance clock floor.

`GET /api/attendance` returns whether Daily Reward earning is enabled, today's
server-derived progress, Account-wide totals, the current Admin-local month and
previous 11 month summaries in newest-first order, older claimable month
summaries, and the server clock fields. Calendar month summaries include empty
months. Older claimable months have no day calendar; they can be claimed
directly. Client calendar navigation uses the returned order: Previous moves to
an older month and Next moves to a newer month.

```json
{
  "enabled": true,
  "today": {
    "earning_date": "2026-09-26",
    "status": "in_progress",
    "capped_cents": 500,
    "daily_threshold_cents": 1000,
    "reward_seconds": 3600
  },
  "totals": {
    "earned_seconds": 3600,
    "claimed_seconds": 0,
    "outstanding_seconds": 3600,
    "claimable_seconds": 0,
    "expired_seconds": 0,
    "forfeited_seconds": 0
  },
  "calendar_months": [],
  "older_claimable_months": [],
  "as_of": "2026-09-26T12:00:00.000Z",
  "admin_local_date": "2026-09-26",
  "admin_local_month": "2026-09"
}
```

Each month summary has `earning_month`, nullable derived `state`, `check_in_count`,
`earned_seconds`, `claimed_seconds`, `outstanding_seconds`,
`claimable_seconds`, `expired_seconds`, `forfeited_seconds`, nullable frozen
`claim_starts_at` / `deadline_at_exclusive` RFC 3339 instants, nullable
Admin-local `claim_starts_on` / `deadline_date_inclusive` dates, `claim_count`,
nullable UTC `latest_claim_at`, and nullable Admin-local `latest_claim_on`. The
Client uses `latest_claim_on` for display and never derives a claim date from the
UTC instant. Older Admin versions may omit `latest_claim_on`. Empty months have
zero totals and null state and claim bounds.

`GET /api/attendance/months/{earning_month}` accepts only a canonical `YYYY-MM`
key in `calendar_months`. It returns the same month summary, `days` for every
calendar date in that month, and `as_of`, `admin_local_date`, and
`admin_local_month`. Each day has `earning_date`, `status` (`checked_in`,
`missed`, `ineligible`, `future`, or `today_progress`), and nullable
`capped_cents`, `daily_threshold_cents`, and `reward_seconds`. Today is live
progress when enabled even while Shop Closed; completed-date eligibility uses
frozen Admin evidence. Older claimable months remain summary-only and return
404 from this calendar route. Malformed month keys return 400.

`POST /api/attendance/claims` takes a strict body; unknown fields are rejected:

```json
{ "request_id": "f6caa06b-c0c7-4be6-9f8a-3498c5e3c612", "earning_month": "2026-08" }
```

A successful claim returns 200 with the original immutable `claim` fact, the
Account's refreshed `remaining_seconds`, fresh `totals`, the fresh `month`
summary, and the same server clock fields. Repeating the same UUID for the same
Account/month returns that original Claim and fresh projections without
crediting Remaining Time again. An authenticated domain denial returns 409
`{ "error": "attendance_claim_denied", "reason": "..." }`, where `reason` is
`month_not_found`, `not_yet_claimable`, `nothing_outstanding`, `expired`, or
`request_id_conflict`; the denial is audited without balance deltas. Malformed
JSON, missing/extra fields, or invalid UUID/month input returns 400 with
`{ "error": "invalid_input", "message": "..." }`. Missing, malformed, revoked,
or stale AccountSession returns an unaudited 401 with
`{ "error": "unauthorized" }`. These claims remain available with earning
disabled and while Shop Closed.

`POST /api/accounts/logout`

Ends the binding without changing the account's Remaining Time. The token is
revoked server-side. Requires `Authorization: Bearer <token>`. Takes no request
body. Logout remains available while Shop Closed.

Tokens are scoped to one account, PC, and session ID. Stale tokens are rejected
after auto-kick, takeover, expiry, logout, or admin-server restart.

## Shop identity and owner gate

`GET /api/shop-status`

Unauthenticated, read-only. Returns the server-resolved Shop Status using the
Admin host's local clock. `next_change_at` is an RFC 3339 timestamp or `null`
when the current state has no scheduled change.

```json
{
  "open": false,
  "next_change_at": "2026-06-21T08:00:00+08:00",
  "reason": "schedule",
  "last_call_active": false
}
```

`last_call_active` is true only during a scheduled Last Call period. It is false
for a zero lead, a Shop Closed status, and every Shop Override; customer
surfaces render the warning from this field without evaluating the schedule.

`GET /api/shop`

Unauthenticated, read-only. Returns the owner-editable shop name and non-secret
Client timing settings plus the wheel's public enabled/disabled discovery flag — never the owner PIN, prize table, or any secret setting (ADR-0027).

```json
{
  "shop_name": "OpenPiso Shop",
  "shutdown_grace_seconds": 300,
  "offline_grace_seconds": 600,
  "theme_version": 1,
  "wheel_enabled": true
}
```

## Theme

`GET /api/theme` on `:3210` is public and returns the shop's raw theme record.
Clients compare its `version` with `theme_version` in `/api/shop` and heartbeat
responses before refetching.

```json
{
  "accent_color": "#15803d",
  "mode_web_admin": "light",
  "mode_kiosk": "light",
  "mode_client": "light",
  "version": 1
}
```

Owner `GET /api/theme` and full-record `PUT /api/theme` are served on `:8080`
and require the `op_owner` session cookie. PUT accepts `#rrggbb` (normalized to
lowercase), `light | dark` modes, atomically increments `version`, and writes no
audit event. Invalid colors/modes return `400`.

`POST /api/owner/verify-pin`

Compares the supplied PIN against the hashed `owner_pin` setting server-side
(ADR-0027). A wrong or not-yet-configured PIN returns HTTP 200 with
`{ "ok": false }` so the Client distinguishes a wrong PIN from a transport
failure. The stored hash is never returned for this online check.

```json
{ "pin": "246810" }
```

Response: `{ "ok": true }`.

## Account transfers and password

`POST /api/accounts/transfer`

Immediate, server-authoritative **Time Transfer** from the signed-in account to
another account by Username (ADR-0030). The sender's account id is taken from
the verified `Authorization` header, never the body. The recipient and amount
are validated server-side (recipient must exist and be active; self-transfer
rejected; sender must have enough seconds). Self-drain to zero is allowed.

Requires `Authorization: Bearer <token>`.

```json
{
  "to_username": "customer_02",
  "seconds": 300
}
```

Returns the same account auth shape as login, with the sender's refreshed
account and active-session balances.

`POST /api/accounts/transfer-points`

Creates an owner-confirmed **Points Transfer Request** (ADR-0030). No points
move yet; the response carries a single-use, time-boxed 6-digit code the
customer reads to the owner, who confirms it in web-admin. It remains available
while Shop Closed.

Requires `Authorization: Bearer <token>`.

All point amounts on the wire are integer **centipoints** (1/100 point,
ADR-0038); `points_centi: 150` transfers 1.50 points.

```json
{
  "to_username": "customer_02",
  "points_centi": 150
}
```

Response: `{ "id": "request-uuid", "code": "123456" }`.

`POST /api/accounts/change-password`

Customer self-service **Password Change** ([ADR-0037](adr/0037-contention-safe-password-change.md)).
Verifies the current password, applies the signup length rule to the new one,
and preserves the Account Session and Active Session. A wrong current password
is a generic HTTP 401. An inactive or removed Account uses the same generic 401
semantics. HTTP 409 with `password change conflict` is reserved exclusively for
a concurrently superseded credential. Unexpected failures return HTTP 500 with
`password change failed`; database details remain only in server diagnostics.
Success atomically writes `account_password_changed`; denied and stale attempts
best-effort write `account_password_change_failed`. Never returns a hash.

Requires `Authorization: Bearer <token>`.

```json
{
  "old_password": "current-password",
  "new_password": "new-password"
}
```

Response: `{ "ok": true }`.

## Voucher and points redemption

`POST /api/vouchers/redeem`

Redeems one active voucher into the logged-in account. Codes are trimmed,
case-insensitive, and dash-insensitive. Guests cannot redeem because the request
requires the `Authorization: Bearer` header. Invalid, spent, expired, and voided
voucher attempts are audited as `voucher_rejected` with distinct reasons and do
not change balances or voucher status. While Shop Closed, redemption is refused
before the voucher is examined with HTTP 409 and the stable body `shop_closed`.

Requires `Authorization: Bearer <token>`.

```json
{
  "code": "OP-12345678-90AB-CDEF-1234567890AB"
}
```

`GET /api/coin-promo`

Requires an Account Session bearer token. Returns the customer-facing Coin Promo
ladder in ascending coin amount order. A disabled Coin Promo hides authored tiers;
an enabled Coin Promo with no tiers also gives the Widget nothing to display.
Money and time remain integer cents and whole seconds:

```json
{
  "enabled": true,
  "tiers": [
    { "amount_cents": 500, "bonus_seconds": 600 },
    { "amount_cents": 1000, "bonus_seconds": 1500 }
  ]
}
```

`GET /api/points/redemption-tiers`

Returns the authenticated Redemption Tier menu, sorted by ascending Points,
plus its integer version. Points are whole points; durations are seconds.

```json
{
  "version": 2,
  "tiers": [
    { "id": 4, "points": 5, "seconds": 3600 },
    { "id": 5, "points": 10, "seconds": 14400 }
  ]
}
```

`POST /api/points/redeem`

Atomically exchanges exactly one Redemption Tier for the logged-in account. It
remains available while Shop Closed.
The request must name a tier id and the menu version previously fetched. The
server re-reads both inside the SQLite transaction; stale versions return 409
and tell the client to refresh, unknown ids return 404, and insufficient Points
return 409. Every failure leaves both balances untouched. Success is logged as
`points_redeemed` and returns the refreshed account-auth shape.

Requires `Authorization: Bearer <token>`.

```json
{
  "tier_id": 5,
  "version": 2
}
```

The voucher and points redemption mutation endpoints return the same account
auth shape as login/claim, with updated account and active-session balances.

`POST /api/wheel/spin`

Spins the wheel for the logged-in account. It remains available while Shop
Closed. The spin is fully
server-authoritative: the server deducts exactly one point (100 centipoints),
rolls its own RNG,
resolves the entire capped free-spin chain, credits any won time AND points, and
audits the result in one SQLite transaction. The client only animates the chosen
outcome.
Guests cannot spin (the request requires the `Authorization: Bearer` header).
A disabled wheel, an unsafe (non-positive house-edge) configuration, and
insufficient points are audited as `wheel_spin_denied` and change no balances.

The audit carries the **net** points delta (winnings − cost). The spin detail
row (inspection only) carries the **gross** points won for reporting
(ADR-0046). Customer-facing wheel responses, including `GET
/api/wheel/settings` and this spin response, contain neither the Bokya Rate nor
true per-segment probabilities. Their absence is an intentional confidentiality
property: the hidden draw bias and effective odds stay owner-only rather than
being disclosed to an untrusted Client. The Client receives equal slices and the
server-resolved outcome only (ADR-0049).

Requires `Authorization: Bearer <token>`. Takes no request body.

Response (account auth shape plus the resolved outcome and the exact segment snapshot used for animation):

```json
{
  "token": "ephemeral-token",
  "account": {
    "id": "account-uuid",
    "username": "customer_01",
    "remaining_seconds": 300,
    "points_balance_centi": 400,
    "status": "active"
  },
  "session": {
    "id": "session-uuid",
    "remaining_seconds": 300,
    "points_balance_centi": 400
  },
  "result": {
    "seconds_won": 300,
    "points_won_centi": 0,
    "free_spins_triggered": 0,
    "segments": [
      {
        "label": "5 minutes",
        "seconds": 300,
        "points_centi": 0,
        "free_spin": false
      }
    ],
    "steps": [
      {
        "segment_index": 0,
        "label": "5 minutes",
        "seconds": 300,
        "points_centi": 0,
        "free_spin": false
      }
    ]
  }
}
```

## Admin Tauri commands

> **Removed in M6 / [ADR-0024](adr/0024-web-admin-sole-owner-surface.md).**
> The desktop Tauri owner console was retired in M6. The 27 typed
> commands that used to be invoked from `apps/admin/src` (`get_dashboard`,
> `owner_is_configured`, `setup_owner`, `owner_login`, `simulate_coin`,
> `get_transactions`, `get_today_report`, `list_pc_settings`,
> `update_pc_settings`, `get_economy_settings`, `update_economy_settings`,
> `create_voucher`, `list_vouchers`, `get_wheel_settings`,
> `update_wheel_settings`, `list_accounts`, `admin_create_account`,
> `admin_reset_account_password`, `admin_delete_account`,
> `get_report_summary`, `get_account_report`, `get_pc_report`,
> `get_points_liability`, `search_audit_log`, `export_audit_log_csv`,
> `create_manual_adjustment`, `search_diagnostic_log`) are gone from the
> Tauri surface. The Tauri webview is now the **customer Central Terminal
> kiosk only** (`open_selection`, `close_selection`, `get_selection`,
> `list_pcs_kiosk`, `get_kiosk_status`, `get_kiosk_config`, `get_theme`,
> `get_shop_name`, `get_shop_status`, `get_kiosk_lock`, `set_kiosk_lock`).
> The owner data shapes that used
> to flow through those commands are reached over the authenticated HTTP
> API below; the owner web app in `apps/web-admin` consumes that API.

The owner HTTP API below is the **sole** owner surface; it is the browser
analogue of the retired desktop console. Settings use integer values:

Voucher response:

```json
{
  "id": "voucher-uuid",
  "code": "OP-12345678-90AB-CDEF-1234567890AB",
  "kind": "sold",
  "status": "active",
  "seconds_value": 3600,
  "points_value_centi": 0,
  "sold_amount_cents": 1000,
  "created_at": "2026-06-21 10:00:00",
  "sold_at": "2026-06-21 10:00:00",
  "redeemed_by_account_id": null,
  "redeemed_by_username": null,
  "redeemed_at": null,
  "expires_at": null,
  "batch_id": null,
  "batch_name": null,
  "batch_prefix": null
}
```

### Vouchers

`POST /web-admin/vouchers` requires:

```json
{
  "prefix": "OP", // optional, 2-8 chars A-Z/0-9, default OP
  "suffix_length": 8, // optional, 6-16, default 8
  "seconds_value": 3600,
  "points_value_centi": 100,
  "sold_amount_cents": 5000, // > 0 → kind = sold, 0 → kind = promo
  "expires_at": null // optional RFC 3339
}
```

Kind is inferred server-side from `sold_amount_cents`. The server generates the
code as `PREFIX-` plus an unambiguous random suffix, dash-grouped every four
characters. A single-mint sold Voucher records cash at mint. `POST /web-admin/vouchers/{id}/void` voids one
active Voucher, retaining its history and any already-recorded revenue.

### Voucher batches

`POST /web-admin/voucher-batches` requires a body:

```json
{
  "prefix": "PROMO", // 2-8 chars A-Z/0-9, always uppercase
  "suffix_length": 8, // optional, 6-16, default 8
  "count": 50, // 1-1000
  "seconds_value": 3600,
  "points_value_centi": 0,
  "cash_amount_cents": 0, // > 0 → kind = sold, 0 → kind = promo
  "expires_at": null // optional RFC 3339
}
```

Response (`name` is an immutable, globally unique server-generated five-character case-sensitive Base62 identifier):

```json
{
  "id": "batch-uuid",
  "name": "aB7x2",
  "prefix": "PROMO",
  "suffix_length": 8,
  "count": 50,
  "kind": "promo",
  "seconds_value": 3600,
  "points_value_centi": 0,
  "cash_amount_cents": 0,
  "expires_at": null,
  "created_at": "2026-07-20 10:00:00",
  "voided_at": null,
  "voided_audit_event_id": null
}
```

Batch-sold vouchers (`cash_amount_cents > 0`) recognise cash at redemption:
one `sold_voucher_redeemed` event carries `cash_flag = true`, the cash amount,
and the credited value. Single-mint sold vouchers redeem with
`cash_flag = false` because their `voucher_sold` event already recorded cash at
mint.

`GET /web-admin/vouchers?limit=100&offset=0&batch_id=<id>&search=<fragment>&status=active`
returns vouchers newest-first. Batched vouchers include joined `batch_name` and
`batch_prefix`; both are `null` for single vouchers. `offset`, `batch_id`,
`search`, and `status` (`active`, `redeemed`, or `voided`) are optional and may
be combined; omitted `status` returns all. `search` matches only code fragments
case-insensitively and ignores dashes.

`GET /web-admin/voucher-batches?limit=100&offset=0` returns batches
newest-first. Each batch includes the fields above plus `redeemed_count` for
progress display. `offset` supports loading older batches for later reopening.
`GET /web-admin/voucher-batches/{id}` returns the same progress fields plus a
`vouchers` array. Voided cards report their persisted `voided` status; redeemed
cards remain `redeemed`.

`POST /web-admin/voucher-batches/{id}/void` atomically changes all still-active
cards in the batch to `voided` (redeemed cards are untouched), marks the batch,
and writes one non-cash batch audit event. Returns the updated batch with
`voided_at` set. All batch endpoints require the owner session cookie.

The web admin exposes `GET /web-admin/wheel` and `PUT /web-admin/wheel` for
Spin the Wheel. Both require the `op_owner` cookie. `update_wheel_settings`
replaces the whole prize table atomically, the optional
`bokya_rate_basis_points` (0–10,000), the `free_spin_cap` (0–10), the
`spin_duration` (1–120), and the `enabled` flag, bumps the stored version, and
refuses a Bokya Rate that the submitted segments cannot satisfy or any
configuration whose expected seconds payout is not below the
point value of the most generous Redemption Tier (highest seconds per point).
Points prizes are deliberately not folded into the guardrail (ADR-0046).
The response includes the recomputed Redemption Tier guardrail fields
`house_edge_basis_points`, `expected_payout_seconds`,
`expected_payout_points_centi`, `point_value_seconds`, and an
`unsafe_house_edge` flag. It also includes the owner-only Shop Rate advisory:
`shop_rate_point_value_seconds`, `shop_rate_house_edge_basis_points`, and
`unsafe_shop_rate_house_edge`. The advisory values one earned point at the
active Shop Rate (`earn_cents_per_point * rate_seconds / rate_amount_cents`)
and never blocks Shop Rate saves or changes customer-facing wheel responses:

```json
{
  "enabled": true,
  "bokya_rate_basis_points": 7500,
  "free_spin_cap": 1,
  "spin_duration": 5,
  "version": 2,
  "segments": [
    { "label": "5 minutes", "seconds": 300, "points_centi": 0, "free_spin": false, "probability_basis_points": 1250 },
    { "label": "Try again", "seconds": 0, "points_centi": 0, "free_spin": false, "probability_basis_points": 3750 },
    { "label": "Try again", "seconds": 0, "points_centi": 0, "free_spin": false, "probability_basis_points": 3750 },
    { "label": "Free spin", "seconds": 0, "points_centi": 0, "free_spin": true, "probability_basis_points": 1250 }
  ],
  "expected_payout_seconds": 94,
  "expected_payout_points_centi": 0,
  "point_value_seconds": 1440,
  "house_edge_basis_points": 9349,
  "unsafe_house_edge": false,
  "shop_rate_point_value_seconds": 15000,
  "shop_rate_house_edge_basis_points": 9937,
  "unsafe_shop_rate_house_edge": false
}
```

On reads, `bokya_rate_basis_points: null` means unset and remains distinct from
numeric zero. Sending `null` on PUT clears the rate. Each owner-read segment's
server-computed `probability_basis_points` values sum exactly to 10,000 and come
from the same distribution the biased draw walks; clients never receive them.
When the rate is unset the draw is exactly `1/n` per segment, so a segment count
that does not divide 10,000 makes these values a whole-basis-point rounding of
that uniform draw rather than a change to the odds spun. Integer division leaves
a remainder of at most one basis point on a group's lowest indices either way;
the Wheel tab renders each group's exact share, so that split is not shown to
the owner (ADR-0049).

Segments carry **no probability weight** (ADR-0047): every segment occupies
`1/n` of the circle and is `1/n` likely when `bokya_rate_basis_points` is null.
When set, the server uses that probability for Bokya landings and divides each
group's probability evenly among its segments (ADR-0049). Sending a `weight`
field is ignored rather than rejected, so an older client degrades instead of
erroring. A live spin refuses a contradictory persisted rate with the distinct
`contradictory_bokya_rate` denial reason, before charging the account.

## Reporting

The owner web admin surfaces the same data over the `:8080` HTTP API as
the retired desktop console. All report endpoints read from the M6
reporting views over canonical `audit_log` and linked `wheel_spins` detail so totals reconcile exactly
with the event stream.

`GET /web-admin/reports/overview` returns the current Sales Report in one
owner-authenticated response. It contains Daily, Weekly, Monthly, and all-time
Total sales, plus every registered PC (including hidden PCs and zero-sales
rows):

```json
{
  "daily": {
    "from_date": "2026-08-06",
    "to_date": "2026-08-06",
    "cash_revenue_cents": 5000
  },
  "weekly": {
    "from_date": "2026-08-03",
    "to_date": "2026-08-09",
    "cash_revenue_cents": 12000
  },
  "monthly": {
    "from_date": "2026-08-01",
    "to_date": "2026-08-06",
    "cash_revenue_cents": 25000
  },
  "total": {
    "from_date": null,
    "to_date": null,
    "cash_revenue_cents": 125000
  },
  "pcs": [
    {
      "id": "pc-01",
      "name": "PC 01",
      "daily_sales_cents": 5000,
      "weekly_sales_cents": 12000,
      "monthly_sales_cents": 25000,
      "total_sales_cents": 125000
    }
  ]
}
```

The server calculates ranges from the admin host's local business date. Weekly
is Monday through Sunday; Monthly is month-to-date. PC sales are cash revenue
anchored to that PC, so sold vouchers without a PC remain in the Summary Total
but are not assigned to a PC.

`POST /web-admin/reports/summary` returns cash
revenue, free time dispensed, points minted by wheel, points liability,
and per-period buckets:

```json
{
  "period": "day",
  "from_date": "2026-06-01",
  "to_date": "2026-06-21",
  "cash_revenue_cents": 5000,
  "free_time_dispensed_seconds": 900,
  "points_minted_by_wheel_centi": 400,
  "points_liability_centi": 300,
  "buckets": [
    {
      "period_label": "2026-06-21",
      "cash_revenue_cents": 5000,
      "free_time_dispensed_seconds": 900,
      "points_minted_by_wheel_centi": 400
    }
  ]
}
```

Cash revenue = coins + sold vouchers. Free time dispensed = promo voucher
redemptions + points redemptions + wheel winnings + manual admin add-time +
Coin Promo bonus seconds. A Coin Promo coin's full cash stays in revenue; only
its bonus seconds count as free time. The two are never blended (ADR-0014).
`points_minted_by_wheel_centi` is the
gross centipoints won through the wheel over the period (not net of spin
costs; ADR-0046). `period` is `day`, `week`, or `month`. Date fields are
optional `YYYY-MM-DD` local-date strings; omitting both reads all history. The
Sales Report page reads this endpoint with `period: "day"` over one calendar
month to draw the Daily Sales Trend line chart. Days with no revenue are absent
from `buckets` and the page fills them as zero; days after the admin host's
business date are left off the plot rather than drawn as zero.

`POST /web-admin/reports/attendance` returns Attendance flow and exposure for
the authenticated Owner Session. Its required `period` is `day`, `week`, or
`month`; optional `from_date` and `to_date` are inclusive Admin-local
`YYYY-MM-DD` bounds. Omitted dates include all flow history. The independently
optional `month_limit`/`month_offset` and `account_limit`/`account_offset` page
Earning Month and Account rows; each defaults to 50 rows, with a maximum limit
of 200. The range filters flow totals and buckets only; `liability` and the two
exposure pages are range-independent. The response also carries durable `as_of`,
Admin-local date/month, lifetime reconciliation totals, current economics
advisory, and a clock-regression advisory. A report may finalize Attendance
calendar state but does not append Audit Log events.

`GET /web-admin/attendance/settings` returns the active immutable version,
economics advisory, current shop-wide outstanding/claimable liability, next
Earning Month, and each setting's effective-change timing. `POST
/web-admin/attendance/settings/preview` takes `{ "enabled": bool,
"daily_threshold_cents": int, "reward_minutes": int, "claim_window_months":
int }` and computes the draft advisory without writing. `PUT
/web-admin/attendance/settings` takes the same fields plus `expected_version`;
it returns the saved version and current advisory/liability. An identical draft
returns `outcome: "unchanged"` before version comparison and writes no version
or audit event. A changed draft with a stale `expected_version` returns HTTP 409
with `{ "error": "attendance_settings_stale", "current_version": int }`;
the client should refresh before retrying. Actual edits append a version and
`attendance_settings_updated` Admin Audit Log row atomically. Threshold cents,
reward minutes, and claim window are validated at the Rust boundary.

`get_account_report` and `get_pc_report` return per-dimension breakdowns for
one account or PC across the same date range:

```json
{
  "id": "pc-01",
  "name": "PC 01",
  "cash_revenue_cents": 100,
  "seconds_sold": 300,
  "free_time_dispensed_seconds": 0,
  "from_date": "",
  "to_date": ""
}
```

`get_points_liability` returns the integer total of outstanding unredeemed
centipoints across active accounts.

## Manual Guest Time adjustment

`POST /web-admin/manual-adjustment` is the owner-only HTTP endpoint that
adjusts a PC's Guest Time. It takes a PC ID, an integer `seconds_delta`
(positive or negative), and an optional reason. Remaining time is clamped
at zero; the canonical `manual_adjustment` audit event records the
**actual applied delta** so report totals reconcile, while the requested
delta is kept in metadata. A negative delta is a compensating event, not
an audit-history edit. The handler broadcasts `pc.updated` after the
transaction commits.

```json
{
  "pc_id": "pc-01",
  "seconds_delta": 300,
  "reason": "promo top-up"
}
```

Auth is the `op_owner` cookie; the body carries no `owner_token` field
after the M6 / ADR-0024 cleanup that removed the desktop Tauri commands
that used to validate it.

Manual adjustments target PC Guest Time only (ADR-0021). Account balance
corrections flow through the existing typed mutation paths (coin events,
vouchers, points redemption, wheel, soft deletion).

## Audit log review

`POST /web-admin/audit/search` returns audit rows reverse-chronologically
with the supplied filters. All filter fields are optional. Auth is the
`op_owner` cookie; the body carries no `owner_token` field after the
M6 cleanup:

```json
{
  "limit": 200,
  "from_date": "2026-06-01",
  "to_date": "2026-06-21",
  "pc_id": "pc-01",
  "account_id": "account-uuid",
  "actor": "admin",
  "outcome": "success",
  "category": "money",
  "event_type": "coin_accepted",
  "search": "coin"
}
```

`category` maps to the ADR-0015 filter groups: `auth`, `money`, `time`,
`points`, `admin`, `system`. Each event type maps to exactly one category.
`actor` is `account`, `admin`, `coin_device`, or `system`. `outcome` is
`success`, `failure`, or `denied`. `search` performs a free-text `LIKE` match
against the event summary or initiating account username.

Each row includes the Rust-computed `category` label, the initiating
`actor_account_username` when the actor is an account, joined target
`account_username` and `pc_name`, typed deltas (`amount_cents`, `seconds_delta`
for Remaining Time, `attendance_seconds_delta` for Attendance Time, and
`points_delta_centi` in centipoints), `cash_flag`, the human-readable `summary`,
and the raw `metadata_json` string. The Web Admin displays Remaining Time and
Attendance Time separately; the CSV includes both integer-second fields. For
historical transfer-in rows, the
initiating account is resolved from `metadata_json.from_account_id`.

`export_audit_log_csv` accepts the same filter shape and returns an RFC 4180
CSV string with a header row and one row per matching audit event. The CSV
includes `actor_account_username` separately from the target
`account_username`. The default limit for export is 50,000 rows. The CSV
respects the selected filters exactly as the on-screen viewer does (ADR-0015).

## PCs

`GET /api/pcs`

Returns all visible PCs (the dashboard's PC list). Each PC includes nullable
`clock_difference_ms`, the latest signed Admin-minus-Client clock measurement.
A positive value means the Client clock is behind; Web Admin shows a standing
inline advisory when the absolute value exceeds 60 seconds.

`GET /api/pcs/:id`

Returns one PC or `404` when the ID is unknown. Owner/web-admin only —
the per-PC client identity surface is MAC-keyed (see below).

## Client identity (ADR-0031)

Identity is **server-owned**: the client sends a per-boot MAC in the
`X-OpenPiso-Mac` header and the server resolves which `pcs` row the
machine is by (a) MAC match, (b) the connection peer address matching a
pre-registered `registered_ip` with `mac_address IS NULL`, or (c)
rejection. The `{pc_id}` path on the client surface is gone — the
client never types a PC ID.

The server reads the source IP from the connection peer address via
`ConnectInfo<SocketAddr>`. The IP is never read from a body or
forwarding header; clients cannot spoof identity. A row whose
`mac_address` is already set is never re-bound by an IP match — the
owner clears the binding by editing the PC in web-admin.

### Heartbeat

`POST /api/identity/heartbeat`

Headers:

- `X-OpenPiso-Mac: aa:bb:cc:dd:ee:01` — the boot MAC, optional. The
  server walks the resolution table anyway; an empty/missing header
  still allows IP-bootstrap binding.

Request body:

```json
{
  "hostname": "DESKTOP-123",
  "client_version": "0.1.0",
  "client_time_unix_ms": 1781959200000,
  "guard_coverage": "covered"
}
```

`client_time_unix_ms` is the Client's current wall-clock reading in Unix
milliseconds. It is sent on every Heartbeat for measurement only; older Clients
may omit it without making the Heartbeat fail. The Admin stores the signed
`clock_difference_ms` on the PC as `Admin receipt time − Client reading`: a
positive value means the Client clock is behind. This measurement is never used
for Tamper Kill attribution. A PC whose absolute difference exceeds 60 seconds
shows an inline standing advisory in Web Admin.

`guard_coverage` is the optional Client Guard Coverage diagnostic (ADR-0076):
`covered` when a release Windows Client currently holds its Client Guard pipe
connection, `unavailable` when it does not, and `not_applicable` from debug and
non-Windows builds, which can never hold one. Older Clients omit the field
without failing the Heartbeat; omission resets any stored diagnostic to
unknown — such PCs read as unsupported or unknown and are never alerted for
Guard coverage, and a stale report cannot outlive the Client that made it. A
present but unknown value is rejected at
the boundary. Admin stores the latest reported value on the PC as diagnostic
data only — it grants no identity, session, money, time, Points, or routing
authority.

#### Tamper-kill report (optional) {#tamper-kill}

When the Client Guard has detected that the Client process was killed, it
reports the kill as part of the heartbeat. The server, never the device or
client, decides attribution and writes an immutable audit row:

```json
{
  "hostname": "DESKTOP-123",
  "client_version": "0.1.0",
  "client_time_unix_ms": 1781959200000,
  "tamper_kill_timestamp": "2026-06-20T10:00:05+00:00",
  "tamper_kill_report_age_ms": 2500,
  "tamper_kill_attempt_count": 3
}
```

- `tamper_kill_timestamp` (string, optional) — the Client-side timestamp
  retained as the stable name of the report and duplicate-detection key. It is
  never parsed, compared with the Admin clock, or used as the kill instant.
- `tamper_kill_report_age_ms` (non-negative integer, optional) — whole
  milliseconds elapsed on the Client between the kill and sending this
  heartbeat. It is sent only with a pending report, must have a
  `tamper_kill_timestamp`, and is a sanity-checked duration rather than a
  freshness rule. A value above 24 hours still records the report but forces
  `subject: unknown`; `0` anchors the kill at receipt. A timestamp without an
  age is accepted for older Clients but forces `subject: unknown`. A non-null
  value that is not an integer is also recorded as `unknown` rather than
  discarding the Tamper Kill; a negative integer is rejected at the boundary.
- `tamper_kill_attempt_count` (positive integer, optional) — number of
  Guard-detected deaths coalesced into this report. It is valid only when
  `tamper_kill_timestamp` is present and defaults to 1.

A missing timestamp when either age or attempt count is supplied, or a
negative age, is rejected with `400 Bad Request`.

Responses:

- **200** with the authoritative `Pc` JSON plus `theme_version` when identity
  is resolved. The `Pc` JSON includes nullable `clock_difference_ms`, the latest
  signed Admin-minus-Client clock measurement. When a tamper-kill was processed,
  the response also includes
  `"tamper_kill_acknowledged": "recorded"` so the Client can clear its
  coalesced state. The response also includes
  `"tamper_kill_subject": "account" | "guest" | "none" | "unknown"`; an
  unrecognised value is degraded to `unknown`. The Client uses this
  server-resolved subject to require Guest continuation only for a guest kick,
  even when PC Guest Time remains; `none` behaves exactly as `unknown`.
  The response also includes `"attendance_supported": true` as an additive
  Daily Reward capability flag. Older Admin versions omit it; Clients start
  with support disabled and preserve the cached value when a later heartbeat
  omits the flag. Omission does not mark the Admin connection offline. The same
  flag is included in `pc.updated` events.
- **404** with `{"error":"pc_unregistered"}` when the machine is
  unknown. The client renders the "ask the owner" Lock copy.
- **4xx/5xx** for other failures (structurally malformed body, server error).
  The client maps these to the existing "Can't reach the server" copy.

#### Diagnostic Request on the response (optional) {#diagnostic-request}

When an owner has placed a **Diagnostic Request** for this PC (ADR-0078,
#364), the 200 response additionally carries
`"diagnostic_request_nonce": "<uuid>"` — the single-use name of the owner's
ask for more evidence from a live Client. The ask rides the response in the
shape of the acknowledgement fields above: optional, absent when there is
nothing armed, and never a session, power, or routing authority (deliberately
not a "Heartbeat order").

The Client answers once per nonce: it uploads one windowed, byte-capped slice
of its own log through `POST /api/forwarded-diagnostics`, echoing the nonce.
Admin repeats the same nonce on every Heartbeat response until that answer
arrives or the ask expires (ten minutes, in-memory; a restart clears it), and
the Client deduplicates by nonce, so a repeated response can never cause a
repeated upload. A PC that is offline or powered off produces no error state
for the owner — the ask simply goes unfulfilled, and the next Support Bundle
for the PC simply does not include a slice it never received. The owner arms
the ask with `POST /web-admin/support/diagnostic-request/{pc_id}`.

Server-side attribution logic (ADR-0048 / ADR-0062):
- With a valid age at or below 24 hours, the Admin computes `kill_instant =
  report receipt time − tamper_kill_report_age_ms` on its own clock, then
  selects the Account Session history row covering that instant →
  `subject: account`. The audit row carries that Account as its Target
  Account. A row that ended before report receipt still qualifies;
  `server_restart` closure is an infrastructure boundary and does not erase
  the historical attribution. Only a still-live matching session is ended as
  `tamper_kick`. Balance and points are untouched.
- A missing age, an age above 24 hours, or any otherwise untrusted report →
  `subject: unknown`, with no session end. The Client also degrades any
  unrecognised response subject to `unknown`.
- With a valid age and no historical Account Session, no live account session,
  but PC Guest Time → `subject: guest`, and a guest `tamper_kick` is written.
  Guest Time is unchanged.
- With a valid age and no Account or Guest session → `subject: none`. No
  companion `tamper_kick` is written.

A report always writes exactly one `tamper_kill` audit row and, where there is
an account or guest session to end, exactly one `tamper_kick` session-end event.
No `logout` or `auto_kick` event is ever produced.

When the Subject is `unknown`, the row's existing `metadata_json` also carries
one diagnostic `reason` (and only `unknown` rows carry it):

- `missing_report_age` — the Client sent no Kill Report Age, so it may be an
  older Client.
- `invalid_report_age` — the age was malformed or exceeded the 24-hour sanity
  limit.
- `current_occupant_not_attributed` — no Account Session covered the derived
  kill instant, so the Admin correctly refused to blame the current occupant.

The Audit Log and Notifications review summaries render these causes in plain
language. Other Tamper Kill Subjects have no `reason` metadata.

### Disconnect

`POST /api/identity/disconnect`

Headers: `X-OpenPiso-Mac: …` (as above). No body. Responses mirror the
heartbeat contract — 200 with the authoritative `Pc` or 404
`pc_unregistered` on rejection.

### Client Guard Poll (ADR-0077)

`POST /api/guard/state`

Headers: `X-OpenPiso-Mac: …`. Sent by the Client Guard, not the Client. The
Guard has no other connection to Admin and opens no listener of its own.

Request body:

```json
{
  "client_connected": false,
  "disarmed": false,
  "relaunch_attempts": 3,
  "forwarded": {
    "lines": [
      {
        "level": "warn",
        "target": "openpiso_client_guard::pipe",
        "message": "rejecting foreign pipe client (process_id=99)",
        "timestamp_unix_ms": 1760000000000
      }
    ]
  }
}
```

- `client_connected` — whether a verified Client currently holds the Guard pipe.
- `disarmed` — whether the Guard stood down for this boot after a PIN-gated
  owner quit.
- `relaunch_attempts` — relaunch attempts spent on the current absence. Admin
  never escalates against a Guard reporting `0`.
- `forwarded` — optional; absent when the Guard has nothing buffered or
  forwarding is gated off. The Guard gains no second network call
  ([ADR-0077](adr/0077-client-recovery-escalation.md),
  [ADR-0078](adr/0078-support-bundle.md)): its batch rides this request, with
  at most 64 lines of at most 2000 bytes per poll and the remainder draining
  across later polls. The lines follow the same shape as
  [`POST /api/forwarded-diagnostics`](#post-apiforwarded-diagnostics) minus
  `source`: the poll is the Guard's, so Admin tags every accepted line with
  the server-resolved PC identity and source `guard`. Because the 200
  response is also this batch's acknowledgement, a poll that carries lines
  from a sender Admin cannot identify — no MAC header, or a MAC no registered
  PC holds — is answered `401` instead of "stay on", so those lines are never
  acknowledged for something Admin never stored; they wait in the Guard's
  buffer for a later poll. A malformed batch is refused whole with the same
  statuses as the batch endpoint. Delivery is at-least-once, as with the
  batch endpoint: a poll response lost in flight makes the Guard resend its
  batch, so Admin may append the same lines twice. A poll without
  `forwarded` behaves exactly as it did before the widening, including
  "stay on" for an unknown MAC.

Response:

```json
{ "power_off": true, "nonce": "…", "decided_at": "…", "poll_interval_secs": 10 }
```

`power_off` is desired state, not a queued command: the Guard may read the same
answer many times and acts once per `nonce`, which it persists. An unknown MAC,
an unregistered PC, or a missing header answers `power_off: false` rather than
an error — a Guard that cannot be identified is never left guessing. The one
exception is a poll that carries a `forwarded` batch from such a sender, which
is refused with `401` so its lines are never acknowledged for something Admin
never stored.

The endpoint resolves the PC read-only by MAC. Unlike the heartbeat it can
never create, bind, or rebind a `pcs` row, and it writes no session, money,
Remaining Time, or Points value.

### PC event stream

`GET /api/identity/events`

Headers: `X-OpenPiso-Mac: …`. Opens a Server-Sent Events stream scoped
to the resolved PC. The server sends an initial `pc.updated` event
immediately, followed by authoritative updates after committed coin
transactions, heartbeat status changes, and timer ticks. A 404
`pc_unregistered` rejection closes the stream; the client retries
quietly on the next tick.

Example event:

```text
event: pc.updated
data: {"id":"pc-01","name":"PC 01","status":"in_use","remaining_seconds":300,"power_state":"active","last_seen_at":"2026-06-20 12:00:00","shop_status":{"open":true,"next_change_at":"2026-06-20T22:00:00+08:00","reason":"schedule","last_call_active":true}}
```

The same `shop_status` object is carried by the existing `pc.updated`
broadcast consumed by connected Clients and the Central Terminal; surfaces
render it and do not evaluate the schedule themselves.

A committed coin also sends `coin.accepted`, scoped to the same PC. Its coin
transaction payload includes integer `seconds_added` (credited total) and
`coin_promo_bonus_seconds` so a logged-in Widget can show the exact Coin Promo
split:

```text
event: coin.accepted
data: {"pc_id":"pc-01","amount_cents":500,"seconds_added":2100,"coin_promo_bonus_seconds":600}
```

The server sends keep-alive comments every 10 seconds. Clients must
reconnect when the stream closes and continue using heartbeats for
liveness and state reconciliation.

### `power_state` (expiry control)

Each broadcast `pc.updated` carries a server-authoritative `power_state`
(ADR-0005, ADR-0022) telling the connected client what to do about expiry:

- `active` — normal operation.
- `warning` — `0 < remaining_seconds ≤ 120`; show a non-blocking low-time
  warning.
- `shutdown` — a connected PC has zero authoritative Remaining Time; run a
  Shutdown Grace countdown (default 300 seconds) and power the PC off (real OS shutdown) if no top-up
  cancels it.

The server commands `shutdown` for any connected PC at zero Remaining Time,
including a freshly booted PC reporting zero. A top-up resets `power_state` to
`active`/`warning` and cancels the shutdown. The initial event on stream open and
read-only PC queries use the derived current state; the SSE filter is scoped to
the resolved `pc_id` server-side, so a single subscription never sees state
for the wrong row. When the Client later loses the server, its separate Offline
Shutdown Countdown is governed by ADR-0045 and cached Offline Grace.

## Coin events

> **Status:** the v1 coin entry point is the **serial** `CoinSource`
> implemented in `apps/admin/src-tauri/src/coin_source.rs` (see [ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md)).
> M2 wires a closed `CoinBatch` to the existing
> `record_coin_event(selected_pc)` path through the server-owned
> `TerminalSelection` (see the "Central Terminal selection" section
> below). The serial source uses a dedicated ~30 ms Pulse Sampler and the
> simulator injects pulses without a sampler thread; both feed the same
> selection path. Every positive closed pulse count is valued by checked
> `pulse_count × 100 cents`; runtime acceptance never reads the retained Pulse
> Map. The `POST /api/coin-events` endpoint remains a legacy LAN/device-shaped
> path; the Tauri simulator helper for the kiosk injects pulses into the active
> `TerminalSelection` instead of posting arbitrary `pc_id` / amount values.
> Coin Trace Ids correlate commit logs with Audit Log metadata and the linked
> transaction; see [ADR-0043](adr/0043-coin-ingestion-sampling-and-observability.md).

`POST /api/coin-events`

Request:

```json
{
  "device_id": "dev-simulator",
  "pc_id": "pc-01",
  "amount_cents": 100,
  "pulse_count": 1,
  "timestamp": "2026-06-20T10:00:00Z",
  "signature": null
}
```

`amount_cents` and `pulse_count` must be positive. When present, `timestamp`
must be RFC 3339; it is recorded as device metadata but does not replace the
server's transaction timestamp. The server always calculates authoritative `seconds_added` from the active
rate plus any applicable Coin Promo bonus and resolves the target from the
selected PC's current session state. Devices cannot submit an account ID.

If the selected PC has an active account session, the coin credits that
account with time and fractional points awarded directly in centipoints;
only the sub-centipoint remainder carries on the account, in centicents
(ADR-0038). If the PC is in guest mode, the coin credits PC Guest Time and
earns zero points. Claiming Guest Time later does not grant retroactive
points.

The seeded `dev-simulator` device is accepted without a signature only while
the `simulator_enabled` setting is `true`. Any other device ID must exist in
`coin_devices`, be active, and provide a `signature` exactly matching its
locally stored secret. Authentication failures return HTTP 401.

The shared-secret comparison is an MVP validation step. The payload is not yet
HMAC-signed and has no replay protection, so real hardware must remain on a
trusted private LAN.

When the server-resolved Shop Status is Shop Closed, the event is refused with
HTTP `409` and the stable error code `shop_closed`. No balance or transaction
is written. The refusal appends a `coin_denied` Audit Log row containing a
server-minted Coin Trace Id in `metadata_json`, and writes a diagnostic record
with the same id. Shop Hours disabled (`enabled = false`) is reported as Shop
Open even when every saved day is closed, so coin intake remains unchanged.

Successful response:

```json
{
  "id": "transaction-uuid",
  "pc_id": "pc-01",
  "pc_name": "PC 01",
  "account_id": "account-uuid",
  "account_username": "Customer_01",
  "amount_cents": 100,
  "seconds_added": 420,
  "coin_promo_bonus_seconds": 120,
  "points_earned_centi": 20,
  "source": "simulator",
  "target_kind": "account",
  "created_at": "2026-06-20 10:00:01"
}
```

## Diagnostic forwarding

> **Status:** implements [ADR-0078](adr/0078-support-bundle.md). The batched,
> device-authenticated endpoint below is the Client's transport; the Guard's
> forwarding rides the [Guard Poll](#client-guard-poll-adr-0077) it already
> makes, with no second network call. Both land in the same forwarded log.

### `POST /api/forwarded-diagnostics`

Accepts a batch of Forwarded Diagnostics from a device on the LAN. This
endpoint is deliberately **not** the Heartbeat: the Heartbeat carries
authority values and must stay small and prompt, so a diagnostic send runs on
its own connection and cadence and can never delay or fail timer
synchronisation.

The device is authenticated the same way as the identity endpoints: the PC is
resolved read-only from the `X-OpenPiso-Mac` header. Lines never carry a
`pc_id` — Admin tags every accepted line with the server-resolved PC identity
and the batch's `source` discriminator, and appends it to the dedicated
daily-rotating forwarded log (`forwarded.<date>.log`), which shares Admin's
log rotation and seven-day retention but is never interleaved into Admin's
own diagnostic log. Nothing is written to SQLite.

Request:

```json
{
  "source": "client",
  "lines": [
    {
      "level": "warn",
      "target": "openpiso_client::tamper",
      "message": "guard pipe lost",
      "timestamp_unix_ms": 1760000000000
    }
  ]
}
```

- `source` is `client` or `guard`.
- `level` is `warn`, `error`, or `info` — INFO only for the named allowlist
  the device applies before sending (ADR-0078): Guard pipe connect and
  disconnect, Client Guard Coverage transitions, Client startup milestones,
  Offline Shutdown Countdown arm and disarm, and Tamper Kill — under a
  device-side rate cap. Heartbeat failure forwards through the unconditional
  WARN/ERROR path instead, and a device buffer overflow emits a forwarded
  marker line stating how many lines were dropped. That count survives marker
  eviction and failed sends; evicted in-flight lines count as lost only if
  Admin does not acknowledge their batch. A Diagnostic Request slice
  may additionally carry `debug` and `trace`: the slice of the Client's full
  log must stay faithful to the levels it found there, and the proactive
  senders never capture those levels.
- `timestamp_unix_ms` is the device wall-clock capture time, kept alongside
  Admin's receive time because a line may sit in the device buffer across an
  Admin outage.
- `message` is at most 2000 bytes; a batch carries at most 64 lines.
- `diagnostic_request_nonce` (string, optional) — present only on the batches
  that carry a Diagnostic Request slice (ADR-0078 / #364). It echoes the nonce
  Admin placed on the Heartbeat response; when Admin sees it back from a
  registered PC it stops delivering that nonce, so the ask is fulfilled
  exactly once. Regular forwarding batches omit the field, and a nonce that
  does not match the armed one is ignored, never consumed.

Responses:

- `200 OK` — `{"accepted": n}`. This 2xx response **is the acknowledgement**:
  the device releases exactly the lines it sent and retains everything else.
  A failed or refused send retains the batch for the next attempt.
- `400 Bad Request` — malformed batch (empty, over the line cap, empty
  message, message over 2000 bytes, negative timestamp). The batch is
  rejected whole; nothing is partially accepted.
- `401 Unauthorized` — missing `X-OpenPiso-Mac` header or a MAC no registered
  PC holds.
- `422 Unprocessable Entity` — the body is not valid JSON, or a `level` /
  `source` value is not one of the known labels.

Unlike the retired single-entry endpoint, there is no per-PC rate cap: the
endpoint is device-authenticated, a batch is bounded at 64 lines of 2000
bytes, and the device-side ring bounds how much any sender can hold, so the
old unauthenticated-flood guard has no one left to guard against.

Device-side gate: forwarding runs in release builds only — of the Client and
of the Client Guard alike; `OPENPISO_FORWARD_DIAGNOSTICS=1` turns it on
deliberately in a development build. The Admin endpoint and sink are
unconditional so they stay testable in development.

### The Diagnostic Request slice {#diagnostic-request-slice}

The slice is the Client's answer to a Diagnostic Request (ADR-0078 / #364):
one windowed, byte-capped slice of its own full log, uploaded through this
endpoint with `source: "client"` and the nonce echoed on every batch. The
window reaches fifteen minutes back from the moment the Client acts; the byte
cap is 64 KiB of serialised wire lines, keeping the newest lines and dropping
older ones first; batches larger than 64 lines drain across several requests,
and a failed or refused batch is retained for the next attempt. The upload is
deliberately never on the Heartbeat path — it runs on the Client's third
connection loop, so building or sending the slice cannot delay or fail timer
synchronisation.

The slice is framed by statement lines on target
`openpiso_client::diagnostic_request` that say exactly what was asked and
what was cut: which nonce it answers, the window bounds, how many window
lines were uploaded, and — when they fired — that the byte cap dropped N
older window lines and that the window excluded N earlier log lines. The
frame is sent even for an empty window, so a fulfilled ask is never mistaken
for a lost one. What lands in the forwarded log is tagged `client` like any
Forwarded Diagnostic, so the next Support Bundle whose window covers it
includes the slice together with its own truncation statements.

The Client keeps one slice in flight: a batch that fails or is refused is
retained for the next attempt (the same stance as Diagnostic Forwarding)
until it is fully uploaded or until a newer ask replaces it — an ask older
than the Heartbeat's ten-minute expiry is unfulfilled by then, and the
replacement is what the owner last asked for.

## Central Terminal selection

> **Status:** the Central Terminal **Selection** state machine
> ([ADR-0023](adr/0023-serial-coin-input-and-terminal-selection.md)) is the
> M2 milestone. Coins enter the shop through the server-owned selection —
> the device payload never carries a `pc_id`. The selection is a
> Tauri-local concern: the `:3210` LAN API does not expose it (M4 adds the
> owner `:8080` surface as a separate concern).

### `open_selection` (Tauri command)

Open a Selection Window for the given PC. Rust refuses the request with the
stable `shop_closed` error code while the server-resolved Shop Status is Shop
Closed; this refusal is diagnostic-only and does not append an Audit Log row.
When Shop Open, it asserts the COM-port set pin server-side; coins accepted
while the window is open are credited to `pc_id`. A second open while one is
active is rejected with HTTP-style `Conflict("terminal in use")` so the kiosk
can show the same canonical message regardless of source. A window already
open at the Shop Closed boundary is not force-closed and continues to its
normal Enter, Escape, or inactivity close.

```ts
import { openSelection } from "./lib/api";
const state = await openSelection("pc-01");
// state.pc_id === "pc-01", state.inactivity_remaining_seconds > 0
```

Returns the broadcast `SelectionState`:

```json
{
  "pc_id": "pc-01",
  "opened_at_unix_ms": 1719460000000,
  "inactivity_remaining_seconds": 15,
  "hard_cap_remaining_seconds": 180,
  "inserted_amount_cents": 0,
  "inserted_seconds": 0
}
```

### `close_selection` (Tauri command)

Close the active Selection Window. The Set Pin is re-inhibited; the source's
buffered gap window is drained and credited to the just-selected PC. A train
that straddles inhibition remains one Coin Batch because later edges are
admitted while its buffer is non-empty; an edge observed with an empty buffer is
refused as an Inhibited Pulse. `reason` is one of `"enter"`, `"inactivity"`,
`"hard_cap"`, `"manual"`, or `"shutdown"`; defaults to `"manual"`.

```ts
import { closeSelection } from "./lib/api";
await closeSelection("enter"); // customer pressed Enter
```

Returns the closed `SelectionState`, or `null` if no window was open.

### `get_selection` (Tauri command)

Snapshot of the current Selection Window, or `null` if no window is open.
The frontend calls this on mount for the initial state and listens to the
`selection-state` Tauri event for live updates.

```ts
import { getSelection } from "./lib/api";
const current = await getSelection();
if (current) {
  // a window is open
}
```

### `selection-state` (Tauri event)

Every published selection change is broadcast on `selection-state` so the
kiosk can advance its countdown / "inserted total" without polling. The payload
is a `SelectionState` while a window is open and `null` when the server closes
the window. `inserted_amount_cents` and `inserted_seconds` reflect the running
total for the current window only — they reset on the next `open_selection`.

```ts
import { listen } from "@tauri-apps/api/event";
import type { SelectionState } from "./lib/types";

await listen<SelectionState | null>("selection-state", (event) => {
  const state = event.payload;
  // update the kiosk UI
});
```

### Authority and invariants

- The Set Pin is asserted **only** while a window is open, and re-inhibited on
  close, timeout, hard cap, crash, and shutdown. A train already in flight may
  straddle inhibition; its buffered tail still credits the just-selected PC as
  one Coin Batch during the configured gap plus scheduling headroom. A refused
  edge with an empty buffer is an Inhibited Pulse and never credits.
- Routing (account vs guest, points earn, sub-threshold carry) stays in
  the existing `sessions::record_coin_event` path per ADR-0008. M2 does
  not change it.
- The selection is in-memory only; a server restart clears the window
  and the next customer interaction re-opens it.
- Owner auth is not required — the selection is the customer-facing
  Central Terminal flow. The M4 owner HTTP API on `:8080` is a separate
  surface (ADR-0024).
- The Tauri `simulate_coin` command is an owner-only development helper that
  injects a pulse count into the active `SimulatorCoinSource`; it does not
  accept `pc_id`, cents, or seconds, so simulated coins still route through the
  active Terminal Selection.

## Central Terminal kiosk surface (M3)

> **Status:** the customer-facing Central Terminal **kiosk** is the M3
> milestone. The commands and events below are the read-only surface
> the kiosk uses to render the PC grid, the coin-insert window, and the
> "out of order" screen. **All authority (money, time, points) still
> lives in Rust**; the kiosk never supplies a `pc_id` for a coin, never
> trusts a device-supplied time or amount, and never mutates balances
> directly. The selection commands above are the only mutation
> surface the kiosk calls.

### `list_pcs_kiosk` (Tauri command)

Returns the live, enriched PC list for the kiosk grid. The same shape
as the owner dashboard's per-PC rows, but without owner auth. The
kiosk calls this on mount and re-merges per-PC `pc-updated` events for
live updates.

```ts
import { listPcsKiosk } from "./lib/api";
const pcs = await listPcsKiosk();
// pcs[].status, pcs[].remaining_seconds, pcs[].timer_mode, pcs[].active_session
```

### `get_kiosk_status` (Tauri command)

Returns the current coin-source health. The M2 simulator is always
`available`; the M4 serial source publishes `out_of_order` whenever the
COM handle is poisoned or missing. The kiosk uses this to choose
between the normal flow and the "out of order" screen, and refuses to
open a Selection Window while `out_of_order` is the current state.

```ts
import { getKioskStatus } from "./lib/api";
const status = await getKioskStatus();
// { kind: "available", reason: "" }
// or { kind: "out_of_order", reason: "Coin acceptor is unavailable" }
```

### `get_kiosk_config` (Tauri command)

Returns the live time-purchase rate so the kiosk's "equivalent time"
preview matches the owner's configured rate instead of a hard-coded
constant. Read-only and unauthenticated (the rate is printed on the
insert screen). The server still computes the authoritative
`seconds_added` when each coin commits; this only drives the on-screen
estimate, using the same integer formula as `pricing::calculate_seconds`
(`seconds = inserted_cents * rate_seconds / rate_amount_cents`).

```ts
import { getKioskConfig } from "./lib/api";
const rate = await getKioskConfig();
// { rate_amount_cents: 100, rate_seconds: 300 }  // ₱1 = 5 minutes
```

The command is also the mount-time fallback for the `shop-rate-updated`
Tauri event. The Central Terminal subscribes before this read, so a Shop Rate
save during boot cannot be lost. The event carries the same `KioskConfig`
payload and updates only the read-only equivalent-time preview.

### `get_shop_name` (Tauri command)

Returns the current non-blank Shop Name directly from the admin-owned SQLite
database. The kiosk calls it during startup, without HTTP, before rendering.

### `get_shop_status` (Tauri command)

Returns the server-resolved Shop Status directly from the admin-owned SQLite
database. The kiosk renders this value and never evaluates the schedule itself.
Live changes continue to arrive on the existing `pc.updated` broadcast.

### `get_kiosk_lock` (Tauri command)

Returns the current Kiosk Lock state (ADR-0056) — whether the Central Terminal
window is pinned above every other window. Ephemeral and read-only; the kiosk
calls it on mount to render its header signal. The value at launch is always
`{ locked: true }`.

### `set_kiosk_lock` (Tauri command)

Engages or releases the Kiosk Lock (ADR-0056).

Releasing (`locked: false`) requires the owner PIN, which Rust verifies — the
kiosk only forwards the typed characters. Repeated wrong PINs are rate-limited
by the same backoff as the Web Admin login, and a throttled attempt is refused
before the credential is verified at all. Engaging (`locked: true`) needs no
PIN, because tightening the kiosk is always safe.

The window flag is applied before the in-memory state moves, so a failed window
call never leaves the UI claiming a state the OS does not have. Successes,
denials, and lockouts are audited as `kiosk_lock_engaged`,
`kiosk_lock_released`, `kiosk_lock_release_denied`, and
`kiosk_lock_release_throttled` — never as a Web Admin login.

```ts
import { setKioskLock } from "./lib/api";
await setKioskLock(false, pin); // { locked: false }
await setKioskLock(true);       // { locked: true }
```

Rejections surface as an `Error`: `invalid owner PIN`, or
`too many attempts; try again in <n>s` once throttled.

### `shop-name-updated` (Tauri event)

Carries the committed Shop Name immediately after web-admin persists it. The
kiosk updates its heading, accessible region label, and document title live.
The native Tauri window title is unaffected.

### `kiosk-status` (Tauri event)

Every change of the coin-source health is broadcast on `kiosk-status`.
The kiosk subscribes for live transitions (e.g. the M4 swap from
`available` to `out_of_order` when the COM port is dropped) and
re-renders accordingly.

```ts
import { listen } from "@tauri-apps/api/event";
import type { KioskStatus } from "./lib/types";

await listen<KioskStatus>("kiosk-status", (event) => {
  if (event.payload.kind === "out_of_order") showOutOfOrder();
  else showGrid();
});
```

### Authority and invariants (M3)

- The kiosk is the **only** consumer of `selection-state` and
  `kiosk-status`; the `:3210` LAN API does not expose them.
- The `pc-updated` broadcast remains the all-PC live stream; the
  kiosk's `list_pcs_kiosk` is the initial snapshot and the
  `pc-updated` events layer on top.
- The kiosk never mutates `pcs.remaining_seconds`, account balances,
  or any other authority directly. Every accepted coin still flows
  through the existing `record_coin_event` path with `pc_id =
selection.pc_id` (ADR-0008 unchanged).

## Owner HTTP API on `:8080` (M4, finalised in M6)

The admin process binds a **second** Axum server on `0.0.0.0:8080` for the
owner web app (ADR-0024). It is entirely separate from the `:3210`
client/device API above; **owner endpoints are never mounted on `:3210`**.
M6 retired the desktop owner console, so `:8080` is now the **sole** owner
surface — every owner mutation that used to flow through a Tauri command
runs through one of the handlers below, in the same Rust process that
hosts the Central Terminal kiosk.

> Current implementation note: the Owner Session cookie, CSRF, Origin, and
> idle/absolute expiry parts of ADR-0074 are implemented (see "Owner Session
> contract" below). Remote Control, WebSocket upgrades, step-up grants, and
> connection cancellation are not. Keep `:8080` on the wired/admin firewall or
> VLAN; it is still plain HTTP.

### Authentication (PIN + session cookie)

The PIN is verified against the existing `owner_credentials` row and an Owner
Session is issued into the existing in-memory `owner_tokens` store, carried in
the `op_owner` cookie. Every protected route requires that cookie; a request
without a live one returns `401`.

A denied **mutation** is recorded as a `web_admin_access_denied` audit event. A
denied **safe read** (`GET`/`HEAD`/`OPTIONS`) is not: the web admin polls
read-only endpoints on a timer, so auditing those turns any unauthenticated page
left open into a steady flood that buries real denials. Nothing is disclosed by
the read — the `401` is returned before the handler runs.

Owner Sessions are in-memory, so restarting the admin app invalidates every
session while the browser still holds its cookie. The web admin treats a `401`
as "session gone" and returns to the login screen rather than continuing to poll.

| Method + path            | Auth             | Body             | Returns                                                        |
| ------------------------ | ---------------- | ---------------- | -------------------------------------------------------------- |
| `GET /web-admin/session` | none (cookie optional) | –   | `{ "configured": bool, "authenticated": bool, "csrf_token"?: "…" }`; `csrf_token` only when authenticated, omitted for anonymous callers; never refreshes activity |
| `POST /web-admin/setup`  | none (first run) | `{ "pin": "…" }` | `{ "csrf_token": "…" }` + `Set-Cookie`. `409` if already configured; seeds the separate Client Owner PIN from the same PIN |
| `POST /web-admin/login`  | none             | `{ "pin": "…" }` | `{ "csrf_token": "…" }` + `Set-Cookie`. `401` on bad PIN; `429` + `Retry-After` while rate-limited |
| `POST /web-admin/credential` | cookie + CSRF + Origin | `{ "current_pin": "…", "new_pin": "…" }` | `{ "ok": true }`; `401` on wrong current PIN, `400` unless new PIN is 4–128 characters |
| `POST /web-admin/activity` | cookie + CSRF + Origin | –          | `{ "ok": true }`; restarts the idle window of a live session |
| `POST /web-admin/logout` | cookie + CSRF + Origin when the session is live | – | `{ "ok": true }` + cookie clear (`Max-Age=0`). Missing, expired, or revoked session: clears the cookie only, idempotent. Live session with bad Origin/CSRF: `403` |

`/web-admin/login` is rate-limited per source IP (peer socket address; no
trusted proxy on the LAN). After 5 consecutive wrong PINs an IP is locked out;
the endpoint returns `429` with a `Retry-After` header (remaining seconds) and
does **not** verify the PIN while locked. Lockout duration doubles per further
lockout burst (≈60s → 120s → … capped at ~15 min) and always decays; a correct
PIN resets that IP's state. State is in-memory only (cleared on restart). The
transition into lockout emits one `owner_login_throttled` audit event (once per
lockout, not per blocked request). This gates the **Web Admin PIN**, distinct
from the Owner PIN (see `docs/glossary.md`). A successful credential rotation
revokes every owner session except the caller's token; it is not subject to the
login throttle because it requires a valid session plus the current PIN.

### Owner Session contract (ADR-0074, implemented)

- `op_owner` is `HttpOnly; SameSite=Strict; Path=/; Max-Age=43200`. It omits
  `Secure` while the accepted deployment remains HTTP. Setup and login never
  return the owner token in JSON; their body is only `{ "csrf_token": "…" }`.
- The server expires an Owner Session after 30 minutes without human input or
  12 hours after issue. Reads and polling never refresh activity. Only
  `POST /web-admin/activity`, reported by the frontend for human pointer,
  keyboard, or touch input (throttled, not synthetic or background events),
  restarts the idle window.
- The CSRF token is session-bound and held only in frontend memory. A reload
  restores it from `GET /web-admin/session`, which returns it only for an
  authenticated caller and omits it for anonymous callers.
- Every authenticated mutation (any non-safe method, including logout while the
  session is live) requires the `X-CSRF-Token` header and an `Origin` exactly
  equal to `http://<Host>`; forwarded headers are not trusted. Safe reads
  require neither. Denied mutations append `web_admin_access_denied`.
- A successful credential rotation preserves the caller's Owner Session and
  revokes every other one (ADR-0050).

Not implemented: WebSocket upgrades, Remote Control PIN step-up/token rotation,
and long-lived-connection cancellation below. In development, the Vite proxy
keeps `changeOrigin: false` so the browser's `Host` reaches the owner server
and matches `Origin`.

### Planned Remote Control endpoints (ADR-0075)

These routes are not implemented yet. All HTTP mutations require the hardened
Owner Session, CSRF token, and exact Origin from ADR-0074.

| Method + path | Body | Result |
| --- | --- | --- |
| `GET /web-admin/remote-control/settings` | none | `{ enabled, allowed_ipv4_cidr, vnc_port }` |
| `PUT /web-admin/remote-control/settings` | `{ enabled, allowed_ipv4_cidr, vnc_port }` | Atomically replaces the record; returns the committed record |
| `POST /web-admin/remote-control/sessions` | `{ pc_id, pin }` | Rotates caller auth, creates the view-only Remote Control Session and 30-second pending grant, returns `{ session_id, csrf_token }` |
| `GET /web-admin/remote-control/socket` | WebSocket upgrade | Consumes the one pending grant bound to the owner cookie; no grant secret appears in the URL |
| `POST /web-admin/remote-control/sessions/{session_id}/control` | none | Creates a 30-second full-control grant under the same session |
| `DELETE /web-admin/remote-control/sessions/{session_id}` | none | Explicit owner disconnect; idempotent |

The session-creation handler resolves `pc_id` to a canonical private
`registered_ip`, confirms membership in Remote Control Settings' IPv4 CIDR,
loads the shop-wide VNC port, rejects unsafe targets, and snapshots the result.
The browser never sends or receives a host, IP, or port. Dashboard rows gain
`remote_control_configured`; it means valid configuration, not reachability.

Exactly one Remote Control Session may exist shop-wide. A second creation
returns `409`. Disabled or invalid configuration also fails before WebSocket
upgrade. TCP and RFB failures render inside the dialog; Rust remains RFB-blind
and does not accept frontend claims as proof of TightVNC authentication.

### Owner endpoints (all require the `op_owner` cookie)

These reuse the exact same `owner`/`db`/`reports`/`accounts`/`audit`/
`diagnostics` calls the retired desktop Tauri commands used — there is no
second implementation of any mutation, and authority (money, time, points,
routing) stays in Rust. M6 removed the `owner_token` field from every request
body. The cookie remains the authentication credential; ADR-0074 mutations
additionally require the `X-CSRF-Token` header and exact Origin.

The Points settings contract exposes the active earn and paid-time rates needed
for live tier previews. The owner UI labels this surface **Points** (and the
earn rate "Credits per point", entered in pesos), but the endpoints, payload
fields, and identifiers keep their `economy` / cents naming. Its safety fields
are computed server-side from the most generous Redemption Tier:

```json
{
  "earn_cents_per_point": 5000,
  "rate_amount_cents": 100,
  "rate_seconds": 300,
  "effective_bonus_basis_points": 960,
  "unsafe_bonus_threshold_basis_points": 2000,
  "unsafe_bonus": false
}
```

Backup endpoints are owner-session-gated on the owner surface at `:8080`:
every request below requires the `op_owner` session cookie. They are not
mounted on the `:3210` client/device API, so clients and coin devices cannot
list, create, download, delete, schedule, or restore Backups.

| Method + path                                      | Mirrors command                | Notes                                                                                                                                  |
| -------------------------------------------------- | ------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /api/theme`                                   | –                              | full theme record                                                                                                                      |
| `PUT /api/theme`                                   | –                              | full-record theme write; atomic version bump; no audit event                                                                           |
| `GET /web-admin/dashboard`                         | `get_dashboard`                | live PC list (enriched)                                                                                                                |
| `POST /web-admin/coin/simulate`                    | `simulate_coin`                | `{ "pulse_count"?: int }`; injects through the active selection; development kiosk exposes ₱1/₱5/₱10/₱20 buttons and never accepts amount/seconds |
| `GET /web-admin/transactions?limit=`               | `get_transactions`             |                                                                                                                                        |
| `GET /web-admin/today`                             | `get_today_report`             |                                                                                                                                        |
| `GET /web-admin/pcs`                               | `list_pc_settings`             | owner view of PC names and terminal visibility                                                                                         |
| `PUT /web-admin/pcs/{id}`                          | `update_pc_settings`           | `{ "name": string, "visible": bool }`                                                                                                  |
| `POST /web-admin/pcs/{id}/sign-out`                | –                              | Owner Sign-Out: `{ "session_id": string }` → `{ "signed_out": bool, "username": string \| null }`; ends the Active Session, balance stays on the account; idempotent — a stale/mismatched `session_id` returns `signed_out: false` rather than ending another session; works on an unreachable PC |
| `POST /web-admin/pc-monitoring/check/{pc_id}`      | –                              | PC Monitoring Check now (ADR-0076): one `PcMonitoringCheck`; strictly non-mutating; no request body — the probe target is always the row's server-owned `registered_ip` |
| `POST /web-admin/pc-monitoring/check-all`          | –                              | Check now for every eligible PC (visible with a registered IP); one result per eligible PC; no body, no writes |
| `GET /web-admin/pc-monitoring/status`              | –                              | PC Monitoring state: `{ "enabled": bool, "platform_supported": bool }`; feeds the standing advisory |
| `PUT /web-admin/pc-monitoring/enabled`             | –                              | `{ "enabled": bool, "current_pin": string }`; requires an authenticated Owner Session AND the current Owner PIN; audited; disabling closes active incidents as monitoring-disabled in one transaction; non-Windows Admin refuses |
| `POST /web-admin/pc-monitoring/suppressions/{pc_id}` | –                            | `{ "duration_minutes": int, "current_pin": string }` → one `MonitoringSuppression`; creates or replaces the per-PC Monitoring Suppression (ADR-0076 / #342); requires Owner Session + current Owner PIN; duration is validated at the Rust boundary (1–60 minutes, fixed v1 policy) and the absolute expiry is Admin-computed; audited; unknown PC 404, hidden PC 400, invalid duration 400 |
| `POST /web-admin/pc-monitoring/suppressions/{pc_id}/cancel` | –                      | `{ "current_pin": string }` → `{ "cancelled": bool }`; cancels the per-PC Monitoring Suppression; same PIN step-up and boundary validation as creation; cancelling with nothing active stays a success (`cancelled: false`) |
| `GET /web-admin/support-bundle?pc_id=&window_seconds=&incident_opened_at=` | –                           | JSON string containing the bounded, redacted Markdown **Support Bundle** (ADR-0078) for one PC; `window_seconds` defaults to 21,600 (6 hours) and is clamped to ≤ 86,400; `incident_opened_at` (UTC RFC3339 or SQLite datetime) anchors the window around that incident instead — one hour before to fifteen minutes after; unparseable anchor 400; unknown PC 404, missing/blank `pc_id` 400 |
| `POST /web-admin/support/diagnostic-request/{pc_id}` | –                              | places the **Diagnostic Request** (ADR-0078 / #364): the Heartbeat response then carries a single-use nonce asking the live Client to upload one windowed, byte-capped slice of its own log through `POST /api/forwarded-diagnostics`, which the next Support Bundle includes; → `{ "ok": true }`; Owner Session only, no PIN, no durable write; re-arming while unexpired re-states the same ask; unknown PC 404, blank `pc_id` 400; an offline or unreachable PC never produces an error — the in-memory ask simply expires unfulfilled after ten minutes |
| `GET /web-admin/host-diagnostics` | –                              | Admin's own health (ADR-0082 / #378): `{ "runs": [ { "version", "pid", "started_at", "ended_at"?, "exit_reason"?: "window_closed" \| "application_exit" \| "panic", "suspected_hard_exit": bool } ] }` newest first — a start with no end on any run but the live one reads `suspected_hard_exit: true` — plus `"panic_file": { "exists", "size_bytes", "last_modified_at"?, "newest_8kb"? }` carrying the newest 8 KB of `logs/panic.log` (lossy UTF-8); empty history and an absent panic file answer cleanly; any authenticated Owner Session, served identically with Diagnostics Mode on and off (ADR-0081) |
| `GET /web-admin/telegram/status` | –                              | Telegram integration state (ADR-0076 / #345): `{ "platform_supported": bool, "connected": bool, "pending_bot_username"?: string, "destination"?: { "bot_username", "chat_title", "chat_id", "connected_at" }, "last_test"?: { "verified_at" }, "delivery"?: { "last_sent_at"?, "last_error"?, "last_error_at"?, "pending_count": number, "queued_count": number, "retrying_count": number, "expired_count": number } }`; never contains the bot token or message content. Delivery counts (#347): pending totals queued (never attempted) plus retrying (failed at least once); expired items were abandoned unsent after the 24-hour retry window |
| `POST /web-admin/telegram/setup/token` | –                         | `{ "token": string }` → `{ "bot_username": string }`; verifies a candidate bot token against the Bot API and holds it in memory only; persists nothing |
| `POST /web-admin/telegram/setup/detect` | –                        | one explicit look for private `/start` messages sent within the last ten minutes using the pending token → `[{ "chat_id": string, "title": string }]`; other messages, groups, and channels are filtered server-side; this setup-time call is the only polling the integration ever performs |
| `POST /web-admin/telegram/connect` | –                             | `{ "chat_id"?: string, "current_pin": string }` → full status; confirms the destination (manual fallback id must be a positive integer) and sends the test message; the new configuration activates only if that test succeeds — failure leaves the working configuration untouched; requires Owner Session + current Owner PIN; audited |
| `POST /web-admin/telegram/disconnect` | –                         | `{ "current_pin": string }` → `{ "ok": bool }`; deletes the protected token and destination in one transaction with its audit event; local PC Monitoring is untouched; requires Owner Session + current Owner PIN |
| `GET /web-admin/economy`                           | `get_economy_settings`         | earn/time rates plus Effective Bonus % from the most generous Redemption Tier; no legacy redeem fields                                 |
| `PUT /web-admin/economy`                           | `update_economy_settings`      | `{ "earn_cents_per_point": int }`; legacy redeem fields are rejected                                                                   |
| `GET /web-admin/shop-rate`                         | –                              | active Shop Rate: `{ "amount_cents": int, "seconds": int }`; owner-cookie authenticated                                               |
| `PUT /web-admin/shop-rate`                         | –                              | appends and activates `{ "amount_cents": int, "seconds": int }`, retiring the old row; amount is 100–100,000 cents in whole-peso steps and seconds are 1–86,400 |
| `GET /web-admin/redemption-tiers`                  | –                              | sorted array of `{ "id": int, "points": int, "seconds": int }`; grandfathered seed may violate new write rules                         |
| `PUT /web-admin/redemption-tiers`                  | –                              | complete atomic replacement: `{ "tiers": [{ "points": int, "minutes": int }] }`; 1–20 rows, unique points ≥5 in steps of 5, minutes ≥1 |
| `GET /web-admin/coin-promo`                        | –                              | Coin Promo enable flag, sorted tiers, Rust-computed tier/account safety basis points, threshold, and unsafe flag                           |
| `PUT /web-admin/coin-promo`                        | –                              | atomic complete replacement: `{ "enabled": bool, "tiers": [{ "amount_cents": int, "bonus_minutes": int }] }`; unsafe still saves      |
| `GET /web-admin/attendance/settings`               | –                              | active immutable version, economics advisory, current liability, and effective-change timing                                               |
| `POST /web-admin/attendance/settings/preview`      | –                              | validates and previews enabled, threshold cents, reward minutes, and claim window without writing                                           |
| `PUT /web-admin/attendance/settings`               | –                              | commits a version and Admin audit atomically; identical drafts are no-op; stale changed versions return structured 409                     |
| `GET /web-admin/shop-hours`                        | –                              | Shop Hours settings, current Shop Status, seven Monday–Sunday rows, and current Shop Override state                                  |
| `PUT /web-admin/shop-hours`                        | –                              | atomic complete replacement; response includes the current Shop Status; validates minutes, lead, and exactly one row per weekday |
| `POST /web-admin/shop-override`                    | –                              | `{ "mode": "force_open" | "force_closed", "expires_at": "RFC3339" }`; replaces the current override and returns settings + status |
| `DELETE /web-admin/shop-override`                  | –                              | clears the current override and returns settings + status; both set and clear append an admin audit event                     |
| `POST /web-admin/coin-promo/preview`               | –                              | validates and computes the same response without persisting; powers live owner previews                                                    |
| `GET /web-admin/vouchers?limit=&batch_id=&search=` | `list_vouchers`                | optional batch filter and case/dash-tolerant code-fragment search                                                                      |
| `POST /web-admin/vouchers`                         | `create_voucher`               |                                                                                                                                        |
| `GET /web-admin/voucher-batches?limit=&offset=`    | –                              | list batches with redemption progress, most recent first                                                                               |
| `GET /web-admin/voucher-batches/{id}`              | –                              | get one batch with redemption progress and its cards                                                                                   |
| `POST /web-admin/voucher-batches`                  | –                              | create a batch of 1–1000 vouchers (see payload below)                                                                                  |
| `POST /web-admin/voucher-batches/{id}/void`        | –                              | void all active cards in the batch                                                                                                     |
| `GET /web-admin/voucher-template`                  | –                              | owner-only `{ html, css, default_html, default_css }` Voucher Template read                                                            |
| `PUT /web-admin/voucher-template`                  | –                              | owner-only full update: `{ "html": string, "css": string }`; each part required and limited to 100,000 bytes                           |
| `GET /web-admin/wheel`                             | `get_wheel_settings`           |                                                                                                                                        |
| `PUT /web-admin/wheel`                             | `update_wheel_settings`        |                                                                                                                                        |
| `GET /web-admin/accounts`                          | `list_accounts`                | owner-authenticated array; includes nullable `last_login_at` derived from the latest successful `login_succeeded` Audit Log event                                                                             |
| `POST /web-admin/accounts`                         | `admin_create_account`         |                                                                                                                                        |
| `POST /web-admin/accounts/import`                  | –                              | owner-only Martec Account Import; `dry_run: true` previews and `dry_run: false` commits atomically                                    |
| `POST /web-admin/accounts/reset`                   | `admin_reset_account_password` |                                                                                                                                        |
| `POST /web-admin/accounts/delete`                  | `admin_delete_accounts`        | atomic soft delete of `{ "account_ids": string[] }`; up to 1,000 explicit IDs, all must be active; forfeits balances, preserves history, ends sessions, revokes Account Sessions, and returns `{ "deleted_count": number }`                                                                 |
| `POST /web-admin/accounts/adjust-time`             | `adjust_account_time`          | signed `delta` seconds; clamped ≥ 0, applied delta audited                                                                             |
| `POST /web-admin/accounts/adjust-points`           | `adjust_account_points`        | signed `delta_centi` centipoints; clamped ≥ 0, applied delta audited                                                                   |
| `POST /web-admin/reports/summary`                  | `get_report_summary`           |                                                                                                                                        |
| `POST /web-admin/reports/attendance`               | –                              | flow range plus independent Earning Month and Account pages; owner-authenticated; current liability is range-independent                 |
| `POST /web-admin/reports/account`                  | `get_account_report`           |                                                                                                                                        |
| `POST /web-admin/reports/pc`                       | `get_pc_report`                |                                                                                                                                        |
| `GET /web-admin/points-liability`                  | `get_points_liability`         |                                                                                                                                        |
| `GET /web-admin/backups`                            | –                              | filesystem-derived Backup Catalog: `{ filename, kind, created_at, schema_version, size_bytes, backup_sets, catch_up }[]`; newest first; `catch_up` marks downtime recovery |
| `POST /web-admin/backups`                           | –                              | `{ "backup_sets": string[] }`; creates a manual Backup and returns `{ filename, size_bytes }`; omitted sets means all seven |
| `GET /web-admin/backups/{filename}`                 | –                              | authenticated browser attachment download |
| `DELETE /web-admin/backups/{filename}`              | –                              | removes the file and appends `backup_deleted` |
| `GET /web-admin/backup-schedule`                    | –                              | owner-only persisted schedule plus `next_due_at` and nullable `mirror_error` standing advisory |
| `PUT /web-admin/backup-schedule`                   | –                              | `{ enabled, frequency, minute_of_day, weekday, day_of_month, backup_sets, retention_count, mirror_directory }`; appends `backup_settings_changed` |
| `GET /web-admin/restore`                            | –                              | current Pending Restore or `null`; owner-only |
| `POST /web-admin/restore`                           | –                              | JSON `{ filename }` from the Catalog or `{ path }` absolute `.sql` path, or authenticated `multipart/form-data` with one `file` field capped at 256 MB; stages, forward-migrates, verifies, and returns a Pending Restore; refusal JSON names `gate`, `error`, and `blocking_pcs` |
| `DELETE /web-admin/restore`                         | –                              | cancels the current Pending Restore and removes its staged database |
| `POST /web-admin/audit/search`                     | `search_audit_log`             | body is an audit filter with optional `limit`/`offset`; returns `{ rows, total }` where `total` counts every row matching the filters, ignoring the page window |
| `POST /web-admin/audit/export`                     | `export_audit_log_csv`         | returns CSV (as a JSON string); `created_at` is local wall clock so the export matches the viewer, and the export ignores `offset` so it always covers the whole filtered set |
| `POST /web-admin/diagnostics/search`               | `search_diagnostic_log`        |                                                                                                                                        |
| `GET /web-admin/diagnostics-mode`                   | –                              | Diagnostics Mode (ADR-0081): `{ "enabled": bool }`; the shop-wide visibility flag for the Diagnostics page, toggled from Settings → Security; defaults to off; visibility only — never a security boundary |
| `PUT /web-admin/diagnostics-mode`                   | –                              | `{ "enabled": bool }` → `{ "enabled": bool }` (stored state); requires an authenticated Owner Session, no PIN step-up; appends `diagnostics_mode_updated` recording the new state |
| `POST /web-admin/manual-adjustment`                | `create_manual_adjustment`     |                                                                                                                                        |
| `GET /web-admin/settings`                          | –                              | `{ "shop_name": string, "shutdown_grace_seconds": number, "offline_grace_seconds": number }` (non-secret only)                     |
| `PUT /web-admin/settings`                          | –                              | `{ "shop_name": string, "shutdown_grace_seconds": number, "offline_grace_seconds": number }`; validates shop name, 5-300 s Shutdown Grace, and positive-integer Offline Grace, audited |
| `PUT /web-admin/owner-pin`                         | –                              | `{ "new_pin": string }`; write-only, hashed server-side, audited `owner_pin_changed`; never readable                                   |
| `GET /web-admin/transfer-requests`                 | –                              | pending Points Transfer Requests (`from_username → to_username`, points, code, age); past-TTL rows are swept to `expired` first        |
| `POST /web-admin/transfer-requests/confirm`        | –                              | `{ "code": "123456" }`; re-validates and moves points atomically; returns the resolved request                                         |
| `POST /web-admin/transfer-requests/reject`         | –                              | `{ "code": "123456" }`; marks the request `rejected`; no points move                                                                   |

The Shop Rate write accepts only a whole-peso amount (`amount_cents` from
100 through 100,000 in 100-cent steps) and 1 through 86,400 seconds. Its name
is derived from those values server-side. The write appends a new `rates` row,
retires the active row, and records a `shop_rate_updated` audit event carrying
both before and after pairs in one SQLite transaction.

Shop Override `expires_at` must be an absolute RFC 3339 instant strictly in the
future when written. While unexpired it wins over the schedule; at expiry the
schedule resumes without a cleanup request. The response shape for both
override mutations is the same as `GET /web-admin/shop-hours` and includes the
resolved `status`.

### PC Monitoring Check now (ADR-0076)

Both Check-now endpoints are owner-session-gated, strictly read-only (no
incident candidates, Audit rows, Notifications, session/money/time/Points
changes), and take no request body — Admin always probes the server-owned
`registered_ip`, never a caller-supplied address. Each result is one
`PcMonitoringCheck`:

```json
{
  "pc_id": "pc-02",
  "pc_name": "PC 02",
  "host_reachability": { "state": "reachable" },
  "client_reachability": "reachable",
  "guard_coverage": "covered"
}
```

- `host_reachability.state` is `reachable`, `unreachable` (no echo reply
  within the fixed one-second timeout), or `unavailable`. Unavailable results
  carry a typed `reason`: `platform_unsupported` (non-Windows Admin never
  manufactures a verdict), `pc_hidden`, `registered_ip_missing`,
  `registered_ip_invalid`, or another eligibility fact. Hidden, addressless,
  and removed PCs are therefore returned explicitly unavailable — never
  healthy.
- `client_reachability` mirrors the heartbeat verdict the dashboard already
  shows (`reachable` within the same staleness window that flips PCs offline).
- `guard_coverage` is the Client's last reported Guard Coverage diagnostic
  (ADR-0076): `covered` or `unavailable` from a release Windows Client, and
  `not_applicable` from debug and non-Windows builds. `unsupported` means no
  Heartbeat has reported the field yet (older Client) — unknown, never a
  fabricated verdict. The check is read-only: it surfaces stored diagnostics
  and never contacts the Client.

`check/{pc_id}` on an unknown id is a 404. `check-all` returns one result per
eligible PC only (visible with a non-blank registered IP), ordered by display
name.

### PC Monitoring continuous detection (ADR-0076)

The shop-wide toggle and its state live behind two owner-session-gated
endpoints:

- `GET /web-admin/pc-monitoring/status` → `{ "enabled": false,
  "platform_supported": true }`. Absent settings state reads as disabled, so
  an upgraded Admin never monitors without an owner decision.
- `PUT /web-admin/pc-monitoring/enabled` takes
  `{ "enabled": bool, "current_pin": string }`. The handler re-proves the
  current Owner PIN server-side (`owner_login_failed` is audited on a wrong
  PIN) before flipping the setting, so a stolen browser session cannot weaken
  monitoring. The change appends a `pc_monitoring_settings_updated` Notice.
  Disabling also closes every still-open incident as `monitoring_disabled` —
  closure rows, their `pc_monitoring_incident_closed` Notices, and the setting
  flip commit in one SQLite transaction. Historical Alerts remain. A
  non-Windows Admin refuses with 400 and reports `platform_supported: false`;
  it never pretends to monitor.

While enabled, the Admin runs its own ten-second probe round on a dedicated
task — independent of the one-second money/time timer. Three conditions open
incidents, each after its own fixed continuous grace:

- A Client Reachability Mismatch opens only after Host Reachable + Client
  Unreachable coexist continuously for 60 seconds; full grace restarts after
  Admin startup, enabling, and each time the host's reachability breaks. An
  accepted heartbeat closes the episode as `client_recovered`; loss of Host
  Reachability or of any observable target closes it as `host_lost` — Client
  recovery unconfirmed.
- Host Probe Coverage loss (#343) opens only while the Client heartbeat
  independently proves the PC is running and every ICMP probe fails
  continuously for two minutes — a powered-off or Client-unreachable PC never
  opens it merely by not answering. A stored address Admin cannot parse as a
  probe target counts too: reachability cannot be tested at all. One successful response closes it as
  `probe_recovered` (a recovery Notice); losing the heartbeat or the observable
  target closes it as `host_lost`. Web Admin copy states that missing coverage
  means unmonitored, not offline or tampered.
- Client Guard Coverage loss (#344) opens only after a release Windows
  Client reports `unavailable` Guard coverage continuously for two minutes
  while its heartbeat proves the PC is running. One covered report closes it
  as `guard_recovered`; losing the heartbeat or the observable target closes
  it as `host_lost` — recovery unconfirmed. `not_applicable` (debug,
  non-Windows) and an omitted field (older Client — unsupported or unknown)
  carry no evidence either way and can never open this incident. The
  diagnostic grants no authority and is never merged with a mismatch.

All three kinds share the generic lifecycle: opening commits a
`pc_monitoring_incident_opened` Alert and one durable incident row in one
transaction (repeated ticks are idempotent; one open episode per PC and kind,
and different kinds on one PC are independent episodes). Admin restarts mark
open incidents `observation_interrupted` without duplicating Alerts. Incident
paths change no PC status, session, money, Remaining Time, or Points.

### Per-PC Monitoring Suppression (ADR-0076)

Monitoring Suppression lets the owner pause new incidents for one visible
registered PC during authorized maintenance without erasing evidence:

- `POST /web-admin/pc-monitoring/suppressions/{pc_id}` takes
  `{ "duration_minutes": int, "current_pin": string }` and returns one
  `{ "pc_id", "pc_name", "expires_at" }`. Creating while a suppression is
  active replaces it. The duration is validated at the Rust boundary against
  fixed v1 policy (1–60 minutes) and `expires_at` is Admin-computed (RFC 3339
  UTC) — no caller supplies an expiry instant.
- `POST /web-admin/pc-monitoring/suppressions/{pc_id}/cancel` takes
  `{ "current_pin": string }` and returns `{ "cancelled": bool }`. Cancelling
  with nothing active stays a success so a stale dashboard row cannot turn a
  repeated click into an error.
- Both endpoints require an authenticated Owner Session AND the current Owner
  PIN (`owner_login_failed` is audited on a wrong PIN) and append one
  `pc_monitoring_suppression_updated` Notice per real mutation. The PC is
  validated at the Rust boundary: unknown ids return 404 and hidden PCs return
  400 — hidden PCs are not monitored, so suppressing one would promise
  protection nobody sees.

One shared monitoring policy gate covers every current and future incident
kind: while a PC's suppression is active, no new incident may open for it.
Incidents already open remain open and visible; suppression never closes or
erases them (truthful closures such as an accepted heartbeat still apply).
Expiry is lazy and durable: the row stores an absolute instant, survives Admin
restarts, and simply stops matching once the clock passes it. Expiry (and
cancellation) restart every kind's full grace period instead of opening an
immediate Alert. The dashboard exposes active suppressions at
`pc_monitoring.suppressions[pc_id]` with their absolute expiry.
Suppression creates no money, time, Points, session, or PC-status mutation,
and the PIN-gated Client Owner Quit can never create it: these routes exist
only on the owner surface behind an Owner Session.

`GET /web-admin/dashboard` gains a `pc_monitoring` object:
`{ "enabled": bool, "active_incidents": { "<pc_id>": ["client_reachability_mismatch"] },
"suppressions": { "<pc_id>": "<RFC 3339 UTC expiry>" } }`. Only active
suppressions appear; the value is the absolute expiry instant the UI displays.
Badges render from these durable open episodes independently of Notification
acknowledgement.

### Support Bundle (ADR-0078)

`GET /web-admin/support-bundle?pc_id={id}&window_seconds={seconds}` is the
owner-gated fetch for the Support Bundle: one self-contained, redacted Markdown
document covering one PC and one window (default 6 hours, hard-capped at 24).
Owner Session only — no PIN step-up, because the bundle is redacted and
read-only. The builder is a plain function over the log directories, the
database, the window, and the PC's Guard Poll snapshot; the endpoint only
validates the boundary (unknown PC 404, blank `pc_id` 400), applies the window
default, and hands over the Guard Poll facts.

In Settings, the download filename keeps the PC identity used to request the
displayed bundle, even if the owner changes the PC picker while it loads or
afterward.

The PC Monitoring Incident that alerted the owner anchors its own bundle
(#363): passing `incident_opened_at` (UTC RFC3339, or the SQLite datetime the
Notifications carry) windows the bundle around that incident — one hour before
to fifteen minutes after — instead of backwards from generation time. An owner
who opens a hours-old Alert gets the slice that explains it, not a window of
quiet that happens to be recent; the anchored incident stays in the Timeline
even when it closed after the window's tail. An anchor the boundary cannot
parse answers 400 rather than silently falling back to the unanchored window.
The anchor itself is not checked against the incident table — a stale or wrong
instant only windows the read-only bundle differently.

Sections render in order — Environment, Admin Host, Verdict, Timeline,
Anomalies, raw appendix. Environment states the PC's stored facts:
registered IP, MAC, last Heartbeat, and last reported Client Guard Coverage,
plus whether the SQLite context loaded completely (a failed query is stated,
never read as silence). Admin Host (ADR-0082 / #378) renders Admin's own
health from the same collector the host-diagnostics endpoint serves, so the
bundle and the owner's screen cannot disagree: the Run Records newest first
with version, PID, start, end, and Exit Reason — a start with no end reads as
a suspected hard exit unless it is the exact record the running process wrote,
matched on PID and start timestamp so a reused PID cannot pass a dead run off
as live — and the panic file's presence, last-modified time, and its newest
8 KB read from one file snapshot, so a concurrent append can never widen the
slice, stated as a slice ("newest 8 KB of N bytes, older bytes stay on disk")
when the file is larger than the cap.

The Verdict names the failure shape in plain language, above the Timeline so
findings survive truncation. Six rules ship (ADR-0078): a foreign Client
image rejected by the Guard (expected and actual image paths, rejection
count), a Guard not running or not polling (no Guard lines in the window and
a missing or stale poll), a Client reconnect flap (repeated pipe
close/connect, reported as the proportion of the window spent disconnected,
including any final disconnect through the window end),
an armed Offline Shutdown Countdown (with any in-window cancellation), clock
skew beyond two minutes, and the coverage contradiction where the stored
coverage reads `unavailable` while the Guard's fresh poll reports the Client
connected: the finding states both observations and declines to assert a
cause, since observations sampled at different times do not prove which
report is wrong or what caused the mismatch. When
no rule matches, the Verdict says so explicitly — silence never reads as
"nothing wrong". The Guard-poll input is Admin's in-memory Guard Poll state
(ADR-0077), which no log row or SQLite record holds; an Admin restart resets
it, after which the Verdict treats the Guard as never having polled.
The Timeline merges four sources into one chronological table — Forwarded
Client lines, Forwarded Guard lines, Admin's own diagnostic lines for that PC,
and the SQLite context that recorded the symptom (incident open/close edges
and in-window Audit Log events; an episode edge stands in for its duplicate
Audit event). Client lifecycle lines carry their per-launch lifetime context
(`process_id=… launch=…`) inline (ADR-0078), so contradictory evidence can be
told apart by process lifetime; lines from before the convention lack the
fields and still render. Anomalies list still-open PC Monitoring Incidents for the PC and
any Forwarded drop markers, which also surface as visible `gap` rows in the
timeline rather than silent holes. The raw appendix is re-serialised from a
field whitelist; `account_id` is not whitelisted.

Redaction is by construction, mirroring the Coin Debug Bundle: device secrets
are structurally excluded (no query selects `coin_devices.secret`, and every
appendix field must be named by the whitelist), account identities are
pseudonymised to stable per-bundle labels — the label replaces the username in
every rendered text, including the appendix `message` — and the size cap
(~250 KB, with a 200-row timeline cap) states every truncation in the document
header rather than leaving it implicit. A 24-hour window can never widen the
scan past the cap: `window_seconds` is clamped server-side.

A Diagnostic Request slice (#364) appears in the Timeline and appendix like
any Forwarded Diagnostic: it is the Client's answer to the owner's ask,
uploaded through the forwarded-diagnostics endpoint, and its own statement
lines say which nonce it answers, what its window covered, and whether the
byte cap or the window cut material away — so the bundle states the slice's
truncation even though the bundle builder itself never post-edits it.

### Host diagnostics (ADR-0082)

`GET /web-admin/host-diagnostics` makes Admin's own health fetchable for the
Diagnostics page's Server tab (#378). One collector produces the evidence for
both this endpoint and the Support Bundle's Admin Host section, so what the
owner reads on screen and what support reads in the bundle can never
disagree. The response carries the Run Records newest first — each with its
app `version`, `pid`, `started_at`, `ended_at` (absent while the run is
unfinished), and `exit_reason` (`window_closed`, `application_exit`, or
`panic`) — plus a `suspected_hard_exit` flag per record: a start with no end
on any run but the live one, a plainly stated absence of evidence, never a
claim about the cause. Records may also include `app_update_result`
(`{ from, to, ok }`) for an App Update outcome (ADR-0086), retained after the
Owner acknowledges it. The `panic_file` object states whether
`logs/panic.log` exists, its `size_bytes`, its `last_modified_at`, and its
newest 8 KB as `newest_8kb` (lossy UTF-8; the full file stays on disk).

Nothing here errors on absence: an empty Run Record history and a missing
panic file answer cleanly with empty runs and `exists: false`. The endpoint
serves any authenticated Owner Session with or without Diagnostics Mode —
the flag is visibility only and is never a boundary (ADR-0081).

### Telegram connect-and-protect (ADR-0076)

The optional Telegram integration connects exactly one BotFather bot to one
private owner chat. It is outbound only: there is no bot command surface, no
ongoing update poller, no webhook, and no inbound port. The single detection
call during setup is the only `getUpdates` request the integration ever makes.
The token is protected at rest with Windows DPAPI under the Admin Windows
identity and is never returned by any response, Audit event, diagnostic, or
error string. Non-Windows Admin reports `platform_supported: false` and
refuses setup instead of storing readable secrets.

Setup is guided and atomic from the owner's perspective:

1. `POST /web-admin/telegram/setup/token` verifies the candidate token via
   `getMe` and holds it in memory only — a restart discards it, nothing is
   persisted, and any previously working configuration stays fully active.
2. `POST /web-admin/telegram/setup/detect` looks once for private `/start`
   messages sent in the last ten minutes, deduplicated by chat id. Other
   messages, groups, channels, and non-message
   updates are dropped server-side, so a non-private destination can never be
   proposed. Manual numeric chat-id entry exists as a fallback and still goes
   through the same test gate; ids must be positive integers (groups are
   negative, so they are rejected at the boundary).
3. `POST /web-admin/telegram/connect` requires the current Owner PIN, sends
   the test message, and only on success protects the token with DPAPI and
   commits configuration plus the `telegram_connected` Alert in one
   transaction. A failed or unreachable test returns an error and leaves the
   previous working configuration untouched; the pending candidate survives
   for a retry without re-entering the token.

`POST /web-admin/telegram/disconnect` (also PIN-gated) deletes the protected
token and destination with one `telegram_disconnected` Notice, together with every
pending delivery queued for that chat. Local PC Monitoring keeps running
unchanged. Disconnecting while nothing was stored stays a success.

Incident delivery (#346, #347): opening or closing a PC Monitoring Incident
while Telegram is connected commits the incident row change, its generic Audit
event, and one redacted `telegram_outbox` item in the same SQLite transaction.
A dedicated ten-second worker sends committed items only. Items with the same
class, incident kind, and 30-second batch window share a message; different
kinds stay separate, and a large fleet is split below Telegram's message limit.
Failures retry with exponential backoff (30 s doubling to a one-hour ceiling)
for at most 24 hours per item, including work that was never attempted while
Admin was down. Messages carry only PC display name, incident time, observed
duration when known, and closure reason — never IP, MAC, Account, username,
customer, balance, Remaining Time, Points, secrets, or raw metadata. Delivery
never acknowledges a local Alert and can never duplicate incidents, Audit
events, money, time, Points, or sessions (an ambiguous network outcome may
duplicate a message). Disabling PC Monitoring enqueues a monitoring-disabled
state message; replacing or disconnecting Telegram deletes queued deliveries.

### Telemetry (ADR-0083)

Telemetry has no owner HTTP routes. Token-bearing Admin builds send the
allowlisted `admin_daily_summary` from a Rust background worker; the Web Admin
browser and Client never contact PostHog. Builds without the compile-time token
cannot send. Event fields are documented in ADR-0083.

### Audit-derived Notifications (ADR-0058)

These owner-authenticated routes are mounted only on `:8080`. They derive
Notifications from classified `audit_log` rows; no notification event table is
used. Only Alert and Notice rows in the inclusive rolling 30-day window are
returned. Each request uses one SQLite database-time snapshot for its window.

| Method + path | Query/body | Returns |
| --- | --- | --- |
| `GET /web-admin/notifications?limit=50&offset=0&severity=alert\|notice&unread=true\|false` | all optional; `limit` is 1–200, `offset` is non-negative | `{ "items": [...], "has_more": bool }` |
| `GET /web-admin/notifications/unread-count` | — | `{ "unread_count": int }` |
| `POST /web-admin/notifications/seen` | no body | `{ "last_seen_at": "YYYY-MM-DD HH:MM:SS" }` |
| `POST /web-admin/notifications/{audit_id}/acknowledge` | no body | `{ "acknowledged_at": "YYYY-MM-DD HH:MM:SS" }` |
| `POST /web-admin/notifications/acknowledge` | `{ "audit_ids": ["audit-uuid", ...] }`, 1–200 ids | `{ "acknowledged_count": int }` |

Bulk acknowledgement takes the explicit Alert IDs the owner had rendered; there
is no server-side "acknowledge everything" sweep. The batch commits in one
SQLite transaction and appends one `notification_acknowledged` event per newly
acknowledged Alert, so `acknowledged_count` counts only Alerts this request
acknowledged. Already-acknowledged ids are no-ops, and ineligible ids — unknown,
non-Alert, or outside the rolling window — are skipped rather than failing the
batch. An empty or over-200 `audit_ids` array returns `400`; the Web Admin
splits a longer selection into sequential 200-id requests.

An omitted `severity` includes both severities. An omitted `unread` includes
both read states. Pagination is applied after classification and read-state
filtering, and `has_more` reports whether another filtered item exists.

Each item also carries `actor_account_username`, nullable and separate from the
target `account_username`. It is resolved from the Actor Account recorded by
`audit_log.actor_account_id`; the target Account is resolved from
`audit_log.account_id`. The field is additive and does not change any existing
field's shape or meaning.
Unknown and Silent event types never appear or affect `unread_count`.

Each list item is shaped as follows (nullable values remain JSON `null`):

```json
{
  "id": "audit-uuid",
  "severity": "alert",
  "event_type": "tamper_kill",
  "outcome": "success",
  "actor": "system",
  "actor_account_id": null,
  "actor_account_username": null,
  "account_id": null,
  "account_username": null,
  "pc_id": "pc-01",
  "pc_name": "PC 01",
  "amount_cents": null,
  "seconds_delta": null,
  "points_delta_centi": null,
  "summary": "Client tamper detected",
  "created_at": "2026-07-20 10:00:00",
  "read": false,
  "acknowledged_at": null
}
```

An Alert is read only when its per-audit acknowledgement exists;
`acknowledged_at` is populated only for Alerts. A Notice is read when its
`created_at` is at or before the global `last_seen_at` watermark and always has
`acknowledged_at: null`. `POST /seen` advances that singleton watermark to the
server's database `CURRENT_TIMESTAMP`, is idempotent, and is unaudited.

The list and unread-count GETs, and successful `POST /seen`, are not audited.
The first successful Alert acknowledgement atomically inserts its read-state
row and appends one `notification_acknowledged` Audit Log event with actor
`admin` and outcome `success`. Repeated acknowledgement is idempotent and
appends no event. Notice, Silent, missing, and out-of-window ids return `404`
without changing state. Unauthenticated GETs return `401` without audit;
unauthenticated POSTs use the normal `web_admin_access_denied` audit path.

### Rejection Credits (ADR-0069)

Owner-authenticated routes on `:8080` for re-crediting a historical coin
whose old acceptance path never valued it. Current automatic Coin Batch
acceptance uses fixed 100-cents-per-pulse authority and creates no new
mismatch rows. Every credit is linked to one historical
`coin_batch_rejected` Audit Log row; there is no free-standing credit endpoint
and no free-text reason field.

| Method + path | Query/body | Returns |
| --- | --- | --- |
| `GET /web-admin/rejection-credits/rejections?account_id=&creditable_only=true` | both optional; `creditable_only` defaults to `false` | `{ "items": [...] }` |
| `GET /web-admin/rejection-credits/denominations` | — | `{ "denominations_cents": [100, 500, 1000] }` |
| `POST /web-admin/rejection-credits` | `{ "rejection_audit_id": "...", "denomination_cents": 2000, "coins": 1, "dry_run": false }` | the credit (see below) |

`GET /rejections` lists `coin_batch_rejected` rows inside the same inclusive
rolling 30-day window the Notification view uses, newest first, capped at 200.
`account_id` narrows to rejections whose **recorded** Account is that account —
never to whoever occupies the PC now. `creditable_only=true` drops rows with no
remaining coins. Each item:

```json
{
  "rejection_audit_id": "audit-uuid",
  "created_at": "2026-08-10 21:14:03",
  "pc_id": "pc-05",
  "pc_name": "PC 05",
  "account_id": null,
  "account_username": null,
  "target_kind": "guest",
  "account_deleted": false,
  "pulse_count": 11,
  "cap_coins": 1,
  "credited_coins": 0,
  "remaining_coins": 1
}
```

`target_kind` is `account` when the rejection recorded one and `guest`
otherwise; it is frozen at the rejection and the caller cannot override it.
`account_deleted` is `true` when the recorded Account has since been deleted —
the row stays listed as evidence but `POST` refuses it. `pulse_count` is `null`
on a suppression summary; `cap_coins` is `1` for a per-batch rejection and the
recorded suppressed count for a summary. `remaining_coins` is
`cap_coins - credited_coins`.

`GET /denominations` returns the distinct `amount_cents` of the **current**
retained Pulse Map, ascending. It is the only legal source for
`denomination_cents`; an amount absent from it returns `400`. This historical
Rejection Credit source is intentionally separate from automatic pulse-sum
authority.

`POST /web-admin/rejection-credits` credits `denomination_cents × coins` to the
frozen target. `dry_run: true` computes and returns the identical body without
writing, so the confirmation dialog previews the exact seconds and points the
server will credit rather than recomputing pricing in the browser. Both modes
return:

```json
{
  "dry_run": false,
  "rejection_audit_id": "audit-uuid",
  "audit_event_id": "audit-uuid",
  "target_kind": "account",
  "account_id": "account-uuid",
  "account_username": "juan",
  "pc_id": "pc-05",
  "pc_name": "PC 05",
  "denomination_cents": 2000,
  "coins": 1,
  "amount_cents": 2000,
  "seconds_added": 6600,
  "coin_promo_bonus_seconds": 600,
  "points_earned_centi": 40,
  "created_at": "2026-08-11 09:02:41"
}
```

`audit_event_id` and `created_at` are `null` when `dry_run` is `true`.
`seconds_added` is the **total** credited, including any Coin Promo bonus, so
one number drives balances and audit exactly as it does for an accepted coin.
A guest credit returns `points_earned_centi: 0` and
`coin_promo_bonus_seconds: 0` — both are account-only at insertion (ADR-0003,
ADR-0052).

Values are computed by the same `compute_coin_outcome` the coin path uses,
inside the credit transaction, under the **current** Shop Rate, Points earn
rate, account carry, and Coin Promo tiers. The caller supplies no time, points,
or promo value.

Failures:

| Status | Cause |
| --- | --- |
| `400` | `coins` not positive; `denomination_cents` absent from the current Pulse Map; `coins` exceeding the rejection's `remaining_coins` |
| `404` | unknown `rejection_audit_id`, an id that is not a `coin_batch_rejected` row, or one outside the 30-day window |
| `409` | the rejection's recorded Account has since been deleted |

A successful non-dry-run credit writes, in one `BEGIN IMMEDIATE` transaction:
the balance update, the canonical `rejection_credit` Audit Log event
(`actor = admin`, `cash_flag = 1`, `outcome = 'success'`, `pc_id` always set,
`account_id` for an account target, `amount_cents`, `seconds_delta`,
`points_delta_centi`; metadata carries `rejection_audit_id`, `pulse_count`,
`denomination_cents`, `coins`, and `coin_promo_bonus_seconds`), and the linked
`rejection_credits` detail row. The count cap is re-checked inside that same
transaction, so two concurrent credits cannot both pass. `rejection_credit` is
classified as an **Alert**. A credit cannot be reversed. `dry_run` requests are
unaudited and take no write transaction.

### Account Import

`POST /web-admin/accounts/import` requires the `op_owner` cookie. The browser
posts the parsed Martec export; the server never opens a caller-supplied
filesystem path. `dry_run: true` returns the plan without writes. Confirming
re-posts the identical payload with `dry_run: false`; valid rows, audit
provenance, and the run summary commit in one SQLite transaction:

```json
{
  "dry_run": true,
  "accounts": {
    "Customer Name": {
      "id": "martec-id",
      "password": "b'$2b$12$…'",
      "credits": 12.5,
      "points": 2.25,
      "enabled": true,
      "last": "2026-01-01T00:00:00"
    }
  }
}
```

`credits` are minutes and become rounded, non-negative `remaining_seconds`;
`points` become rounded, non-negative integer `points_balance_centi`. The
Python bytes wrapper around `password` is tolerated, but the inner value must
be a 60-character `$2a$`, `$2b$`, or `$2y$` bcrypt hash. Invalid row data
appears as a skipped row; a malformed/non-Martec payload is rejected.

The response contains `rows` with `outcome` (`created` or `skipped`),
`original_username`, optional `final_username`, converted balances, and an
optional machine-readable `skip_reason`. `totals` contains `created`,
`skipped`, `total_seconds`, and `total_points_centi`:

```json
{
  "rows": [
    {
      "outcome": "created",
      "original_username": " Customer Name ",
      "final_username": "Customer_Name",
      "remaining_seconds": 750,
      "points_balance_centi": 225
    },
    {
      "outcome": "skipped",
      "original_username": "existing",
      "remaining_seconds": 0,
      "points_balance_centi": 0,
      "skip_reason": "existing_account_conflict"
    }
  ],
  "totals": {
    "created": 1,
    "skipped": 1,
    "total_seconds": 750,
    "total_points_centi": 225
  }
}
```

Username whitespace is converted with the lenient normaliser and is reported
as a rename. A commit stores the bcrypt hash without hashing, writes nullable
Martec provenance on each Account, emits one non-cash (`cash_flag: false`)
`account_created` audit event per created Account, and emits one
`account_import_completed` summary event. Owner Account responses include
`imported_from`, `imported_at`, and derived `legacy_not_logged_in` provenance;
customer-facing Account responses omit those owner-only fields. An active
Username is always skipped: `already_imported` identifies an active previous
Martec import and `existing_account_conflict` identifies another active Account.
A Username held only by deleted Account history is reported as `created` and
creates a new Account identity; the deleted tombstone and its history are not
changed. Manual Account creation and signup still reject that Username. Preview
and commit use the same classification, so an active imported conflict is
`already_imported`, an active non-imported conflict is
`existing_account_conflict`, and deleted-only history is import-only reuse.
Unexpected database failures roll back the entire run.

Voucher Template HTML supports `{{code}}`, `{{time}}`, `{{points}}`,
`{{price}}`, `{{shop_name}}`, `{{expires_at}}`, and `{{prefix}}`. Rendering and
printing happen only in sandboxed owner-browser iframes. Values are HTML-escaped,
script elements are removed, and the generated document applies a no-script CSP.
The template CSS owns `@page`, grid, and page-break layout.

Request bodies are the same shapes the retired Tauri commands accepted.
M6 removed the `owner_token` field from every input struct; the `op_owner`
cookie is the only owner-auth surface. Error mapping matches the `:3210`
API: `InvalidInput → 400`, `Unauthorized → 401`, `Conflict → 409`,
`NotFound → 404`.

### Coin-device settings + test acceptor (M4)

Owner-editable coin-acceptor settings, persisted in migrations `0007` and
`0023`. Runtime acceptance reads only the inter-coin gap and derives checked
`pulse_count × 100 cents`; the retained Pulse Map is used for physical
calibration and historical Rejection Credit denomination choices.

| Method + path                      | Body / Returns                                                                                                                                                                                                        |
| ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /web-admin/coin-device`       | `{ com_port, baud_rate, set_pin_line, coin_pin_line, coin_pin_polarity, inter_coin_gap_ms, inactivity_timeout_seconds, hard_cap_seconds, pulse_map: [{ pulse_count, amount_cents }], calibration_required }`                                                   |
| `GET /web-admin/coin-device/ports` | `{ name, label, detected }[]` — host-detected ports merged with the saved COM Port; owner cookie required; scan failures return the standard error response rather than an empty array |
| `PUT /web-admin/coin-device`       | same shape (no `owner_token` field after M6); validated and rewritten in one transaction with a `coin_device_settings_updated` audit event                                                                            |
| `POST /web-admin/coin-device/test` | `{ ok, simulator, message }` — with no COM port the shop is on the simulator and the test is a no-op success; with a port it opens the serial handle (set pin left **inhibited**, fail-closed) and reports the result |
| `GET /web-admin/coin-device/calibration` | `CalibrationStatus \| null`; returns the current in-memory capture for owner UI recovery after navigation |
| `POST /web-admin/coin-device/calibration` | `{ target_amount_cents }` → `CalibrationStatus`; starts an owner-only local-serial capture, never a payment |
| `GET /web-admin/coin-device/calibration/{capture_id}` | `CalibrationStatus`; polls bounded sample progress and expires abandoned captures |
| `POST /web-admin/coin-device/calibration/{capture_id}/sample` | `CalibrationStatus`; inhibits first, records one non-empty sample, and remains inhibited between samples |
| `POST /web-admin/coin-device/calibration/{capture_id}/arm` | `CalibrationStatus`; owner action that enables the next physical sample after the inter-sample inhibit |
| `POST /web-admin/coin-device/calibration/{capture_id}/cancel` | `CalibrationStatus`; inhibits and discards pulses |
| `POST /web-admin/coin-device/calibration/{capture_id}/apply` | `CoinDeviceSettings`; applies only five consistent samples and rejects pulse-count conflicts |
| `GET /web-admin/coin-device/debug-bundle` | JSON string containing the bounded, redacted Markdown Coin Debug Bundle produced by the diagnostic builder; coverage starts at the current **Coin Debug Window** start |
| `GET /web-admin/coin-device/acceptor-health` | `{ debug_window_start, timing_status, implausible_timing_rate }` — the current **Coin Debug Window** start, S1 timing status, and a nullable S1 rate |
| `GET /web-admin/coin-device/debug-window` | `{ since }` (RFC3339 UTC) — the current **Coin Debug Window** start |
| `POST /web-admin/coin-device/debug-window` | advances the window start to now and returns `{ since }`; destroys nothing (logs/transactions untouched) — only the next bundle's coverage narrows |

`CalibrationStatus` contains `capture_id`, `target_amount_cents`, `phase`,
`sample_counts`, `current_pulse_count`, required sample count, and bounded
wall-clock deadlines. Physical capture requires a configured available serial
source and no Selection Window. The capture shares the existing sampler and
terminal lock; simulator and remote-device claims are rejected. Five
individual samples must be non-empty and equal before Apply is accepted. The
captured amount is diagnostic evidence only: no coin transaction, Guest Time,
Points, Coin Promo, or Coin Trace Id is created. Start, completion, cancel,
and apply are recorded as owner audit events.

The **Coin Debug Window** start is in-memory runtime state (never persisted): it
begins at the admin process start, advances to "now" on every successful `PUT
/web-admin/coin-device` and on `POST …/debug-window`, only ever narrows the
bundle (the two-hour cap still clamps it), and resets to the process start on
restart. It is deliberately not folded into the `CoinDeviceSettings` payload.

`GET /web-admin/coin-device/acceptor-health` is the owner-facing S1 health
contract. It returns one object, not per-batch rows:

```json
{
  "debug_window_start": "2026-08-12T10:00:00+00:00",
  "timing_status": "unavailable_until_filtered",
  "implausible_timing_rate": null
}
```

S1 renders the missing filtered inter-pulse timing inline and does not run or
infer an **Implausible Timing** detector. Historical diagnostic evidence that
remains valid for the Coin Debug Bundle is retained; the retired unmapped-event
endpoint has no compatibility alias.

`set_pin_line ∈ {rts, dtr}`; `coin_pin_line ∈ {cts, dsr, ri, cd}`;
`coin_pin_polarity ∈ {normally_open, normally_closed}` (default
`normally_open`); money stays integer cents and the gap is 1-10000 integer
milliseconds. A `null`/blank
`com_port` means "no hardware — use the simulator source". A successful PUT
replaces the inhibited source and timer configuration immediately; it is
rejected while a Selection Window, active calibration capture, or in-flight
coin drain is active.

`GET /web-admin/coin-device/ports` requires the `op_owner` session cookie and
returns one object per host-reported **Detected Serial Port**:

```json
[
  { "name": "COM3", "label": "CH340 (wch.cn)", "detected": true },
  { "name": "COM7", "label": "Saved port", "detected": false }
]
```

The list is naturally sorted by trailing port number. A saved non-empty COM
Port that is missing from the scan is retained with `detected: false`; blank or
null saved values add no entry. The Simulator choice is a Web Admin UI option,
not part of this response. Enumeration failures return the normal error
response; an empty array means enumeration succeeded but found no ports. The
endpoint only reads the host registry and does not open serial handles. PUT
continues to trim blank values, permits undetected ports, and performs no live
scan validation.

### Central Terminal Autostart (ADR-0055)

Reads and writes the live Autostart state from the host machine. The
**registry is the sole source of truth** — there is no SQLite mirror.
Autostart is **logon-scoped, not boot-scoped**; unattended recovery after a
power cycle requires owner-provisioned Windows autologon.

| Method + path                | Request               | Returns                                                                                                                                    |
| ---------------------------- | --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /web-admin/autostart`   | —                     | `{ enabled: bool, supported: bool, editable: bool, exe_path: string }` — requires the `op_owner` session cookie                            |
| `PUT /web-admin/autostart`   | `{ enabled: bool }`   | The same response shape, re-read from the host after the write. Requires an owner session.                                                |

`editable` is `false` in debug builds to prevent stale Run-key entries
pointing into a build tree. `supported` is `false` on platforms that have no
Autostart implementation.

A `PUT` while `editable` or `supported` is false is refused with a client
error and changes nothing. Every attempted change — success **and** failure
— appends an audit event with the `admin` actor and `{ enabled, exe_path }`
metadata.

```text
> GET /web-admin/autostart
< 200 OK
< { "enabled": false, "supported": true, "editable": true, "exe_path": "C:\\Program Files\\OpenPiso\\openpiso-admin.exe" }

> PUT /web-admin/autostart
> { "enabled": true }
< 200 OK
< { "enabled": true, "supported": true, "editable": true, "exe_path": "C:\\Program Files\\OpenPiso\\openpiso-admin.exe" }
```

### Admin App Update checks (ADR-0086)

These routes exist only on `:8080`, never on the Client/device API at `:3210`.
All require the `op_owner` Owner Session cookie; POST and PUT also require
its CSRF token and exact same-origin `Origin` header (ADR-0074).

| Method + path | Request | Returns |
| --- | --- | --- |
| `GET /web-admin/update` | — | Current App Update state; no outbound request |
| `POST /web-admin/update/check` | No body | Check the Release Feed now, then return state; works even with auto-check off |
| `PUT /web-admin/update/auto-check` | `{ "enabled": bool }` | Persist the preference, then return state |
| `POST /web-admin/update/install` | `{ "version": string, "acknowledgeSchemaChange"?: bool }` | Prepare the offered App Update, persist restart marker, hand off to Installer; return state if still running |
| `POST /web-admin/update/rollback` | `{ "version": string, "confirmVersion": string }` | Prepare a current Compatible Rollback candidate; same handoff/response |
| `POST /web-admin/update/ack` | No body | Clear the resolved restart marker and result, then return state |

State uses camelCase:

```json
{
  "currentVersion": "0.11.5",
  "currentSchemaVersion": 58,
  "autoCheck": true,
  "lastCheckedAt": "2026-10-13T08:00:00Z",
  "available": {
    "version": "0.12.0",
    "schemaVersion": 59,
    "notes": "Owner-facing Markdown release notes",
    "pubDate": "2026-10-13T00:00:00Z",
    "latestJsonUrl": "https://github.com/H3atman/openpiso/releases/download/v0.12.0/latest.json",
    "raisesSchema": true
  },
  "rollbacks": [],
  "runningSessionCount": 2
}
```

- `currentVersion` comes from the Admin binary; `currentSchemaVersion` is the
  applied migration maximum in SQLite, not a client-supplied value.
- `available` is the newest stable release with greater semantic-version
  precedence. `raisesSchema` compares its Schema Version with the database's.
  Build metadata alone does not make a version newer. Drafts and prereleases
  are excluded.
- `rollbacks` contains only older stable releases with exactly the current
  Schema Version, newest first. Entries have the same Release shape as
  `available`; `raisesSchema` is false.
- `runningSessionCount` counts PCs with positive currently running time,
  including Guest and bound Account Sessions. Parked Guest time underneath
  an Account Session is not running time; each PC counts at most once.
- `lastCheckedAt` is null before the first completed check; afterwards it is
  a UTC RFC3339 timestamp, including failed checks. Feed errors return HTTP
  `200` with `lastError`, clear stale `available`/`rollbacks`, and remain visible
  to GET. A successful recheck clears `lastError`. Absent `lastError` and
  `available` are omitted, not null. Database/audit failures use normal HTTP
  errors. Check state is runtime-only and starts empty on restart.
- Auto-check defaults on when `settings.app_update_auto_check` is absent.
  Startup and six-hour ticks read this persisted preference before checking;
  disabling suppresses automatic checks, not explicit Check now requests.
  GET and PUT never fetch releases. Check and preference routes never install.
- POST appends `app_update_checked` with the `admin` actor, success/failure
  outcome, and `{ "error": string | null }` metadata. PUT commits
  `app_update_auto_check_changed` and `{ "enabled": bool }` atomically with the
  setting. Authentication/CSRF/Origin denials follow the existing audit rules.

Install and Compatible Rollback:

- `version` must exactly match `available.version` for install, or an entry in
  the current `rollbacks` for rollback; supplied URLs, Schema Versions or notes
  never select an artifact. Invalid targets return `400` before any download.
- Schema-raising installs require `acknowledgeSchemaChange: true`; missing or
  false is `400`. Same-schema installs need no acknowledgement. Rollback
  requires `confirmVersion` to match `version` exactly, including no `v` prefix
  or whitespace; mismatches are `400`.
- During preparation, GET state includes optional
  `download: { "targetVersion": string, "percent": int }` (0–100). Poll GET
  while POST waits; GET never waits for the download. Concurrent installs or
  rollbacks return `409`. Pending restart/results also block another install
  until acknowledgement. `runningSessionCount` is advisory, never a gate.
- The Installer seam is two-phase: `prepare(Release, rollback, progress)`
  downloads/verifies and returns a `PreparedInstall`; only then does Rust
  commit the restart marker and audit before calling `PreparedInstall.install`.
  The production signed Tauri/NSIS adapter is supplied by #422; until wired,
  the fail-closed adapter returns `500`, without writing a marker.
- Successful preparation records `app_update_install` or `app_update_rollback`
  (Admin, success), with `{ fromVersion, version, startedAt }`, atomically with
  the marker. This means handoff requested, not proof that installation
  succeeded. Rejected gates, concurrent attempts and Installer errors append
  the same event type with failure and `{ version, error }`. Download/signature
  failures leave no marker or pending restart result, but record the failed
  `{ from, to, ok: false }` outcome in Diagnostics/Run Records and emit a
  Diagnostics error, using the same evidence mechanism as restart resolution.
  Launch failures resolve a failed result immediately,
  clear progress, and can be acknowledged without restarting. Normal HTTP error
  handling applies to database, audit and Installer errors.
- On startup Rust compares the persisted marker target with the running binary.
  Optional `lastResult: { "from": string, "to": string, "ok": bool }` appears in
  state; `to` is the attempted target (not the running version on failure).
  `currentVersion` always names the actual binary. The result is persisted once
  and survives further restarts until POST ack; it is omitted before resolution
  and after acknowledgement, as is `download` when idle.
- Resolution records one System `app_update_result` audit (success/failure),
  with `{ result, startedAt, runningVersion }`. Run Records and host diagnostics
  retain optional `app_update_result: { from, to, ok }` after acknowledgement;
  failures also emit a Diagnostics error. Legacy Run Records without the field
  still load. POST ack atomically clears the marker/result and writes Admin
  `app_update_result_acknowledged`. Ack before restart resolution returns `409`,
  preserves the marker, and appends a failure audit; idle ack is harmless and
  audited. Authentication/CSRF/Origin denials use existing middleware rules.

Production anonymously lists the public GitHub Releases of
`H3atman/openpiso` and parses each stable release's `release.json`
(`version`, `schema_version`, `notes`, `pub_date`). Its `latest.json` asset URL
is retained for the later signed install flow. Legacy releases missing either
artifact are skipped; malformed metadata, HTTP failures and rate limits are
check errors. Requests carry a fixed `OpenPiso-App-Update` User-Agent, never
shop data, cookies, credentials or Owner tokens.

For debug fixture harnesses only, `OPENPISO_RELEASE_FEED_BASE_URL` replaces
`https://api.github.com`; fixtures implement
`/repos/H3atman/openpiso/releases?per_page=100&page=N` and asset URLs.
HTTP is allowed only for loopback fixtures, whose assets must have the same
origin. Production asset URLs stay under the public releases repository.
Release builds compile out the environment override entirely.

Download progress, restart results, install and rollback routes are added by
subsequent App Update tickets; they are not part of this check-only API.

### Owner web app static assets (M5)

The same `:8080` server serves the Vite + Svelte owner web app as static
assets. These routes are **unauthenticated** by design — they are the login
shell; every data/mutation route above still requires the owner cookie.

| Method + path        | Returns                                                     |
| -------------------- | ----------------------------------------------------------- |
| `GET /admin`         | the SPA shell (`index.html`)                                |
| `GET /admin/{*path}` | the matching built asset, or the shell for in-memory routes |

The handler is hardened against path traversal: any request containing a `..`
(or a Windows drive/separator) segment never reaches the filesystem join and
falls back to the shell, so it can never read outside the web-admin build
directory. A missing concrete asset (a path with a file extension) returns
`404`; an extensionless route returns the shell so client-side routing works.

ADR-0074/0075 implementation must add HTTP security headers to this separately
served browser app. Its CSP defaults to same-origin resources, blocks objects
and framing, and limits `connect-src` to required same-origin HTTP and `ws://`
connections. The kiosk `tauri.conf.json` CSP does not govern these responses.
