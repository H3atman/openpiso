# ADR-0028: Client kiosk OS-hardening (overrides the no-hardening stance)

- Status: Accepted
- Date: 2026-06-28
- Amended: 2026-08-01 — §3 **window-level prevention** added (`skipTaskbar`,
  refused `WM_CLOSE`, foreground-scoped all-modes Alt+F4). The original text
  left the *window* levers unaddressed, and the "prevention is closed" reading
  it produced is narrowed here, not overturned: Assigned Access, Ctrl+Alt+Del,
  and registry-disabled Task Manager are all exactly as decided.
- Deciders: shop owner (product)
- Overrides: the MVP "no invasive OS-hardening / anti-cheat" stance.
- Relates to: ADR-0005 (diskless reboot reset, watchdog), ADR-0027 (untrusted
  customer lock + owner PIN gate), ADR-0029 (one reconfigured window — the
  window `skipTaskbar` and the close refusal apply to), ADR-0048 (Client Guard:
  the response to the kills this ADR still does not prevent).

## Context

ADR-0027 established that the customer is untrusted and the Fullscreen Lock must
genuinely block, but it accepted that — under the prior "no OS-hardening" rule —
a customer with OS-level access (Windows key, Alt-Tab, Ctrl+Alt+Del → Task
Manager) could still drop to the desktop underneath the Lock.

The owner decided that gap is unacceptable and explicitly approved OS-hardening
to prevent customer exploits, overriding the prior stance. (AGENTS.md lists
"invasive OS restrictions" under *Do Not Do **Without Explicit Approval***; this
ADR is that approval.)

Hard Windows constraints shaped the decision:

- **Ctrl+Alt+Del is the Secure Attention Sequence** and cannot be intercepted by
  a normal application. Mitigating it requires machine-level configuration (a
  custom Credential Provider or shell/Group-Policy lockdown), not runtime app
  code.
- True lockdown needs broad OS privileges (global keyboard hooks, registry,
  elevation) that exceed the minimal-capability Tauri policy.
- A misfiring hardener can lock out the **owner** too, so a reliable owner
  escape must survive hardening.

## Decision

Adopt **Windows-supported kiosk lockdown** as the primary mechanism, with
app-level soft hardening layered on top.

1. **Primary: Windows Assigned Access / Shell Launcher v2.** The Client is made
   the machine's shell for the customer account, so Explorer is absent and most
   escapes are closed by the OS itself and survive reboot. This is **owner
   machine provisioning** (Windows Pro/Edu/Ent), documented in
   `docs/installation.md` / `docs/hardware.md` — not runtime app code.
2. **Secondary: app-level soft hardening.** A low-level keyboard hook swallows
   the Windows key, Alt-Tab, Alt-Esc, Ctrl-Esc while the Lock is active, and the
   Lock re-asserts itself on focus loss. This catches casual escapes on machines
   not yet (or only partially) provisioned.
3. **Window-level prevention of the ordinary close levers.** Amended 2026-08-01.
   This layer did not exist when the ADR was written, and the "prevention is
   closed, the rest is response" framing it produced (restated by
   [ADR-0048](0048-client-guard-tamper-kill.md)) was too broad: on a PC that is
   *not* provisioned with Shell Launcher the taskbar is present, and its context
   menu **Close window** killed the Client for free. Three changes close the
   window-level levers without reopening anything this ADR rejected:
   - **`"skipTaskbar": true`** as a static `tauri.conf.json` flag, so the Client
     has no taskbar button and therefore no taskbar context menu, in every view
     mode. It is static rather than per-mode so the three geometry paths cannot
     drift; the debug-only `apply_dev_lock_bypass` restores the button so
     `tauri dev` still gives a normal desktop window.
   - **`WindowEvent::CloseRequested` is refused** unless a legitimate exit is
     already in flight. Every window-level kill lever — taskbar *Close window*,
     the system menu, Alt+F4, any third party politely posting `WM_CLOSE` —
     converges on this one event, so one refusal closes all of them. The
     authorization is an `EXIT_AUTHORIZED` flag raised by `initiate_exit`, the
     single chokepoint for the owner PIN quit (ADR-0027), the expiry power-off
     (ADR-0005), and the Offline Shutdown (ADR-0045). **Only the owner PIN quit
     raises it.** The two power-offs do not: the OS shutdown is forced (`/f`,
     see ADR-0022), so Windows terminates the Client without asking it to
     close, and a power-off that never actually happens — refused, aborted, or
     failed after `shutdown.exe` was spawned — would otherwise leave the window
     closable for the rest of the session, which is exactly the free-PC bypass
     this refusal exists to prevent.
     The flag is read only by the refusal decision, never by the exit itself, so
     a stuck flag can never *prevent* a legitimate exit. Enforcement is gated on
     `cfg(all(windows, not(debug_assertions)))`, matching `execute_os_shutdown`
     and the keyboard hook, so no developer is ever trapped.
   - **Alt+F4 is hooked in every view mode**, not only under the Lock — but only
     while the Client is the **foreground** window. `WH_KEYBOARD_LL` is global:
     an unscoped all-modes block would have stopped the paying customer closing
     their own browser. The foreground test is what keeps the block honest, and
     it never widens what the hook swallows. This is belt-and-braces over the
     `CloseRequested` refusal, which already covers Alt+F4.

   None of this touches Ctrl+Alt+Del, Task Manager, or the Assigned Access
   posture. A Task Manager **End task** is not a `WM_CLOSE` and is untouched
   here; that remains the Client Guard's job (ADR-0048).
