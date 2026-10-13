# ADR-0048: Client Guard — Tamper Kill detection, relaunch, and kick

- Status: Accepted
- Date: 2026-07-26
- Amended: 2026-08-01 — the Context's "prevention is settled and closed" is
  narrowed (window-level prevention now exists; see
  [ADR-0028](0028-client-kiosk-os-hardening.md) §3), the *Considered options*
  entry on preventing the kill is corrected, and the delivered implementation
  facts are folded into the Decision. The tamper model itself is unchanged.
- Amended: 2026-08-02 — the **customer-facing warning screen is removed**. A
  Tamper Kill is now logged and never announced; see *The kill is logged, never
  announced* below, which replaces the original "The warning blocks once, and
  survives being dodged". The crash false-positive mitigation is corrected
  accordingly. Detection, relaunch, reporting, attribution, and `tamper_kick`
  are all unchanged.
- Amended 2026-08-05 — a relaunched Client holds the SSE state channel behind
  the reporting heartbeat until the Tamper Kill is acknowledged, so a stale
  pre-kick account snapshot cannot render Insert Coin before the authoritative
  Shutdown Grace countdown. A guest Tamper Kick preserves Guest Time but shows
  a neutral Session ended Lock requiring explicit **Continue Guest session**;
  adding more Guest Time does not bypass that acknowledgement.
- Amended 2026-08-07 — Tamper Kill attribution distinguishes an idle PC
  (`subject: none`) from an unresolved or untrusted attribution
  (`subject: unknown`).
- Amended 2026-08-09 — attribution remains the server's job and the device
  timestamp remains a hint, never an authority. The replaced trust-window
  sentence was: "The timestamp is trusted only inside [PC last heartbeat,
  report receipt]; outside that window, or when it is malformed, the event is
  recorded as unknown." The Client now sends **Kill Report Age** in whole
  milliseconds; the Admin derives the kill instant as report receipt minus
  that age. A missing age or one beyond 24 hours records `unknown`; the
  timestamp remains only the stable report name, so duplicate retries still
  coalesce. Kill Report Age and the related diagnostic Client clock difference
  are approved protocol-only integer-millisecond exceptions to the project's
  integer-second authority-duration invariant; neither changes balances or
  timers.
- Amended 2026-09-14 — an unexplained pipe drop waits an **end-session
  grace** (3 s) for Windows to say the session is ending before it is
  classified. The Guard accepts **pre-shutdown** (replacing shutdown) and
  **session change: logoff** from the Service Control Manager as legitimate
  notices. The first relaunch after a genuine Tamper Kill is no longer instant;
  see *Windows ends the Client before it tells the Guard* below.
- Deciders: shop owner (product)
- Refines: [ADR-0005](0005-tamper-response-and-expiry-powraoff.md) (replaces its
  unimplemented "watchdog scheduled task baked into the master image" with a
  concrete Windows service, and closes its open question about what a relaunched
  Client does with the still-live session)
- Relates to: [ADR-0015](0015-audit-log.md) (distinct session-end event types),
  [ADR-0062](0062-account-session-history.md) (durable history for future
  kill-instant attribution),
  [ADR-0027](0027-client-lock-and-owner-pin-gate.md) (Owner PIN gate, untrusted
  customer), [ADR-0028](0028-client-kiosk-os-hardening.md) (kiosk lockdown, and
  its explicit rejection of registry-disabled Task Manager),
  [ADR-0031](0031-client-config-persistence-and-pc-identity.md) (MAC-keyed PC
  identity), [ADR-0045](0045-offline-shutdown-countdown.md) (a legitimate
  Client-initiated exit path).

## Context

ADR-0005 decided that a killed Client should be relaunched by a "persistent
client-side watchdog (Windows service / scheduled task), baked into the diskless
master image." That decision was never implemented — no watchdog exists in the
repository, and the master-image framing meant nothing shipped with the app. A
customer who presses Ctrl+Alt+Del and ends the Client task today gets a bare
desktop for the remainder of their paid time, and the owner gets no record that
it happened.

