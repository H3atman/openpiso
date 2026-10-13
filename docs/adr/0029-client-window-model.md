# ADR-0029: Client three-format window model

- Status: Accepted
- Date: 2026-06-28
- Deciders: shop owner (product)
- Relates to: ADR-0022 (expiry grace period), ADR-0027 (lock + owner PIN),
  ADR-0008/0023/0026 (central terminal owns coin entry and guest time).

## Context

The redesigned Client replaces a single 760×720 window with three logical views:
the **Fullscreen Lock**, a floating **Widget**, and a **Mini Bar**. Coins and
guest time come from the **Central Terminal** (ADR-0008/0023/0026); the per-PC
Client is where **accounts** log in. Two independent things can give a PC usable
time — guest time (terminal) and an account session (client login) — and the
views must route correctly between them.

## Decision

### 1. One window, reconfigured — not three OS windows

A single `main` window's size / decorations / always-on-top / position are
switched by Rust commands (`enter_fullscreen_mode`, `enter_widget_mode`,
`enter_mini_mode`). Rationale: the three views are mutually exclusive, so this
keeps capabilities minimal (one window label), avoids cross-window state sync,
and avoids multi-window IPC. Revisit only if the Mini Bar must ever be visible
*simultaneously* with the Widget.

### 2. Lock mechanism: borderless-maximized, re-asserting

The Lock is **borderless-maximized + undecorated + always-on-top**, not true
`fullscreen: true` — toggling true fullscreen on/off for the frequent Lock↔Widget
size changes is glitch-prone on Windows, while resizing an undecorated
always-on-top window is smooth and visually identical. The Lock **re-asserts on
focus loss** as a best-effort barrier; genuine confinement is delegated to kiosk
OS-hardening (ADR-0028).

### 3. Auth lives on the Lock; usable time auto-enters the Widget

The login / sign-up box lives **only** on the Lock. Guest auto-entry means a
guest with `remaining_seconds > 0` auto-enters the Widget when a heartbeat
raises the remaining time — no "Continue as guest" click is needed. An account
session likewise auto-enters the Widget. The Widget then renders in a reduced
guest flavor (Time + Owner-PIN-gated Settings, with persistent Minimize and
"Log in") or an account flavor (full self-service). Guest Settings uses the
same inline gate and relock rules as Account Settings; no Account-only customer
controls are exposed. A reload while usable time remains resumes the Widget for
both guest and account sessions, with transient tab selection reset to Time.

A Guest's explicit **Log in** action is the sole exception to usable Guest Time
auto-entry. It sets a narrow, frontend-only authentication-route intent that
selects the Fullscreen Lock without changing or persisting View Mode. The Lock
offers **Return to Guest session**, which clears the intent and restores the
active Widget with Guest Time intact. Successful authentication also clears it
and follows normal Account view selection. Expiry, shutdown, owner lock, logout,
and other authoritative Lock conditions clear or override it. This transient
routing intent is not the removed guest-entered flag: it records an explicit
destination request, never whether a Guest may use paid time.

### 4. View is authority-derived with one transient route intent

The view is computed from authoritative timer and Account Session state, plus
the explicit frontend-only authentication-route exception in §3. The
`selectClientView` pure selector replaces the previous inline derivation; no
guest-entered flag exists:

- **Guests auto-enter** when `remaining_seconds > 0` — the same condition that
  drives the Widget/Mini for accounts. No "entered" flag exists; a reload with
  usable time resumes the Widget.
- **Accounts auto-enter** from `account_auth`, which persists in `SharedState`,
  so a reload resumes the Widget.
- **View Mode** (Widget vs Mini Bar) is persisted per Client in `ClientConfig`,
  default Widget.

### 5. Return-to-Lock triggers (authoritative)

From Widget/Mini, return to the Lock immediately when any of:

1. `remaining_seconds` reaches 0 / `expired`;
2. `power_state === "shutdown"` — Lock in shutdown-grace mode, **unchanged from
   ADR-0022**, overrides everything;
3. `overlay_locked === true`.

Account **logout** clears any authentication-route intent and then follows the
normal Guest rule: usable Guest Time selects the persisted View Mode; otherwise
the Lock remains.