4. **Optional owner-controlled sign-out policy.** The PIN-gated Client Settings
   can write the Windows `NoLogoff`, `HideFastUserSwitching`, and
   `DisableLockWorkstation` policy values. The machine-level value is applied by
   one release-Windows Rust elevation request; a declined UAC request rolls the
   user-level values back. The toggle never writes power-option policies, so
   Restart and Shut down remain available for recovery. Disabling it deletes
   exactly those three values.
5. **Owner escape must survive hardening.** The Owner PIN gate (ADR-0027) remains
   functional with lockdown fully on; the owner's maintenance path is a separate
   Windows admin account whose shell is normal Explorer (Shell Launcher v2
   per-account shells), reached without depending on the Client to "exit to
   desktop."

## Considered options

- **App-level soft hardening only.** Stops casual escapes but not Ctrl+Alt+Del /
  Task Manager. Kept as the *secondary* layer, rejected as the *whole* answer.
- **Hide the window from the taskbar per view mode** (a `set_skip_taskbar` call
  in each of the three geometry paths) instead of one static config flag.
  Rejected: three call sites are three chances to drift, and the one that
  forgets is the one the customer finds.
- **Leave `WM_CLOSE` alone and let the Guard relaunch after a taskbar close.**
  Rejected: it works, but it converts a preventable non-event into a
  `tamper_kill` row, a kicked session, and a warning screen for a customer who
  clicked a menu item Windows offered them.
- **Full custom hardening** (shell replacement + registry-disabled Task Manager +
  keyboard filter driver). Maximum lockdown, but maximum fragility, maintenance,
  and brick risk. Rejected in favour of the OS-supported Assigned Access path.
- **Keep the no-hardening stance** (ADR-0027's accepted gap). Rejected by the
  owner: a free desktop underneath the Lock is a free, unmetered PC.

## Consequences

- This is no longer pure app code: the robust protection depends on **per-machine
  provisioning** the owner performs once (and that fits the diskless master-image
  model of ADR-0005 — bake the lockdown into the image).
- The Client gains a capability and a Rust dependency for the keyboard hook;
  document the addition in the capability file per AGENTS.md.
- Ctrl+Alt+Del is mitigated only by the Assigned Access configuration, not by the
  app; without provisioning, that escape remains open. Installation docs must say
  so plainly.
- The owner can brick their own kiosk if provisioning is wrong; the separate
  admin-account shell + the Owner PIN are the documented recovery paths and must
  be tested before shipping.
- The optional sign-out policy is not a replacement for Assigned Access / Shell
  Launcher. Its registry adapter is Rust-only and release-Windows-gated; the
  webview receives only the typed PIN-gated command.
- A future reader must not water this back down to "no OS-hardening"; that stance
  was explicitly overridden here.
- **The window is now unclosable by ordinary means in an installed release
  build.** The only ways out are the Owner PIN quit, the expiry power-off, the
  Offline Shutdown, and the OS. Anyone testing a release build who expects to
  close the window with Alt+F4 will find they cannot; that is the feature.
- **A refused close is a non-event for the Client Guard.** Refusing `WM_CLOSE`
  never terminates the process, so no `ExitRequested` fires, the pipe never
  drops, and ADR-0048's classification is untouched — no `tamper_kill`, no
  `tamper_kick`, no warning screen. Prevention and response do not overlap.
- **The default check run does not compile any of this.** `kiosk_hardening`'s
  hook body, `execute_os_shutdown`'s real branch, and now
  `CLOSE_REFUSAL_ENFORCED`'s enforcing value are all
  `cfg(all(windows, not(debug_assertions)))`. A clean
  `cargo clippy --workspace --all-targets` proves nothing about them; a session
  touching these paths must also run
  `cargo clippy --release -p openpiso-client --lib -- -D warnings`. The pure
  decisions (`should_block_key`, `should_allow_close`) are unit-tested
  everywhere.
- **No system tray.** Under Shell Launcher the Windows shell (Explorer) is
  replaced, so there is no taskbar/notification area for a tray to live in. The
  plan's system-tray feature (and its delivery phase) is dropped; all owner
  actions move to the PIN-gated in-app settings panel.
