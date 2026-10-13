# ADR-0075: Web Admin Remote Control through TightVNC

- Status: Accepted
- Date: 2026-08-25
- Deciders: shop owner (product)
- Relates to: [ADR-0015](0015-audit-log.md),
  [ADR-0031](0031-client-config-persistence-and-pc-identity.md),
  [ADR-0065](0065-phone-designed-vs-phone-tolerable-owner-screens.md), and
  [ADR-0074](0074-owner-session-hardening.md)

## Context

Each PC already runs TightVNC. The owner wants live assistance on occupied PCs,
unattended maintenance on idle PCs, and passive monitoring. A browser cannot
speak raw RFB to TightVNC, so embedded control needs a WebSocket-to-TCP bridge.

The installed TightVNC version and security mode remain unknown. Shipping is
blocked until an installed PC verifies the configured TightVNC security mode,
WebSocket bridge connectivity, authentication, and occupied-PC consent flow.

## Decision

### Authority and privacy

A **Remote Control Session** may start at any time, including while a customer
uses the PC. OpenPiso shows no indicator on the PC, requires no customer notice,
and permits silent monitoring. The Web Admin identifies view-only and active
control states to the owner.

Exactly one OpenPiso Remote Control Session may exist shop-wide. A second
attempt is rejected, never used to replace the first. Native VNC viewers remain
outside OpenPiso authority and accounting.

### Architecture

The Web Admin embeds pinned `@novnc/novnc`. A deep Rust `remote_control` module
inside the Admin process owns grants, target checks, shop-wide concurrency,
TCP/WebSocket relay, cancellation, timeouts, and audit. `web_admin.rs` remains a
thin HTTP adapter. An internal connector seam uses TCP in production and
in-memory duplex streams in tests.

The bridge extends the authenticated `:8080` server. It adds no listener,
sidecar, Tauri command, capability, shell access, process access, or broad
filesystem access. Rust relays bytes without parsing RFB; noVNC and TightVNC
own authentication, encoding, and input semantics.

### Settings and target selection

**Remote Control Settings** is a disabled-by-default, shop-wide record under
Settings -> PCs with Enabled, one Allowed PC IPv4 Subnet in CIDR notation, and
one VNC Port. The port defaults to `5900` and accepts `1024` through `65535`.
The record contains no credentials. Rust stores it through typed accessors over
the existing `settings` table and updates all fields plus one
`remote_control_settings_updated` Audit Log event in one SQLite transaction.

`registered_ip` remains owner-authored and device-immutable, but now also names
the Remote Control target. New and edited values must be canonical private IPv4
addresses. Existing invalid values survive migration and startup but leave that
PC unconfigured for Remote Control. Rust revalidates the address against the
configured CIDR and rejects loopback, multicast, link-local, unspecified, and
Admin-host targets whenever it creates a grant.

Dashboard receives `remote_control_configured`, which means only that server
configuration is valid. OpenPiso does not probe reachability. The Remote action
is hidden while the feature is disabled and disabled with a repair reason when
that PC's configuration is invalid.

### Credentials and connection flow

**Remote Control Credentials** are two shop-wide TightVNC passwords: one
view-only and one full-control. OpenPiso never stores them. The browser holds a
password only in dialog memory and sends it through noVNC's RFB exchange.
Disclosure of either password compromises that access level on every PC; this
blast radius is accepted.

After ADR-0074's PIN step-up, Rust creates a 30-second, single-use pending grant
bound to the rotated Owner Session, PC, target snapshot, and requested access.
No grant secret appears in a URL. The next matching same-origin WebSocket
atomically consumes it on its first upgrade attempt. Initial failure requires a
new PIN step-up.

The session starts view-only. Enabling keyboard and mouse creates a second
one-time grant under the same Remote Control Session without another PIN
step-up. View-only remains connected until the full-control RFB connection
succeeds, then the dialog swaps sockets. Failed full-control authentication
leaves view-only connected. Both bridge sockets may briefly belong to the same
Remote Control Session.

noVNC requests a shared VNC connection, scales the framebuffer to fit, offers a
1:1 display toggle, and never resizes the remote desktop. The first version has
no monitor selector. It disables clipboard, file transfer, and power actions.
Ctrl-Alt-Delete requires full-control authentication and confirmation.

### UI and lifecycle

Remote Control is a dedicated full-viewport Dashboard dialog, operable at the
Web Admin's 360 px phone floor. Escape closes view-only mode. During control it
is sent to the PC; the explicit Disconnect action requires confirmation. Route
change, tab or browser close, component teardown, owner-session termination,
Remote Control Settings change, Admin restart, and socket failure disconnect
without automatic reconnection.

One session lasts at most 15 minutes without owner input and 2 hours absolute.
Framebuffer traffic does not reset the input timeout. TCP connect timeout is 5
seconds. Browser WebSocket messages and relay chunks are capped at 64 KiB with
bounded backpressure. Any validation measurement that disproves these transport
limits requires an explicit amendment.

### Persistence and audit

Owner Sessions, pending grants, and active Remote Control state remain in
memory. There is no `remote_control_sessions` table. The append-only Audit Log
is the durable history.

`remote_control_started`, `remote_control_enabled`, and
`remote_control_ended` share a Remote Control Session id. They use the `admin`
actor and Admin category, produce no Notification, and record typed PC id,
requested access, timestamps, end reason, and duration. They never record IP,
byte counts, passwords, framebuffer, clipboard, or input. Because Rust is
RFB-blind, these events describe bridge state and requested access, not proven
TightVNC authentication.

The closed end-reason vocabulary is `owner_disconnect`,
`browser_disconnected`, `target_disconnected`, `idle_timeout`,
`absolute_timeout`, `owner_session_ended`, `settings_changed`, `relay_error`,
and `admin_interrupted`. Admin startup appends `remote_control_interrupted` for
an unmatched start. Authorization denials go to Audit; network and RFB failures
go to diagnostics or the terminal end reason. Five failed Remote Control
step-ups append `remote_control_step_up_denied` under ADR-0074.

## Rejected alternatives

- Direct browser-to-VNC cannot speak ordinary raw RFB and bypasses server-owned
  target policy.
- websockify adds a Python process and split lifecycle; it remains a throwaway
  compatibility-test tool only.
- Apache Guacamole adds a gateway stack for protocols OpenPiso does not need.
- A native viewer does not provide embedded Dashboard control.
- HTTPS/WSS and encrypted RFB were rejected for this version. Plain HTTP and
  classical RFB risks are accepted only on the isolated networks and firewall
  rules above.
