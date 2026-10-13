# ADR-0058: Audit-derived Web Admin notifications

> **Amendment:** [ADR-0070](0070-credit-the-pulse-sum.md) removes new
> `coin_batch_rejected` production. Historical rows remain readable and
> creditable through Rejection Credit; other notification classifications are
> unchanged.
>
> **Amendment:** [ADR-0076](0076-client-reachability-mismatch-monitoring.md)
> permits one narrow off-device exception: optional, outbound-only Telegram
> delivery of locally recorded PC Monitoring Incidents. The Audit Log remains
> canonical and Telegram delivery never acknowledges an Alert.

- Status: Accepted
- Date: 2026-08-02
- Amended: 2026-08-03 — scoped bulk acknowledgement is now allowed. The
  original "no bulk clear" rule is replaced below; grouping and acknowledgement
  notes remain out.
- Amended: 2026-08-09 — PC reachability events drop from Notice to Silent; see
  the amendment in "New events needed by the projection".
- Amended: 2026-08-26 — ADR-0076 permits optional Telegram delivery for PC
  Monitoring Incidents without creating a second event history.
- Deciders: shop owner (product)
- Relates to: [ADR-0015](0015-audit-log.md) (canonical Audit Log),
  [ADR-0024](0024-web-admin-sole-owner-surface.md) (Web Admin is the sole owner
  surface), [ADR-0051](0051-web-admin-url-routing.md) (authoritative Web Admin
  URLs).
- Refined by: [ADR-0064](0064-owner-visible-coin-loss-notices.md) for the
  owner-visible coin-loss event boundary and its Notice reasoning.

## Context

The Audit Log is complete but too broad to act as an attention queue. The owner
needs a Web Admin inbox for events worth noticing, a persistent signal for
money/abuse/tamper incidents, and a low-friction way to clear routine
operational information. OpenPiso remains local-first and single-owner; the
Audit Log must stay canonical rather than being copied into a second event
store.

## Decision

### Notifications are a derived Audit Log view

A **Notification** is an `audit_log` row within a fixed rolling 30-day window
whose event type is classified as an **Alert** or **Notice**. The existing audit
id is its identity, `audit_log.summary` is its display copy, and its typed audit
columns supply actor, account, PC, money, time, points, outcome, and timestamp.
Raw metadata and deep links are not exposed by the Notifications API.

The classifier lives in `openpiso-core`, is fixed in code, and depends only on
`event_type`:

```rust
pub enum NotificationSeverity {
    Alert,
    Notice,
    Silent,
}

pub const fn classify_audit_event(event_type: &str) -> NotificationSeverity
```

Outcome, actor, and value magnitude never alter severity. Unknown event types
default to `Silent`; adding an event does not make it owner-visible by accident.
The classified sets are:

- **Alert:** `account_deleted`, `account_points_adjusted`,
  `account_time_adjusted`, `account_transfer_out`,
  `kiosk_lock_release_throttled`, `manual_adjustment`,
  `owner_login_throttled`, `pc_removed`, `points_transfer_out`,
  `restore_applied`, `tamper_kill`, `voucher_batch_voided`, `voucher_voided`.
- **Notice:** `account_import_completed`, `account_login_throttled`,
  `account_password_reset`,
  `autostart_changed`, `autostart_change_failed`, `backup_created`,
  `backup_deleted`, `backup_settings_changed`,
  `backup_schedule_caught_up`, `backup_schedule_failed`,
  `coin_device_recovered`, `coin_device_settings_updated`,
  `coin_device_unavailable`, `coin_promo_settings_updated`,
  `coin_sampling_anomaly`, `coin_batch_rejected`, `economy_settings_updated`,
  `kiosk_lock_engaged`,
  `kiosk_lock_released`, `offline_grace_updated`, `owner_configured`,
  `owner_credential_changed`, `owner_pin_changed`, `pc_pre_registered`,
  `pc_settings_updated`, `points_transfer_expired`, `points_transfer_rejected`,
  `points_transfer_requested`, `redemption_tiers_updated`,
  `restore_cancelled`, `restore_requested`, `shop_closed`,
  `shop_hours_settings_updated`, `shop_name_updated`, `shop_opened`,
  `shop_override_cleared`, `shop_override_set`, `shutdown_grace_updated`,
  `voucher_batch_minted`, `wheel_settings_updated`.

