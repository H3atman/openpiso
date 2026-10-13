# ADR-0076: Monitor host and Client reachability separately

- Status: Accepted
- Date: 2026-08-26
- Amended: 2026-09-02 — "The first phase detects and reports only. It never
  changes PC state, sessions, balances, time, or Points" is **narrowed** by
  [ADR-0077](0077-client-recovery-escalation.md). A PC whose Guard is polling,
  reports no connected Client, and reports a failed relaunch may be powered
  off, ending its session as `escalated_power_off`. Balances, time, and Points
  remain untouched, and detection for every other case is unchanged. That ADR
  also adds `escalated_power_off` as an incident closure reason and gives
  Client Guard Coverage a direct source that does not depend on the Client
  heartbeat.
- Relates to: [ADR-0048](0048-client-guard-tamper-kill.md),
  [ADR-0058](0058-audit-derived-web-admin-notifications.md)

## Context

The PC grid reports Client heartbeats, while an owner-run ping tool reports
whether each Windows host responds on the LAN. A host can respond while its
Client is absent. OpenPiso does not currently compare those facts, so the owner
must detect that condition manually. A ping response alone does not prove
tampering.

## Decision

Admin Rust will monitor **Host Reachability** for every visible registered PC
using its server-owned `registered_ip`, while the existing heartbeat remains
the authority for **Client Reachability**. A host that stays reachable while
its Client does not becomes a **Client Reachability Mismatch** after a grace
period. Monitoring runs continuously while Admin runs, with startup debounce
and explicit maintenance suppression.

Admin sends an ICMP echo directly, without starting a shell process, every ten
seconds with a one-second timeout. A setup check warns when a PC never answers
ICMP. A mismatch opens only after Host Reachability and missing Client
Reachability coexist continuously for 60 seconds. The same delay applies after
Admin startup and after a host first becomes reachable.

Monitoring is a shop-wide feature that is off after upgrade until the owner
reviews the probe check and enables it. It covers every visible registered PC;
there is no permanent per-PC opt-out. Telegram is configured and enabled
separately.

Enabling or disabling monitoring requires the current Owner PIN and is audited.
While disabled, Web Admin shows a standing advisory. Disabling sends Telegram a
state-change message when delivery is available and closes active incidents as
`monitoring_disabled`, never as recovered. Historical Alerts remain.

One durable incident exists per uninterrupted mismatch episode. Opening it
appends one Alert. It survives Admin restarts without producing a duplicate.
One accepted Client heartbeat or loss of Host Reachability closes it
immediately, recording which condition ended it. An open incident produces no
repeated Alerts.

All three monitored failures share one durable **PC Monitoring Incident** model
with typed kinds `client_reachability_mismatch`, `host_probe_coverage`, and
`client_guard_coverage`. Generic `pc_monitoring_incident_opened` and
`pc_monitoring_incident_closed` Audit events classify every opening as an Alert
and every closure as a Notice. The kind is typed incident data rather than an
unbounded event name.

Dedicated incident and Telegram delivery tables hold lifecycle state and the
bounded delivery queue. Incident opening or closure, its Audit event, and any
Telegram queue item commit in one SQLite transaction. An open incident that
spans an Admin restart remains open but is marked `observation_interrupted`;
display and delivery must not claim its full wall-clock duration was
continuously observed.

ICMP is evidence, not tamper-proof presence detection. A customer can block it
or move the host away from its registered address. A PC that cannot answer the
probe is marked unmonitored rather than assumed safe. When an open mismatch
loses Host Reachability, its closure says that the host stopped responding and
Client recovery remains unconfirmed; it never says the Client recovered.

When Client Reachability independently proves a host is running but ICMP fails
continuously for two minutes, a separate **Host Probe Coverage** incident opens
with its own Alert, dashboard badge, and Telegram delivery item. One successful
probe resolves it with a Notice. A powered-off PC cannot open this incident
because it supplies no Client heartbeat proving the host is running.

The owner may suppress detection for one PC for at most 60 minutes. Suppression
applies to mismatch, Host Probe Coverage, and Client Guard Coverage monitoring.
It expires automatically, prevents new incidents, and neither erases nor closes
an incident already open. The dashboard shows its expiry. Expiry starts a fresh
full grace period rather than opening an incident immediately.

