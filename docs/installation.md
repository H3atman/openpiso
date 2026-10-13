# Installation

OpenPiso is a local-first LAN system. Two Tauri apps run side by side on
the **server PC** (the Central Terminal + the admin Rust process), and a
browser owner web app is served from the same Rust process at
`http://<server>:8080/admin`. The customer-facing Central Terminal Tauri
window is a fullscreen kiosk on that same machine; the **client Tauri
apps** run on each rentable PC and talk to the server over HTTP on
`:3210`.

## Topology

```text
            ┌──────────────────────────────────┐
            │         server PC                │
            │                                  │
   kiosk ◀──┤  Central Terminal Tauri window  │  (fullscreen, single window)
   coins ──▶│  Rust process (openpiso-admin)  │
            │   ├─ :3210 — client/device API   │ ◀── client Tauri apps
            │   └─ :8080 — owner web admin     │ ◀── owner browsers (PIN login)
            └──────────────────────────────────┘
```

The Central Terminal Tauri window is a single fullscreen webview; the
desktop owner console was retired in M6 / [ADR-0024](adr/0024-web-admin-sole-owner-surface.md)
and there is no other GUI on the server PC.

ADR-0075 plans a same-origin Remote Control path from the owner browser through
`:8080` to the Admin process, then over RFB/TCP to TightVNC on a registered PC.
That bridge and ADR-0074's hardened Owner Sessions are not implemented yet.

## Prerequisites

- **Bun** — the workspace package manager; install via
  <https://bun.sh>.
- **Rust stable** (MSVC toolchain on Windows).
- **Tauri v2 system prerequisites** — see
  <https://tauri.app/start/prerequisites/>. Windows additionally needs
  the Microsoft C++ Build Tools and WebView2.
- An **SQLite** build (no separate install — the workspace uses a
  bundled driver via `sqlx`). The can't-boot recovery procedure additionally
  needs the stock `sqlite3` command-line executable; install it from the
  SQLite project or the operating system package source before an emergency.

If a Windows shell defines `CC` as an MSYS or MinGW `gcc` while using
the MSVC Rust toolchain, remove that variable before Rust builds the
bundled SQLite:

```powershell
Remove-Item Env:CC
```

## First-time setup

From the repository root:

```bash
bun install
bun run build:web-admin
```

`bun install` resolves every workspace (admin, client, web-admin). The
web-admin build step produces `apps/web-admin/dist`, which the admin
Rust process serves at `/admin` (M5).

### Kiosk clock setup

Before putting a Client PC into service, set its Windows clock to synchronize
with the Admin host (and verify the time zone/date are correct). Repeat this for
every kiosk and confirm the result with `w32tm /query /status`. This is required
for trustworthy Audit Log timestamps and reports. OpenPiso also measures the
remaining Admin/Client difference on every Heartbeat and shows an inline Web
Admin advisory above 60 seconds; that measurement does not affect authority or
Tamper Kill attribution.

## Running locally

Start the admin Tauri app first (it owns SQLite, the timers, and both
HTTP servers):

```bash
bun run dev:admin
```

In a second shell, start the client Tauri app for each rentable PC and
point it at the admin's LAN address. The client window is per-PC; the
default PC ID is `pc-01`.

```bash
bun run dev:client
```

### Client Guard — Tamper Kill protection (ADR-0048)

The **Client Guard** (`OpenPisoClientGuard`) is a Windows SYSTEM service
shipped with the Client installer. It hosts a named pipe that the Client
connects to at startup; if the Client is terminated without sending a
clean-exit message (Task Manager end task, crash, or any unexplained death),
the Guard **relaunches it into the interactive session**.

- The first relaunch follows a short **end-session grace** (about 3 s) during
  which the Guard waits for Windows to report pre-shutdown or session logoff;
  a Windows sign-out or restart is therefore not a Tamper Kill. Repeated deaths inside a short window back
  off exponentially (1 s, 2 s, 4 s… capped at ~30 s). The Guard **never
  permanently gives up**. It retains the exact launched process handle until
  that PID connects: while that Client remains alive but has not connected, the Guard logs the stuck
  state instead of spawning duplicates; retries resume if the process exits.
- Legitimate exits (expiry power-off, offline shutdown, owner `quit_client`)
  send a typed clean-exit message over the write-only, ACL'd pipe so the Guard
  does not treat them as Tamper Kills. The Guard validates the connected
  process image before accepting a message. A dropped pipe is detected from
  the blocking pipe read, not process polling.
- A PIN-gated owner `quit_client` stands the Guard down for the current boot,
  so maintenance leaves the Client closed. The next Client start re-arms it;
  rebooting the Guard starts armed again. Expiry power-off and Offline Shutdown
  Countdown do not stand it down.
- A standard user (the seated customer) **cannot stop the service** from
  Task Manager — only a Windows administrator can.
- A crash is intentionally classified as a Tamper Kill because the Guard
  cannot distinguish it from Task Manager's End task. Relaunch arguments carry
  the kill timestamp and coalesced attempt count into the existing heartbeat
  report path.
- The Client enforces **startup arbitration** (ADR-0080): exactly one
  participating Client runtime runs per shop PC, whatever the launch source
  (Autostart, shortcut, Guard relaunch, manual double-click). An ordinary
  duplicate launch exits immediately and invisibly — no second window, no
  focus change, no Guard pipe, no heartbeat — and if it carried a Guard
  Tamper Kill report, it hands that report durably to the running Client
  first. If a launch cannot consult the coordination object at all (denied
  access, or something else owns the name), it exits nonzero and records a
  **coordination failure** rather than admitting a second runtime. A Client
  in a different Windows session loses the same way; it is never terminated
  or migrated, and the supported deployment remains one active interactive
  shop session. The coordination object is machine-wide
  (`Global\OpenPiso-Client-Runtime`) and version-independent, so shortcuts,
  working directories, executable renames, and updated installs of the same
  Client all converge on one runtime.
