# ADR-0087: Publish source under PolyForm Shield 1.0.0

- Status: Accepted
- Date: 2026-10-14
- Supersedes: source-privacy part of [ADR-0086](0086-admin-app-update.md)

## Context

Public source lets shops inspect, build, and adapt OpenPiso without exposing
local records, deployment secrets, or internal investigation material. The
maintainer wants to permit shop operation without enabling competing products.

## Decision

Publish the source under **PolyForm Shield 1.0.0**, with Rafael Villanueva's
Required Notice in the root `LICENSE`. This is **source-available**, not
OSI-approved open source. Shops may use it free of charge, including to run
their business. Providing a competing product is prohibited under the license's
Noncompete terms, including selling/reselling OpenPiso as a competing substitute.
The full license controls, including its stated exceptions; this summary adds
no restrictions. Contributions are accepted under the same project license.

Publish Releases and the signed Admin updater feed in the public source
repository. ADR-0086's amended bridge plan moves existing installs from the
legacy feed without changing the signing key. Publishing source does not
change update authority, signing, or LAN boundaries.
Third-party code and fonts retain their licenses, documented in
`THIRD-PARTY-NOTICES.md`. Both NSIS installers display the project license.

## Consequences

Forks must retain required notices, observe the noncompete terms, and configure
their own release repository, updater keys, and secrets. Private shop data and
build secrets remain untracked. ADR-0086's source-private decision is superseded;
its Admin-only update and rollback rules remain in effect.
