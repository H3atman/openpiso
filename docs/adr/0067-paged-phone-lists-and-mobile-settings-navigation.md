# ADR-0067: Paged phone lists and mobile Settings navigation

- Status: Accepted
- Date: 2026-08-10
- Deciders: shop owner (product)
- Amends: [ADR-0065](0065-phone-designed-vs-phone-tolerable-owner-screens.md)
- Relates to: [ADR-0051](0051-web-admin-url-routing.md)

## Context

ADR-0065 established `ResponsiveRows` as the shared seam for the
phone-designed table surfaces. It also described the mobile owner navigation
as the existing sidebar/sheet model, while the implementation had an
unnecessary mobile gate that hid Settings children. The Accounts screen now
needs a bounded list for phone use without weakening the seam's record-set
invariant.

## Decision

### Paged phone lists

A phone-designed screen may page its already-filtered and already-sorted
collection above `ResponsiveRows`. The screen owns a local shown-count,
initially ten, and reveals ten more per explicit **Load More** action. The
screen passes the sliced collection as the seam's one `rows` collection;
`ResponsiveRows` gains no paging props and remains unaware of paging.

This is a bounded carve-out to ADR-0065's prohibition on record omission: the
screen may omit records that have not yet been revealed, but both the desktop
row branch and the phone card branch still receive and iterate the exact same
sliced collection. The seam therefore continues to prevent branch-specific
record omission or drift. This decision is intentionally screen-local rather
than a new generic paging abstraction.

The shown-count resets to ten when the screen's question changes through a
Status filter, Search term, or sort column/direction change. A plain refresh
keeps the count because it re-asks the same question. The action is explicit,
full-width, and phone-sized; it reports the shown and total matching counts.
After revealing a page, focus moves to its first newly revealed Account.
Paging applies to both the desktop and phone branches. Existing selection
semantics remain unchanged, including selections for Accounts currently
outside the revealed slice.

### Mobile Settings navigation

The mobile navigation renders a Settings item's children through the same
collapsible submenu as desktop. Tapping the Settings parent toggles its
children without navigating or closing the navigation sheet; choosing a child
navigates and closes the sheet. This reverses the mobile submenu gate that
existed when ADR-0065 was written and records the behavior delivered in #310,
so the gate must not be restored by reading the original ADR.

This amendment does not change the separate bare `/admin/settings` route
exception: below 768px that path still renders the Settings section index,
as decided in ADR-0065.

## Consequences

- Large phone-designed collections remain bounded and scannable without
  duplicating responsive iteration logic.
- A screen that needs paging implements the small screen-owned slice and
  count logic; `ResponsiveRows` stays a simple responsive collection seam.
- Owners can reach every Settings section from the mobile navigation while
  retaining the existing sidebar/sheet model and URL authority.
- The two branches can still be reviewed as an identical record set after the
  screen applies paging.