ADR-0028 closed the casual escapes (Win, Alt+Tab, Alt+Esc, Ctrl+Esc) with a
keyboard hook and delegated real lockdown to Assigned Access / Shell Launcher
v2. It also **explicitly rejected** registry-disabling Task Manager as part of
"full custom hardening" — maximum lockdown at maximum fragility and brick risk.
Ctrl+Alt+Del is the Secure Attention Sequence and cannot be intercepted by app
code at all.

So prevention is settled: on a machine that is not fully provisioned, Task
Manager is reachable and the Client can be ended. This ADR does not reopen that.
It decides what happens *afterwards*.

> **Amended 2026-08-01.** The original text said prevention was "settled and
> closed" and built its rationale on that. One sliver was neither: the *window*
> levers — the taskbar context menu's **Close window**, the system menu, Alt+F4
> — still killed the Client for free on an unprovisioned PC, and the first
> installed release proved it. [ADR-0028 §3](0028-client-kiosk-os-hardening.md)
> now closes them with `skipTaskbar`, a refused `WM_CLOSE`, and a
> foreground-scoped all-modes Alt+F4 hook. **Nothing in this ADR changes as a
> result.** A refused close never terminates the process, so the pipe never
> drops and the Guard sees nothing at all — a refusal is not a clean exit, it is
> a non-event. Task Manager's **End task** is not a `WM_CLOSE` and is entirely
> unaffected; it remains this ADR's job, which is why the Guard is still the
> load-bearing layer and not a backstop for a fixed bug.

Two facts shape the response. First, ADR-0005's core observation still holds:
**the money is already safe**, because the authoritative timer runs on the
server (ADR-0001). Killing the Client does not stop time burning; it just
freezes the customer's own screen. The response is therefore about **friction,
evidence, and deterrence**, not theft prevention. Second, an unattended PC with
no Client on top of it is a privacy and revenue hole regardless of who caused
it — a crash deserves a relaunch just as much as a kill does, even though only
one of them deserves an accusation.

## Decision

### The Client Guard is a Windows service, shipped with the Client

A new binary, the **Client Guard**, runs as a Windows service under SYSTEM
(`OpenPisoClientGuard`, package `client-guard` in `apps/client-guard`). It carries no Tauri
dependency. The **Client's own installer registers and starts it**, and
unregisters it on uninstall — it is not a separate artefact and not a documented
manual step, because an optional install step is a step that gets skipped and
then the protection silently does not exist.

A service was chosen over a scheduled task or a sibling helper process precisely
because a standard user — which is what the seated customer is — cannot stop a
SYSTEM service from Task Manager. The mutual-guard sibling-process alternative
was rejected: Task Manager ends process trees, so the pair dies together.

The cost is real and accepted: relaunching an interactive UI from session 0
requires `WTSGetActiveConsoleSessionId` plus token duplication and
`CreateProcessAsUser`. The Guard queries the active console user's token as
LocalSystem, duplicates it as a primary token, builds that user's Unicode
environment, targets `winsta0\\default`, and uses the Client install directory as
the child working directory. No attached console or no logged-on user is a
temporary failure: the existing bounded backoff retries until a user session is
available. This is the most fragile part of the design and the part that cannot
be unit-tested; it is exercised only on a real release-Windows kiosk.

**One resolved path, verified before use.** Relaunch and pipe-client image
validation both call `relauncher::resolve_client_path`, so they cannot disagree
about which image is the Client. The installed file name is
`openpiso-client.exe` — Tauri names the main binary after the Client's Cargo
**package** name, not `productName` — and the resolver requires the file to
exist rather than trusting the guess. A first release shipped the `productName`
spelling: every relaunch failed with `ERROR_FILE_NOT_FOUND` while the real
Client was rejected on the pipe as an unexpected image. A unit test now pins
`CLIENT_EXE` to the Client's package name and fails if `mainBinaryName` ever
overrides it.