All other event types are silent. Low-level events stay silent where an
aggregate or canonical event exists: for example, `login_failed` is silent
while `owner_login_throttled` is an Alert, and `tamper_kick` is silent while
`tamper_kill` is the Alert.

### Read state is global and hybrid

There is one owner, so read state is shop-global rather than per-user:

- singleton `notification_read_state` (`id = 1`, `last_seen_at`) is the Notice
  high-water mark;
- `notification_alert_ack` is keyed by `audit_log.id` and stores one
  `acknowledged_at` per Alert.

Opening Notifications advances the Notice watermark. Alerts never clear from
attention merely because they were viewed; each needs an explicit, idempotent
acknowledgement. The first acknowledgement and its
`notification_acknowledged` Audit Log event commit in one SQLite transaction;
repeats write nothing. Acknowledgement means only “reviewed”: it performs no
remediation, disables no PC, changes no balance or points, and does not combine
repeated incidents. Every repeated `tamper_kill` remains a separate Alert with
its own acknowledgement. There is no grouping and no acknowledgement note.

> **Amended 2026-08-03.** The original text also ruled out any bulk clear.
> Owners accumulating dozens of Alerts had no way to clear a reviewed queue
> except one click per Alert, so scoped bulk acknowledgement is now allowed
> under constraints that keep the model intact: the owner submits the explicit
> Audit Log IDs their Notifications page has rendered, never a server-side
> "acknowledge everything" sweep, so Alerts on unloaded pages stay in
> attention. The batch commits in one SQLite transaction and appends one
> `notification_acknowledged` event per **newly** acknowledged Alert, so
> per-Alert traceability and idempotency are unchanged. Ineligible IDs —
> unknown, non-Alert, or outside the rolling window — are skipped instead of
> failing the batch, because the rendered list can go stale between render and
> click; already-acknowledged IDs stay no-ops. A batch carries 1–200 IDs,
> matching the list page cap so one request cannot open an unbounded
> transaction. Bulk acknowledgement is offered only on the Notifications page,
> where the Alerts are readable; the inline Alert banner keeps its
> preview-and-navigate role. There is still no un-acknowledge.

Migration `0036` creates both tables, seeds `last_seen_at` with
`CURRENT_TIMESTAMP`, and inserts baseline acknowledgement rows for existing
Alerts. Baseline rows append no `notification_acknowledged` event because they
record upgrade eligibility, not an owner action. This makes all history
predating an install or upgrade ship read without losing the two-table model.
Alert acknowledgements are retained indefinitely. The 30-day limit is only a
notification query boundary; Audit Log retention remains forever.

### Owner API and liveness

The authenticated `:8080` owner server provides list, unread-count, Notice-seen,
and Alert-acknowledge endpoints under `/web-admin/notifications`, the last in a
single-id and a bulk form. List reads are
reverse chronological and paginated; unread count is unacknowledged Alerts plus
Notices newer than the watermark. The existing
`idx_audit_log_event_type_created` index and the acknowledgement primary key
cover these reads; no new Audit Log index is added.

Safe-read authentication failures return `401` without audit. Denied mutations
use the existing `web_admin_access_denied` path. Successful reads and Notice
watermark advances are not audited; the first Alert acknowledgement is.

The authenticated Web Admin root polls unread count and unacknowledged Alerts
every 15 seconds. Polling is retained because the Web Admin already uses it and
has no server event channel; no SSE channel or page-local poller is introduced.

### Web Admin surface

Notifications is a top-level `/admin/notifications` route. Its sidebar badge is
one combined count of unacknowledged Alerts plus unread Notices. Following the
selected Variant B prototype, the page has an Alert attention queue and Notices
grouped by day. The Notice list is not filtered by read state, so the rolling
window stays browsable after the watermark advances; unread Notices are marked
inline for the current visit. Acknowledging an Alert removes it from the active
queue, with acknowledged history available on demand. An "Acknowledge all"
action in the attention header acknowledges the rendered Alerts behind one
confirmation. A quiet state replaces both queues
when nothing needs attention.

Unacknowledged Alerts also render as one compact, collapsible inline banner on
every authenticated page. Previewing or collapsing it never acknowledges an
Alert. This is a standing advisory inside the normal layout, not an overlay.

### New events needed by the projection

Classification creates no audit rows. Existing emit sites remain unchanged
except for the operational event types listed below; the reachability pair is
retained in the Audit Log but is Silent:

