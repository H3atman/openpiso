# ADR-0088: Spin Password Confirmation

- Status: Accepted
- Date: 2026-10-18
- Deciders: shop owner (product)
- Amends: [ADR-0012](0012-spin-the-wheel.md), [ADR-0046](0046-spin-the-wheel-points-prize.md) (spin request now carries a password when the setting is on)
- Relates to: [ADR-0004](0004-account-authentication.md), [ADR-0005](0005-tamper-response-and-expiry-powraoff.md), [ADR-0061](0061-account-login-throttle.md)

## Context

Customers asked for a password prompt on spins. A logged-in PC left unattended, or used by someone else, lets that person spend the Account's points on the wheel. Per ADR-0005 the client is untrusted, so a prompt enforced only in the UI would not protect anything.

## Decision

- The customer re-enters their existing Account password for every paid spin. Free spins in a Free Spin Chain never prompt; the server resolves the chain, so there is nothing to confirm.
- `POST /api/wheel/spin` takes a JSON body `{"password": string}`. The server verifies it before the spin transaction. A missing password returns 401 `{"error":"password_required"}`; a wrong one returns 401 `{"error":"invalid_password"}`. Neither charges a point.
- Wrong attempts count toward the Account Login Throttle (ADR-0061), in the same bucket as login, so the spin endpoint cannot be used to brute-force a password. A locked Account gets 429.
- The owner toggles `password_confirmation_required` on `wheel_settings` in Web Admin wheel settings. It is OFF by default, for new installs and upgrades. Clients read it from `/api/wheel/settings` as `password_confirmation_required`. A client with stale settings reacts to `password_required` by showing the prompt.

### Rejected

- **Separate spin PIN**: a new credential to set, reset, and throttle.
- **Owner PIN**: wrong actor; the customer, not the owner, is being confirmed.
- **UI-only check**: bypassable and violates Rust authority.
- **Grace window**: extra state, and not what was asked.
- **Per-wheel-open prompt**: leaves later spins unprotected after the first.

## Consequences

Shops that enable it trade one extra password entry per paid spin for protecting points on shared or unattended PCs. Spin attempts share the login lockout, so a customer who mistypes repeatedly can also be locked out of login until the throttle expires. Shops that leave it off see no change.
