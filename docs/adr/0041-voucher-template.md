# ADR-0041: Owner-only Voucher Template rendering

- Status: Accepted
- Date: 2026-07-21
- Deciders: shop owner (product)
- Refines: ADR-0024 (web-admin owner surface), ADR-0040 (Voucher Batch)

## Context

Voucher Batches are reopenable for re-print, but each shop needs control over the physical card layout. Template content is trusted owner-authored HTML and CSS, while Voucher values remain server-owned data. Executable JavaScript is unnecessary and would expand the browser security surface.

## Decision

OpenPiso stores one shop-wide **Voucher Template** in the admin `settings` table as HTML and CSS. A migration seeds a built-in printable card; the owner-only web-admin API reads and atomically updates both parts. The API also returns the built-in default so reset-to-default restores and persists it.

The HTML is a per-card fragment. Its CSS owns the complete print layout, including `@page`, the card grid, and page-break rules. The owner's browser repeats the fragment once per Voucher and performs pure substitution for `{{code}}`, `{{time}}`, `{{points}}`, `{{price}}`, `{{shop_name}}`, `{{expires_at}}`, and `{{prefix}}`. Every substituted value is HTML-escaped; a missing value becomes an empty string.

Rendering occurs only in sandboxed owner-browser iframes for preview and print. Generated documents prohibit scripts with CSP and remove script elements before rendering. No customer surface receives or executes Voucher Template content. Printing uses the browser-native print dialog and can reload any past Voucher Batch.

## Consequences

- Rust remains authoritative for stored Voucher and shop data; formatting is browser-only.
- Owners can author HTML/CSS, but JavaScript does not execute in v1.
- The built-in default exists both in the migration seed and application fallback/reset constants; changes must keep them synchronized.
- Browser print results depend on the owner's browser and printer settings.