### Liveness is a held pipe, and legitimate exits announce themselves

The Guard hosts a named pipe, ACL'd so the interactive user may connect but may
not impersonate the server end. The SDDL descriptor is carried through a real
`SECURITY_ATTRIBUTES` struct; a Windows test creates a kernel pipe object and
reads its DACL back as SYSTEM full (`FA`) and an interactive-user mask of
`0x120192`: pipe write plus required metadata/synchronization rights, explicitly
excluding pipe read and `FILE_CREATE_PIPE_INSTANCE`. The test then writes through
a client handle and confirms a competing server instance is denied. The Client
connects at startup and **holds the pipe open for its lifetime**. A
dropped pipe *is* process death — no process polling, no PID races, and
detection is immediate. A connection establishes liveness only after its process
image matches the installed Client. Rejected connections never emit a lifecycle
event, relaunch, or Tamper Kill; foreign images produce an immediate durable
warning, then at most one warning per 30 seconds of continued attempts carrying
the suppressed count. Failures to inspect an image produce an error. Neither is
reported to the server.

Because that liveness model is edge-triggered on pipe events, a Client that
never connects is never guarded and never noticed: no connection means no
disconnect, so no relaunch and no signal. The Client autostarts from an
`HKCU\...\Run` key while the Guard is an `auto` service — they race at logon,
and a `Run` key cannot express a service dependency. The Client therefore
**retries the connect in the background and never gives up** (every second for
the first 30 s, every 30 s thereafter), so losing the logon race costs seconds
rather than the whole session, and a Guard repaired mid-session is picked up.

Before any intentional exit the Client sends a typed **clean-exit** message:
expiry power-off (ADR-0005), Offline Shutdown Countdown (ADR-0045), and the
PIN-gated owner `quit_client` (ADR-0027). The Guard additionally treats an OS
shutdown notification as legitimate, so a machine going down does not
manufacture accusations against whoever happened to be logged in.

**Anything else is a Tamper Kill.** The Guard is the sole judge; the
classification is never made by the dying process.

A marker file was rejected for this handshake: a standard user can create a file
by hand, and a forgeable clean-exit erases the evidence the feature exists to
collect. The pipe ACL is load-bearing, not incidental.

### Windows ends the Client before it tells the Guard

**Amended 2026-09-14.** The original Guard classified a disconnect the instant
the pipe dropped and excused it only if `SERVICE_CONTROL_SHUTDOWN` had already
arrived. Windows ends the interactive user's processes **before** it notifies
services of a shutdown, and a Windows sign-out sends no shutdown control at
all. So an ordinary sign-out or restart looked like an unexplained death: a
PC 03 Support Bundle (2026-09-14) showed a customer's logout followed seven
seconds later by a `tamper_kill`, and a relaunched Client failing with
`os error 1018` in a session already being torn down.

When the pipe drops with **no typed message**, the Guard now waits an
**end-session grace** of about 3 seconds for one of two Service Control Manager
notices:

- **pre-shutdown** — accepted instead of shutdown, since a service cannot
  accept both; it also stops the Guard, as shutdown did; and
- **session change: logoff** — it does **not** stop the Guard, which must
  survive a sign-out to guard the next logon. Other session-change events are
  acknowledged and ignored.

A notice before the deadline (a notice exactly at it counts) makes the death an
OS shutdown: no Tamper Kill, no relaunch, no kick. Otherwise it is a Tamper
Kill exactly as before. The Guard logs which notice it accepted. A typed
clean-exit is still final and is never delayed. A notice older than the grace
at the moment of the disconnect is stale and excuses nothing, and the notice is
cleared after classification and whenever a Client connects, so a sign-out
elsewhere cannot hide a later kill. The kill timestamp is taken when the pipe
drops, before the wait. The logoff notice is not matched to the Client's
session: on a multi-session PC, another session's sign-out inside the grace
would excuse a kill. Kiosks run one console session, so this is accepted.

