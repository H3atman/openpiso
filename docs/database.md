# Database

SQLite is the local source of truth. Connections enable foreign keys and WAL
mode. Schema changes are applied in numbered migrations. Before migrating or seeding,
startup refuses unknown or newer migration versions and gaps in the known history;
version 0 remains the fresh-install marker.

## M1 account tables

`accounts` stores the preserved display username, an active-only unique normalized
username (deleted tombstones may retain the same normalized Username), Argon2
password hash (or transitional imported bcrypt before first login), integer-second time balance, integer centipoint
points balance (ADR-0038),
point earn carry in centicents, lifecycle status, and deletion timestamps. Soft
deletion changes status to `deleted`, sets both balances to zero, and preserves
the row permanently so historical foreign keys remain valid.

`owner_credentials` contains exactly one row (`id = 1`) with the Argon2 Web
Admin PIN hash. Setup and authenticated current-PIN rotation write it in a
transaction; rotation revokes other in-memory owner tokens while preserving the
caller's token (ADR-0050). Owner and customer auth tokens are not durable
database state.

ADR-0074 keeps Owner Sessions in memory and adds issue time, last human
activity, a CSRF token, and revocation cancellation. ADR-0075 likewise keeps
pending Remote Control grants and live Remote Control state in memory. Neither
decision adds a durable session table. Both remain pending implementation.

Migration `0035_account_import_provenance.sql` adds nullable
`accounts.imported_from` and `accounts.imported_at` columns. Native and
pre-existing rows remain NULL. Account Import writes `imported_from =
'martec'` and one run timestamp; `imported_from` is the neutral source-system
value behind the owner's Legacy label, while `imported_at` identifies the
import run. These provenance columns have no index. Migration
`0037_active_account_username_uniqueness.sql` replaces the original table-level
Username uniqueness with a unique partial index on active Accounts, preserving
deleted tombstones and allowing Account Re-import. Owner bulk deletion accepts
up to 1,000 explicit active Account IDs in one `BEGIN IMMEDIATE` transaction;
invalid batches roll back and append one bounded `account_delete_failed` audit
row, while success returns the deleted count after session/token cleanup. A
derived API flag,
`legacy_not_logged_in`, identifies imported Accounts whose bcrypt credential has
not yet been upgraded by login, without storing login history.

## Canonical audit log

`audit_log` is append-only and includes typed event/outcome/actor columns,
optional account and PC dimensions, integer money/time/points deltas
(points in centipoints), cash
classification, summary, metadata, and timestamp. Indexes cover timestamp,
event type, actor, account, and PC. SQLite triggers reject updates and deletes.

`coin_transactions` and `manual_adjustments` remain one-to-one detail tables.
Each row links to one canonical event through unique `audit_event_id`; triggers
reject new unlinked rows. Migration `0002_accounts_audit.sql` backfills one
canonical event per legacy financial row, absorbs matching MVP `audit_logs`
coin events to avoid duplicates, migrates other legacy rows as
`legacy_event`, and then removes `audit_logs`.

Tamper Kill reports append `tamper_kill` and, when ending an account or guest
session, `tamper_kick`; attribution details stay in `metadata_json`. The
`kill_timestamp` is the stable report name, while `report_age_ms` records the
Client-supplied elapsed age used to derive the Admin-clock kill instant. A
missing or over-24-hour age records `unknown` without discarding the report.
Unknown rows also carry exactly one diagnostic `reason`: `missing_report_age`,
`invalid_report_age`, or `current_occupant_not_attributed`. Other Subjects do
not carry this field.

### Shop Rate history

The existing `rates` table is the Shop Rate history. It has exactly one active
row for new purchases; an owner change inserts a new row with a server-derived
`name`, retires the previous active row, and appends `shop_rate_updated` in the
same SQLite transaction. Its metadata carries both before and after
amount-cents/seconds pairs. Amounts are integer cents in whole-peso steps and
time is integer seconds. Rate rows are append-only: amount, seconds, and name
are never edited in place and no row is deleted, so each committed Coin Event's
computed seconds remain explainable under the rate that was active then.

### Account Session history

Migration `0039_account_session_history.sql` adds `account_session_history`, a
historical record of Account Session ids, Accounts, PCs, start/end timestamps,
and end reasons. New rows and normal closures are written in the same SQLite
transaction as their existing `login` or session-end Audit Log event. A same-PC
re-login resumes the existing row.

The table is history, not live state: bindings and tokens remain in Admin
memory per ADR-0013. Once during startup, recovery closes rows left open by the
previous process as `server_restart`, using the PC's last known heartbeat for
`ended_at` (or the current database time when no heartbeat exists). Recovery
writes no session-end Audit Log event and leaves already-closed rows untouched.
The history is consulted for new Tamper Kill attribution at the trusted kill
instant. Existing audit rows are never backfilled; the history is not live
state and does not restore bindings after restart.

### Client clock measurement

Migration `0040_pc_clock_difference.sql` adds nullable
`pcs.clock_difference_ms`. Each Client Heartbeat may carry its current Unix
millisecond reading; the Admin stores `Admin receipt time − Client reading` in
that column. A positive value means the Client clock is behind. Missing readings
leave the previous measurement unchanged, preserving compatibility with older
Clients. The measurement is diagnostic only and is never read by Tamper Kill
attribution.

