# Development

Install and check all workspaces from the repository root:

```bash
bun install
bun run build:web-admin
bun run check
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bun run check:windows   # on Linux/macOS; skip on Windows
```

Client real-command tests (`cargo test -p openpiso-client --lib real_command_tests`)
build a windowless real Tauri app and run on Windows and Linux. Linux needs a
working X11/Wayland display (or `xvfb-run`); no display is a failure, not
skipped coverage. macOS still compiles the workspace, but these scenarios are
not built there (Wry needs the main thread), so real-command verification is
unavailable on macOS.

## Windows compile check from Linux

Most Client and Client Guard code is `cfg(windows)`, so a Linux `cargo clippy`
never compiles it. `bun run check:windows` runs clippy against the real
`x86_64-pc-windows-msvc` target through
[cargo-xwin](https://github.com/rust-cross/cargo-xwin): the whole workspace
with `--all-targets`, then `openpiso-client --lib --release` for the
release-Windows-gated kiosk hardening. Run it with every test run.

One-time setup (LLVM's `clang`, `lld-link` and `llvm-lib` must be on `PATH`;
the first run downloads the Windows SDK and CRT into
`~/.cache/cargo-xwin`, accepting Microsoft's license):

```bash
rustup target add x86_64-pc-windows-msvc
cargo install --locked cargo-xwin
```

The script creates empty stand-ins for missing Tauri build inputs
(`apps/*/dist`, `target/release/client-guard.exe`) and removes them afterwards.

It compiles only; it runs no tests. Windows runtime behaviour — the service,
named pipes, SCM notices, relaunch — still needs `cargo test --workspace` on
Windows and the installed-kiosk checks in [installation](installation.md).

## Focused Client startup & handoff regression (Windows)

The local Tamper Kill handoff (ADR-0079) and the startup arbitration gate
(ADR-0080) share one process-level regression harness at
`apps/client/src-tauri/tests/handoff_harness.rs`. It launches real
child processes of the production Client binary — sender-only submissions,
ordinary receivers, and competing full launches through the production
entrypoint ordering — against one isolated config directory and one
isolated test Admin per scenario, and observes persistence, recorded
Heartbeat requests, the gate's own evidence lines, and exit codes. It runs
as part of `cargo test --workspace`; to run it alone:

```bash
cargo test -p openpiso-client --test handoff_harness
```

Outcome on the reference Windows dev machine (2026-09-09): 17 scenarios
passed in ~2.5 minutes — the seven handoff scenarios (sender-only
validation/durability/inertness, live-receiver receipt with
server-acknowledged clearing, duplicate delivery without attempt-count
inflation and inert retry after acknowledgement, distinct-report
coalescing, Admin-outage retention and retry, receiver death with
successor recovery, and barrier-released concurrent senders that cannot
take the inbox past its documented capacity) plus the startup-arbitration
scenarios: the inert pre-fix capture (two admitted launches without the
gate — a debug-build scenario), one admitted runtime out of two
barrier-synchronized launches, a report-carrying loser that hands off
durably before its handled-success exit with successor recovery after
owner death, denied and incompatible coordination objects failing
truthfully, 100 synchronized stress rounds with two to four contenders
(barrier and staggered rounds admit exactly one runtime per round with
every other contender a handled duplicate; owner-death injection rounds
wait for a contender to actually reach admission, kill it while the others
are still deciding, and assert directly that no two admitted lifetimes
overlap — each admitted process records its admission instant and the
kernel supplies its exit instant — so legitimate sequential succession
still passes), the overlap-detection demonstration for that assertion and
its sequential-succession counterpart, full-runtime simultaneous launches
with handoff and inert plain duplicates, and full-runtime crash recovery.
Every gate was verified failing-then-passing at the seam: with the runtime
ingest loop removed the handed-off report never reaches a heartbeat and the
live-receiver scenario times out red; with the arbitration gate inert the
pre-fix capture scenario shows two admitted competing launches and the
owner-death overlap check reports the overlap; with the inbox admission
lock removed all sixteen concurrent senders are accepted past the cap. Each
full-runtime receiver spawns a WebView2 runtime, so the scenarios serialize
themselves internally and write receiver stdout mirrors to
`target/tmp/receiver-*.log` for post-mortem.

The harness activates the production gate in the debug test build through
the test-only `OPENPISO_CLIENT_ARBITRATION_ENFORCE=1` override and isolates
its native coordination objects through
`OPENPISO_CLIENT_ARBITRATION_NAME`; production never sets either variable,
and debug/non-Windows development stays multi-instance. A release Windows
run needs no override — the release child enforces natively — and compiles
the debug-only scenarios (the inert pre-fix capture and the overlap
demonstration) out, so it runs 15 scenarios.

`bun run build:web-admin` produces the `apps/web-admin/dist` static
assets the admin Rust process serves at `:8080/admin`. The dev-mode
admin spawns it from the on-disk `dist/` directory; the release build
falls back to the bundled Tauri resource path.

Telemetry is compiled in only when the builder supplies their own
`OPENPISO_POSTHOG_TOKEN`; plain builds send nothing. Telemetry (ADR-0083) reads `OPENPISO_POSTHOG_TOKEN` and
`OPENPISO_POSTHOG_HOST` at Rust compile time; the host defaults to
`https://us.i.posthog.com`. Any build compiles the token in when it is set in
the environment or in the gitignored root `.env.local` (Bun loads it for
`bun run`), including `bun run build:installers` and `bun run dev:admin`;
`build:installers` prints whether Telemetry is on. Set the host only if
overriding the default. A token-bearing build sends the Daily Summary
automatically with no owner opt-out (ADR-0083).
Without a token the build is inert: no outbound Telemetry requests. Never
commit the token or put it in source files; a runtime environment change
cannot enable an already-built binary.

Build both Windows NSIS installers and collect them into separate folders:

```bash
bun run build:installers
```

On Windows, the script uses the native MSVC Cargo toolchain. On Linux and macOS,
it cross-compiles both apps and the Client Guard for
`x86_64-pc-windows-msvc` through `cargo-xwin`; install the Windows Rust target,
`cargo-xwin`, LLVM (`clang`, `lld-link`, and `llvm-lib`), and NSIS first. The
cross-build artifacts remain under
`target/x86_64-pc-windows-msvc/release/bundle/nsis`; the script collects the
final installers into:

- `dist/installers/admin-web-admin/` — Central Terminal installer with web-admin bundled.
- `dist/installers/client/` — Client installer.

The local offline build explicitly overrides Admin's
`bundle.createUpdaterArtifacts` to `false`, so sideload installers need no signing
key. It does not change the shipped verification key or weaken App Update's
signature checks. Do not use that override for CI releases.

### Signed Admin updater builds (#422 / #425)

Admin's `apps/admin/src-tauri/tauri.conf.json` enables only the `nsis` bundle
with `createUpdaterArtifacts: true`; use the normal Admin Tauri build for CI.
For `x86_64-pc-windows-msvc`, the output under `bundle/nsis/` is
`OpenPiso Central Terminal_<version>_x64-setup.exe` and its `.exe.sig` (Tauri v2
artifacts, not a v1-compatible zip). #425 publishes these with a per-release
`latest.json` whose `windows-x86_64` entry contains that EXE URL and signature,
and `release.json`. The Rust Installer selects that release's own `latest.json`,
not a moving latest-release endpoint. No static updater endpoints or frontend
updater permissions are configured.

**TODO (#425):** before the first signed release, the maintainer must replace
`plugins.updater.pubkey` in the Admin config with the generated minisign public
key content. `TODO_REPLACE_WITH_MINISIGN_PUBLIC_KEY_SEE_DOCS_RELEASING` is an
intentionally invalid placeholder, so verification fails closed. Keep the private
key out of the repository; CI signing uses `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Follow the keypair, offline backup, secrets,
and real Windows update/rollback/relaunch checklist in `docs/releasing.md`
(added by #425). Authenticode is not part of this setup.

## Release command

From a clean `main` checkout, set `$EDITOR` to a command that waits until the
notes review finishes (for example, `code --wait`), then run:

```bash
bun run version:patch       # or version:minor / version:major
bun run version:patch --dry-run
bun test scripts/bump-version.test.ts
```

Before changing any file (dry runs included), the command fetches `origin/main`,
fast-forwards `main` when behind, refuses local commits ahead of it, an existing
target tag, or mismatched workspace versions, then uses `gh` to confirm the
release secrets exist and refuses a missing or placeholder Admin updater public
key. It bumps all package, Tauri, and Cargo versions together and refreshes both
lockfiles. It drafts English owner-facing notes with `codex exec` in a read-only
sandbox; unavailable, failed, empty, or malformed AI output falls back to
conventional commits grouped under New / Fixed / Changed (at most eight bullets).
The log starts at the latest reachable stable `vX.Y.Z` tag, or `988425a` (the
0.11.5 bump) for the first release.

Review and edit `docs/releases/vX.Y.Z.md` in `$EDITOR`. The command commits the
bump and reviewed notes, creates `vX.Y.Z`, then asks before atomically pushing
`main` and that tag to `origin`. `--yes` skips only the push confirmation, never
the editor review. Declining leaves a local commit and tag and prints the exact
push command. Non-interactive runs without `--yes` never push. The dry run checks
preconditions and prints the next version without writing release files or invoking
Codex, the editor, lockfile updates, or a push.

If a lockfile update, editor, commit, or tag command fails, the release stops;
inspect the working tree and any local commit/tag before retrying. The script
does not undo edits or delete tags automatically. CI consumes the committed
notes at `docs/releases/vX.Y.Z.md` as the release body (ADR-0086).

## Run order

The admin Tauri app owns SQLite, the timers, and both HTTP servers
(`:3210` for the client/device API and SSE event stream, `:8080` for
the owner web admin). Start it first, then start each client Tauri
app on its own PC:

```bash
bun run dev:admin
bun run dev:client
bun run dev:web-admin   # optional, only for working on the owner web app
```

For faster admin startup when an existing `apps/web-admin/dist` build is
sufficient, skip its production rebuild:

```bash
bun run dev:admin:fast
```

Use `bun run dev:web-admin` alongside the fast command when editing the owner
web app; Vite serves it with HMR and proxies API requests to the admin server.
Use the regular `dev:admin` command when `:8080/admin` needs freshly built
static assets.

For phone development, Vite listens on all interfaces by default. Open the
LAN address printed by Vite at `/admin/` on the phone. The dev server proxies
owner routes, so use it only on a trusted admin/shop LAN and never
port-forward its port. If the host also connects to customer Wi-Fi, set
`OPENPISO_WEB_ADMIN_DEV_HOST` to the wired/admin interface address before
running `bun run dev:web-admin`.

On Windows with the MSVC Rust toolchain, do not set `CC` to an MSYS or
MinGW GCC executable. Unset it before compiling bundled SQLite:

```powershell
Remove-Item Env:CC
```

## What's where

| Path                         | Role                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `apps/admin/src-tauri/src/`  | Rust process — Tauri kiosk commands, `:3210` server, `:8080` web admin, DB, coin pipeline, timers, terminal selection, audit, accounts, reports. The persistence layer (`src/db/`) sits behind the `db::...` facade; [`db/mod.rs`](../apps/admin/src-tauri/src/db/mod.rs) is the authoritative routing map. Tests sit beside the module they exercise. |
| `apps/admin/src/`            | Svelte Central Terminal kiosk (fullscreen, customer-facing). M6 retired the owner dashboard that used to live here. The window is always-on-top at every launch (**Kiosk Lock**, ADR-0056); the owner releases it by double-clicking the Shop Name and entering the owner PIN, and the header dot is the on/off signal. Lock state is ephemeral and lives in `src-tauri/src/kiosk_lock.rs`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `apps/web-admin/src/`        | Svelte owner web app (Vite + Svelte + shadcn-svelte). The sole owner surface (ADR-0024).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `apps/client/src-tauri/src/` | Rust client app — heartbeats, SSE, countdown, expired overlay, window-mode geometry (`window_mode.rs`), owner-PIN verify + account self-service commands, per-boot MAC collection (`mac_address.rs`), JSON-on-disk `ClientConfig` (`config_store.rs`).                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `apps/client/src/`           | Svelte client UI — `App.svelte` router + `views/` (Fullscreen Lock, Widget, Mini Bar) + shared `lib/components/` (shadcn-svelte).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `crates/openpiso-core/`      | Shared Rust crate — pricing, points, sessions, audit types, wheel, reports.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `docs/`                      | Architecture, API, database, hardware, installation, development, glossary, ADRs, milestone tracker.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |

M6 retired the desktop owner console (ADR-0024). The Tauri surface is the
`generate_handler!` list in [`lib.rs`](../apps/admin/src-tauri/src/lib.rs):
only the kiosk commands defined in
[`commands.rs`](../apps/admin/src-tauri/src/commands.rs). The retired owner
commands were deleted from that module, so re-registering one fails to
compile. No test pins the registered set.

## Brand assets

Open Frame v3 logo. Greens `#123c1c` / `#71da88`. Each Vite app serves
`public/favicon.svg` + `favicon.ico`. Web Admin ships the symbol and horizontal
lockup (light and reversed) in `apps/web-admin/src/lib/assets/logo/`, swapped
by `brand-logo.svelte` on the `dark` class. The Client boot splash uses
`apps/client/public/openpiso-horizontal-reversed.svg`. The symbol also sits
beside the Shop Name in the Admin kiosk header, the Client Fullscreen Lock and
Widget headers (`apps/{admin,client}/src/lib/assets/logo/`; Client uses
`BrandMark.svelte`), and in the Web Admin top bar below `md` only. Wordmarks are Noto Sans
Bold outlined to paths (SIL OFL 1.1); `OFL.txt` and `NOTICE.txt` sit beside
each copied lockup. To change the logo, replace these files from the kit; the
Tauri bundle icons live in each `src-tauri/icons/`.

## Client window model & owner gate

The Client is one Tauri window reconfigured into three views (ADR-0029); see
[architecture.md](architecture.md#client-window-model-and-owner-gate) for the
model. To exercise it in development:

- **View transitions.** Start with no time → the **Fullscreen Lock** is shown.
  Add time (insert a coin from the Central Terminal / web-admin simulator) and
  the Guest **Widget** mounts automatically; Account login remains on the Lock.
  **Minimize** →
  the **Mini Bar** (not OS minimization; top-center of the current monitor);
  **expand** returns to
  the Widget. The choice persists in `ClientConfig.view_mode`. Drain time to 0
  or trigger shutdown → back to the Lock. The router that decides the view is
  `apps/client/src/App.svelte`; the geometry is `window_mode.rs`.
- **Lock blocking.** The Lock has no customer Quit and **re-asserts on focus
  loss** — Alt-Tab away from it and it re-raises/refocuses. `enter_fullscreen_mode`
  is idempotent, so the re-assertion does not flicker.
- **Owner PIN gate.** Set the PIN from web-admin (Settings → owner PIN). On the
  Lock, **⚙ Settings** opens the `OwnerPinGate`; the server URL / PC ID form only
  appears after the PIN is verified server-side (`POST /api/owner/verify-pin`).
  A wrong PIN clears the field; the unlock resets every time the dialog closes.
  The unlocked panel also exposes the **default View Mode** and a PIN-gated
  **Quit** (there is no customer-facing Quit). **Offline fallback:** after one
  successful online verify the client caches the strong hash in memory, so a
  later admin-server outage still lets the owner unlock against the cached hash
  (ADR-0027 §3); repointing the client at another server drops the cache.
- **Kiosk keyboard hook.** A release **Windows** build installs a low-level
  keyboard hook (`kiosk_hardening.rs`) that swallows Win / Alt-Tab / Alt-Esc /
  Ctrl-Esc **only while the Lock is active** (the frontend flips it via
  `set_kiosk_lock_active` on each view change). **Alt+F4 is the exception**: it
  is blocked in *every* view mode, but only while the Client is the foreground
  window — `WH_KEYBOARD_LL` is global, so an unscoped block would stop the
  paying customer closing their own browser. Debug / non-Windows builds skip
  the install, so `tauri dev` is never trapped; the block decision is
  unit-tested (`should_block_key`). Ctrl+Alt+Del is never blockable by app code
  (ADR-0028) — only Assigned Access provisioning mitigates it.
- **Window close refusal.** In a release **Windows** build the Client refuses
  `WindowEvent::CloseRequested` unless `initiate_exit` has raised
  `EXIT_AUTHORIZED` (owner PIN quit, expiry power-off, Offline Shutdown), so the
  taskbar context menu, the system menu, and Alt+F4 all fail closed; the window
  also carries a static `"skipTaskbar": true` so there is no taskbar button to
  right-click. `tauri dev` never enforces either (`CLOSE_REFUSAL_ENFORCED` is
  `cfg(all(windows, not(debug_assertions)))`, and `apply_dev_lock_bypass`
  restores the taskbar button). The decision is unit-tested
  (`should_allow_close`). A refused close never terminates the process, so it is
  invisible to the Client Guard — no `tamper_kill`, no kick (ADR-0028 §3).
- **These paths do not compile in the default check run.** `kiosk_hardening`'s
  hook body, `execute_os_shutdown`'s real branch, and the enforcing value of
  `CLOSE_REFUSAL_ENFORCED` are all release-Windows-gated. After touching them,
  also run `cargo clippy --release -p openpiso-client --lib -- -D warnings`.

## Diagnostic logging

Both the admin server and client Tauri apps use the `tracing` crate
for structured developer/diagnostic logging. These logs are
separate from the business `audit_log` table and are not retained
forever.

### Admin server

- **Format**: JSON lines written to daily rotating files.
- **Location**: `<app_data_dir>/logs/diagnostics.YYYY-MM-DD.log`
- **Retention**: ~7 days (files older than 7 days are cleaned on
  startup).
- **Default level**: `INFO` (shows informational, warning, and
  error messages).
- **Override**: set the `RUST_LOG` environment variable to adjust
  the log level:

  ```bash
  RUST_LOG=debug bun run dev:admin
  RUST_LOG=trace bun run dev:admin
  ```

### Run Records

Admin keeps one **Run Record** per process lifetime: app version, PID, start
time, and — once the process exits through its own shutdown path — an end time
and an **Exit Reason** (`window_closed`, `application_exit`, or `panic`). The
records live in `<app_data_dir>/run_records.json` as a rolling set of the
newest ten, deliberately outside `logs/`, so the retention sweep never removes
them (ADR-0082). At startup, a newest record with a start and no end is
classified and logged as a **suspected hard exit** — a plainly stated absence
of evidence, never a claim about the cause. The file holds no money, time,
session, or Points data; a missing or corrupt file yields an empty history
rather than a failed startup.

The running process identifies its own record in memory by PID *and* the exact
start timestamp it wrote, never by PID alone: Windows reuses PIDs, so an older
unfinished record can carry the number the live process now has. Only that
exact record is completed at shutdown or reported as still running; every other
unfinished record stays a suspected hard exit. When the startup write fails,
the run owns no record, and shutdown completes nothing rather than rewriting
history.

### Crash evidence

A panic used to leave nothing behind: the release binary is built with
`windows_subsystem = "windows"`, so the default panic hook's stderr output goes
nowhere and the only symptom is Windows' "has stopped working" dialog. Both
apps install a panic hook that appends the payload, thread, source location,
and backtrace to a `panic.log` file synchronously, then re-emits it through
`tracing`: Admin's at `<app_data_dir>/logs/panic.log`, the Client's at
`%APPDATA%\org.openpiso.client\logs\panic.log` (ADR-0082; the Client hook
records evidence only — it has no Run Records to complete). `panic.log` is not
rotated — it should stay empty.

Two scripts support field diagnosis of a crash that only reproduces on a shop
PC:

- `scripts/windows/collect-crash-evidence.ps1` — run elevated. Arms WER
  LocalDumps for `openpiso-admin.exe` so the next crash leaves a full dump in
  `%LOCALAPPDATA%\CrashDumps`, and zips the existing WER events, system events,
  diagnostics logs, and environment details to the Desktop. Re-run it after a
  crash to collect the dump.
- `scripts/windows/sample-terminal-health.ps1` — leave running. Appends a CSV
  row per minute of working set, private bytes, handles, and thread count for
  the Central Terminal and its WebView2 children, so a leak-shaped crash is
  visible as a ramp before the gap.

- **Bench coin filter**: raise only the coin targets when investigating the
  acceptor, for example:

  ```bash
  RUST_LOG=info,openpiso_admin_lib::coin_source=debug,openpiso_admin_lib::terminal_selection=info,openpiso_admin_lib::coins=info bun run dev:admin
  ```

  A blanket `RUST_LOG=debug` is deliberately discouraged: SQL statement
  logging can bury the coin path under unrelated database noise. Raw pin
  samples are DEBUG; edge, batch, port, Set Pin, and commit diagnostics stay
  visible at their normal structured levels.

- **Console**: human-readable output mirrored to stdout at the
  same level.

### Client

- **Format**: JSON lines in daily rotating files, mirrored as human-readable
  stdout for development.
- **Windows location**:
  `%APPDATA%\org.openpiso.client\logs\client.YYYY-MM-DD.log`.
- **Retention**: current day plus about seven prior days. On a diskless Client,
  local files still evaporate on reboot.
- **Default level**: `INFO`.
- **Override**: set the `RUST_LOG` environment variable.
- **Forwarding**: WARN and ERROR lines are buffered on the device and sent
  to the admin server's batched `POST /api/forwarded-diagnostics` endpoint
  (ADR-0078), where they land in the dedicated forwarded log. Forwarding is
  release-only; set `OPENPISO_FORWARD_DIAGNOSTICS=1` to enable it in a
  development build. Routine heartbeat and reconnect noise stays local and
  is not forwarded.

### Expiry shutdown in development

The client performs a real OS power-off at expiry only in **release
builds** (the diskless image). **Debug builds** — including
`bun run dev:client` — never shut the machine down: they log the
intent (`OS shutdown suppressed …`) and continue. Non-Windows builds
also only log. This lets you exercise the warning banner and grace
overlay against a dev admin server without powering off your
workstation. See [diskless.md](diskless.md) for the production
behaviour.

### Support Bundle

Download diagnostic evidence from Web Admin → Settings → Support. The Audit
page remains the business audit surface. ADR-0078 supersedes the proposed
interleaved developer-log viewer, which was never built.

## Browser test harnesses

Client browser regression harnesses render real production components against
a headless Chrome instance via the Chrome DevTools Protocol (CDP).

### Prerequisite

- **Google Chrome** (or Chromium) installed. Browser harnesses search
  common install paths and `$CHROME_PATH`.

### Owner Session harness

Drives the real Web Admin against a running Admin over headless Chromium (no
mocks): setup or login, mutation with CSRF/Origin, reload/bootstrap, human
versus polling/synthetic activity, logout. Idle and absolute expiry are covered
by the Rust route tests, not here.

Prerequisites:

- Chrome or Chromium: `$CHROME_PATH`, a cached Playwright Chromium
  (`~/.cache/ms-playwright`), or a common install path.
- A running **isolated** Admin serving `:8080` with a throwaway data dir (for
  example `XDG_DATA_HOME=$(mktemp -d)`). The harness sets up or logs in the
  Owner and **mutates the shop name**; never point it at a real shop.
- The Web Admin served from that Admin (built assets) or the Vite dev server,
  reachable at `OPENPISO_OWNER_URL`.

```bash
OPENPISO_OWNER_URL=http://127.0.0.1:8080/admin bun run apps/web-admin/scripts/owner-session-browser.ts
```

Use the exact `/admin` entrypoint and avoid the trailing-slash `/admin/`,
which currently returns 404 (existing quirk).

Env: `OPENPISO_OWNER_URL` (default `http://127.0.0.1:8080/admin`),
`OPENPISO_OWNER_PIN` (optional; random PIN for a fresh setup), `OPENPISO_E2E_OUT`
(artifact dir), `CHROME_PATH`, `OPENPISO_E2E_ACTIVITY_TIMEOUT_MS`. Output is a
`owner-session-browser.json`, scanned for the PIN and tokens before write, plus
screenshots of the post-login UI (not scanned). No package script is defined.

#### App Update fixture scenario (ADR-0086)

The App Update launcher extends the same Owner Session harness: row and hover
summary, full panel/Markdown notes, Check now, persisted automatic-check toggle,
schema acknowledgement, exact typed Compatible Rollback confirmation, rollback
Schema Version filtering, and a simulated successful restart toast/acknowledgement.

Run on **Linux with a display** (or `xvfb-run`) and free ports `8080` and `3210`.
It refuses occupied ports, starts only its own Admin, and uses a fresh
`XDG_DATA_HOME`. Build a **debug** binary: release builds ignore the feed override.

```bash
bun install --frozen-lockfile
bun run build:web-admin
bun --filter @openpiso/admin build:frontend
cargo build -p openpiso-admin
bun run apps/web-admin/scripts/app-update-browser.ts
```

`OPENPISO_ADMIN_BINARY` optionally selects the debug Admin; otherwise it uses
`${CARGO_TARGET_DIR:-target}/debug/openpiso-admin`. `OPENPISO_E2E_OUT` selects
artifacts (default `/tmp/openpiso-app-update-<timestamp>`); Chrome/activity timeout
options above still apply. The launcher ignores Owner URL/PIN overrides, uses a
random PIN, and retains its throwaway DB path in `app-update-fixture.json`.

The local server supplies three stable releases: a newer schema-raising release,
a same-schema older release, and a lower-schema older release. The real Admin
first migrates its fresh DB; while stopped, the launcher seeds its restart marker,
then starts it again to resolve the result normally. No production test hook is
added. Both final installer buttons are **never pressed**, and the report asserts
no install/rollback POST was sent. As defense in depth, every fixture `latest.json`
is deliberately invalid and contains no installer URL, signature, or executable.
The fixture report also checks no manifest was requested. Actual signed Windows
installation/relaunch remains the manual release gate in `docs/releasing.md`.

Outputs: the secret-scanned `owner-session-browser.json`, six `update-*.png`
screenshots plus the Owner Session screenshots, `app-update-fixture.json` (feed
requests, fixture versions/schema, result), and `admin.log`. A failed browser run
captures `failure.png`; a failed launcher still writes the fixture report. Screenshots,
logs and retained DBs are local test artifacts, not suitable for publishing unchecked.

### Fullscreen Lock harness

Covers every lock
state (setup, unregistered, connecting, disconnected, session-ended,
insert-coin, shutdown), responsive viewports,
authentication modes, zero-time account recovery, voucher/points
redemption, the owner PIN gate, and first-time configuration.

```bash
bun run test:fullscreen-lock --filter=@openpiso/client
```

### Widget tabs harness

Covers the final Account and Guest tab matrix, Claim confirmation, Voucher and
Points redemption, Spin result, transfers/code, Password Change, Guest
authentication routing, Settings protection, persistent errors/footer behavior,
re-entry reset, and every transactional overlay's reachability at Widget
geometry. The command includes focused Claim, Redemption Tier menu, and Password
Change regressions.

```bash
cd apps/client && bun run test:widget-parity
```

### Change Password dialog harness

Covers duplicate-submit prevention (busy/locked state), success
message rendering, 409/500-style error messages, and interaction
restoration after failure. Skips with a clear message if Chrome is
not found, so it never blocks `bun test`.

```bash
bun run test:change-password-dialog --filter=@openpiso/client
```

## Optional agent skills

Agent skills are not build dependencies. Install outside the tracked source with
`bunx skills add dchuk/claude-code-tauri-skills` (Tauri v2 references) and
`bunx skills add huntabyte/shadcn-svelte --skill shadcn-svelte`.
The former local lock also listed `mattpocock/skills`: optional
`bunx skills add mattpocock/skills --skill domain-modeling --skill grill-with-docs --skill grilling`.
The lock recorded sources, not installer commands; these commands use the
skills CLI. Check upstream instructions and select only relevant skills; project
rules override generic examples. Never commit agent-local settings or secrets.