**The cost is the instant first relaunch.** A genuine Tamper Kill now relaunches
after up to the grace, and because the Guard loop is single-threaded, other pipe
events wait behind it too. Both are accepted: a few seconds of bare desktop is
still friction, and the money is safe on the server regardless. If kiosk logs
show a notice routinely trailing the drop by more than 3 seconds, raise the
grace rather than add a second mechanism.

**Rejected: the Client announces the end of the session.** The Client could send
`OsShutdown` when it receives Windows end-session window messages, but any
process on the desktop can post those to its window. That is a forgeable excuse
to kill the Client for free, and it would let the dying process classify its
own death. The notices the Guard accepts come only from the Service Control
Manager.

### Crashes are treated as Tamper Kills, and this is deliberate

The Guard cannot distinguish a Task Manager "End task" from a segfault, and
does not try. Both produce a Tamper Kill: relaunch, report, kick.

This is chosen with the false-accusation cost understood. The alternative —
letting an unexplained death pass quietly — hands a tamperer the exact loophole
worth finding, since any reliable way to crash the Client on demand would
become a free unmetered PC. The chosen mitigation is that the consequences are
**deliberately mild** (see below): a customer wrongly caught by our own bug
loses a re-login and nothing else — since the 2026-08-02 amendment they are
never told they were suspected — and the owner sees the pattern in the audit
feed because a real bug fires repeatedly across PCs while a real tamperer does
not.

### Relaunch is immediate, then backs off, and never gives up

The first relaunch is instant — deterrence has to feel instant. (Since the
2026-09-14 amendment it follows the end-session grace, a few seconds at most.)
Repeated deaths
inside a short window back off exponentially (1s, 2s, 4s… capped near 30s), and
repeats inside that window are **coalesced into one audit event carrying an
attempt count** rather than one row per cycle.

The Guard never permanently gives up. A crash-looping PC that stays dark is
worse than a noisy log, and "give up after N failures" would reward anyone who
found a reliable startup crash with a permanently unguarded machine.

A successful `CreateProcessAsUserW` launch is tracked by retaining that exact
process handle until that process connects (matched by PID). While the process
is alive but has not connected, the Guard emits a durable warning and suppresses duplicate
launches; if it exits, retries resume on the existing bounded backoff. This
avoids image-name/session scans and single-instance-mutex ambiguity while
preserving the never-give-up policy.

### Guard failures are durably diagnosable

A Windows service has no reliable interactive stdout. The Guard therefore writes
tracing events through a non-lossy queue to daily rotating JSON files under
`%ProgramData%\OpenPiso\logs` (`client-guard.<date>.log`). It prunes expired
Guard logs at startup and caps retained files during long service uptimes to the
current day plus about seven prior days. Relaunch failures retain the Win32 API
name and error code. If file initialization fails, the Guard falls back to stderr
rather than refusing to protect the PC.

The installed Client also writes per-user daily JSON files under
`%APPDATA%\org.openpiso.client\logs` (`client.<date>.log`), while retaining
stdout for development. Its first failed pipe connection records the exact
`CreateFileW` error before retries continue, so a packaged Client leaves usable
connection evidence even without a console. These local files remain disposable
under the diskless model; meaningful diagnostics are still forwarded to the
admin server where possible.

### The relaunched Client reports; the server attributes

The Guard hands the relaunched Client a "restarted after Tamper Kill" flag plus
the kill timestamp and attempt count. The **Client** reports it, on its existing
MAC-identified heartbeat path (ADR-0031). The Guard does not talk to the admin
server: a second network client would duplicate config, MAC resolution, and
identity, and would add a new authenticated caller to the admin API for no gain.

The report is **persisted locally and retried until acknowledged**, so a kill
during an admin-server outage is not a kill that goes unrecorded — and the
offline window is exactly when a tamperer would try.

