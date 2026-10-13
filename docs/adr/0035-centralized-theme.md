# ADR-0035: Centralized theme — one shop-wide accent, per-surface light/dark, server-stored

- Status: Accepted
- Date: 2026-07-17
- Deciders: shop owner (product)

## Context

All three surfaces (web-admin, admin kiosk, client) import the same static
`packages/ui/src/theme.css` — shadcn oklch variables with a green primary ramp.
The only theming control anywhere is web-admin's **browser-local** ThemeToggle
(`theme.svelte.ts` + localStorage), which themes one browser, not the shop:
kiosk and client have no mode control at all, and the accent is hardcoded.

The owner wants the shop to look like *their* shop — one accent color and a
light/dark choice per surface — set once, applied everywhere, live.

## Decision

### Server-stored raw record; TypeScript owns the color math

The theme is a typed singleton SQLite table `theme_settings` (wheel-style
`id = 1`, atomic `version` bump per write): a hex accent (`#rrggbb`,
lowercase), three per-surface modes (`light | dark` — no `system`; surfaces
are shop-controlled), and one integer version. Rust validates
(regex/enum, full-record PUT, last-write-wins per ADR-0011) but **never does
color math** — a pure, dependency-free TS **ramp generator** in `packages/ui`
turns the record into CSS.

Endpoints: owner `GET`/`PUT /api/theme` on :8080 (`OwnerSession`); public
`GET /api/theme` on :3210 (needed pre-login on the lock screen; a color is not
a secret). `theme_version` is stamped into the heartbeat response and
`GET /api/shop`; consumers refetch only when it moves.

### Ramp generation: anchor snap

The picker controls one accent; the generator derives the ramp (`primary`,
`primary-foreground`, `chart-1..5`, `sidebar-primary`, `ring`) for both modes
by keeping the current green ramp's **lightness anchors exactly** and taking
only hue (and scaled, capped chroma) from the pick. Pick lightness is
deliberately ignored: contrast is structurally guaranteed by the fixed
anchors, and extreme picks (near-white, near-black, gray) degrade gracefully
instead of breaking legibility. Neutrals and `destructive` are untouched.
Generating from the seed reproduces today's ramp exactly, so rollout is a
visual no-op.

### Runtime application, live propagation

Each app injects one swapped `<style id="openpiso-theme">` element over the
static `theme.css` (cascade override; the `dark` class on `<html>` is kept and
now driven solely by the central mode). Each app caches the **raw record** in
localStorage and regenerates at boot before mount — offline-safe and
generator-upgrade-safe, which a cached CSS string would not be. Delivery
reuses existing channels: client = heartbeat-tracked version → Rust fetch →
`theme-updated` Tauri event; kiosk = direct Tauri push from the PUT handler +
`get_theme` command at startup; web-admin = one ~15 s root-layout poller.

Web-admin's browser-local ThemeToggle is **deleted**; the legacy
`openpiso.theme` key is removed at boot.

### Owner surface

Settings → Appearance: accent hero row (picker + presets + reset-to-green)
over three surface cards, each with its own light/dark toggle and a live
mini-preview rendered with the generated palette. Save goes through a confirm
dialog (current→new swatches + a live-mid-session warning) — deliberate
friction, since the theme hits rented PCs mid-session.

### No audit event — deliberate deviation

Theme writes record **no** audit event, deviating from the
`*_settings_updated` convention. ADR-0015's scope is who-did-what and
value-moving events; the theme is cosmetic, moves no money/time/points, and is
single-owner. If staff roles ever arrive (ADR-0011 revisited), reconsider.

## Out of scope

Radius/density controls, logo/branding upload, per-PC or per-surface accent
colors (one shop-wide accent only).

## Consequences

- New migration + two owner endpoints + one public endpoint;
  `theme_version` rides the heartbeat and `/api/shop`.
- `packages/ui` gains its first runtime TS export (the generator); all three
  apps gain a small boot-time theme module.
- The dark ramp has not been judged at real size on kiosk/client
  (light-only in practice today) — an explicit pre-ship acceptance check in
  the spec; any fix is component-level, not a generator change.
- `docs/api.md`, `docs/database.md`, and `docs/architecture.md` are updated
  with the build.
