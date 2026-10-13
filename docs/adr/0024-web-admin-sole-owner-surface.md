# ADR-0024: `:8080/admin` is the sole owner surface; PIN login

- Status: Accepted
- Date: 2026-06-27
- Deciders: shop owner (product)
- Supersedes: the **owner-login** portion of ADR-0011 (soft-delete decision stands).
- Relates to: ADR-0023 (serial coin input / terminal kiosk).

> **Superseded in part by [ADR-0074](0074-owner-session-hardening.md):** the
> sole-owner-surface and PIN decisions remain; the plain, unbounded cookie and
> absence of CSRF hardening do not.

## Context

In v1 the server PC becomes a full-screen customer **Central Terminal** kiosk
(ADR-0023), so there is no room for a desktop owner console on that machine.
The owner needs to manage the shop from another device on the LAN. The plan
originally proposed a *settings-only* `:8080/admin` while keeping the desktop
console as the full-fidelity recovery surface — but retiring the console
removes that fallback.

## Decision

- **The desktop Tauri admin console is retired.** Its 27 commands are ported to
  an authenticated HTTP API served on **`:8080`**, alongside the owner web app
  (Vite + Svelte + shadcn-svelte). This is the **sole** owner surface in v1, so
  it carries **full parity** — live dashboard, economy/wheel settings, vouchers,
  manual adjustments, read-only reports, audit search/export, and destructive
  account admin (create/reset/delete).
- **Owner auth is a simple PIN**, no hardening. The PIN replaces the owner
  password of ADR-0011 (whose only consumer, the console, is gone). Login
  verifies the PIN and issues the existing owner session token, carried in a
  plain cookie. No httpOnly/SameSite/CSRF ceremony.
- **Trust boundary is the LAN plus the PIN, nothing more.** `:8080` stays
  separate from the `:3210` client/device API; owner endpoints are never exposed
  on `:3210`. Public-internet exposure remains unsupported.

## Considered and rejected

- *Settings-only `:8080` + keep the desktop console* (the plan's original): the
  console can't run on a kiosk-only server, and tying it to that machine defeats
  remote management. Rejected.
- *Hardened cookie/CSRF auth*: explicitly declined for a local-only,
  single-operator shop. Accepted as a known trade-off.

## Consequences

- A money-moving and account-deleting surface is reachable by anything on the
  shop LAN with only a PIN and no brute-force protection. If customer Wi-Fi
  shares the LAN, bind `:8080` to the wired/admin interface or a separate
  VLAN. This is an ops note, recorded deliberately, not a v1 blocker.
- ADR-0011's soft-delete rules (history preserved, balance forfeited, session
  ended, username retired) are unchanged and still authoritative.
- The single-owner audit limitation of ADR-0011 (no per-staff attribution)
  carries over unchanged.
