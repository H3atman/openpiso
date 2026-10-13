# ADR-0083: Admin sends one always-on, anonymous daily summary to PostHog

- Status: Accepted
- Date: 2026-09-23
- Deciders: shop owner (product)
- Relates to: [ADR-0054](0054-backup-and-restore.md) (backups, whose excluded
  tables now include telemetry state),
  [ADR-0076](0076-client-reachability-mismatch-monitoring.md) (the Telegram
  outbox this copies), [ADR-0024](0024-web-admin-sole-owner-surface.md) (the
  Web Admin owner surface), and
  [ADR-0078](0078-support-bundle.md) (Diagnostic Forwarding, which stays
  LAN-only and is not telemetry).

## Context

We cannot answer a basic project question: how many shops run OpenPiso, how
many PCs they serve, and which versions are in the field. Telemetry required explicit approval under `AGENTS.md`;
the product owner has now approved an always-on, narrowly allowlisted scope for
token-bearing builds. The owner has also chosen
**PostHog Cloud, US region, free plan**, and created the project with
**Discard client IP data** enabled.

What we need is small: one number per shop per day is enough to count active
installations, and the Admin already knows how many Clients sent a valid
Heartbeat. Nothing about a customer, a session, or money is needed. The
research behind this decision, with primary-source citations, is in the
feasibility note that preceded it.

Constraints that shape the design:

- Rust owns authority, and the frontend is a thin client. A browser SDK would
  run in the owner's phone or the kiosk webview, need CSP changes, and can be
  ad-blocked.
- Shops lose internet for days. An in-memory queue loses those days.
- The PostHog project token is a public, write-only key. Anyone who has it can
  send events to the project.
- The PostHog free plan gives 1M events a month, keeps events for 1 year, and
  that retention cannot be shortened.

## Decision

### Admin sends one Daily Summary per reporting day in token-bearing builds

**Telemetry** is always on when the Admin is built with the project token.
It starts without an owner action. There is no owner opt-out, disable, or ID
reset control. This must be disclosed plainly before installation:
the destination is PostHog Cloud US, the exact event fields are below, and
retention is one year. There is no Settings toggle or Telemetry management API.

In token-bearing builds, the Admin sends one `admin_daily_summary` event per **reporting
day**: a calendar day at a fixed UTC+08:00 (Asia/Manila, no DST), the same
boundary for every shop whatever the Admin host's timezone. The summary is
taken at the first worker tick of the day (or the first tick after startup),
so a day the Admin never runs has none. The payload is a closed allowlist:

| Property | Meaning |
| --- | --- |
| `schema_version` | Payload version, starts at `1` |
| `period_utc8` | The UTC+08:00 day the summary was taken, `YYYY-MM-DD` |
| `admin_version` | Admin app version |
| `clients_active_24h` | PCs whose `last_seen_at` (latest valid Heartbeat) is within the 24 h before the snapshot |
| `clients_active_30d` | PCs whose `last_seen_at` is within 30 days |

It never sends shop names, hostnames, IPs, MACs, `pc_id`s, Accounts,
customers, sessions, usernames, money, time, Points, transactions, the Audit
Log, diagnostic text, or free-form strings. Adding a property is a
`schema_version` bump and an amendment to this ADR.

The event also carries `distinct_id` (the Installation ID), `uuid` (stable
across retries), and `timestamp` (the snapshot instant) and each request carries `sent_at`, so a summary retried days later lands on its own day and
PostHog corrects a skewed Admin clock. These are event envelope fields, not
properties. `period_utc8` still comes from the Admin clock and is
directional.

An **active installation** is an Installation ID that sent a Daily Summary for
a given reporting day. It does not mean the owner used the Web Admin, or that anyone
used a PC.

### The Installation ID is random and local

On first use in a token-bearing build the Admin generates a v4 UUID, the
**Installation ID**, and stores it in SQLite. It is sent as `distinct_id`.
It is never derived from hardware, network, shop, or owner data. The owner
cannot reset or remove it through a Telemetry control. No disable/reset
routes or Telemetry consent audit events are exposed.

Telemetry tables are excluded from backups, so restoring one backup into two
shops cannot give them the same ID. A restored token-bearing Admin generates
a new Installation ID. A reinstall with a fresh database counts as a new
installation, which can overcount monthly actives; charts say so.

### Plain HTTP from Rust, not a PostHog SDK

The Admin posts to `https://us.i.posthog.com/batch/` with the `reqwest` client
it already has. Every event sets `$process_person_profile: false` (no person
profile) and `$geoip_disable: true` (no location lookup), and carries its
outbox row's UUID so a retry is the same event.

- **Not `posthog-rs`:** it is pre-1.0 and changing fast, pulls a newer
  `reqwest` than ours plus unrelated dependencies, and queues in memory only,
  so we would still need our own outbox.
- **Not `posthog-js`:** browser-side capture breaks "Rust owns authority" and
  needs CSP changes. With Rust-side capture there is no Tauri capability, CSP,
  or plugin change, and the Client app is untouched.

### Summaries wait in a SQLite outbox, isolated from authority paths

This copies Telegram delivery. A daily job writes one outbox row per
`period_utc8`, replacing any unsent row for the same day, and records that
day as the last summarised period in the same transaction, so a sent (and
deleted) row is never summarised again. A worker on its own
task sends all due rows in one request with a 15 s timeout, backs off from
30 s up to 1 h, and drops rows older than 30 days. It never runs inside the
Heartbeat, timer, coin, session, or startup paths, and it never blocks
shutdown. A PostHog response is never allowed to change OpenPiso's behaviour.
Logs record outcomes without the token or the Installation ID.

### The token is a build-time value

The project token and host are read at compile time from
`OPENPISO_POSTHOG_TOKEN` and `OPENPISO_POSTHOG_HOST`, which the release
build step exports into its environment (a CI secret or the release shell);
no build script loads them from a file. A build without a token,
such as a development build or a fork, cannot send to our project. A
token-bearing build sends automatically; it does not expose a Telemetry toggle.
The token is never committed.
Personal API keys are never shipped.

## Consequences

- We can chart daily, weekly, and monthly active installations, summed active
  PCs, and Admin version adoption. The numbers are directional, not audited:
  anyone with the public token can send fake events.
- Volume is about 30 events per installation per month, so the free plan's 1M
  events cover about 33,000 running shops. The organization has no payment
  method; if one is ever added, billing limits go on first.
- Events are kept for 1 year and cannot be kept for less. PostHog cannot
  delete one installation's events without a person profile; there is no
  owner-facing deletion or opt-out remedy. This tradeoff must be disclosed before installation, not framed as consent.
- PostHog still sees the source IP of each request before discarding it. The
  project setting, not our code, is what keeps IPs out of stored events.
  Always-on delivery means the owner cannot avoid that disclosure by declining Telemetry.
- The region is permanent on the free plan; moving to EU would mean a new
  project and losing history.
- The glossary gains **Telemetry**, **Installation ID**, and **Daily
  Summary**. Diagnostic Forwarding remains LAN-only and is still not telemetry.
- Deferred: Client version buckets (would need a `pcs` column), an
  `admin_platform` property, and an `admin_started` event. Each is a
  `schema_version` bump if we want it later.
