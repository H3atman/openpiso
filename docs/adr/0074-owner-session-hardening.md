# ADR-0074: Harden every Owner Session

- Status: Accepted
- Date: 2026-08-25
- Deciders: shop owner (product)
- Supersedes: the unhardened owner-auth decision in
  [ADR-0024](0024-web-admin-sole-owner-surface.md)
- Refines: [ADR-0050](0050-web-admin-pin-rotation.md)

## Context

ADR-0024 accepted a plain, unbounded owner cookie with no CSRF protection
because the Web Admin was LAN-only. Remote Control adds silent viewing and
keyboard and mouse authority over every configured PC. A stolen owner cookie
would therefore grant more than shop-record mutations.

The Web Admin remains plain HTTP on an operator-isolated admin network. HTTPS
and `Secure` cookies are still rejected for this version because LAN
certificate provisioning is not part of the installation model. The resulting
plaintext PIN, cookie, framebuffer, and input risk is explicit and accepted.

## Decision

An **Owner Session** is an in-memory server record with an opaque token, a
session-bound CSRF token, issue time, last human activity, and a cancellation
signal. Admin restart invalidates every Owner Session.

The server sets `op_owner` with `HttpOnly`, `SameSite=Strict`, `Path=/`, and
`Max-Age=43200`. It omits `Secure` while the Web Admin uses HTTP and never
returns the owner token in JSON. The server expires a session after 30 minutes
without human pointer, keyboard, or touch input, or after 12 hours absolute.
Background polling and framebuffer traffic do not refresh activity. Remote
keyboard and mouse input does through the explicit human-activity path.

The frontend keeps the CSRF token only in memory. Every authenticated mutation
requires it and an exact same-origin `Origin`; WebSocket upgrades require the
exact Web Admin origin. Safe reads do not require CSRF.

Starting a Remote Control Session requires the current Web Admin PIN. Success
rotates the caller's owner token and CSRF token before Rust creates a pending
Remote Control grant; other Owner Sessions remain valid. Five consecutive
failed Remote Control step-ups revoke the caller's Owner Session and append one
`remote_control_step_up_denied` Audit Log event.

Logout, idle or absolute expiry, explicit revocation, and Admin shutdown notify
and close every socket owned by the affected session. Web Admin PIN rotation
keeps ADR-0050's caller Owner Session and revokes all other Owner Sessions, but
it ends every Remote Control Session, including the caller's.

The Admin process continues to listen on `0.0.0.0:8080`. The installer and
operator keep it off customer-facing networks with the existing OS firewall or
VLAN rules.

## Consequences

- Every owner mutation, not only Remote Control, adopts CSRF and bounded
  sessions.
- Dashboard polling cannot keep an unattended browser logged in.
- Token revocation needs a cancellation registry rather than an upgrade-only
  cookie check.
- HTTP peers on the admin network can still observe or alter traffic. Network
  isolation remains mandatory.