- **Rollout gate**: arbitration only coordinates between participating
  binaries. A legacy Client already running from before an upgrade never
  acquires the coordination object, so the gate cannot evict or coordinate
  it. After upgrading the Client, restart the PC (or quit every running
  Client and start the new one) so exactly one participating runtime owns
  the machine; do not expect duplicate protection while an old and a new
  build run side by side.
- The Guard has **no Tauri dependency**. It is a minimal Win32 binary.
- Relaunch uses the active console user's primary token, Unicode environment,
  `winsta0\\default` desktop, and the Client install directory as its working
  directory. If no console user is available (logged out or during a session
  transition), the attempt is logged and the existing backoff retries until a
  user session appears. A locked user remains logged on; the relaunched window
  becomes visible after unlock.
- Guard diagnostics are daily JSON files at
  `%ProgramData%\OpenPiso\logs\client-guard.<date>.log`; expired Guard files are
  pruned at startup, and rotation retains at most the current day plus about
  seven prior days. Check this log for the Win32 API and error code when relaunch
  fails.
- A release Windows Client reports with every Heartbeat whether it currently
  holds its Client Guard pipe connection (ADR-0076). If a Client reports no
  connection for two continuous minutes, Web Admin opens a **Client Guard
  Coverage** Alert for that PC. This is detection only: check this section's
  installation steps and the logs above on that PC — OpenPiso never attempts
  to install, start, or repair the Guard service by itself. Debug and
  non-Windows Clients are excluded from this check.
- Installed Client diagnostics are daily JSON files at
  `%APPDATA%\org.openpiso.client\logs\client.<date>.log`, under the kiosk user's
  profile. A failed Guard connection records the exact `CreateFileW` error here.
  These files are bounded similarly but remain disposable on a diskless PC.
- Client launch timing is recorded in the same file under the
  `openpiso_client::startup` target: the startup-arbitration admission line
  (`client startup arbitration: this launch owns the runtime`), then
  `native_placeholder_visible`,
  `logging_ready`, `app_built`, `setup_entered`, `setup_finished`, and
  `frontend_ready`, each with `elapsed_ms` measured from process start.
  A losing duplicate launch records
  `client startup arbitration: duplicate launch suppressed` in its own
  process's log, one line per rejected process; a launch that could not
  consult the coordination object records a coordination-failure warning
  with the Win32 error code. All arbitration evidence carries the process ID
  and per-launch token, so a reused PID never reads as one continuous
  process.
  `native_placeholder_visible` exists only when the release-Windows placeholder
  starts successfully. `frontend_ready` is the explicit handoff after Svelte
  has painted two frames. The native `OpenPiso / STARTING` placeholder stays
  topmost until this handoff. Use these milestones to tell a slow WebView2 cold
  start
  (`app_built` → `setup_entered`) apart from slow frontend load
  (`setup_finished` → `frontend_ready`) when a shop PC is slow to present the
  Lock after boot.

Before deploying a Client image, cold-boot an installed release build on the
slowest supported PC. Confirm the native placeholder appears before WebView2,
ordinary Alt+F4 does not close it, and the Lock replaces it without exposing the
desktop. Repeat with the Admin unavailable: startup failure must leave the
placeholder or Lock visible, not an usable desktop.

The Guard layers on top of, and does **not** replace, the Assigned Access /
Shell Launcher v2 provisioning. It is a best-effort deterrence layer, not a
hardening guarantee.

### Client Guard installation (elevation requirement)