Connection loss formerly kept the Widget/Mini Bar alive while local time
remained. That client-side stance is superseded by [ADR-0045](0045-offline-shutdown-countdown.md): a previously-connected Client immediately
returns to the Lock and starts the Offline Shutdown Countdown. Setup,
connecting, unregistered, and never-connected states retain their existing
surfaces.

## Considered options

- **Three separate OS windows.** Rejected: more capabilities, cross-window sync,
  multi-window IPC, for mutually-exclusive views.
- **True `fullscreen: true` for the Lock.** Rejected: worse for frequent size
  transitions and re-assertion.
- **View as a pure function of timer state.** Rejected in the original ADR. The
  amended selector also consumes Account Session state and the narrow transient
  authentication-route intent; usable time alone remains sufficient for normal
  Guest auto-entry. The guest-entered flag was removed.
- **Persist the guest "entered" flag in Rust.** Removed entirely (guest auto-entry
  replaced the need for the flag).
- **Login box inside the Widget too.** Rejected: duplicates auth UI and muddies
  "the Lock owns auth"; a one-way "Log in" shortcut back to the Lock suffices.

## Consequences

- The frontend becomes a thin view-router over three Svelte views; the Lock
  generalizes the old expired-overlay.
- New Rust window commands and capabilities are needed (size/position/fullscreen/
  always-on-top/decorations/show/hide/center/start-dragging); document each in the
  capability file per AGENTS.md.
- The Mini Bar's top-center position must be computed from the *current monitor's*
  work area, not a hard-coded pixel.
- The shutdown-grace path (ADR-0022) is preserved exactly. Offline Shutdown
  Countdown is a separate client-side Lock condition defined by ADR-0045.

## Amendment (2026-07-25, issue #117): Offline Shutdown Countdown

[ADR-0045](0045-offline-shutdown-countdown.md) supersedes the connection-loss
rule in §5. The Client now locks and counts down after a previously-connected
Client loses the admin server; a successful reconnect restores normal routing.
The absolute deadline is persisted for relaunch, expired-on-launch gets a
10-second re-warning, and owner-unlocked settings pause the countdown until the
panel closes. See ADR-0045 for the complete persistence and pause behavior.

## Amendment (2026-07-03, bug #12): Widget geometry 320×480