- `coin_device_unavailable` and `coin_device_recovered`: one event per real Coin
  Source availability edge, never per retry and never for the simulator;
- `coin_sampling_anomaly`: one rate-limited, cause-keyed aggregate event for
  Inhibited Pulses, with a pulse count and one of the two observable sampler
  states in the summary and `reason` metadata: unavailable/recovering, or
  healthy with no Selection Window and the Set Pin inhibited. The latter names
  idle insertion and inhibit wiring as possibilities rather than inventing a
  diagnosis; the Coin Debug Bundle retains its timestamp and reason for field
  feedback. The Notice never carries raw samples or individual noisy pulses
  and has no PC or Account subject;
- `coin_batch_rejected`: historical rows remain classified as Notices so the
  owner can review and use Rejection Credit. S1 creates no new rows, has no
  per-batch owner list, and has no closing suppression producer. Commit
  rejections remain Diagnostic Log evidence, as refined by ADR-0064;
- `backup_schedule_failed`: one failed due scheduled Backup attempt;
- `backup_schedule_caught_up`: one successful startup catch-up for a missed due
  time, committed with the schedule advance so a crash cannot lose it;
- `pc_went_offline` and `pc_came_online`: one event per server-observed PC
  reachability transition, committed with the status transition. *(Amended
  2026-08-09: both classify as Silent now; see below.)*

> **Amended 2026-08-09.** `pc_went_offline` and `pc_came_online` are demoted
> from Notice to **Silent**. The PC grid already shows live reachability
> status, so a Client PC rebooting, sleeping, dropping Wi-Fi, or being
> switched off at close produced a Notice carrying no new information, and the
> combined sidebar badge counted them — a non-zero badge must mean something
> worth reading. Both types are removed from the allowlisted Notice set, so
> rows already recorded in the rolling 30-day window fall out of the
> Notifications list and unread count with no schema change, migration, or
> backfill; existing shops lose up to 30 days of reachability Notices,
> including unread ones, on upgrade. Emit sites are retained unchanged: the
> Audit Log still records every transition, with its transition source, in the
> same SQLite transaction as the status change, and `pc_went_offline` remains
> server-visible reachability evidence for diagnosing Client Offline Shutdown
> Countdown and Client Guard behavior. The countdown itself uses Client-local
> connection state, while Guard liveness uses the held pipe. These reachability
> events are redundant with state already visible in the live PC grid.

Those subsystems add no separate diagnostic event: `pc_went_offline` provides
server-visible reachability evidence, while `tamper_kill` provides Guard evidence.
Shop-hours continuation is intentional, not a violation. Backup Mirror failure
keeps its existing standing advisory rather than entering this event taxonomy.

## Considered and rejected

- **Materialized `notifications` event table:** duplicates canonical Audit Log
  rows and can drift from reporting/audit truth. Rejected for a derived view
  plus minimal read state.
- **Owner-tunable severity or mute preferences:** adds policy and configuration
  drift to a single-owner v1. Rejected for a fixed, reviewed taxonomy.
- **Burst grouping/collapse machinery:** adds identity and lifecycle semantics.
  Rejected; classify aggregate/terminal events and keep repeated Alerts
  individually accountable.
- **General off-device delivery:** phone, email, push, or cloud relay conflicts
  with this local-first effort and would introduce a separate delivery system.
  Rejected. ADR-0076 later permits one narrow, owner-enabled Telegram adapter
  for PC Monitoring Incidents; the local Audit Log remains canonical.
- **Admin-app OS notifications:** the Web Admin is the sole owner surface under
  ADR-0024; a Tauri notification plugin and capability would cross that
  boundary. Rejected.
- **State-based health channel:** current outage/overdue state is a different
  standing-advisory model. Rejected; this surface contains immutable events
  only.
- **Web Admin SSE:** adds a new server channel for a 15-second-latency owner
  signal. Rejected in favour of the existing polling pattern.

## Consequences

- The Audit Log remains the sole event history; Notifications can always link
  back to the same immutable audit ids without copying event payloads.
- A migration and transactional repository/API paths are required even though
  the notification rows themselves are derived.
- New audit event types remain invisible until deliberately added to the core
  classifier.
- Opening Notifications clears Notice unread state globally for every owner
  browser, while Alert acknowledgement remains durable and per event.
- The API reference is updated when the endpoints ship, not ahead of their
  implementation; implementation PRDs carry that documentation acceptance
  criterion.
