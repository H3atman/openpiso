# ADR-0090: Client adoption and server discovery

- Status: Accepted
- Date: 2026-11-17
- Deciders: shop owner (product)
- Supersedes: ADR-0031 §2 step 3 ("reject unknown, pre-register only") and its
  rejected option "auto-create a pending/hidden PC". The rest of ADR-0031
  (config persistence, MAC-keyed identity, hardware replacement) stands.
- Relates to: ADR-0024 (web-admin sole owner surface), ADR-0027 (Owner PIN
  gate), ADR-0075/0076 (`registered_ip` consumers), ADR-0089 (client UI state
  machines).

## Context

Connecting one PC costs the owner four typed fields: PC id, display name and
static IP in the Web Admin, then the full `http://<ip>:3210` URL on the Client.
The flow also has traps: a row added without the exact static IP never binds
and nothing says why; unknown Clients are rejected silently, so the owner
cannot see who is knocking; the Admin never shows its own address; and a
mistyped URL can only be changed behind an Owner PIN the Client cannot verify.

ADR-0031 rejected auto-creating PCs because the identity endpoint is
unauthenticated, so any LAN device could spawn unbounded junk rows. That
objection is to *rows*, not to letting the owner see and approve a knock.

## Decision

### 1. Unknown Clients become Pending PCs; the owner adopts them

A heartbeat whose MAC and source IP resolve to no `pcs` row is still rejected
with `pc_unregistered`, but the server now remembers it as a **Pending PC**:
MAC, source IP, hostname, Client version, last seen.

- Pending PCs live **in memory only**, never in SQLite. The list is capped and
  entries expire shortly after the Client stops knocking, so the
  unauthenticated endpoint cannot grow anything durable. A report without a
  MAC is never pending.
- The rejection carries an **Approval Code**, a short server-derived code the
  Client shows on its Lock and the Web Admin shows beside the Pending PC, so
  the owner can match a screen to a list entry.
- The owner **approves** a Pending PC in the Web Admin (authenticated Owner
  Session). Approval is the only thing that creates the `pcs` row: the server
  assigns the next free id and a default display name the owner may override,
  binds the MAC, and records the observed source IP as `registered_ip` when no
  other row holds it. The next heartbeat identifies by MAC (ADR-0031 step 1).
- Pre-registration by static IP (ADR-0031 step 2) keeps working unchanged.

No money, time or session authority changes: an unapproved Client has no PC
row and can hold nothing.

### 2. An unconfigured Client discovers the Admin by UDP broadcast

A Client with no saved server URL broadcasts a discovery request on UDP
`3210`; the Admin answers with its device API URL and shop name. One answer →
the Client saves it and connects; several → the operator picks. Manual entry
remains as the fallback and accepts a bare host.

**Discovery runs only while the Client has no saved server URL.** A configured
Client never follows a broadcast answer, so a device joining the LAN later
cannot pull configured PCs to a fake server. Owner approval (§1) is the trust
step for a new Client; for the first connection the owner is physically at the
PC.

### 3. Onboarding is guidance on existing surfaces, not a wizard

The Web Admin shows the server's own LAN address and the live Pending PC list
after first-run PIN setup. The Client's setup flow is one ADR-0089 machine:
looking for server → waiting for approval → ready.

## Considered options

- **Pending PCs as hidden `pcs` rows.** Rejected for ADR-0031's original
  reason: durable rows from an unauthenticated endpoint.
- **Pairing code typed on the Client.** Rejected: the goal is zero typing on
  the Client, and approval in the Web Admin gives the same owner-in-the-loop
  guarantee.
- **mDNS for discovery.** Rejected: a new dependency and unreliable under
  Windows firewall and multi-NIC hosts. A UDP broadcast needs only tokio and
  one inbound firewall rule beside the existing TCP `3210` rule.
- **Rediscover the Admin after its address changes.** Deferred: following a
  broadcast from a configured Client is only safe with a per-PC secret issued
  at approval. Until then an address change is fixed through the PIN-gated
  settings.

## Consequences

- `pc_unregistered` gains an `approval_code` field; the Client's
  "unregistered" Lock copy becomes "waiting for owner approval".
- The Web Admin gains a Pending PC list with Approve; the Admin gains a UDP
  listener on `3210` and its installer a matching firewall rule.
- `registered_ip` may now be server-observed at approval rather than
  owner-typed. Shops on DHCP should reserve addresses if they use Remote
  Control or PC Monitoring, which still dial `registered_ip`.
- Fresh installs no longer need seeded demo PC rows.