Attribution is the server's job and only the server's job. The Client derives
**Kill Report Age** from two readings of its own clock at the kill and report
send, and does not persist it; every retry and post-reboot report derives a
fresh age. This protocol-only diagnostic is an explicit narrow exception to
integer-second authority durations; it never changes balances or timers. The
server computes the kill instant as report receipt minus that age, then consults
Account Session history for the covering session. Age `0`
anchors the instant at receipt. Rows that ended before report receipt still
identify the historical Account; a `server_restart` end is infrastructure
closure and does not erase that identity. A missing age or an age beyond the
24-hour sanity limit records `unknown`; the limit is not a freshness rule, so a
report delayed by hours can still attribute. The device-supplied timestamp is
only the stable report name and duplicate-detection key — a **hint, never an
authority**, per the durable invariant. The process we are policing never gets
to name the suspect.

### The audit row distinguishes guest from unattributable

The event type is `tamper_kill`. `metadata_json.subject` is one of
`account` | `guest` | `none` | `unknown`, with `account_id` set only in the
`account` case. `none` means no session was active; `unknown` means attribution
could not be resolved or trusted.

Without that field the log cannot tell "a guest killed it" from "we could not
work out who killed it" — both are a null `account_id`, and one is evidence
while the other is noise. It rides in the existing `metadata_json` rather than a
new column, because a migration for a field only one event type uses is not
earned; separate event types were rejected for multiplying the vocabulary
along a dimension that is really one attribute.

### The forced session end is `tamper_kick`, and it confiscates nothing

The session is ended under its own event type, `tamper_kick`, joining
`logout` / `auto_kick` / `taken_over` / `expired`. ADR-0015 is explicit that a
log which says "logout" when the customer was kicked is a log that lies, so
reusing `auto_kick` with a metadata reason was rejected.

**Balance and points are untouched.** Account time is account-bound, so a forced
logout preserves it and costs only a re-login; guest time is PC-bound and stays
on the PC. Forfeiting the remaining time was considered and rejected: it makes
every crash and every false positive financially destructive, and turns a
deterrent into a counter dispute.

Guests are kicked to the Fullscreen Lock too, and must explicitly continue,
even though there is no account to log out. Uniformity is the point — every
tamperer hits the same wall, and a guest session is not a free pass.

A kill with **no session at all** (idle PC at the Lock) still relaunches and
logs as `subject: none`. There is nothing to kick. `unknown` is reserved for
attribution that could not be resolved or trusted; it never stands in for an
idle PC. An `unknown` row records one diagnostic reason in its metadata:
`missing_report_age`, `invalid_report_age`, or
`current_occupant_not_attributed`. The review summary expands the reason into
plain language; other Subjects carry no reason. ADR-0015's observation that
failures and denials are the highest-value rows applies directly: someone
probing an idle machine is exactly the reconnaissance signal the owner wants,
and it is the state a tamperer tests first.

### The kill is logged, never announced

**Amended 2026-08-02.** The original decision here was a blocking full-screen
warning on relaunch — *closing OpenPiso is not allowed, this attempt has been
recorded and reported to the owner* — with a single Acknowledge button, backed
by a persisted flag so killing the Client again could not dodge it. That screen,
its persisted flag, and its two Tauri commands are **removed**.

The relaunched Client now shows nothing. It comes back to the Fullscreen Lock
without a tamper accusation. For an account or an idle PC this converges on the
server-commanded state, including the fresh Shutdown Grace countdown. For a
Guest with preserved Guest Time, the server acknowledgement leaves the Client
on a neutral Session ended Lock with an explicit **Continue Guest session**
action. Adding Guest Time does not silently clear that lock.

The warning told a tamperer three things worth more to them than the reprimand
was worth to us: that detection exists, that it fired, and that it reports
somewhere. That is an invitation to probe for the boundary — kill it a dozen
ways and read the screen to learn which ones were noticed. Silence gives that
back nothing. The evidence still reaches the owner in full: the report is
persisted and retried until acknowledged, and the `tamper_kill` row with its
`subject` attribution is unchanged.

