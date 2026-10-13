# ADR-0065: Phone-designed vs phone-tolerable Web Admin screens

- Status: Accepted
- Date: 2026-08-09
- Deciders: shop owner (product)
- Amends: the **bare-Settings resolution** portion of [ADR-0051](0051-web-admin-url-routing.md)
- Relates to: [ADR-0024](0024-web-admin-sole-owner-surface.md) (Web Admin as
  the sole owner surface)

## Context

The Web Admin is the sole owner surface (ADR-0024), and the owner commonly
reaches it from a phone on the shop LAN. A desktop layout that merely survives
a narrow viewport is not enough for the owner to issue a Voucher, make an
Adjustment, inspect an Account, or read the Sales Report during a shift. The
phone work must also avoid creating a second set of rules that screens can
silently escape as the surface grows.

Not every screen needs the same level of phone-specific composition. The
decision therefore needs to bind both a self-describing screen class and the
mechanics that class requires. The class list alone would rot when a new screen
is added: its owner would not know whether it needs cards, scrolling, or only
basic operability.

## Decision

### Terms and screen classes

A **phone-designed** screen treats a phone, down to the supported 360px floor,
as a design target. Its primary workflows are composed for one-handed use;
where it has a data table covered by the responsive-row seam below, its rows
become stacked cards below the breakpoint.

A **phone-tolerable** screen remains desktop-first but is fully readable and
operable on a phone. Its tables may retain horizontal scrolling, and it does
not receive a bespoke card layout merely because it is being viewed on a
phone. Tolerable does not mean disabled, clipped, or inaccessible. These terms
are defined in this ADR because their meaning is specific to this decision; no
glossary entry or glossary change is needed.

The classes are closed until a later ADR changes them:

- **Phone-designed:** Dashboard, Vouchers, Adjustments, Transfers, Accounts,
  Notifications, Sales Report.
- **Phone-tolerable:** Audit, Backups, Settings, including every Settings
  section.

The six table surfaces covered by the phone-designed card treatment are the
two Vouchers tables, three Accounts tables, and the Sales Report PC breakdown.
The Audit, Backups, and Settings tables (including Coin Promo, Points, and Shop)
keep horizontal scrolling. A screen added later must be assigned to one of the
two named classes and follow that class's obligations before it ships.

### One responsive switch point

There is one responsive switch at **768px**. Below 768px, phone-designed table
surfaces use their card variant; at 768px and above they retain their desktop
table variant. The switch is not pushed down to 640px: a 768px tablet in
portrait cannot usefully read a wide multi-column row, and waiting until 640px
would make that device endure the desktop layout despite having the same narrow,
one-handed constraints.

The same below-768px boundary governs the phone-specific shell tightening,
16px form controls to prevent iOS focus zoom, and thumb-usable touch-target
sizing. Desktop density and control sizing remain unchanged at 768px and above;
fixing a phone must not redesign the monitor layout.

### `ResponsiveRows` owns the table/card seam

`ResponsiveRows` is the one shared component for a responsive row collection.
Its contract is:

- it owns the row collection;
- it owns iteration over that collection;
- the screen supplies a row snippet and a card snippet; and
- each snippet receives one item and supplies that screen's cell/card markup.

The component branches in JavaScript using the existing mobile media-query hook,
not by rendering both branches and hiding one with CSS. Exactly one variant
exists in the DOM at a time. The shared iteration makes it structurally
impossible for the table and cards to show different records, while keeping
per-screen formatting — outcome pills, money formatting, actions, and other
cell-level decisions — next to the screen that owns it.

Both variants must receive the same row collection. The card is allowed to
reorder or group the fields for phone reading, but it must not omit a record or
quietly change the screen's data semantics.

### Overlays convert by width

Transactional overlays convert by width, not by the screen class beneath them.
Below 768px, an overlay wider than `max-w-md` becomes a full-screen sheet using
the existing sheet primitive. Narrow confirmations remain compact centered cards
when they fit within the 360px floor. This converts wide Account Import,
Backup restore confirmation, and Theme confirmation overlays even though Backup
and Settings are phone-tolerable: width predicts whether the overlay is broken,
not the class of the page that opened it. A broken restore action is worse than
an unoptimized table.

Informational content remains inline. Only transactional dialogs use the
full-screen sheet treatment.

### Navigation and Settings

The mobile navigation sheet closes when the owner selects a screen. A bottom tab
bar is not introduced. The navigation model remains the existing sidebar/sheet
model, and the route table remains flat and closed.

The bare `/admin/settings` path is the one deliberate viewport-dependent
rendering exception. At 768px and above it keeps ADR-0051's existing behaviour:
it resolves to Points and replaces the current history entry. Below 768px it
renders a Settings section index listing every section, so the owner chooses a
section before entering it. Named Settings URLs, deep links, login
preservation, Back/Forward, and every other route keep ADR-0051's URL authority.
An unknown Settings section still follows ADR-0051's useful default resolution;
this decision does not create aliases or new routes.

This is the single viewport-dependent URL-path rendering exception. It is
confined to `/admin/settings`, a path that already had special resolution
behaviour and did not name a screen. If a second path needs the same exception,
that is evidence this approach was wrong and must be revisited rather than
extended by convention.

## Considered and rejected

- **CSS-only duplicate markup:** rendering `hidden md:table` and
  `md:hidden` duplicates every data node. It permits the two iterations to
  drift, and environments without CSS see both variants. Rejected in favour of
  JavaScript branching with exactly one DOM variant.
- **A column-config table component:** a generic column table would centralize
  the wrong seam, fight per-screen formatting and actions, and make the card
  field layout mechanical. Rejected; `ResponsiveRows` shares collection and
  iteration while screens retain readable row/card snippets.
- **A bottom tab bar:** a second navigation model would need to stay in sync,
  hide the breadth of the owner surface, and consume scarce phone vertical
  space. Rejected; the existing navigation sheet closes on selection instead.
- **A drawer dependency:** adding a drawer library for responsive overlays adds
  production dependency and capability surface when the existing sheet
  primitive is sufficient. Rejected; use the vendored sheet primitive and make
  the conversion a width rule.

## Consequences

- The owner gets phone-designed workflows for the seven high-frequency screens,
  while Audit, Backups, and Settings remain fully usable without forcing a
  costly redesign of their tables.
- `ResponsiveRows` prevents table/card record-set drift, but the two snippets
  still intentionally permit field-layout drift. The owner reviews those card
  fields on a real phone.
- Wide transactional dialogs remain usable regardless of the page class that
  opens them. Narrow confirmations retain their compact desktop composition.
- Phone-only density changes preserve the existing desktop experience.
- The Web Admin has one bounded URL exception: the already-special bare
  Settings path. The URL remains authoritative for every named route, and a
  second viewport-dependent path is a design failure signal.
- No Rust, API, database, authentication, route-table, or authority change is
  implied. Phone support remains a Web Admin presentation concern.

## Implementation boundary

The implementation must keep the class names and this breakpoint literal in
shared documentation/code, use `ResponsiveRows` for the listed phone-designed
table surfaces, and test the seam's desktop branch, phone branch, and identical
record iteration. Visual density, touch comfort, overlay composition, and the
per-screen card fields are verified on a real phone over the shop LAN rather
than by asserting CSS class names. The mobile media-query API belongs in the
test setup, not behind a production-only fallback branch.