Download `OpenPiso Client_X.Y.Z_x64-setup.exe` from the
[latest release](https://github.com/H3atman/openpiso/releases/latest),
transfer it to each customer PC, and install manually. Client and Guard are not
auto-updated by Admin.

Because the Client Guard is a Windows service, installing or uninstalling the
Client **now requires elevation** (Run as Administrator). The NSIS installer
shows a **UAC prompt** on launch. This is a change from earlier versions that
did not need administrator privileges.

The installer automatically:
1. Copies `client-guard.exe` to
   `%ProgramFiles%\OpenPiso Client\resources\client-guard.exe`.
2. Registers and starts the `OpenPisoClientGuard` service, pointing `binPath=`
   at that exact path.

The uninstaller automatically:
1. Stops the running Guard service.
2. Unregisters it.

No separate installation step or artefact is needed — the Guard is bundled
inside the Client installer.

On upgrade, the installer stops the stable `OpenPisoClientGuard` service,
points it at the new `client-guard.exe`, starts it again, and removes the old
`openpiso-client-guard.exe` resource. The service and named-pipe identities do
not change.

> **The install path is load-bearing.** `bundle.resources` in
> `apps/client/src-tauri/tauri.conf.json` is a **map**, not an array, precisely
> so the Guard lands at `resources/client-guard.exe`. Tauri rewrites
> each `..` in a relative resource path into a literal `_up_` directory, so the
> array form installs the binary at `$INSTDIR\_up_\_up_\_up_\target\release\`
> and every service start fails with *"the system cannot find the file
> specified"* — a break that is silent unless you look in Event Viewer. The
> `binPath=` in `windows/installer-hooks.nsh` must always match the map's
> target path.
>
> The hooks now check the `sc create` / `sc start` exit codes and show a
> blocking error if the Guard cannot be registered or started, so a broken
> install no longer reports success.

`bun run build:installers` builds the Guard as part of the Client bundle — the
client's `beforeBuildCommand` runs the target-aware `bun run build:guard`.
Native Windows builds use Cargo directly. Linux and macOS installer builds use
`cargo-xwin`, then stage the Windows executable at
`target/release/client-guard.exe` for the existing Tauri resource map. A bare
`bun run build:client` on Windows therefore still produces a complete installer.

> **Development note:** The Guard is **not installed** by `bun run dev:client` /
> `tauri dev`. It only activates on a release build installed via the NSIS
> installer. This matches the existing release-Windows gating of
> `execute_os_shutdown` and the ADR-0028 keyboard hook.
>
> **Real-kiosk verification remains required.** A Windows release build verifies
> that the Win32 imports link, but the LocalSystem token privileges, unlocked /
> locked / logged-out session behavior, interactive-user process ownership, pipe
> reconnection, and rapid-kill backoff require the service installed via NSIS.
> Capture `%ProgramData%\OpenPiso\logs\client-guard.<date>.log` and the kiosk
> user's `%APPDATA%\org.openpiso.client\logs\client.<date>.log` on failure.
> Unit tests do create a real Windows pipe object and read its DACL back, but
> LocalSystem/session behavior still requires the installed release build; it
> is not exercised by `bun run dev:client`. This mirrors the existing treatment
> of `execute_os_shutdown` in `shutdown.rs`.
>
> Verify the installed release on the kiosk: killing the Client must trigger
> the Guard response; restarting it must restore the lock and reconnect.
> Capture both logs above on failure.

### Client launch at startup

In the Client's PIN-gated **Settings**, **Launch at startup** is on by
default on a fresh install. When enabled, the Client registers itself to
launch at every Windows sign-in; disabling the toggle deregisters it. The
setting is stored in the Client's local config and survives restarts. The
registration is per Windows account: it applies to the account that toggles
it (normally the kiosk auto-login account), not every account on the PC. This
is independent of the Guard.

### Disable Windows sign-out

The Client's PIN-gated **Settings** also has **Disable Windows sign-out**, off by
default. When enabled, it writes three Windows policy values for the current
user/machine: removes **Sign out**, hides **Switch user**, and disables
**Win+L**. It deliberately leaves the Ctrl+Alt+Del **Restart** and **Shut down**
commands available as the recovery path.

Enabling or disabling the toggle makes one machine-level registry change and
therefore shows one Windows **UAC** consent prompt. If consent is declined,
the toggle remains off/on as before and the Client shows the error; it never
claims protection that was not applied. The user-level policies are rolled back
when the machine-level operation is declined. The setting is local to this
Client config and is not an admin-server setting.

**Manual recovery if the owner is locked out:** use the separate Windows owner
administrator account, restart or shut down the customer session (these power
options remain available), then open an elevated Command Prompt. Remove the
machine value:

```text
reg delete "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System" /v HideFastUserSwitching /f
```

To remove the customer-profile values, replace `<kiosk-user>` below with the
customer Windows account, load its profile hive, delete the two values, then
unload it:

```text
reg load HKU\OpenPisoRecovery "C:\Users\<kiosk-user>\NTUSER.DAT"
reg delete "HKU\OpenPisoRecovery\Software\Microsoft\Windows\CurrentVersion\Policies\Explorer" /v NoLogoff /f
reg delete "HKU\OpenPisoRecovery\Software\Microsoft\Windows\CurrentVersion\Policies\System" /v DisableLockWorkstation /f
reg unload HKU\OpenPisoRecovery
```

The Client toggle can then be left off or enabled again after verifying both
the owner account recovery path and the kiosk account behavior.

The admin binds:

- `0.0.0.0:3210` — client/device HTTP API and SSE event stream.
- `0.0.0.0:8080` — owner PIN login, dashboard, settings, reports,
  audit, accounts, and the bundled web-admin static assets at
  `/admin`. **The Central Terminal Tauri window is not on these
  ports**; it is a desktop window on the server PC.

The owner browser URL is `http://<server-lan-ip>:8080/admin`. The
default seeded data installs the four demo PCs (`pc-01`…`pc-04`),
the `₱1 = 5 minutes` rate, and the `dev-simulator` coin source so
the kiosk works without a serial acceptor wired in.

### Central Terminal Autostart (ADR-0055)

The Central Terminal can be set to launch automatically when the Admin PC's
Windows user logs on. The owner toggles this in **Settings &rarr; Central
Terminal Autostart** on the web admin.

- **What it writes:** A per-user `Run` registry entry at
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` pointing to the
  Central Terminal executable. The entry is named `org.openpiso.admin`.
- **Logon scope, not boot scope.** Autostart fires when the Windows user signs
  in &mdash; it does not start the Central Terminal when the machine boots
  (no `HKLM\...\Run` or Scheduled Task is created). Unattended recovery after
  a power cycle additionally needs **Windows autologon** configured on the
  Admin PC.
- **OpenPiso never configures autologon.** Provision it separately through
  Windows settings (`netplwiz` on Windows 10/11) or via the Registry
  (`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon`). Do not
  use a customer account for autologon &mdash; use the same Windows user
  account the Admin PC runs under.
- **Starts off.** Autostart ships disabled; no registry entry is written
  until the owner turns it on.
- **Debug build limitation.** In debug (`cargo dev`) builds, the toggle is
  read-only. This prevents stale `Run` entries pointing into a build tree
  that no longer exists after a recompile.

### Kiosk Lock — always-on-top (ADR-0056)

The Central Terminal window starts pinned above every other window, so nothing
can cover the coin-intake screen. There is no setting for this: it is on at
every launch.

- **Reading the state.** The dot beside the Shop Name in the kiosk header is
  the signal. Filled with a ring plus a `Kiosk · Locked` subtitle means the
  window is pinned; a hollow, muted dot plus `Kiosk · Unlocked` means it is
  not.
- **Releasing it.** **Double-click the Shop Name** in the kiosk header, then
  enter the owner PIN (the same PIN as the web admin). Other windows can then
  be brought in front of the kiosk. The window stays fullscreen &mdash; use
  Alt-Tab to reach the desktop.
- **Re-engaging it.** Double-click the Shop Name again. No PIN is needed to
  re-pin the window.
- **Grid only.** The gesture does nothing while a customer has a Selection
  Window open (the insert screen). Wait for the kiosk to return to the grid.
- **Nothing is remembered.** Restarting the Central Terminal always brings it
  back pinned, so a forgotten unlock is fixed by a restart.
- **Repeated wrong PINs lock the release out** for a minute, doubling up to
  ~15 minutes. Every attempt, and every lockout, is written to the audit log.

## Ops: bind `:8080` to the wired/admin network (ADR-0024)

Because `:8080` exposes money-moving and account-deleting endpoints
behind a single PIN, the operator is responsible for keeping the
endpoint off any customer-facing network. The default `0.0.0.0:8080`
bind is correct on a private admin VLAN, but if customer Wi-Fi
shares the same physical network, **bind `:8080` to the wired/admin
interface or a separate VLAN before opening the shop**. Concrete
options:

- **Per-interface bind (Linux/macOS).** Set the bind address via the
  OS firewall or a process supervisor so only the wired NIC accepts
  `:8080`. The Rust process itself binds to `0.0.0.0:8080`; the
  firewall restricts the listening socket to the admin subnet, e.g.
  with `iptables`:

  ```bash
  iptables -A INPUT -p tcp --dport 8080 \
           -i eth_admin -j ACCEPT
  iptables -A INPUT -p tcp --dport 8080 -j DROP
  ```

  (or the equivalent `nft`/`pf` rule for the host's firewall).

- **Windows Defender Firewall with Advanced Security.** Add an
  inbound rule for `tcp/8080` scoped to the wired/admin interface
  ("Local IP addresses" → "These IP addresses" → the admin subnet
  only) and deny the rule on the public profile.

- **VLAN separation.** Put the server PC's wired NIC on a separate
  VLAN, route that VLAN to the owner laptop(s), and keep customer
  Wi-Fi on a different VLAN with no route to `:8080`. The
  `0.0.0.0:8080` bind is then only reachable from the admin VLAN.

The `:3210` client/device API follows the same rule but is more
permissive: it must be reachable by every rentable PC on the LAN, so
it lives on the same VLAN as the PCs themselves.

> Current implementation note: ADR-0074's CSRF, SameSite, expiry, token
> rotation, and socket cancellation are accepted but not implemented. Keep the
> existing firewall/VLAN isolation before and after they land. Remote Control
> deliberately remains plain HTTP/`ws://`, so network peers could observe or
> alter PIN, cookie, framebuffer, and input traffic.

## PC Monitoring Check now (ADR-0076)

The built-in Check-now probe replaces the owner's manual ping comparison with
one Web Admin action. Windows-only support facts:

- **Windows Admin only.** The ICMP echo goes directly through the Windows IP
  Helper API (`IcmpSendEcho`) with a one-second timeout; no `ping`,
  PowerShell, shell, or sidecar process is ever started. Non-Windows Admin
  builds report every Host Reachability check as unavailable — they never
  claim a PC is healthy.
- **Register the static IP first.** Only visible PCs with a non-blank
  `registered_ip` are probed. A PC without a registered address comes back as
  explicitly unavailable instead of healthy.
- **ICMP can be blocked.** Windows Firewall or security software may silently
  drop echo requests. If a running PC keeps reporting "Host: no reply" while
  its Client heartbeat shows reachable, allow inbound ICMPv4 echo request on
  that PC. A ping result is diagnostic evidence only — it never changes PC,
  session, money, time, or Points state.
- **Rollout comparison.** Keep an independent batch ping script (for example,
  registered PCs at `192.168.1.101`–`.104`) as an independent field check while rolling
  out Check now. The script only samples the registered hosts and leaves no
  incident, Alert, or Telegram state. Compare its output with Check now,
  then enable continuous monitoring only after the host list and firewall
  rules agree. The script remains useful during maintenance, but it is not a
  runtime dependency and cannot replace the durable PC Monitoring history.

## Enabling continuous PC Monitoring (ADR-0076)

Check now is a one-shot comparison. Continuous detection — one Alert per
Client Reachability Mismatch episode — is shop-wide, off after upgrade, and
turned on deliberately:

1. Open **Settings → PC Monitoring** in Web Admin and flip the toggle. The
   change asks for your **current Web Admin PIN**, is audited, and covers every
   visible registered PC with a registered address; there is no per-PC opt-out.
2. While monitoring runs, Admin probes each eligible PC every ten seconds. A
   mismatch Alert opens only after the host keeps answering while its Client
   stays silent continuously for 60 seconds — startup, enabling, and network
   transitions all get that full grace before anything is recorded.
3. While monitoring is disabled, Web Admin shows a standing advisory on the
   dashboard so an intentional or forgotten off state stays obvious.
4. Disabling closes active incidents as *monitoring disabled* (never as
   recovered) but keeps every historical Alert in Notifications.

Incidents are evidence for review, never enforcement: monitoring never locks or
powers off a PC, ends a session, or touches money, Remaining Time, or Points.
A closure that says Client recovery is unconfirmed means exactly that — power
loss or blocked ICMP is not presented as recovery.

## Suppressing one PC during maintenance (ADR-0076)

Authorized maintenance (reinstalling the Client, swapping network hardware,
re-imaging the machine) looks exactly like the condition monitoring watches
for: the host keeps answering while its Client is down. Instead of ignoring a
truthful Alert, suppress that PC first:

1. Open **Web Admin → Dashboard**, find the PC's row, and choose **Suppress**.
   Pick 15/30/45/60 minutes and confirm with your **current Web Admin PIN** —
   the same step-up as enabling or disabling monitoring.
2. While suppressed, no new incidents of any kind open for that PC. Incidents
   already open stay open and visible; suppression never erases evidence.
3. The row shows the absolute expiry (for example *Suppressed until Aug 26,
   2:30 PM*). Suppression expires automatically — at most 60 minutes — and
   expiry restarts the full grace period instead of opening an immediate
   Alert, so finishing early needs no cleanup. Use **Cancel Suppression** on
   the row to end it sooner.
4. Quitting the Client cannot create suppression: only Web Admin can. The
   Client's quit guidance says the same thing — suppress before maintenance,
   or Admin may truthfully record an Alert while you work.

## Connecting Telegram alerts (ADR-0076)

PC Monitoring can optionally deliver Alerts to one private Telegram chat.
The integration is outbound only: it never accepts bot commands, polls on a
schedule after setup, opens ports, or adds webhooks. The token is protected
with Windows DPAPI under the Admin's Windows login, is never displayed again,
and the whole integration is excluded from Backups — a restored Admin starts
disconnected until you reconnect here. Non-Windows Admin cannot connect
Telegram and says so instead of pretending.

1. In Telegram, message **@BotFather**, send `/newbot`, and follow its prompts
   to create a bot for your shop. Copy the token it gives you — it looks like
   `123456789:AA...`.
2. Still in Telegram, find your new bot and send it any private message (for
   example `/start`) **from your own account**.
3. Open **Web Admin → Settings → Telegram Alerts** and paste the token, then
   choose **Verify token**. Admin checks the token with Telegram; nothing is
   stored yet, and an existing connection stays active meanwhile.
4. Choose **Look for my message**. Web Admin lists the private chats that
   wrote to the bot (groups are ignored) and shows the detected chat.
   Typing the numeric chat ID by hand exists as a fallback and still requires
   the same test below.
5. Choose **Send test & connect** and confirm with your **current Web Admin
   PIN**, exactly like enabling monitoring or suppressing a PC. A test
   message must actually arrive before the new configuration replaces the old
   one — a failed test leaves your working setup untouched. Success is
   audited.

Disconnecting uses the same PIN step-up and deletes the protected token and
the chat destination. Local PC Monitoring keeps running either way: delivery
is optional, detection is not lost.

## Telemetry (ADR-0083)

A release Admin built with the project token sends Telemetry automatically
(ADR-0083). The owner cannot opt out or reset the Installation ID; there are
no Telemetry controls in Settings. Admin sends one `admin_daily_summary` per UTC+08:00 reporting day to **PostHog Cloud US**:
a random Installation ID (`distinct_id`), Admin version, day, and counts of
PCs seen by Heartbeat in the last 24 hours and 30 days. It sends no shop or
customer identifiers, IPs, MACs, sessions, money, time, or diagnostic text;
PostHog sees the source IP in transit but the project is configured to discard
it; events create no person profiles or GeoIP lookup.
Events are retained for **one year**. PostHog cannot delete a single
installation's past events without a person profile. Telemetry state is
excluded from Backups, so a restored Admin gets a new Installation ID when
Telemetry resumes. Development and fork builds without the project token
cannot send. This is an explicit privacy tradeoff of the always-on decision.

## Planned Remote Control deployment (ADR-0075)

Remote Control ships disabled. Do not enable it until one production PC passes
all checks below:

1. Record the installed TightVNC version, listening port, and RFB security
   mode.
2. Configure TightVNC as a Windows service with two distinct shop-wide
   passwords: View-only and Primary full-control. OpenPiso never stores either.
3. Restrict the TightVNC port in each PC's firewall to the Admin host. Keep the
   Admin-to-PC hop on the isolated wired LAN; unencrypted RFB is an accepted
   risk only there.
4. Test official noVNC with throwaway websockify for display, keyboard, mouse,
   confirmed Ctrl-Alt-Delete, sign-in and lock screens, UAC secure desktop,
   service-mode startup, both passwords, fit scaling, 1:1 display, phone use,
   and the actual multi-monitor framebuffer.
5. Measure a session on shop hardware. Confirm the five-second connect timeout,
   64 KiB messages and relay chunks, 15-minute input-idle timeout, and two-hour
   absolute limit work without stalls.

After implementation, Settings -> PCs exposes only Enabled, Allowed PC IPv4
Subnet, and VNC Port. The port defaults to `5900`; the CIDR starts unset. The
Remote action stays hidden until enabled. Each `registered_ip` must be a
canonical private IPv4 address inside that CIDR.

OpenPiso allows silent viewing and control of occupied PCs, displays no
connection indicator on the PC, and requires no customer notice. This is a
deliberate owner policy in ADR-0075.

## Backup files and sensitive data

The Admin stores the primary Backup filesystem directory beside the database:
`<Admin app data directory>/backups/`. On Windows the default data directory is
`%APPDATA%\org.openpiso.admin`; on Linux it is
`~/.local/share/org.openpiso.admin`. A full Backup that includes the
**Credentials** and **Terminals** Backup Sets contains device secrets and the
hashed **Web Admin PIN** (the owner's web-admin credential). This is sensitive
material: never commit, email, or otherwise share a full Backup.

For a shareable diagnostic/readable export, create a manual Backup with both the
**Credentials** and **Terminals** Backup Sets deselected. This removes device
secrets and the Web Admin PIN hash; the Accounts Backup Set still contains
customer password hashes, so share only with an intended trusted recipient. It
is not valid whole-database restore input because restore requires every Backup
Set.

The Telegram integration is excluded from every Backup regardless of selected
sets (ADR-0076): no Backup file ever contains the bot token, the connected
chat, or queued deliveries, and restoring any Backup leaves Telegram
disconnected until the owner repeats the guided setup above.

### Delivery health

While Telegram is connected, the same Settings section shows delivery health
inline (ADR-0076 / #346): when the last message was delivered, the most recent
error, and the queue state (#347) — how many items are still queued (never yet
attempted), how many are retrying after failed attempts, and how many expired
unsent after their 24-hour retry window. Nothing waiting and nothing expired
reads as *Delivery is healthy*. Expired items are never re-sent; local Alerts
remain the canonical history either way. Health shows counts only — never a
token or message content. Replacing or disconnecting Telegram clears queued
deliveries for the old destination; local Alerts remain.

### Admin downtime and restarts

PC Monitoring and Telegram delivery behave truthfully across Admin downtime:

- Incidents already open when Admin stops stay open; startup marks them
  *observation interrupted*, so any Telegram closure names that gap instead of
  claiming a continuously observed duration.
- Queued deliveries live in the database: pending alerts survive a restart and
  send once Admin is back online, retrying with bounded backoff for up to 24
  hours before expiring unsent.
- If Admin stops between Telegram accepting a message and recording it, the
  same line may arrive twice. Every line carries its PC display name and exact
  timestamp, so a duplicate is recognizable. Local incidents, Alerts, money,
  time, and Points are never duplicated by this.

## Admin App Update (ADR-0086)

Sign in to the web admin and select the version row above the Owner menu in
its sidebar (on a phone, open the sidebar first). The row shows the installed
Admin version or **Update available · vX.Y.Z**. Hover for a short summary; tap
or click for release notes, **Check now**, last checked time, and automatic
checks. Automatic checks default on at startup and every six hours; turning
these off suppresses automatic outbound checks, not **Check now**.

**Install** asks for confirmation and restarts the Admin after the signed
installer is verified. A release that raises the **Schema Version** also
requires **I understand**: returning below that boundary needs a
Pre-Migration Backup restore. The panel shows download progress. Running PCs
produce a count warning, not a block: paid time is preserved, Clients enter
the Fullscreen Lock under the Offline Shutdown Countdown and resume when the
Admin reconnects. Sign in again after the restart to see its success/failure
toast; acknowledgement clears the restart result, not its Diagnostics history.
Manifest, download and signature failures also stay in Diagnostics/Run Records,
without a restart marker.

Expand **Compatible Rollback** for older stable releases with the same Schema
Version. Read the red warning and type the exact target version (no `v` prefix
or spaces) before confirming. Other Schema Versions are not offered. App
Update covers Admin only, not Clients or Client Guard.

### Windows installer

For the Owner flow, verification, restart outcomes and Compatible Rollback, see
[Admin App Update](#admin-app-update-adr-0086) above. Windows Admin uses NSIS
installers with `currentUser` (no UAC), keeping the existing install path and
HKCU Autostart registration. The installer runs in `passive` mode: the kiosk
hides, a progress window appears, and Admin closes and relaunches automatically.

Download [Admin and Client installers](https://github.com/H3atman/openpiso/releases/latest)
from the public release. Client remains manual-install only and includes the
Guard service (`perMachine`, UAC required); it is never installed by Admin App Update.

Offline shops can still sideload the installer from `bun run build:installers`
(no signing secrets needed), or transfer the published setup EXE from the public
releases repository to the Admin PC and run it as the same Windows user. Existing
0.11.x installs without the updater need one manual sideload of an updater-enabled
build before App Update works. Never use a lower-schema installer against a newer
database; follow the recovery procedure below instead.

The committed Admin updater public key is the real upstream verification key.
Forks must replace it with their own key and complete signing setup and the real
Windows install/relaunch gate in [releasing](releasing.md) before publication.

## Can't-boot recovery from a Pre-Migration Backup

An older Admin refuses to start against a database migrated by a newer Admin.
To return to an older Admin, restore its matching Pre-Migration Backup; direct
database downgrade is unsupported.

Use this only when a bad migration prevents the Admin from starting and the
owner web admin is unavailable. Stop OpenPiso first and make sure no Admin
process has `openpiso.db` open. This procedure uses the canonical `.sql`
**Pre-Migration Backup**, not its byte-exact `.db` companion: the SQL Backup is
self-describing schema plus data and can be replayed by stock `sqlite3` without
OpenPiso.

1. Copy the current database aside before changing anything. Also remove its
   SQLite WAL sidecars; a sidecar from the failed run must not be replayed:

   ```bash
   set -euo pipefail
   DATA_DIR=/path/to/admin-app-data
   PRE_MIGRATION=$(ls -t "$DATA_DIR"/backups/openpiso-pre_migration-*.sql | head -n 1)
   test -s "$PRE_MIGRATION"
   mv "$DATA_DIR/openpiso.db" "$DATA_DIR/openpiso.db.cannot-boot"
   rm -f "$DATA_DIR/openpiso.db-wal" "$DATA_DIR/openpiso.db-shm"
   ```

   On Windows PowerShell, use the equivalent paths and file operations:

   ```powershell
   $ErrorActionPreference = 'Stop'
   $DataDir = Join-Path $env:APPDATA 'org.openpiso.admin'
   $PreMigration = Get-ChildItem "$DataDir\backups\openpiso-pre_migration-*.sql" |
     Sort-Object LastWriteTime -Descending | Select-Object -First 1
   if (-not $PreMigration -or $PreMigration.Length -eq 0) { throw 'No Pre-Migration Backup found' }
   Move-Item "$DataDir\openpiso.db" "$DataDir\openpiso.db.cannot-boot"
   foreach ($Sidecar in @("$DataDir\openpiso.db-wal", "$DataDir\openpiso.db-shm")) {
     if (Test-Path -LiteralPath $Sidecar) {
       Remove-Item -LiteralPath $Sidecar -Force
       if (Test-Path -LiteralPath $Sidecar) { throw "Could not remove $Sidecar" }
     }
   }
   ```

2. Replay the selected `.sql` file into a new database with stock `sqlite3`.
   Do not replay into an existing database:

   ```bash
   set -euo pipefail
   RECOVERED="$DATA_DIR/openpiso.recovered.db"
   test ! -e "$RECOVERED"
   sqlite3 -bail "$RECOVERED" < "$PRE_MIGRATION"
   test "$(sqlite3 "$RECOVERED" 'PRAGMA integrity_check;')" = ok
   MIGRATION_VERSION=$(sqlite3 "$RECOVERED" \
     'SELECT MAX(version) FROM _openpiso_migrations;')
   test -n "$MIGRATION_VERSION"
   mv "$RECOVERED" "$DATA_DIR/openpiso.db"
   ```

   In Windows PowerShell, the replay command is:

   ```powershell
   $Recovered = Join-Path $DataDir 'openpiso.recovered.db'
   if (Test-Path -LiteralPath $Recovered) { Remove-Item -LiteralPath $Recovered -Force }
   if (Test-Path -LiteralPath $Recovered) { throw 'Could not clear recovery target' }
   & sqlite3 $Recovered ".read '$($PreMigration.FullName)'"
   if ($LASTEXITCODE -ne 0) { throw 'sqlite3 replay failed' }
   $Integrity = (& sqlite3 $Recovered 'PRAGMA integrity_check;').Trim()
   if ($LASTEXITCODE -ne 0 -or $Integrity -ne 'ok') { throw 'integrity check failed' }
   $MigrationVersion = (& sqlite3 $Recovered 'SELECT MAX(version) FROM _openpiso_migrations;').Trim()
   if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($MigrationVersion)) { throw 'migration history is missing' }
   Move-Item $Recovered (Join-Path $DataDir 'openpiso.db')
   ```

3. Start OpenPiso. It sees the older schema version and runs the pending
   migrations normally, creating a new mandatory Pre-Migration Backup before
   doing so. Keep `openpiso.db.cannot-boot` and the selected recovery files
   until the shop has been checked.

If the integrity check fails, stop and do not start OpenPiso against that file.
Do not use the `.db` companion as though it were a Catalog entry: it is a direct
rollback artifact, not a Backup Kind.

## Checksum recomputation (escape hatch only)

Readable is not the same as editable. Restore refuses a hand-edited Backup when
the SHA-256 checksum in its header does not match every byte after the
`-- body-begins` marker. Recomputing that checksum is an **escape hatch, not a
supported workflow**; it does not make an incomplete Backup valid, and restore
still requires all seven Backup Sets and a body whose per-table row counts match
the ones its header records.

If an emergency investigation has deliberately edited the SQL body, preserve a
copy first, then run this Windows PowerShell byte-preserving recomputation. It
changes only the 64 hexadecimal characters after `-- sha256: ` and avoids
newline/BOM conversion:

```powershell
$Path = 'C:\path\to\backup.sql'
$Bytes = [IO.File]::ReadAllBytes($Path)
$Text = [Text.Encoding]::UTF8.GetString($Bytes)
$Marker = "-- body-begins`n"
$MarkerAt = $Text.IndexOf($Marker, [StringComparison]::Ordinal)
$LabelAt = $Text.IndexOf('-- sha256: ', [StringComparison]::Ordinal)
if ($MarkerAt -lt 0 -or $LabelAt -lt 0 -or $LabelAt -gt $MarkerAt) { throw 'Not an OpenPiso Backup header' }
$BodyAt = $MarkerAt + [Text.Encoding]::UTF8.GetByteCount($Marker)
$Body = New-Object byte[] ($Bytes.Length - $BodyAt)
[Buffer]::BlockCopy($Bytes, $BodyAt, $Body, 0, $Body.Length)
$Digest = [Security.Cryptography.SHA256]::Create().ComputeHash($Body)
$Hex = -join ($Digest | ForEach-Object { $_.ToString('x2') })
$HexBytes = [Text.Encoding]::ASCII.GetBytes($Hex)
$HashAt = $LabelAt + '-- sha256: '.Length
[Buffer]::BlockCopy($HexBytes, 0, $Bytes, $HashAt, 64)
[IO.File]::WriteAllBytes($Path, $Bytes)
```

## Real-hardware coin acceptor bring-up

The M1/M2/M4 coin pipeline already supports the real serial source
(`SerialCoinSource` in `apps/admin/src-tauri/src/coin_source.rs`);
the **coin-device settings** persisted in migration `0007` (COM port,
set/coin line pair, inter-coin gap, retained Pulse Map) and the
**Selection Window timers** from `0008` are loaded by
`ConfiguredCoinSource::from_settings` at startup. Runtime acceptance uses
fixed checked `pulse_count × 100 cents` authority and does not read the map. To bring the
hardware up at a real shop:

1. Open `http://<server>:8080/admin` on the owner laptop.
2. Sign in with the PIN (first-run `POST /web-admin/setup`, then
   `POST /web-admin/login` on subsequent visits).
3. Go to **Settings → Coin device**. Set:
   - `com_port` — the COM port the acceptor is wired to (e.g.
     `COM3` on Windows, `/dev/ttyS0` on Linux). Leave blank to
     stay on the simulator.
   - `baud_rate` — the acceptor's serial baud (typically `9600`).
   - `set_pin_line` — the control line the server drives to
     enable/inhibit the acceptor (`rts` or `dtr`).
   - `coin_pin_line` — the status line the server reads for pulse
     edges (`cts`, `dsr`, `ri`, or `cd`).
   - `coin_pin_polarity` — `normally_open` for pulse-high acceptors, or
     `normally_closed` for idle-high/pulse-low acceptors (default:
     `normally_open`).
   - `inter_coin_gap_ms` — the quiet gap that closes a Coin Batch
     (1–10000 ms; default `600`, a field-informed starting point that still
     requires bench validation).
   - `pulse_map` — retained pulse-count/denomination evidence for physical
     calibration and historical Rejection Credit choices. It does not alter
     automatic acceptance, which is always 100 cents per pulse. Fresh installs
     include the ₱20 candidate; upgrades preserve existing rows.
   - `inactivity_timeout_seconds` and `hard_cap_seconds` — the
     Selection Window timers (default `15 s` and `180 s`).
4. Click **Save** to apply. The setting is rejected if a Selection
   Window is currently open.
5. Click **Test acceptor**. The endpoint opens the configured
   serial handle with the set pin **inhibited** (fail-closed) and
   reports whether the COM port is healthy. With no `com_port`
   configured, the test is a no-op success on the simulator.
6. Save the serial settings, then use the inline **₱20 Calibration** flow:
   record five individual genuine ₱20 coins and Apply only when all counts
   agree. Captures create no transactions and settings changes are blocked
   while capture is active. Only one application may own the COM port at a time.
7. Walk through the full bench test procedure in `docs/hardware.md`
   (inhibit at idle, guest/account credit, close/re-inhibit, rapid ₱10
   behavior, and post-calibration authoritative ₱20 credit).

The simulator remains the default source. The fresh-install ₱20 row is a
candidate only; the owner opts in to the serial source by configuring the COM
port and must complete physical calibration before customer use.

## Attendance Rewards rollout (ADR-0084)

Attendance Rewards ships disabled on fresh installs and upgrades. Defaults are a
₱10 daily threshold (`1000` cents), a 1-hour reward (`3600` seconds), and one
following local calendar month to claim; no pre-feature history is backfilled.
Enabling it changes earning prospectively; it does not backfill history.
Disabling it stops new earning but does not remove already-earned Attendance
Time or disable claims; outstanding time remains claimable until its Earning
Month expires. The Client calls the feature
**Daily Reward**.

For a post-release Attendance defect, use this rollback ladder:

1. Disable Attendance Reward in Web Admin to stop new earning. Already-earned
   Attendance Time remains claimable until its claim window expires.
2. Prefer a forward-fix release; do not downgrade the migrated database.
3. If the prior Admin cannot start against the upgraded database, restore the
   matching Pre-Migration Backup and run the prior Admin binary that contains
   the migration-version guard. **This loses every database change made after
   that backup.** Follow [Can't-boot recovery from a Pre-Migration
   Backup](#cant-boot-recovery-from-a-pre-migration-backup).

## First-run checklist

- [ ] `bun install` and `bun run build:web-admin` complete.
- [ ] `bun run dev:admin` starts the Tauri kiosk window.
- [ ] `http://127.0.0.1:8080/admin` renders the login shell.
- [ ] `POST /web-admin/setup` issues a cookie token and the
      dashboard loads four demo PCs.
- [ ] **Pre-register each PC** (ADR-0031): in the web admin
      (Settings → PCs → Pre-register), add a row with the PC's
      display name, the static IP it lives on, and leave the MAC
      field blank. The first boot from that IP binds the row's MAC.
- [ ] `POST /web-admin/coin-device/test` returns
      `{ ok: true, simulator: true, message: "No COM port
      configured; running on the simulator source." }`.
- [ ] `:3210/health` returns `{ status: "ok", version: … }`.
- [ ] A client Tauri app on a different PC heartbeats against
      `<server-lan-ip>:3210` and its row turns `online` on the
      dashboard. On the first boot the client shows "Registering
      this PC…", then "This PC isn't registered yet" until the
      pre-registration row exists.
- [ ] A subsequent reboot of the same client identifies by MAC
      and reconnects without re-binding. The client never asks the
      owner to type a PC ID — the server owns the binding.
- [ ] The firewall / VLAN rule keeps `:8080` off the customer
      network.

After ADR-0074/0075 implementation, also verify:

- [ ] Owner cookies are HttpOnly and SameSite Strict, responses expose no owner
      token, mutation requests reject missing CSRF or wrong Origin, and Owner
      Sessions expire after 30 minutes without human input or 12 hours total.
- [ ] Remote Control remains disabled until its CIDR, port, PC IP, TightVNC
      profile, two passwords, and PC firewall rule pass validation.
- [ ] Only one OpenPiso Remote Control Session starts shop-wide; it begins
      view-only, full control requires its second password, and Ctrl-Alt-Delete
      requires confirmation.
- [ ] Logout, expiry, PIN rotation, Settings changes, route or browser close,
      and Admin restart end control without automatic reconnect.

## Client kiosk hardening (Assigned Access / Shell Launcher v2)

The seated customer is untrusted (ADR-0027). The OpenPiso Client Lock blocks
in-app and the app-level keyboard hook swallows the casual escape chords (Win,
Alt-Tab, Alt-Esc, Ctrl-Esc) **while the Lock is showing**, but the robust
lockdown is **per-machine OS provisioning** the owner performs once on each
customer PC (ADR-0028). Bake it into the diskless master image
([diskless.md](diskless.md#master-image-setup)) so every booted PC is hardened.

**Window-level prevention (ADR-0028 §3) — nothing to configure.** In an
installed release build the Client cannot be closed by ordinary window means:

- It has **no taskbar button** (`skipTaskbar`), so there is no taskbar context
  menu offering **Close window**. On a PC where Shell Launcher is not yet
  provisioned — where a taskbar exists at all — this was previously a one-click
  free kill.
- It **refuses `WM_CLOSE`** unless a legitimate exit is already under way. The
  system menu, a posted close, and Alt+F4 all fail silently and leave a line in
  the Client log. The only exits are the Owner PIN **Quit**, the expiry
  power-off, and the Offline Shutdown Countdown.
- **Alt+F4 is blocked in every view mode**, not only under the Lock — but only
  while the Client is the foreground window, so a paying customer can still
  Alt+F4 their own browser.

A refused close does **not** terminate the process, so the Client Guard sees
nothing: no relaunch, no `tamper_kill`, and no kick. This is
prevention, and it is deliberately separate from the Guard's response. Task
Manager's **End task** is not a window close and is unaffected by any of the
above — that is what the Guard is for.

None of this applies to `bun run dev:client`; a developer is never trapped.

> After installing or upgrading a kiosk PC, verify the close prevention above
> and the Guard's kill/relaunch/kick response using the installed release.

**Two Windows accounts per customer PC (Windows Pro / Education / Enterprise):**

1. **Customer account — kiosk shell.** Make the OpenPiso Client the account's
   shell with **Shell Launcher v2** (or single-app **Assigned Access**), so
   Explorer never loads: no desktop, taskbar, or Start menu under the Lock, and
   the configuration survives reboot. Enable the Shell Launcher feature, then
   set the Client executable as that account's custom shell.
2. **Owner account — normal Explorer.** A **separate Windows administrator
   account** keeps the standard Explorer shell for maintenance. This is the
   recovery path that does **not** depend on the Client "exiting to desktop":
   sign out of the customer account and into the owner account to service the
   PC. Test this before shipping.

**Owner exit paths (must both be verified before a PC goes live):**

- In-app: the **Owner PIN** unlocks the Client settings panel and **Quit**
  (ADR-0027). The PIN works even when the admin server is unreachable, *as long
  as the owner has verified it online at least once during the current Client
  run* (the offline fallback caches the verifier in memory only).
- OS-level: the **separate owner admin account** above.

**Known gap — Ctrl+Alt+Del.** The Secure Attention Sequence can **never** be
intercepted by application code. It is mitigated only by the Assigned Access /
Shell Launcher provisioning above (which removes the desktop/Task Manager the
customer would otherwise reach). On a machine that is not yet provisioned,
Ctrl+Alt+Del → Task Manager remains an open escape; do not put such a PC on the
floor.

**Brick risk.** Misconfigured Shell Launcher can lock the owner out too. Keep
the separate admin account working and the Owner PIN set before enabling the
custom shell, and verify both exits on a test PC first (ADR-0028).