**The kick remains a partial tell, and that is accepted.** A customer who kills
the Client and finds themselves logged out can infer that *something* noticed.
But that signal reveals nothing about the mechanism, what was recorded, or
whether an owner will ever look — and it is indistinguishable from an ordinary
session end, which is exactly the ambiguity worth keeping. Do not "finish the
job" by removing the kick: the kick is the only consequence left, and dropping
it makes a Tamper Kill free.

Deterrence was the original rationale for the screen, and it is knowingly given
up. There was never evidence a warning deterred anyone, and the money was
already safe (the timer is server-authoritative); what the feature actually
buys the owner is the record, not the reprimand.

### Owner visibility is the audit feed, and nothing more yet

`tamper_kill` and `tamper_kick` land in the ADR-0015 review screen with
plain-language summaries. No PC-grid badge, no live toast. The alerting design
is deferred until the owner knows how often this actually fires — building
notification surfaces for an event with an unknown rate is how an owner learns
to ignore notifications.

### A PIN-gated owner quit stands the Guard down for the boot

After a clean owner `quit_client`, the Guard disarms for that boot and re-arms
the moment the Client next runs. ADR-0028 requires that owner escape survive
hardening, and a Guard that relaunches the app the owner just closed makes
maintenance impossible.

There is no timer. A fixed stand-down window would interrupt long maintenance at
the worst moment, and the diskless model (ADR-0005) already provides the reset:
a reboot restores the armed state unconditionally. Disarming requires the Owner
PIN, so it is not a path the customer can reach.

## Considered options

- **Prevent the kill instead** (disable Task Manager via policy). Rejected by
  ADR-0028 already; restated here so a future reader does not re-propose it.
  Note this rejects the *Task Manager* lever specifically. Preventing the
  **window** levers was not considered here at all and turned out to be both
  cheap and correct; it now lives in ADR-0028 §3 and removes a class of
  `tamper_kill` rows that should never have been raised.
- **Server-side inference from the heartbeat gap.** No new plumbing at all, but
  a heartbeat gap is indistinguishable from a network drop, a reboot, or a laggy
  PC. Guaranteed false accusations against paying customers.
- **Scheduled-task or sibling-process watchdog.** Simpler, no elevation, works in
  dev — but killable by the same standard user we are policing.
- **Punitive forfeiture of remaining time.** Strongest deterrent, rejected: it
  converts our own crash bugs into customer refund disputes.

## Consequences

- The Client installer now requires **elevation** to register a service. This is
  a change to the install experience and must be documented in
  `docs/installation.md`.
- **Dev builds have no protection**, because the service is not installed by
  `tauri dev`. This matches the existing release-Windows gating of
  `execute_os_shutdown` and the ADR-0028 keyboard hook: the pure decision logic
  (clean-exit classification, backoff schedule, attribution rules, and startup
  log cleanup) is unit-tested everywhere, while the appender's rolling-file cap,
  LocalSystem token privileges, locked and
  logged-out session transitions, interactive ownership, and pipe reconnection
  are only ever exercised on a real kiosk. The durable Guard log is the evidence
  source for those checks.
- The `tamper_kick` session-end type must be added everywhere the existing
  session-end types are enumerated, including the ADR-0014 report views that
  filter on `event_type IN (...)`.
- A customer determined enough to reach an admin account can still stop the
  service. As ADR-0005 established, the arms race ends at the power button and
  the diskless reboot; this is "good enough," not absolute, and is accepted on
  the same terms.
- Because crashes are reported as Tamper Kills, a Client stability regression
  will surface as a burst of `tamper_kill` rows across many PCs. That is an
  intended diagnostic signal, but it means the audit feed must not be read as
  proof of customer misconduct without looking at the pattern.
