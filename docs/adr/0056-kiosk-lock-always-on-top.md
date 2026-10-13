# ADR-0056: Kiosk Lock — always-on-top by default, released by a hidden gesture plus PIN

- Status: Accepted
- Date: 2026-07-31
- Deciders: shop owner (product)
- Relates to: [ADR-0015](0015-audit-log.md) (owner actions),
  [ADR-0023](0023-serial-coin-input-and-terminal-selection.md) (Selection Window),
  [ADR-0024](0024-web-admin-sole-owner-surface.md) (owner surface),
  [ADR-0029](0029-client-window-model.md) (Client window model, the
  always-on-top prior art), [ADR-0050](0050-web-admin-pin-rotation.md) (owner
  PIN), [ADR-0055](0055-central-terminal-autostart.md) (Autostart, single
  instance)

## Context

The Central Terminal window is already `fullscreen`, `decorations: false`, and
`resizable: false`. That shapes the window but does not defend its Z-order:
anything that raises itself — an update prompt, a browser, an owner's own
Alt-Tab — draws over the customer surface. The kiosk is the shop's coin-intake
screen, so a window sitting on top of it is a revenue problem, not a cosmetic
one.

The Client already solves the same problem for the Fullscreen Lock with
`set_always_on_top(true)` (ADR-0029). The Central Terminal never got the
equivalent, because until Autostart (ADR-0055) it was always launched by hand
by someone standing at the machine.

The owner still needs a way out: the Admin host is a real PC, and reaching the
web admin, the file system, or a settings dialog means letting another window
come forward. So the pin needs a release, and the release needs to be something
a customer will not find and could not use if they did.

## Decision

### The Kiosk Lock is always-on-top, and nothing else

The **Kiosk Lock** governs exactly one window flag: always-on-top. Fullscreen,
decorations, and resizability are unchanged by it, and it does not block keys or
harden the OS. Releasing the lock lets other windows cover the kiosk; it does
not un-fullscreen it, minimize it, or close it.

Keeping the scope to one flag is deliberate. A single boolean that means one
observable thing can be signalled honestly in the header and reasoned about by
the owner; a compound "kiosk mode" cannot.

### It boots locked, always

`tauri.conf.json` declares `alwaysOnTop: true` so the window is *born* pinned —
there is no startup frame in which a stray window can land on top. The Rust
setup hook then re-asserts `set_always_on_top(true)` and constructs the
in-memory lock in its locked state.

Both halves are required. The config alone would leave Rust's state a guess;
the setup hook alone would leave a visible unpinned moment during launch.

### The state is ephemeral

The lock lives only in memory (`KioskLock` in the admin crate). There is no
`settings` row, no migration, and no Web Admin control. Every launch starts
locked regardless of how the previous run ended.

This is the whole point of "the kiosk starts always on top": a persisted unlock
would mean a terminal that reboots — including via Autostart, ADR-0055 — into
an unpinned state nobody chose. It also gives the owner a crude but reliable
recovery path, since restarting the Central Terminal always re-pins it.

### Release is a hidden gesture plus the owner PIN

Double-clicking the **Shop Name** in the kiosk header opens a PIN prompt;
the owner PIN releases the lock. The gesture stays undiscoverable — no button
role, no cursor change, no tooltip, no keyboard equivalent — but the obscurity
is not the control. The PIN is. Anyone who discovers the double-click still
cannot unpin the terminal.

The gesture is inert while a Selection Window is open (ADR-0023). A customer is
then feeding coins against a live server-side inactivity timer, and no owner
gesture may steal focus or keyboard input from that. The lock is releasable from
the grid only.

### Re-engaging needs no PIN

Double-clicking the Shop Name while unlocked re-pins the window immediately.
Tightening the kiosk is the safe direction, and requiring a credential for it
would only tempt an owner to leave the terminal unpinned.

Re-engaging deliberately does **not** clear the throttle: an attacker must not
be able to reset a lockout by toggling the lock.

### Rust owns the PIN and the flag

One Tauri command (`set_kiosk_lock`) verifies the PIN and applies the window
flag. The capability stays `core:window:default`; the mutating
`core:window:allow-set-always-on-top` permission is **not** granted, so the
webview cannot change the pin on its own. The kiosk forwards typed characters
and renders the returned state; it decides nothing.

The window flag is applied *before* the in-memory state moves, so a failed
window call can never leave the UI claiming a state the OS does not have.