A PIN-gated Owner Quit on the Client does not create Monitoring Suppression.
The Client cannot grant itself server-owned suppression. The owner suppresses
that PC through Web Admin before maintenance; forgetting may create a truthful
mismatch Alert.

The ten-second probe cadence, 60-second mismatch grace, two-minute coverage
grace, and 60-minute maximum suppression are fixed policy in v1 rather than
owner settings.

The first phase detects and reports only. It never changes PC state, sessions,
balances, time, or Points. Each incident is recorded locally as an Alert before
any delivery attempt. Optional Telegram delivery is best-effort and cannot
become the event history or authority. This deliberately revisits ADR-0058's
rejection of off-device delivery with one narrow, owner-enabled exception.

Telegram v1 delivers to one private owner chat. Opening and closure each produce
at most one delivery item. Items of the same kind created within 30 seconds are
batched into one message, while their local incidents remain separate. Messages
identify each PC by display name and give the incident time, duration when
known, and closure reason. They contain no IP, Account, balance, or customer
data.

The Telegram integration is outbound-only. It accepts no bot command and opens
no inbound port or webhook. Setup asks the owner to create a bot, send it a
private `/start`, enter the token in Web Admin, confirm the detected private
chat, and send a test message. Manual chat-id entry is a fallback only.

Pending deliveries survive Admin restart and retry with bounded exponential
backoff for at most 24 hours. If an incident closes before its opening message
is delivered, the queue replaces both intended messages with one resolved
summary. Telegram has no delivery idempotency guarantee at this boundary, so a
rare duplicate after an ambiguous network result is accepted. Delivery never
acknowledges the local Alert. Settings shows delivery health inline.

The owner enters the bot token through Web Admin. Admin protects it at rest with
Windows DPAPI, never returns or logs it, and excludes it from Backups. Restoring
or moving Admin therefore requires the owner to enter the token again.
Changing or disconnecting Telegram requires the current Owner PIN.
Disconnecting deletes the protected token, destination, and pending deliveries;
local monitoring and Alerts continue.

Replacing a token or destination requires a successful test message before the
new protected configuration replaces the old one. A failed test leaves the
working configuration untouched.

The entire Telegram integration is excluded from Backups, not only the token.
A restored Admin starts with Telegram disconnected until the owner completes
setup again.

The PC dashboard shows an active mismatch badge independently of Notification
read state. The opening event is an Alert and its closure is a Notice.
Acknowledging the Alert records review but does not hide the badge while the
incident remains active.

The dashboard is the only current-state view. Notifications retains its normal
30-day window, and the Audit Log remains permanent history; v1 adds no separate
monitoring report. Local Alerts link to the PC and show the observed evidence,
temporary suppression, Remote Control when configured, and Guard/install
troubleshooting. Telegram directs the owner to Web Admin instead of carrying
repair instructions.

Web Admin provides a non-mutating `Check now` action for one PC and for all
visible PCs. It shows the current Host, Client, and Guard facts without opening
or closing incidents. The owner's batch ping script remains an independent
rollout check, not an OpenPiso runtime dependency.

### Client Guard coverage

A release Windows Client reports the diagnostic fact that it currently holds
its Client Guard pipe connection. Debug and non-Windows Clients report the
check as not applicable. Two continuous minutes without coverage opens a
separate durable Alert, dashboard badge, and Telegram delivery item. Restored
coverage closes it with one Notice. The episode survives Admin restart without
duplicate Alerts. This signal never changes PC, session, money, time, or Points
authority and is never merged with a Client Reachability Mismatch.

### Availability boundary

Monitoring exists only while Admin runs. Admin cannot report its own process,
host, or internet outage to Telegram; doing so requires an external watchdog.
V1 accepts that boundary and adds no second service or cloud heartbeat.

PC Monitoring and Telegram delivery are supported only by Windows Admin in v1.
Their Windows ICMP and DPAPI boundaries report unavailable on other platforms;
they never manufacture healthy results.