Migration `0045_pc_guard_coverage_diagnostic.sql` adds nullable
`pcs.last_guard_coverage` (ADR-0076 / #344). Each Heartbeat may carry a typed
Guard Coverage outcome (`'covered'`, `'unavailable'`, or `'not_applicable'`,
validated at the Rust boundary); the Admin stores the latest reported value.
The stored diagnostic always describes the current Client process: a Heartbeat
that omits the field (an older Client) resets it to unknown, so a stale report
from a previous build can never keep or create a Guard Alert, and body-less
Admin-side keepalives leave it untouched. NULL — never reported — reads as
unsupported or unknown and can never create a Guard Alert. The column is
diagnostic only: no authority path reads it.

### Operational Audit Log Notices

The operational Audit Log events admitted by ADR-0058 are immutable
rows in `audit_log`, not standing health records:

- `coin_device_unavailable`, `coin_device_recovered`, and
  `coin_sampling_anomaly` come from the real Coin Source's best-effort bridge.
  Availability is edge-triggered; sampling anomalies are bounded aggregates.
  The simulator emits none, and a failed bridge append never alters sampling
  or manufactures a replacement event.
- Historical `coin_batch_rejected` rows remain readable and creditable through
  the Rejection Credit repositories and owner API. Current Terminal Selection
  does not produce new rows: every positive Coin Batch is valued by the fixed
  pulse-sum authority.
- `backup_schedule_failed` records one failed due scheduled Backup attempt.
  `backup_schedule_caught_up` is written only after a successful startup
  catch-up; the Backup artifact/header remains filesystem truth.
- `pc_went_offline` records a stale-heartbeat or explicit-disconnect transition,
  while `pc_came_online` records the next accepted heartbeat. Both are Silent
  (ADR-0058, amended 2026-08-09): reachability is already live in the PC grid,
  so these rows are not owner-visible in Notifications, but are retained as
  the canonical server-visible offline signal. Status changes
  and these audit rows are appended in the same SQLite transaction, so a failed
  audit append rolls the status change back. Repeated ticks, disconnects, and
  heartbeats do not add rows without a transition.

Metadata is bounded and owner-safe: operational rows carry only normalized
reasons/counts, due times and Backup filenames, or the fixed transition source.
No raw samples, secrets, or frontend authority values are stored.

The durable MVP `sessions` table is unchanged by M1 and is not used for active
account bindings. M2 keeps those bindings in Rust memory and adds no schema
migration. The table remains legacy/unused until a later migration explicitly
removes or repurposes it.

## M2 authoritative time writes

Guest Time remains `pcs.remaining_seconds`. The timer excludes any PC with an
active account binding, so Guest Time can remain parked while the customer
decides whether to claim it.

Logged-in Remaining Time is `accounts.remaining_seconds`. The admin timer
updates each active account once per second in SQLite. An account reaching zero
is clamped at zero, receives an `expired` audit event, and loses its in-memory
binding after the transaction commits.

A successful claim sets the PC Guest Time to zero, adds the same integer number
of seconds to the account, and appends the `claim` audit event in one
transaction. Session bindings and account tokens are deliberately absent from
SQLite.

## M3 points economy and coin routing

`economy_settings` contains one local row with the active points earn rate and
unsafe-bonus warning threshold. Its released `redeem_points` and
`redeem_seconds` columns remain in SQLite but are dormant: no settings contract,
redemption path, or safety calculation reads or writes them.

`redemption_tiers` stores the ADR-0039 owner-authored guaranteed redemption
options as unique integer `points` and integer `seconds`. Reads sort by points.
Migration 0014 seeds exactly one row from the former economy redeem pair,
without normalizing it; this permits a pre-existing invalid pair to remain
unchanged. Complete replacement writes run in one transaction and Rust validates
1–20 rows, unique whole points that are multiples of 5 and at least 5, and whole
minutes of at least 1 before converting minutes to stored seconds. The tier with
the highest seconds-per-point ratio drives the headline Effective Bonus %, wheel
point value, and wheel house-edge guardrail.

`coin_transactions` records the server-resolved target:

- `pc_id` is always the selected PC.
- `account_id` is set only when that PC had an active account session at the
  routing instant.
- `target_kind` is `guest` or `account`.
- `points_earned_centi` records the centipoints awarded by that coin event.
- `seconds_added` remains the authoritative total credited; `coin_promo_bonus_seconds`
  records its non-negative Coin Promo component and defaults to zero for history.

`coin_promo_settings` is a disabled-by-default singleton. `coin_promo_tiers`
stores unique positive coin amounts mapped to positive bonus seconds. Owner
writes validate the complete input first, then replace the enable flag and all
rows atomically; they append one `coin_promo_settings_updated` admin audit event.
The authored tiers are read inside the immediate coin transaction and core
decomposes the coin amount across them largest-first (ADR-0070 batches are coin
trains, not single coins); core applies the result only to Account targets.
Audit metadata records the tier-covered amount and applied bonus.

Logged-in sub-threshold purchases carry cents in
`accounts.point_earn_carry_centicents`; guest purchases earn no points and
no carry. Since ADR-0038, earning is fractional and direct: eligible cents
convert straight to centipoints and only the sub-centipoint remainder
carries, stored in centicents so any earn rate stays exact.
The canonical `audit_log` row for a coin event stores the same money, seconds,
points, account, PC, source, and device facts for reporting. Coin batches may
also carry a Coin Trace Id in the audit metadata JSON, which joins diagnostic
records to the linked transaction without a trace-id column.

## M4 vouchers and guaranteed redemption

`vouchers` stores owner-minted single-use codes. Codes are unique, random, and
server-generated. `code_normalized` is the unique uppercase, dash-free lookup
key, allowing case- and dash-insensitive redemption. `kind` is `sold` or
`promo`, inferred from cash at creation; `status` is `active`, `redeemed`, or
`voided`; values are integer `seconds_value`, `points_value_centi`, and
`sold_amount_cents`. Single-mint sold vouchers require a positive cash amount
and a `sold_at` timestamp; batch-sold cards keep `sold_at` null until
redemption. Promo vouchers store zero cash and no sale timestamp. `expires_at`
is nullable and omitted by default. An individual void persists `voided` and
links its non-cash `voucher_voided` audit event; it never removes history or
reverses recorded sold-voucher revenue.

Voucher redemption updates the voucher status, redeemed account reference, and
account balances in one transaction. The foreign key to `accounts` preserves
history through account soft deletion. Indexes cover status lookup, kind/date
reporting, redeemed account history, and voucher audit-event links.

Canonical audit event types added in M4:

- `voucher_sold`: cash revenue for sold vouchers at owner mint/sale recording.
- `voucher_minted`: non-cash promo voucher creation.
- `voucher_voided`: individual void of an active Voucher; non-cash and
  history-preserving.
- `sold_voucher_redeemed`: prepaid voucher value credited to an account.
- `promo_voucher_redeemed`: free voucher value credited to an account.
- `voucher_rejected`: invalid, spent, expired, or voided voucher redemption
  attempt.
- `points_redeemed`: guaranteed points-to-time redemption.
- `redeem_denied`: insufficient-points denial.

Guaranteed points redemption selects one versioned `redemption_tiers` row.
The transaction revalidates the menu version and tier id, deducts
`points * 100` centipoints, credits the tier seconds, and appends the audit
event atomically. At the seeded default tier, 1,000
centipoints (10.00 points) add 14,400 seconds. Redemptions debit
`accounts.points_balance_centi`, credit `accounts.remaining_seconds`, and
write the same seconds and negative centipoint delta to `audit_log`.

## M5 Spin the Wheel

`wheel_settings` holds one local row: the master `enabled` flag, the nullable
`bokya_rate_basis_points` (0–10,000), the `free_spin_cap` (0–10),
`spin_duration` (1–120, default 5), `password_confirmation_required` (0/1,
default 0, migration 0059, ADR-0088: `POST /api/wheel/spin` must carry the
Account's password; off for new and existing shops), and a `version` that
increments on every edit. A null Bokya Rate means unset and preserves the uniform draw; numeric zero
is a configured Bokya Rate, not null. `wheel_segments` stores the ordered prize
table.
Each segment carries a `label`, `seconds` (non-negative), `points_centi`
(non-negative), and `free_spin` (boolean). A segment is a prize pair — it can
award time, points, a free spin, or any combination. "Win nothing" is simply a
segment with `seconds = 0`, `points_centi = 0`, and `free_spin = false`. The
tagged `kind` column is gone (migration 0025).

There is **no `weight` column** (migration 0027, ADR-0047). Every segment
occupies `1/n` of the circle and is `1/n` likely while Bokya Rate is null. A set
Bokya Rate instead controls the server-side Bokya/non-Bokya distribution while
keeping equal slice geometry (migration 0028, ADR-0049). Rarity within either
group is expressed by repeating a prize across rows.

Points prizes are deliberately unguarded (ADR-0046): saving a points-heavy
configuration that is provably self-sustaining always succeeds. The time-axis
guardrail (house edge against the most generous Redemption Tier) is untouched
and still refuses a configuration whose expected seconds payout is not strictly
below the point value of the most generous Redemption Tier. A separate owner-only
advisory compares the expected payout with the paid-time value of one earned
point under the active Shop Rate, using `earn_cents_per_point * rate_seconds /
rate_amount_cents` with integer division. That advisory never blocks a Shop Rate
save or changes Client wheel responses. Points winnings are not folded into the
seconds expectation.

The seed installs a disabled wheel with a safe default table (expected payout
~219 s against a 1,440 s point value). `update_wheel_settings` replaces the
whole segment table atomically and bumps the version.

`wheel_spins` records one detail row per paid spin, linked one-to-one to its
canonical `wheel_spin` audit event (ADR-0018). It stores `points_spent_centi`
(always 100 — one whole point), `seconds_won`, `points_won_centi` (gross),
`free_spins_triggered`, the resolving `config_version`, nullable
`bokya_rate_basis_points` in force, nullable `remaining_before_seconds` (the
account's Remaining Time immediately before the spin resolved), and an
`outcome_json` snapshot of the chosen segments. Null on a spin means that spin
genuinely ran with Bokya Rate unset.
Pre-feature rows remain null because those draws were uniform; no configured
rate is inferred or backfilled. The nullable rate distinguishes historical
unbiased spins and makes a biased period reconstructable after settings and
segments are rewritten. The audit carries the **net** points delta
(winnings − cost); the detail row carries **gross** points won, because gross
turnover is what the reporting ticket needs (ADR-0046).

`remaining_before_seconds` (migration 0050) is null for rows written before it,
never backfilled to zero: an unknown starting balance must not masquerade as a
zero-time spin in odds breakdowns. It exists as fairness evidence — the draw
never reads Remaining Time (`WheelConfig::resolve` consumes only segments,
Bokya Rate, the free-spin cap, and the RNG stream), and the column lets reports
disprove allegations like "spinning with zero time wins jackpots more often":

```sql
SELECT remaining_before_seconds > 0 AS had_time,
       COUNT(*) AS spins,
       SUM(seconds_won >= 900) AS jackpots
FROM wheel_spins
WHERE remaining_before_seconds IS NOT NULL
GROUP BY had_time;
```

Spins are server-authoritative and transactional: the server loads the config,
selects the most generous Redemption Tier, recomputes the house edge against its
point value, deducts exactly one point (100 centipoints) on a
`points_balance_centi >= 100` guard, resolves
the capped free-spin chain
with its own RNG, credits any won seconds and won points, and writes both the
audit event and the detail row in one transaction.

Canonical audit event types added in M5:

- `wheel_spin`: a successful paid spin; net points delta (winnings − cost),
  won-seconds delta, non-cash. Feeds Free Time Dispensed in reporting. Metadata
  includes `remaining_before_seconds`, mirroring the detail-row stamp.
- `wheel_spin_denied`: a denial because the wheel is disabled, the house edge
  is non-positive, or the account has too few points. Changes no balance.
- `wheel_settings_updated`: an owner edit to the wheel configuration.

The wheel house edge is coupled to the most generous Redemption Tier; editing
tiers can change it, and `update_wheel_settings` refuses any configuration whose
expected payout is not strictly less than that tier's point value.

## M6 reporting and audit review

Migration `0006_reporting_audit_review.sql` adds four reporting views over the
canonical `audit_log`, covering indexes for audit review dimensions, and a
manual-adjustment reporting index. No new tables are created; manual
adjustments remain PC-bound per ADR-0069 (which supersedes ADR-0021).

Reporting views:

- `report_cash_revenue` — cash that entered the shop: `coin_accepted`,
  `voucher_sold`, `sold_voucher_redeemed`, and (since migration `0041`)
  `rejection_credit` events with `cash_flag = 1` and `amount_cents > 0`. Single-mint sold vouchers use `voucher_sold` at mint and
  redeem with `cash_flag = 0`; batch-sold cards use the cash-bearing redemption
  event, so neither path double-counts. The view exposes `business_date`,
  `business_week`,
  `business_month`, `account_id`, `pc_id`, `event_type`, `amount_cents`,
  `seconds_delta`, `audit_event_id`, and `created_at` so per-period and
  per-dimension totals reconcile exactly with canonical events.
- `report_free_time_dispensed` — time given away without cash:
  `promo_voucher_redeemed`, `points_redeemed`, `wheel_spin`, and
  `manual_adjustment` events with `seconds_delta > 0`, plus only the positive
  `coin_promo_bonus_seconds` from each uniquely linked coin transaction. A Coin
  Promo coin's paid time is excluded, while its full amount remains Cash
  Revenue. Sold voucher redemption is also excluded (it credits prepaid time,
  not free time). Same period and dimension columns as cash revenue, plus
  `seconds_delta`. Migrations `0030_coin_promo_reporting.sql` and
  `0041_rejection_credits.sql` replace the view without changing this column
  shape; the latter adds the Rejection Credit promo split.
- `report_points_ledger` — per-account points earn, redeem, wheel-bet, and
  voucher points movements. One row per canonical event that changed an
  account's points balance. Supports per-account points history and the
  liability total derivation.
- `report_points_liability` — current snapshot of total outstanding unredeemed
  centipoints across active accounts. Not a historical event sum; it reads
  `accounts.points_balance_centi` where `status = 'active'` and exposes
  `total_points_liability_centi`.

Covering indexes added:

- `idx_audit_log_outcome_created` — (outcome, created_at) for filtering the
  audit viewer by success/failure/denied.
- `idx_audit_log_event_type_created` — (event_type, created_at) for
  reverse-chronological reads filtered by event type.
- `idx_manual_adjustments_pc_created_at` — (pc_id, created_at) for per-PC
  manual-adjustment reporting.

Manual adjustments are now a first-class owner mutation path. The
`create_manual_adjustment` Rust function takes a PC ID and an integer
`seconds_delta`, clamps the applied delta so Guest Time never goes below zero,
writes the canonical `manual_adjustment` audit event with the **actual applied
delta** in `seconds_delta` (so reports reconcile exactly), stores the requested
delta in metadata, and inserts the linked `manual_adjustments` detail row in
the same SQLite transaction. The audit event's `summary` includes the optional
reason so the audit viewer's free-text search can find it. A negative delta is
a compensating event, not an audit-history edit (ADR-0015).

## Voucher batches (migrations 0016, 0019, 0020, 0021)

`voucher_batches` stores one row per batch generation action. Columns: id,
immutable globally unique `name` (five case-sensitive Base62 characters), prefix
(2-8 uppercase A-Z/0-9), suffix_length (6-16, default 8), count (1-1000),
kind (sold/promo inferred from cash_amount_cents), uniform seconds_value and
points_value_centi, cash_amount_cents, optional expires_at, created_at,
voided_at, voided_audit_event_id, and audit_event_id (the single
`voucher_batch_minted` event).

Codes follow the format `PREFIX-XXXX-...` with suffix characters from the
32-char unambiguous set (A-Z minus O/I, 2-9), dash-grouped every 4. Uniqueness
is enforced by a UNIQUE index on `vouchers.code`; the server retries on
collision.

`vouchers.batch_id` (nullable) links each card to its batch. For batch-sold
cards (`cash_amount_cents > 0`), `sold_at` is NULL at mint — cash is recognised
at redemption per ADR-0040. Redemption writes one `sold_voucher_redeemed`
event carrying `cash_flag = true`, the cash amount, and the credited value.
Single-mint sold redemption carries `cash_flag = false` because its cash was
already recorded by `voucher_sold` at mint.

When a batch is voided, one SQLite transaction sets the batch's `voided_at`,
changes every still-active card to the persisted `voided` voucher status, and
writes one `voucher_batch_voided` audit event. Redeemed cards remain unchanged.
An individual active Voucher (batched or single-mint) may instead be voided in
its own transaction with one `voucher_voided` audit event.
Migration 0019 also backfills cards from batches voided before physical status
persistence was introduced.

Canonical audit event types added in migration 0016:

- `voucher_batch_minted`: single batch-level mint event; no cash at mint.
  Actor = `admin`, cash_flag = false.
- `voucher_batch_voided`: batch void action; records active card count.
  Actor = `admin`, cash_flag = false.

## Voucher Template (migration 0018)

Migration `0018_voucher_template.sql` seeds the shop-wide Voucher Template in
`settings` as `voucher_template_html` and `voucher_template_css`. The built-in
card includes every supported placeholder and the CSS owns `@page`, the card
grid, and page-break rules. Owner updates replace both values in one SQLite
transaction. The template is never exposed by the customer API; only the
owner-session-guarded `:8080` endpoints read or update it.

## M4 coin-device settings (Central Terminal)

Migration `0007_coin_device_settings.sql` plus `0023_coin_pin_polarity.sql`
add owner-editable coin-acceptor calibration (ADR-0023 / ADR-0024). These are read by the coin pipeline (M1/M2)
at startup so the inter-coin gap is no longer hard-coded; runtime pulse-sum
authority is fixed at 100 cents per pulse and the retained Pulse Map is not
part of Coin Source configuration.

- `coin_device_settings` — a single row (`id = 1`):
  - `com_port` — the serial port the acceptor is wired to. `NULL`/blank means
    "no hardware configured" and the simulator source is used.
  - `baud_rate` — serial baud rate (`> 0`, default `9600`).
  - `set_pin_line` — control line the server **drives** to enable/inhibit the
    acceptor (the set pin): `rts` or `dtr`.
  - `coin_pin_line` — status line the server **reads** for pulse edges (the
    coin pin): `cts`, `dsr`, `ri`, or `cd`.
  - `coin_pin_polarity` — wiring polarity normalized by the Pulse Sampler:
    `normally_open` (default, pulse high) or `normally_closed` (idle high,
    pulse low).
  - `inter_coin_gap_ms` — quiet gap that closes a Coin Batch (`>= 1`, default
    `600`; the field-informed value still requires bench validation).
- `coin_device_pulse_map` — `(pulse_count PK, amount_cents)` with both columns
  `> 0`. Retained for physical calibration and historical Rejection Credit
  denomination choices; automatic Coin Batch acceptance never reads it.
  Automatic authority is checked `pulse_count × 100` cents.

Fresh installs use the field-informed 600 ms gap (migration
`0022_inter_coin_gap_default.sql`) and the candidate ₱1/₱5/₱10/₱20 map.
Migration `0038_fresh_coin_20_candidate.sql` records the fresh-install-only
₱20 candidate. It is applied by the migration runner only when migration
history was empty; upgrades leave every existing pulse-map row untouched.
The ₱20 candidate is retained in the owner-visible map and calibration flow;
its map value does not alter runtime authority. The gap remains owner-editable
and must be validated against the actual acceptor on the bench.

`db::update_coin_device_settings` validates the lines, baud, gap, and retained
map, rewrites the single settings row and the whole pulse map in one
transaction, and records a
`coin_device_settings_updated` audit event. The owner edits these through the
`:8080` API (`GET`/`PUT /web-admin/coin-device`); they are not exposed on
`:3210`. Duplicate pulse counts are rejected rather than silently deduplicated.

The owner-only calibration endpoints use the live serial source and sampler
under the Terminal Selection lock. Five individual samples are captured
without creating coin transactions; only an explicit Apply writes the
pulse-map setting and audit event. A capture expires after 60 seconds of
inactivity or five minutes absolutely and is inhibited on cancellation,
expiry, source loss, or process restart.

Migration `0008_terminal_selection_timers.sql` follows the released 0007
migration without editing it. It adds owner-editable Selection Window
`inactivity_timeout_seconds` and `hard_cap_seconds` columns and a `pcs.visible`
flag. Hidden PCs remain in financial history and owner reports but are excluded
from the kiosk list and cannot open a new Terminal Selection.

Acceptor Health is a derived owner read, not durable database state. It reports
`debug_window_start` from the in-memory Coin Debug Window, the fixed S1
`timing_status` value `unavailable_until_filtered`, and a nullable
`implausible_timing_rate` (currently `NULL`). No migration, per-batch health
row, or Pulse Map mismatch record is created; valid historical Audit Log and
Coin Debug Bundle evidence remains untouched.

## M9 account-to-account transfers

Migration `0009_account_transfers.sql` adds the two durable tables behind
the customer-to-customer transfer feature ([ADR-0030](adr/0030-account-transfers.md)).
The change is additive — no released migration is edited — and follows the
canonical-audit pattern: each detail row links 1-to-1 to an `audit_log`
event through a `NOT NULL audit_event_id` foreign key plus a unique
`audit_event_id` index, so the detail table can never drift from the event
stream.

- `account_transfers` — **immediate time** transfers. Columns: `id`,
  `from_account_id`, `to_account_id`, `seconds` (> 0), `created_at`,
  `audit_event_id`. Two FKs to `accounts`. Indexes on each account id
  (for sender/recipient history) and a unique `audit_event_id` index.
- `points_transfer_requests` — **owner-confirmed points** workflow.
  Columns: `id`, `from_account_id`, `to_account_id`, `points_centi` (> 0),
  `code` (6-digit string, unique among `pending` requests via a partial
  unique index), `status`
  (`pending` | `confirmed` | `rejected` | `expired`), `created_at`,
  `resolved_at`, `audit_event_id`. Indexes on each account id and on
  `(code, status)` for the pending-code lookup. The `code` is a
  customer-presented confirmation token; the owner enters it in
  web-admin to release the points (the web-admin session is the real
  authority, not the 6-digit code). It is **time-boxed**: confirm/reject
  reject a code older than `POINTS_TRANSFER_TTL_SECONDS` (~10 min) on
  read, and the background sweep marks past-TTL `pending` rows `expired`.
  Uniqueness is scoped to live requests so a resolved code may recur.

The `settings` table also stores `offline_grace_seconds`, originally seeded to `120` by
migration 0024; migration 0054 bumps untouched old defaults to `600`. It is the positive-integer Offline Grace duration delivered by
`GET /api/shop`; Clients cache the last-seen value in their local config for
use while the admin server is unreachable. It is distinct from
`shutdown_grace_seconds`, the expiry shutdown countdown.

Both tables also expose the `owner_pin` setting (`settings.key =
'owner_pin'`) used by the client-side owner gate
([ADR-0027](adr/0027-client-lock-and-owner-pin-gate.md)). The hash is
written with the same Argon2 path the account passwords use, and the
client caches it for offline verification — the live value never reaches
the client for the online check.

Canonical audit event types added in M9:

- `account_transfer_out` / `account_transfer_in` — paired, written in the
  same SQLite transaction as the `account_transfers` insert; non-cash;
  one event per currency leg.
- `points_transfer_requested` — written when a customer initiates a
  pending request; the `code` lives only in `metadata_json` and in the
  request row, not in the human-readable `summary`.
- `points_transfer_out` / `points_transfer_in` — written when the owner
  confirms; paired, non-cash, one event per currency leg.
- `points_transfer_rejected` — written when the owner rejects; no
  balance movement, status → `rejected`, `resolved_at` set.
- `points_transfer_expired` — written by the periodic sweep that
  marks past-TTL pending rows `expired` (actor = `system`).
- `account_password_changed` — written atomically with a successful
  self-service Password Change; actor = `account`, distinct from the owner
  Password Reset event `account_password_reset`.
- `account_password_change_failed` — best-effort Password Change denial audit;
  actor = `account`; metadata reason is `bad_password` or `stale_credential`.
- `shop_name_updated` — written when the owner edits the shop name.
- `offline_grace_updated` — written when the owner edits Offline Grace.
- `shop_hours_settings_updated` — written when the owner replaces Shop Hours.
- `shop_override_set` — written when the owner sets a Shop Override; metadata
  records its mode and absolute expiry.
- `shop_override_cleared` — written when the owner clears a Shop Override.
- `shop_opened` / `shop_closed` — written by the system exactly once for each
  observed Shop Closed ↔ Shop Open transition. Metadata records the transition
  cause (`schedule`, `override`, or `master_toggle`) and resolved reason, so a
  past state remains reconstructable even after the schedule changes.
- `owner_pin_changed`, `owner_pin_verify_succeeded`,
  `owner_pin_verify_failed`, `owner_pin_verify_unconfigured` — written
  on the Client Owner PIN lifecycle and verify endpoint.
- `owner_credential_changed`, `owner_credential_change_denied` — written for
  successful and wrong-current-PIN Web Admin PIN rotation attempts.

Time transfers re-read the sender's balance inside the transaction and
validate via `openpiso_core::transfer::validate_transfer`; the same
validator gates the points request's soft check and
`confirm_points_transfer`'s re-validation, so the sender may not have
spent/spun in the interim. `confirm_points_transfer` and
`reject_points_transfer` claim the request with a guarded
`pending → confirmed/rejected` update and abort if it affects no row, so a
code is strictly single-use even under concurrent or double-submitted
owner actions.

## Shop Hours (migrations 0031–0032)

`shop_hours_settings` is a disabled-by-default singleton containing the master
`enabled` flag and non-negative `last_call_lead_minutes` (seeded to 15).
`shop_hours_schedule` contains exactly seven Monday-through-Sunday rows with a
mode (`open_period`, `closed_all_day`, or `open_24_hours`) and nullable integer
open/close minutes. Fresh installs seed every day to `08:00 -> 22:00`.
`shop_override` is an optional singleton holding a force mode and absolute
expiry. `shop_hours_runtime_state` stores the last state observed by the
server's one-second timer. It is updated in the same transaction as transition
audit events, so a restart or repeated evaluation cannot duplicate an event.
The first observation establishes a baseline without emitting an event; a
master toggle to `enabled = false` resets that baseline silently.

`db::update_shop_hours` validates the complete seven-row input before a
`BEGIN IMMEDIATE` transaction, replaces the settings and schedule rows
atomically, and appends exactly one `shop_hours_settings_updated` audit event
with actor `admin`. Rejected input cannot partially change the previous
configuration. `db::set_shop_override` parses an absolute RFC 3339 expiry and
requires it to be strictly later than the Admin host's current instant, then
upserts the singleton and appends one `shop_override_set` audit event with
actor `admin`. The event metadata records the mode and canonical expiry.
Validation is repeated immediately inside the write transaction so a
near-expiry request cannot commit after its expiry. `db::clear_shop_override`
deletes the singleton and appends one `shop_override_cleared` admin event.
Expired rows are retained: the resolver ignores them at and after expiry, so
the schedule resumes without an external action; a later set or clear can
replace or remove the retained row. `db::shop_status` converts the persisted
rows to the `openpiso-core` resolver using the Admin host's local clock; the
resolved value is also carried by existing `pc.updated` broadcasts. The
migration is inert because `enabled = false`.

## Backup Schedule (migrations 0033–0034)

`backup_schedule_settings` is the enabled-by-default Backup Schedule singleton
table (`id = 1`):

| Column | Fresh-install default | Meaning |
| --- | --- | --- |
| `enabled` | `1` | Backup Schedule is active |
| `frequency` | `daily` | Daily at the configured local time |
| `minute_of_day` | `240` | 04:00 local time |
| `weekday` | `0` | Monday when Weekly is selected |
| `day_of_month` | `1` | First day when Monthly is selected |
| `interval_seconds` | `60` | Reserved for debug assertions; not a release frequency |
| `backup_sets` | Accounts, Configuration, Ledger, Vouchers, Terminals, Runtime State, Credentials | Sets included in scheduled Backups |
| `retention_count` | `14` | Scheduled Backup retention count |
| `mirror_directory` | `NULL` | No Mirror Directory |
| `next_due_at` | `NULL` | Initialized by the scheduler on its first tick |
| `mirror_error` | `NULL` | Persistent Mirror Directory advisory |

All scalar constraints are duplicated in the pure `openpiso-core` schedule
validator. The release scheduler supports Daily, Weekly, and Monthly; a
persisted `interval` row is interpreted as Daily. `db::update_backup_schedule`
validates before a `BEGIN IMMEDIATE` transaction, updates the singleton, and
appends `backup_settings_changed`.

A separate 30-second scheduler writes one `scheduled` Backup when
`next_due_at` has passed, advances it past all missed occurrences, and marks
long-downtime catch-up files in their header and Backup Catalog entry. After
every successful write, retention prunes each Backup Kind independently:
scheduled files use the configured count, Pre-Migration and Pre-Restore keep 10
each, and manual files are never pruned automatically. Disabled schedules do no
work; scheduled files are not audited and use the configured Backup Set
selection.

There is deliberately no `backup_catalog` table. The **Backup Catalog** is
derived by scanning the primary Backup filesystem directory and parsing each
self-describing `.sql` header. Files are the source of truth, so a database
restore cannot revert the Catalog, and a corrupt database cannot take away the
list of recovery options. The Catalog therefore needs neither its own
migration nor repair when a file is removed outside the app.

## Staged and Pending Restore

Restore acceptance parses the Backup header, verifies its SHA-256 body checksum,
requires all seven Backup Sets, and refuses a schema newer than the running
migration list. It then refuses while any PC has `in_use` status, Remaining Time,
or an active account binding; the owner response names the blocking PCs. The SQL
is replayed into a fresh database beside the live file. Every table's row count
is then verified against the header **before** any migration runs, where the
staged database must still be an exact reproduction of the file, so equality
holds for every table with no per-table exemptions. The replay is then
forward-migrated with the standard migration runner (including its migration-history
guard), seeded, and checked with
`PRAGMA foreign_key_check`. Row counts are deliberately not re-checked after
forward-migration: migrations both insert rows (0018 and 0024 add settings keys)
and rebuild tables (0025 and 0027 rewrite the wheel tables), so comparing a
migrated database against an older header would refuse every older Backup.

A verified candidate is recorded in `.pending-restore.json` beside the Backup
Catalog and takes effect only on the next launch. Startup writes the outgoing
live database as a `pre_restore` Backup, swaps in the staged database, clears the
marker, and appends `restore_applied` to the restored audit log. The owner can
cancel a Pending Restore before launch. Staging never writes to the live pool;
`restore_requested` is the pre-swap breadcrumb expected to survive in the
outgoing Pre-Restore Backup; cancelling before launch appends
`restore_cancelled` to the live audit log. The owner web endpoint also accepts a
single browser-uploaded multipart `file`, capped at 256 MiB; its temporary file
is removed after header refusal, staging failure, or success.

## Centralized theme settings

Migration `0012_theme_settings.sql` adds the `theme_settings` singleton
(`id = 1`): one lowercase `#rrggbb` `accent_color`, `light | dark` modes for
web-admin, kiosk, and Client, and a positive integer `version`. SQLite CHECKs
mirror Rust boundary validation. Fresh databases seed `#15803d`, all-light,
version 1. Each full-record owner write updates all fields and increments the
version in one transaction. Theme changes deliberately create no audit event
(ADR-0035).

## Fractional points (centipoints)

Migration `0013_fractional_points_centipoints.sql` re-denominates every
points column to integer **centipoints** (1/100 point, ADR-0038) under a
`_centi` name with a ×100 backfill: `accounts.points_balance_centi`,
`audit_log.points_delta_centi`, `coin_transactions.points_earned_centi`,
`vouchers.points_value_centi`, `wheel_spins.points_spent_centi`, and
`points_transfer_requests.points_centi`. The earning carry becomes
`accounts.point_earn_carry_centicents` (1/100 cent) so sub-centipoint
remainders stay exact for any earn rate. In the same transaction, accrued
carry is **paid out** into the visible balance at the earn rate in effect at
migration time — the one sanctioned exception to never re-pricing history.
The `report_points_ledger` and `report_points_liability` views are rebuilt
against the new names, and the audit append-only trigger is dropped and
restored verbatim around the ×100 backfill (scale change, not a content
change). The earn setting keeps whole-point units. The old `redeem_points` and
`redeem_seconds` columns are not re-denominated and are now dormant.

Planned ADR-0075 events use the existing append-only `audit_log`:
`remote_control_started`, `remote_control_enabled`,
`remote_control_ended`, `remote_control_interrupted`,
`remote_control_settings_updated`, and `remote_control_step_up_denied`.
Session-bound events share one Remote Control Session id in metadata and
use the typed `pc_id` column. They omit target IP, byte counts, credentials,
framebuffer, clipboard, and input. Startup appends `remote_control_interrupted`
for an unmatched start; no audit row is updated in place.

## M6 owner-console retirement

M6 retired the desktop Tauri owner console (ADR-0024). No new migrations
landed: the schema, indexes, and reporting views from M1–M5 already cover
the full owner surface. The mutation paths are exactly the same Rust
functions (`db::create_voucher`, `db::update_economy_settings`,
`db::create_manual_adjustment`, `db::update_pc_settings`, …) the retired
Tauri commands used to call. The only change is that the **Tauri webview
is no longer a caller** — every owner mutation now flows through one of
the `:8080/admin` HTTP handlers, which reuse the same
`owner`/`db`/`reports`/`accounts`/`audit`/`diagnostics` functions.

The `AppState` in the Tauri process no longer carries `owner_tokens`
(those live only in `web_admin::OwnerServerState` now), and every
mutation input struct in `models.rs` has dropped the
`owner_token: String` field that used to be set by the retired desktop
command callers. The `web_admin_access_denied` audit event type is the only
record of unauthenticated request attempts on the HTTP surface, and it covers
**denied mutations only** — a denied safe read (`GET`/`HEAD`/`OPTIONS`) is
rejected with 401 without an audit row, because the web admin polls read-only
endpoints on a timer and auditing those would bury real denials under a steady
flood from any unauthenticated page left open. The
`owner_login_succeeded` / `owner_login_failed` events cover successful and
failed login PIN attempts, `owner_credential_changed` /
`owner_credential_change_denied` distinguish Web Admin PIN rotation outcomes,
`owner_login_throttled` records the transition into a per-IP login lockout
(once per lockout, not per blocked request), and `account_login_throttled`
records the transition into a per-Username Account Login lockout.

## Server-side PC identity (ADR-0031)

Migration `0010_pc_identity.sql` adds two nullable columns to `pcs`:

- `mac_address` — the durable identity hint. Set on the first heartbeat
  from a pre-registered row whose `mac_address` is `NULL` and whose
  `registered_ip` matches the connection source IP. Never re-bound by
  IP after this (a hardware/NIC replacement is the only case where
  the owner clears the binding).
- `registered_ip` — the owner-authored static IP used by current code as the
  step-2 bootstrap match key. ADR-0075 also makes it the server-owned Remote
  Control target after canonical private-IPv4 and configured-CIDR validation.
  Device traffic cannot change it; the owner edits it through Web Admin.

Both are nullable. Existing seeded PCs ship with `NULL` on both
columns and are re-bindable by the owner pre-registering a static IP
on the PC edit page (or by leaving NULL and pre-registering fresh
rows). Each column gets a `UNIQUE` partial index `WHERE … IS NOT
NULL` so two rows cannot both claim the same MAC or registered IP,
while a normal `COUNT(*)` scan is still index-friendly. SQLite
forbids partial `UNIQUE` indexes; the partial index is the documented
equivalent for nullable columns.

The pure `(mac, source_ip, rows) -> Decision` resolver lives in
`pc_identity::resolve`; the DB layer (`db::pc_identity_rows`,
`db::apply_identity_decision`, `db::heartbeat_by_identity`,
`db::disconnect_pc_by_identity`) loads the candidate rows, applies the
decision inside a transaction, and performs the bind-on-step-2
update atomically with the rest of the heartbeat. The bind update
guards `WHERE mac_address IS NULL` so a row already bound by a
concurrent heartbeat is not re-bound; the loser of that race
short-circuits to `Reject` and the client retries against the
now-bound row.

Canonical audit event types added in M2:

- `pc_pre_registered` — written when the owner pre-registers a PC by
  static IP (`POST /web-admin/pcs`); actor = `admin`.
- `pc_settings_updated` — already existed for `name`/`visible`
  edits; now also carries `registered_ip` / `mac_address` changes
  (clearing a NIC binding is a normal `pc_settings_updated` with
  `mac_address: null` in the metadata).

## Planned Remote Control settings (ADR-0075)

A new migration seeds three typed keys in the existing `settings` table:

- `remote_control_enabled = false`;
- `remote_control_allowed_ipv4_cidr`, initially unset;
- `remote_control_vnc_port = 5900`, constrained to `1024..=65535`.

Typed repository functions read and validate the record. One write replaces all
three values and appends `remote_control_settings_updated` in the same
`BEGIN IMMEDIATE` transaction. Remote Control Credentials are never stored.
Changing any field cancels the in-memory Remote Control Session after commit.
Existing invalid `registered_ip` values survive migration and startup but leave
their PC unconfigured for Remote Control until the owner fixes them. New and
edited values are stored as canonical private IPv4 strings.

## Audit-derived notifications (migration 0036)

Migration `0036_notification_read_state.sql` adds the two durable read-state
contracts from ADR-0058. `notification_read_state` is a singleton with
`id = 1` and `last_seen_at TEXT NOT NULL`; it is seeded with
`CURRENT_TIMESTAMP` at migration time as the global Notice watermark.
`notification_alert_ack` stores one `audit_log_id` primary key and
`acknowledged_at TEXT NOT NULL`, with a foreign key to `audit_log(id)`. The
migration seeds acknowledgement rows, timestamped with migration time, for
pre-existing Audit Log rows whose event types are in the fixed Alert allowlist.
It emits no `notification_acknowledged` events, because those rows establish
upgrade eligibility rather than owner action. Alert acknowledgements are
retained indefinitely.

Notifications are a derived Audit Log view over a fixed rolling 30-day query
window; no materialized notification table is created. Audit Log retention and
its existing indexes are unchanged. Notices use the global watermark, while
Alerts require an explicit acknowledgement per audit id.

## Rejection Credits (migration 0041)

Migration `0041_rejection_credits.sql` adds the durable half of ADR-0069: an
owner re-credit of a coin the acceptor physically took but never valued.

`rejection_credits` is a detail table hanging off its canonical Audit Log
event, exactly as `manual_adjustments` does (ADR-0018):

| Column | Notes |
| --- | --- |
| `id` | UUID primary key |
| `rejection_audit_id` | FK → `audit_log(id)`; the `coin_batch_rejected` row this repairs. Required — there is no free-standing credit |
| `audit_event_id` | FK → `audit_log(id)`, unique; the canonical `rejection_credit` event |
| `pc_id` | FK → `pcs(id)`, NOT NULL; frozen from the rejection |
| `account_id` | FK → `accounts(id)`, nullable; frozen from the rejection, `NULL` for a guest credit |
| `target_kind` | `'account'` or `'guest'` |
| `denomination_cents` | the Pulse Map denomination the owner picked, `> 0` |
| `coins_credited` | `> 0`; the audit event's `amount_cents` is `denomination_cents × coins_credited` |
| `seconds_added` | total credited, including any promo bonus |
| `coin_promo_bonus_seconds` | `>= 0`; the giveaway split, always `0` for a guest credit |
| `points_earned_centi` | `>= 0`; always `0` for a guest credit |
| `created_at` | `CURRENT_TIMESTAMP` |

Indexes: `idx_rejection_credits_rejection` on `rejection_audit_id` for the
count-cap read and the creditable-rejection list, and a unique index on
`audit_event_id` so one canonical event can never gain a second detail row.

No `coin_transactions` row is written. That table means "a coin was accepted and
valued by the coin path"; a Rejection Credit is an owner assertion about a coin
that path rejected, and folding it in would corrupt every coin-pipeline metric
that reads the table as machine truth.

**Double-credit guard.** There is no unique index on `rejection_audit_id`,
because a suppression summary is creditable in parts. Instead the credit
transaction re-reads
`SELECT COALESCE(SUM(coins_credited), 0) FROM rejection_credits WHERE
rejection_audit_id = ?` and refuses when `sum + requested` exceeds the cap: `1`
for a per-batch rejection, and the rejection metadata's `suppressed_count` for a
closing summary. Because the check runs inside the same `BEGIN IMMEDIATE`
transaction as the insert, two concurrent credits cannot both pass.

**Reporting.** Both reporting views are rebuilt by this migration.
`report_cash_revenue` gains `rejection_credit` in its event-type allowlist —
`cash_flag = 1` alone is not sufficient there — so the money books as Cash
Revenue, because the coin is in the coin box. Nothing else about that view
changes. `report_free_time_dispensed` gains a third
`UNION ALL` over `rejection_credits.coin_promo_bonus_seconds` joined to its
canonical event, mirroring the `coin_transactions` arm added in migration
`0030`. A credit's promo component therefore lands on the cost line exactly as a
real coin's does, while `rejection_credit` stays out of the base select so its
paid seconds are never double-counted as free time. The view's column shape is
unchanged.

## PC Monitoring Incidents (migrations 0042, 0044, 0046)

Migration `0042_pc_monitoring_incidents.sql` adds the durable half of PC
Monitoring (ADR-0076): one row per uninterrupted degraded-coverage episode for
one PC. Migration `0044_host_probe_coverage_close_reason.sql` rebuilds the
table to extend the `close_reason` CHECK with `'probe_recovered'` (#343), and
migration `0046_guard_recovered_close_reason.sql` rebuilds it again to add
`'guard_recovered'` (#344); all rows and indexes are preserved.

`pc_monitoring_incidents` shares the typed-detail-table shape of
`manual_adjustments` and `rejection_credits`, but its lifecycle hangs off two
generic Audit events instead of one:

| Column | Notes |
| --- | --- |
| `id` | UUID primary key |
| `pc_id` | FK → `pcs(id)`, NOT NULL |
| `kind` | `'client_reachability_mismatch'`, `'host_probe_coverage'`, or `'client_guard_coverage'`. The kind is typed incident data — it never forks the Audit event name. All three kinds open today |
| `opened_at` | `CURRENT_TIMESTAMP` at open commit |
| `closed_at` | NULL while open |
| `close_reason` | `'client_recovered'` (an accepted heartbeat), `'probe_recovered'` (one successful ICMP response ended Host Probe Coverage loss), `'guard_recovered'` (the Client reported covered again after Guard Coverage loss), `'host_lost'` (the evidence sustaining the episode ended; recovery stays unconfirmed), or `'monitoring_disabled'`. A CHECK enforces `(closed_at IS NULL) = (close_reason IS NULL)` |
| `observation_interrupted` | set once Admin restarts over a still-open episode, so display never claims the downtime span was continuously observed |
| `open_audit_id` | FK → `audit_log(id)`; the canonical `pc_monitoring_incident_opened` Alert, committed in the same transaction as the row |
| `close_audit_id` | FK → `audit_log(id)`; the canonical `pc_monitoring_incident_closed` Notice, committed in the same transaction as the closure |

Indexes: a unique partial index on `(pc_id, kind) WHERE closed_at IS NULL`
makes repeated monitor ticks idempotent — a second open episode for the same
PC and kind cannot exist even under concurrent writers — plus
`idx_pc_monitoring_incidents_pc` on `(pc_id, closed_at)` for dashboard badge
lookups.

The shop-wide toggle lives in the existing `settings` key/value table as
`pc_monitoring_enabled` (`'true'`/`'false'`). The key is deliberately absent
from this migration: an upgraded Admin starts disabled and monitoring begins
only after an audited owner decision. Incidents travel with the **Ledger**
Backup Set as permanent owner-reviewable evidence.

## Monitoring Suppression (migration 0043)

Migration `0043_pc_monitoring_suppressions.sql` adds the durable half of
per-PC **Monitoring Suppression** (ADR-0076 / #342): at most one row per PC,
so creating a suppression while one is active replaces it in place.

| Column | Notes |
| --- | --- |
| `pc_id` | Primary key; FK → `pcs(id)` — one suppression per PC at a time |
| `expires_at` | Admin-computed absolute expiry instant (RFC 3339 UTC). Duration policy (1–60 minutes) is enforced in Rust before the row exists, and display shows this exact instant rather than a countdown |
| `created_at` | `CURRENT_TIMESTAMP` at insert/replace |

Active means only that the absolute expiry is still in the future: expired
rows lazily stop matching every read instead of requiring a sweeper task. The
row survives Admin restarts — a restart can neither extend nor lose a running
suppression. Each mutation commits with its generic
`pc_monitoring_suppression_updated` Notice (`created`, `replaced`, or
`cancelled` action in the metadata) in one SQLite transaction. The table
travels with the **Configuration** Backup Set: temporary monitoring
configuration, not evidence.

## Client Recovery Escalation (migration 0051)

Migration `0051_client_recovery_escalation.sql` carries the durable half of
ADR-0077. It rebuilds two CHECK constraints and adds one table; SQLite cannot
alter a CHECK, so both rebuilds preserve every row and index unchanged.

- `account_session_history.end_reason` gains `escalated_power_off`, a
  session-end that is deliberately not `tamper_kick`. It records a monitoring
  action with no attribution and no accusation.
- `pc_monitoring_incidents.close_reason` gains `escalated_power_off`. Once the
  PC is off it stops answering ICMP, and closing that episode as `host_lost`
  would blame the machine for something Admin did.
- `pc_escalation_cooldowns` holds one row per PC that has been powered off.

| Column | Notes |
| --- | --- |
| `pc_id` | Primary key; FK → `pcs(id)` ON DELETE CASCADE |
| `last_power_off_at` | RFC 3339 UTC instant of the last Escalated Power Off |

The cooldown is durable rather than in-memory precisely because the escalation
clock is not: a restart must restart the clock, but it must never license a
second power off inside the same hour. A diskless PC that reboots straight back
into the same fault would otherwise cycle on the escalation clock all night.

Two settings rows carry the policy: `client_recovery_escalation_enabled`
(absent means disabled, so no upgrade authorises powering off customer PCs) and
`client_recovery_escalation_delay_secs`. The delay's 30-second floor is applied
on read as well as on write, so no stored value can authorise a power off on
less evidence than the ADR requires. Both changes commit with a
`pc_recovery_escalation_settings_updated` Notice. The cooldown table travels
with the **Configuration** Backup Set; the incident and Audit rows remain
evidence in the **Ledger** set.

## Telegram integration (migration 0047)

Migration `0047_telegram_integration.sql` adds `telegram_config` (ADR-0076 /
#345): the durable half of the optional Telegram alerts integration. A CHECK
keeps it single-row (`id = 1`) because v1 supports exactly one bot and one
private owner chat.

| Column | Notes |
| --- | --- |
| `bot_username` | Bot identity reported by `getMe`; display only |
| `chat_id` / `chat_title` | The one private destination chat and its owner-facing name |
| `token_protected` | BotFather token encrypted with Windows DPAPI under the Admin Windows identity (hex-encoded ciphertext). Never returned by any API response, Audit event, diagnostic, or error; only Admin's Rust HTTPS delivery path decrypts it in memory |
| `connected_at` / `verified_at` | When the configuration was stored and when it last passed a real test send. A failed replacement attempt never touches this row |
| `open_audit_id` | FK → `audit_log(id)`; the canonical `telegram_connected` Alert, committed in the same transaction as the row (upsert on replace) |

Connecting commits configuration plus its Audit event in one transaction;
disconnecting deletes the row and appends the `telegram_disconnected` Notice
the same way, together with every pending delivery queued for that chat.
Local PC Monitoring tables are untouched by both paths.

### Telegram delivery outbox (migration 0048)

Migration `0048_telegram_outbox.sql` adds `telegram_outbox` (ADR-0076 /
#346): the durable, restart-safe queue between locally canonical incident
history and best-effort Telegram delivery. Opening or closing an incident
while Telegram is connected composes one redacted message line and commits it
here in the same transaction as the incident row change and its Audit event;
the delivery worker ([`crate::telegram_delivery`]) reads committed rows only.

| Column | Notes |
| --- | --- |
| `item_class` | `opening` or `closure` |
| `incident_kind` | Incident kind used in the batch key; different kinds never share a fleet message. NULL is reserved for shop-wide state messages and legacy rows |
| `batch_bucket` | Persisted epoch bucket for inspection; the worker checks exact creation times so items within 30 seconds across a bucket edge still batch |
| `message_text` | Final redacted line: PC display name, incident time, observed duration when known, closure reason. Never IP, MAC, Account, username, customer, balance, Remaining Time, Points, secrets, or raw metadata |
| `incident_id` | FK → `pc_monitoring_incidents(id)`; NULL for the monitoring-disabled state message |
| `created_at` / `next_attempt_at` | RFC 3339 UTC; the worker attempts items whose next attempt is due |
| `status` | `pending` → `sent`, or `abandoned` after the 24-hour retry window closes unsent |
| `attempts` | Failed-attempt counter; backoff doubles from 30 s to a one-hour ceiling |
| `last_error` / `last_error_at` / `sent_at` | Delivery health surfaced inline in Settings via `GET /web-admin/telegram/status` |

Migration `0049_telegram_delivery_batches.sql` adds `incident_kind` and the
batch lookup index. The worker abandons pending rows at the 24-hour cutoff even
if no attempt happened while Admin was stopped, and splits a large same-kind
fleet into messages below Telegram's 4096-character limit. Replacing a Telegram
configuration clears the old destination's delivery rows in the same
transaction, so an old message cannot reach the new destination and its health
history is not shown for the new chat.

An undelivered opening collapses into one resolved summary when its incident
closes before delivery (ADR-0076). Disconnecting deletes all outbox rows;
sent and abandoned rows older than 30 days are pruned by the worker. The
table is excluded from Backups alongside `telegram_config`.

The entire integration is deliberately excluded from Backups:
`backup::BACKUP_EXCLUDED_TABLES` names `telegram_config` and
`telegram_outbox`, so no Backup Set ever exports their rows. A restored or
moved Admin therefore starts with Telegram disconnected until the owner
completes setup again — DPAPI ciphertext from another Windows identity would
be unreadable anyway.

## Telemetry (migration 0053)

Migration `0053_telemetry.sql` adds `telemetry_settings` (singleton `id = 1`)
and `telemetry_outbox` (one unsent Daily Summary per fixed UTC+08:00
`period_utc8`; due-time index). Settings hold the random `installation_id`
and `last_summary_period`; no consent or enabled flag is stored. The outbox
holds `event_uuid`, allowlisted `payload`, retry `attempts`, `next_attempt_at`,
`last_error`, and `created_at`. The first token-bearing worker tick creates the
Installation ID when absent. Recording a period and queuing its summary share
a transaction; retries keep the event UUID.

`backup::BACKUP_EXCLUDED_TABLES` excludes both tables' rows. A restored Backup
has their schema but no settings row; a token-bearing Admin generates a new
Installation ID automatically. Restoring cannot duplicate an ID or pending
summaries across shops (ADR-0083).

## Diagnostics Mode (migration 0052)

Migration `0052_diagnostics_mode.sql` adds `diagnostics_mode_settings`
(ADR-0081): the shop-wide visibility flag for the Diagnostics page. A
single-row table (`id = 1`), following this repo's convention of one table per
setting, seeded off — an upgrade must never reveal a surface the owner has not
asked for.

| Column | Notes |
| --- | --- |
| `id` | Singleton (`CHECK (id = 1)`) |
| `enabled` | 0/1, default 0 |

The flag is visibility only: no capture, verbosity, or retention behavior
anywhere in Admin or the Client branches on it, and it is not a security
boundary — it grants no authority and touches no money, time, Points, or
session data. Reads go through `GET /web-admin/diagnostics-mode` under an
Owner Session; writes go through `PUT /web-admin/diagnostics-mode` the same
way (no PIN step-up) and commit the flag and one `diagnostics_mode_updated`
audit event recording the new state in a single immediate transaction, so it
stays answerable who revealed the surface and when. The table travels with the
**Configuration** Backup Set.

## Attendance Rewards (S1 foundation, S2 earning, S3 claims/forfeiture and S4 reporting; ADR-0084 proposed)

[ADR-0084](adr/0084-attendance-rewards.md) defines the separate
Account-only Attendance Time liability. S1 adds the foundation.
`attendance_settings_versions` stores complete immutable settings: monotonic
`version`, checked 0/1 `enabled`, positive integer `daily_threshold_cents` and
`reward_seconds`, `claim_window_months` in `1..120`, server `effective_at`, and
optional unique settings audit event link. Seed v1 disabled with 1,000 cents,
3,600 seconds and one following month; real edits append a version with audit,
identical saves do not append.

`shop_hours_versions` and `shop_hours_version_days` retain complete structured
Shop Hours snapshots and seven weekday periods; `shop_override_history` retains
force-open/closed effective and expiry instants. One captured UTC seed instant
baselines current hours and only a still-active Override; historical coins are
not backfilled. Subsequent owner changes record the live setting, historical
evidence and audit atomically. No-op Shop Hours/Override writes retain existing
Audit Log and Shop Status observation behavior but add no historical evidence.
`attendance_calendar_dates` freezes each completed Admin-local date and its
single restart-observed UTC offset; `attendance_eligible_intervals` stores that
date's sorted, disjoint half-open UTC eligibility intervals without per-boundary
offsets. Touching intervals merge; each segment is resolved against the Shop
Hours and Override evidence in effect at its start.
`attendance_clock_state` is a singleton (`id = 1`) carrying durable `utc_floor`
and monotonic `open_local_date`; date and deadline authority never moves
backward when the host clock or timezone regresses. Read-time deadline crossing
and mutations advance the floor in an immediate transaction. Completed dates
and frozen UTC deadlines never recalculate.

Migration `0056_attendance_earning.sql` adds the S2 earning records:

- `attendance_contributions` is append-only and keyed by
  `source_audit_event_id`; it records `source_kind`, the selected
  `configuration_version`, Account, immutable local earning date, positive
  cents, source instant, and an optional original rejection event for a
  Rejection Credit. `admission_sequence` is an immutable database-assigned
  order used to reconstruct which Contribution first froze the daily snapshot;
  a late Rejection Credit's original `source_at` can be earlier than
  Contributions admitted before it. Rebuild uses that first Contribution's
  stored configuration version, never a version inferred from `source_at`.
- `attendance_daily_progress` has one row per Account/date. Its first admitted
  Contribution freezes configuration version, threshold, reward, and first
  source event; `capped_cents` is the only mutable Attendance projection and is
  capped at that frozen threshold.
- `attendance_earning_months` has one row per Account/local earning month and
  freezes the claim-window configuration version, month count, claim start at
  the following local month's midnight, and exclusive UTC deadline. Its
  `claim_local_dates` JSON stores inclusive local `start` and `end` dates. It is
  created only by the first Check-In in that month. If the earning month's
  opening instant predates all settings versions (the rollout month), the first
  Check-In freezes the earliest available settings version.
- `attendance_check_ins` is append-only, links one successful Check-In Audit
  event to its Account/date and Earning Month, and records the frozen snapshot,
  threshold-crossing source, trigger PC, and original earning instant. One
  Account can have at most one Check-In per date.
- Nullable `audit_log.attendance_seconds_delta` is separate from
  `seconds_delta`, which remains Remaining/Guest Time; a Check-In records only a
  positive Attendance Time delta.

Accepted Account Coin Events and qualifying Account Rejection Credits write the
Contribution, capped progress, Check-In, Earning Month, and audit atomically in
their existing SQLite transaction. An accepted coin qualifies at server commit
when its earning date has any effective Shop Open overlap; the commit itself may
occur while closed. A Rejection Credit uses the original rejection instant and
frozen Account attribution, and qualifies only when Attendance was enabled and
Shop Open at that instant. Disabled coin paths make no Attendance writes. Guest
coins and non-coin value sources do not qualify.
Startup compares daily progress with immutable Contributions and rebuilds only
that projection under `BEGIN IMMEDIATE` if it differs; rebuild selects the
configuration snapshot by the first admitted Contribution's stored version.
Startup refuses if the rebuild fails. Calendar finalization, eligibility and
settings live in `db/attendance.rs`; earning, Check-In/Earning Month and
projection rebuild live in `db/attendance_earning.rs`. Status is derived, not
stored. Expiry is derived at the exclusive month deadline, not stored as status
or a scheduler event.

Migration `0057_attendance_claims.sql` adds two immutable ledger tables:

- `attendance_claims` stores `audit_event_id` (unique FK to `audit_log`),
  globally unique lowercase canonical `request_id`, `account_id`,
  `earning_month_id`, nullable `claim_pc_id`, positive `claimed_seconds`, and
  `created_at`. Indexes cover `(earning_month_id, created_at)` and
  `(account_id, created_at)`. The row links one successful `attendance_claim`
  event; its insert guard checks the event's Account, PC, Remaining Time delta,
  and negative Attendance Time delta, plus the Account/month link. A claim
  transfers all currently claimable seconds from one month; retrying the same
  UUID for that Account/month returns the original result, while reuse for
  another Account/month is audited as a denial. Authenticated domain denials
  store a typed reason (`month_not_found`, `not_yet_claimable`,
  `nothing_outstanding`, `expired`, or `request_id_conflict`) on the Audit Log
  and create no claim detail; inactive or deleted Accounts fail without an
  audit.
- `attendance_forfeitures` stores `account_id`, `earning_month_id`, the
  `deletion_audit_event_id`, positive `forfeited_seconds`, and `deleted_at`.
  `(deletion_audit_event_id, earning_month_id)` is unique; an index covers
  `(account_id, earning_month_id, deleted_at)`. Each row records one positive
  per-month allocation against the existing `account_deleted` Audit Log event.
  Its insert guard verifies the Account/month link and the event's negative
  aggregate Attendance Time delta. A deletion captures its instant after
  `BEGIN IMMEDIATE`, derives each month at that instant (so an exclusive
  deadline crossed before deletion is expired), and writes the aggregate delta
  and all allocations in the same transaction. No separate expiry or forfeiture
  Audit Log event is created.

Claim and deletion advance the durable UTC floor through
`db::attendance::finalize_in_transaction` in the same `BEGIN IMMEDIATE`
transaction; expiry uses `max(host UTC, saved floor)`. Claim balance credit,
Audit Log, and detail commit together; deletion, `account_deleted`, and
forfeiture allocations commit together. Any failure rolls back the full
operation. These immutable facts survive Account deletion; only daily progress
is reconstructed from Contributions. Earned seconds reconcile to
claimed + outstanding + expired + forfeited; claimable is a subset of outstanding.

Migration `0058_attendance_reporting.sql` preserves existing
`report_free_time_dispensed` sources and adds successful Attendance Claims from
`attendance_claims.created_at`, `claimed_seconds`, Account and claim PC. Denied
claims have no detail and are excluded. Claims affect Free Time Dispensed only;
Cash Revenue and Points remain unchanged.

`db::attendance_reports::attendance_report` reads the report in one SQLite
`BEGIN IMMEDIATE` transaction and calls the shared finalizer before deriving
Earning Month expiry. Its `as_of` is the returned durable floor, preventing
expired state from reviving after a clock regression. A report can advance the
clock floor and finalize crossed calendar dates, but writes no Audit Log rows.
It captures one `as_of`, `admin_local_date`, and `admin_local_month`. Day/week/month
flow buckets use the selected Admin-local date range, attributing earned time to
the Check-In earning date, claimed time to claim-detail `created_at`, expiry to
the exclusive Earning Month deadline, and forfeiture to `deleted_at`. Earning
Month and Account pages paginate independently with a default limit of 50 and
maximum of 200; current outstanding and claimable liability remains
range-independent and is reported in seconds by Account. The report also
includes effective Attendance bonus economics and a clock-regression advisory;
neither changes authority or writes an Audit Log event.

Backup Set ownership: **Configuration** — settings versions, Shop Hours
versions/days, Override history, calendar dates and eligible intervals;
**Accounts** — daily progress and Earning Months; **Ledger** — Contributions,
Check-Ins, Claims and forfeitures; **Runtime State** — clock singleton. The
explicit map includes each table in its assigned Set. Full Restore replaces all
Sets. An old full Backup upgrades to empty, disabled Attendance; partial exports
cannot restore. Migration and staged Restore verification check foreign keys,
source/config and event/detail links, progress equivalence, frozen bounds,
intervals, reconciliation and Backup Set completeness. Corrupt immutable
evidence requires a known-good full Backup, never synthetic reconstruction.

## App Update automatic checks (ADR-0086)

The existing key/value `settings` table stores `app_update_auto_check` as
`"true"` or `"false"`; a missing key means enabled. No new table or migration
is needed. Owner changes commit the value and the
`app_update_auto_check_changed` audit event together in one `BEGIN IMMEDIATE`
transaction. Release Feed state stays in memory; the current Schema Version
is read from the applied maximum in `_openpiso_migrations`.


App Update restart state uses two additional keys in the same `settings` table:

- `app_update_restart_marker`: JSON `{ fromVersion, targetVersion, startedAt }`.
  Written only after download/signature preparation succeeds, atomically with
  the install/rollback request audit, before installer handoff.
- `app_update_last_result`: JSON `{ from, to, ok }`. Startup compares the target
  with the running binary once and commits this result with its outcome audit;
  subsequent restarts retain the original result. Installer launch failure
  resolves it immediately so the Owner is not stranded awaiting a restart.

Owner acknowledgement deletes both keys with its audit in one transaction.
Unresolved markers cannot be acknowledged. An outstanding marker prevents a
second install. No migration or payment/session mutation is involved. The Run
Record file also retains optional `app_update_result` for Diagnostics, independently
of acknowledgement; old Run Records without that field remain readable.
