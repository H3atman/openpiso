# ADR-0061: Account Login Throttle — flat per-Username lockout

- Status: Accepted
- Date: 2026-08-07
- Deciders: shop owner (product)
- Relates to: [ADR-0004](0004-account-authentication.md) (Account
  authentication and owner Password Reset), [ADR-0015](0015-audit-log.md)
  (canonical Audit Log), [ADR-0017](0017-username-identity.md) (normalized
  Username identity), [ADR-0024](0024-web-admin-sole-owner-surface.md) (Web
  Admin owner surface and its per-IP PIN throttle),
  [ADR-0037](0037-contention-safe-password-change.md) (Password Change),
  [ADR-0050](0050-web-admin-pin-rotation.md) (session-gated Web Admin PIN
  rotation), [ADR-0056](0056-kiosk-lock-always-on-top.md) (Kiosk Lock throttle),
  [ADR-0058](0058-audit-derived-web-admin-notifications.md) (Audit-derived Web
  Admin Notifications)

## Context

Account login is the remaining weak-credential path without a throttle. An
attacker can move between PCs and rapidly guess another customer's Password,
putting portable Remaining Time and Points at risk.

The two existing throttles deliberately solve different problems. Web Admin
PIN login is keyed per peer IP and uses an escalating `60 → 120 → … → 900`
second backoff (ADR-0024). Kiosk Lock release reuses that escalation with one
local counter (ADR-0056). Account login is different: the thing that needs
protection is the Account, while the person who can be harmed by escalation is
the Account's owner. A per-Username escalating window would therefore be a
weapon aimed at the victim rather than the attacker.

## Decision

- Throttle **Account login only**. Registration and Password Change are
  unaffected. Password Change is already session-gated; the reasoning in
  ADR-0050 for not rate-limiting Web Admin PIN rotation applies here too. The
  Web Admin PIN and Kiosk Lock throttles remain unchanged.
- Key the throttle on the normalized **Username**: the trimmed,
  ASCII-lowercase identity defined by ADR-0017. Do not key it on peer IP or
  `pc_id`.
- Only a Username that resolves to an existing Account receives a bucket.
  Unknown Usernames are never tracked.
- Track consecutive failed Account logins. Ten consecutive failures produce a
  flat, repeatable **60-second lockout** for that Username. The window expires
  automatically and never escalates. While locked, refuse the attempt before
  verifying the Password.
- Run the throttle check before the Shop Closed/open gate and before Password
  verification. A locked-out attempt therefore does not exercise the
  credential or create an unrelated shop-closed denial.
- Clear a Username's bucket after a successful login or an owner-performed
  Password Reset. There is no owner "clear lockout" control.
- Keep throttle state in memory only. There is no table or migration; an Admin
  restart clears all buckets.
- Return a throttled login as HTTP `429` with the remaining lockout seconds in
  `Retry-After` and a customer-readable wait message.
- Continue recording each failed credential attempt in the Audit Log as
  `login_failed`. A refusal during an active lockout is not a credential
  attempt and emits no per-attempt failure event. Emit one
  `account_login_throttled` event when a Username enters lockout, not one event
  per refused attempt. Its metadata records the normalized Username, the
  diagnostic source IP (not a key), the consecutive failure count, and the
  lockout length. Classify this event as an informational Web Admin Notice
  under ADR-0058; per-attempt failures remain the detailed history.

## Considered options

### Keying on peer IP or `pc_id`

Rejected. A customer can move between PCs, so a per-IP bucket does not follow
an Account. `pc_id` is client-supplied and can be rotated or falsified; either
key would let an attacker obtain fresh guesses instead of protecting the
Account.

### Reusing the escalating attempt-state type as-is

Rejected. Its escalation is appropriate for an owner credential but harmful
when the key is the victim's Username: a griefer could drive an innocent
customer into progressively longer lockouts.

### Generalising the existing type into a shared policy-configurable type

Rejected. The existing Web Admin and Kiosk Lock policies are settled and
working. A configurable abstraction would force a refactor across them and
make this deliberate policy divergence look like incidental configuration.

### Capping the existing escalation at sixty seconds

Rejected. A capped escalating type would retain state and fields whose
escalation no longer has meaning, inviting a later change to restore the
wrong policy. This throttle is a separate flat policy, not a damaged copy of
the existing one.

### Bucketing unknown Usernames

Rejected. An attacker could submit endless made-up strings and grow the
Admin's in-memory state without limit. Unknown identities have no Account to
protect, so they do not receive buckets.

## Consequences

- Differential throttling creates a Username-enumeration oracle: eventually
  observing a lockout distinguishes an existing Account from an unknown
  Username. This is accepted because Usernames are semi-public in the shop and
  registration already permits enumeration.
- Lockout state is lost on Admin restart. This keeps the mechanism simple and
  avoids schema/migration work; restarting is also a deterministic way to
  clear transient state.
- Anyone who knows a Username can hold that Account in a repeating
  sixty-second lockout by continuing to fail. This griefing remains possible,
  but the flat window prevents the throttle from escalating the victim's harm.
- A patient attacker who stays under the threshold — nine failures per minute
  indefinitely — is never throttled and never notified. The only signal is the
  per-attempt `login_failed` Audit Log events; sustained-failure detection is a
  separate future policy.
- The Account login policy is visibly distinct from the Web Admin PIN and
  Kiosk Lock policies: normalized Username key, ten-failure threshold, and
  fixed sixty-second windows. Successful login and Password Reset provide
  recovery without another owner control.
- Lockout transitions produce one informational owner Notification while the
  Audit Log retains every failed attempt, balancing awareness against feed
  noise.