The Widget was originally 200×300. The dialogs then present (owner settings,
transfer time/points, change password) rendered as `WidgetOverlay` cards inside
that window and were measurably unusable there: cards needed 240–294 px of
width and up to ~590 px of height, so inputs and buttons were clipped outside
the window with no scroll path. Owner settings later moved to an inline,
PIN-gated fourth Account tab (#75); transactional dialogs retain the overlay
geometry rules below.

Decision (owner preference: bigger widget, A+C hybrid from the bug-#12
triage):

- **Widget is 320×480** (`WIDGET_WIDTH`/`WIDGET_HEIGHT` in `window_mode.rs`,
  mirrored by the `h-[480px] w-[320px]` classes in `Widget.svelte`; drift is
  pinned by `apps/client/scripts/widget-geometry.test.ts`).
- **`WidgetOverlay` is a scroll container** (`overflow-y-auto`, `p-4`,
  `place-items-center-safe`): any transactional dialog taller than the window
  scrolls instead of clipping.
  Plain `place-items-center` is forbidden there: it pushes an overflowing
  card's top above the scroll origin, unreachable.

The behavioral checks are the headless-Chrome Widget tab and dialog harnesses.
The Settings tab is shared by Account and Guest flavors and uses the Widget
panel's vertical scroll region; transactional cards use `WidgetOverlay`
scrolling.

## Amendment (2026-08-03): Widget is sized in logical pixels

The 320×480 geometry above was applied with `PhysicalSize`, so on a DPI-scaled
display the CSS viewport was *smaller* than 320×480 — 256×384 at 125% (the
Windows default on most current laptop panels) and 213×320 at 150%. No fixed
320×480 layout can fit that, so the tab panel scrolled on the majority of real
client hardware regardless of how its contents were arranged.

Decision:

- **`apply_widget` sizes the Widget with `LogicalSize`.** The CSS viewport is
  now a true 320×480 on every display; the window is correspondingly larger in
  physical pixels at 125%/150%, which is the intended behavior for a
  fixed-layout UI. Pinned by `apps/client/scripts/widget-geometry.test.ts`.
- **The Mini Bar (`MINI_WIDTH`/`MINI_HEIGHT`) is knowingly left on
  `PhysicalSize`.** It has the same defect (80×24 CSS at 125%) but no headless
  harness, so the change could not be verified in the same pass. Known issue,
  not a decision that physical sizing is correct there.

## Amendment (2026-08-03): the Time tab fits without page scroll

The Time tab overflowed the panel in both flavors — the account flavor by
~25 px in its plainest state and ~126 px with the Claim button, last-call
banner and coin-feedback line present; the guest flavor by ~92 px. The fixed
`h-[200px]` timer frame consumed most of a ~260 px worst-case panel.

Decision — the Time tab renders as one page in **every** state (claim
available, last call active, coin feedback showing, Coin Promo expanded), in
both the account and guest flavors:

- **The timer frame flexes** (`flex-1 min-h-0`); every sibling is `shrink-0`.
  The timer absorbs slack instead of reserving a fixed 200 px, so no content
  was removed to make room.
- **The timer has a floor**: `text-6xl` normally, stepping down to `text-3xl`
  while the Coin Promo ladder is expanded and restoring on collapse. Yielding
  space is a direct, reversible response to the customer's own tap.
- **The panel keeps `overflow-y-auto`.** The guarantee is enforced by test,
  not by `overflow-hidden`: a future miss must degrade to a scrollbar, never
  to a clipped, unreachable control.
- The guarantee is asserted in `widget-tabs-browser.ts` against a worst-case
  harness state (`scrollHeight <= clientHeight`, ladder open and closed, both
  flavors). The former `[data-time-frame]` height === 200 assertion is
  replaced by a floor assertion.

## Amendment (2026-08-09, revised 2026-08-28): native startup placeholder

Client PCs are **diskless**: every boot reads the WebView2 runtime over the
network from the image server, and the writeback layer is discarded on
reboot, so every production start is a genuine cold start. Startup logs from
a cold boot show ~24.5 s between the `app_built` and `setup_entered`
milestones — the webview creation inside `app.run()`, before any of our
setup code runs. Our own work is not the cost: `setup_entered` →
`setup_finished` is ~31 ms and the frontend mounts ~350 ms later. Warm
restarts reach `frontend_ready` in under 1.2 s.

With `"visible": true` the customer spent that entire cold load looking at a
blank always-on-top fullscreen rectangle covering the screen. Hiding the
window removed the blank rectangle but exposed the Windows desktop until
WebView2 finished. A customer could use that interval to generate more load or
attack Client startup.

Decision — the `main` window is created with **`"visible": false`** and is
covered by a release-Windows native Win32 placeholder created before Tauri
enters WebView2 startup. The placeholder is an opaque, topmost popup across the
virtual desktop. It draws only `OpenPiso` and `STARTING` with GDI, ignores
ordinary `WM_CLOSE`, and owns no money, time, session, configuration, or
network authority. Debug and non-Windows builds do not create it.

After Svelte mounts, it calls `prepare_startup_handoff`. Rust reveals and
keeps Tauri outside the topmost band behind the native placeholder. The
now-visible webview completes two animation frames and calls `frontend_ready`.
Rust moves the painted Tauri window into the topmost band, focuses it, posts a
private close message to the placeholder thread, and joins it. This two-phase
overlap prevents both a paused hidden-window animation callback and a desktop
flash. Before creating the process's first HWND, the placeholder establishes
the same Per-Monitor-V2 DPI-awareness context used by Tao so mixed-DPI virtual
desktop geometry remains correct. The reveal remains one-shot, so the `show()`
in each `apply_*` geometry call is unaffected.

If the frontend has not called in within 10 seconds of `setup`, a
release-Windows build keeps the native placeholder visible and logs a warning.
Builds without the placeholder retain the former blank-window fallback.

This does not make the cold load faster or ensure Windows starts the Client.
It closes the visible desktop interval only after the process exists. The
remedies for WebView2 load time remain image-side (a pre-warmed `EBWebView`
profile baked into the boot image, pinning the WebView2 runtime in the boot
server's cache, a fixed-version runtime, staggering client autostart against
the boot storm).