PIN verification reuses the owner credential via a new non-auditing
`owner::verify_pin`, and the release is rate-limited by the same
`AttemptState` backoff as the Web Admin login — with a single counter rather
than a per-IP map, because the gesture is physically local to the terminal.
A throttled attempt is refused before the credential is exercised at all.

### Kiosk unlocks are audited as themselves

`kiosk_lock_released`, `kiosk_lock_engaged`, `kiosk_lock_release_denied`, and
`kiosk_lock_release_throttled` are their own event types. A kiosk unlock is
deliberately *not* recorded as `owner_login_succeeded`: the Web Admin login
history has to keep meaning "someone signed in to the web admin".

Every denial is audited, not only the transition into lockout (which is where
the Web Admin login draws the line). The kiosk is physically exposed to
customers, so each failed attempt at it is worth seeing.

### The signal is the header dot

The kiosk header's previously decorative dot becomes the lock signal, next to
the Shop Name it is attached to: filled and ringed when locked, hollow and
muted when unlocked, with the subtitle reading `Kiosk · Locked` or
`Kiosk · Unlocked`.

Unlocked is the *quieter* state on purpose. It is the anomaly the owner needs
to notice, but the screen faces paying customers and must not present a normal
owner action as a fault.

### The prompt self-closes

The PIN prompt dismisses itself after 30 seconds without a keystroke, so an
owner who walks away mid-unlock never leaves a credential prompt facing the
next customer. Escape and Cancel close it immediately.

## Considered options

### Bare double-click with no PIN

Rejected. It is the fastest for the owner, but the gesture is on a customer-
facing screen and security by obscurity is not security. The PIN costs one
dialog.

### A harder secret gesture instead of a PIN (triple-click, hold, hidden corner)

Rejected for the same reason, with worse ergonomics: an owner who has to
remember a fiddly gesture is an owner who will discover the release does not
work when they need it.

### Persist the lock state, or expose an owner toggle in the Web Admin

Rejected for this change. Persisting contradicts "the kiosk starts always on
top" and would let a rebooted terminal come up unpinned. A remote toggle is a
larger surface (endpoint, section, tests) for a control the owner can already
exercise while standing at the machine, which is the only place the gesture is
usable anyway.

### Release fullscreen too, or build a full "kiosk mode" flag

Rejected as scope. Alt-Tab already reaches other windows once the pin is off,
so dropping fullscreen adds nothing the owner needs. A compound mode flag would
also make the header signal ambiguous about what is actually on.

### Auto re-lock on a timer

Rejected for now. It self-heals a forgotten unlock, but it needs a definition
of "owner activity" that the kiosk cannot observe (the owner is by then working
in another window), and would re-pin the kiosk over whatever they were doing.
Restarting the Central Terminal remains the deterministic re-lock.

### Set the flag from the frontend with `core:window:allow-set-always-on-top`

Rejected. It splits authority: the PIN check would live in Rust while the flag
it guards is flippable from the webview, which makes the guard decorative.

### Reuse `owner::authenticate` for the PIN check

Rejected. It writes `owner_login_succeeded` / `owner_login_failed`, so kiosk
unlocks would silently pollute the Web Admin sign-in history. Extracting
`verify_pin` keeps one credential path with two honest audit trails.

## Consequences

- The glossary term **Kiosk Lock** is authoritative: the Central Terminal's
  always-on-top state. It is distinct from the Client's **Fullscreen Lock**
  (ADR-0029), which is a whole window format, and from the **Set Pin**, which
  is the physical coin boundary.
- The kiosk command surface grows by two (`get_kiosk_lock`, `set_kiosk_lock`),
  the first mutating commands on a webview that was otherwise read-only. Both
  are still customer-safe: one is a read, and the other is PIN-gated in Rust.
- `AppError` gains a `Window` variant. It can never originate from an HTTP
  request, but `AppError` is the one error type for both surfaces, so the
  existing exhaustive matches now name it.
- An owner who unlocks and then forgets has an unpinned kiosk until the next
  restart. That is the accepted cost of dropping the auto re-lock timer, and
  the header signal is what makes it noticeable.
- Losing the owner PIN means losing the release. Recovery is the same as the
  Web Admin's (ADR-0050): credential wipe and setup — or simply restarting the
  Central Terminal, which re-pins it anyway.
- The lock defends Z-order only. It is not OS hardening: the Admin host still
  relies on ADR-0028-style provisioning for anything stronger.
