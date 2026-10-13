# AGENTS.md

OpenPiso is a source-available (PolyForm Shield 1.0.0), local-first LAN coinslot and session-control system
for Pisonet shops:

- **Admin** (`apps/admin`) — Tauri app: owns SQLite, timers, the coin pipeline,
  the client/device API (`:3210`), and the owner web admin (`:8080`).
- **Web admin** (`apps/web-admin`) — Svelte owner web app; the sole owner
  surface (ADR-0024).
- **Client** (`apps/client`) — per-PC Tauri app: timer, heartbeat, lock screen.
- **`crates/openpiso-core`** — shared Rust domain logic.
- **`firmware/esp32-coinslot`** — placeholder coin-acceptor firmware.

The MVP is shipped; current work is the expansion phase (accounts, points,
vouchers, tamper response). SQLite is the source of truth.

## Read These First

These are authoritative; do not restate or contradict them:

- `docs/glossary.md` — ubiquitous language. Use these exact terms in code,
  schema, API, and UI.
- `docs/adr/` — architecture decisions (see `docs/adr/README.md`). The relevant
  ADR governs any area you touch.
- `docs/development.md` — commands, run order, and the "What's where" layout
  table. Also: `architecture.md`, `api.md`, `database.md`, `hardware.md`,
  `installation.md`.
- Client browser verification: [docs/development.md#browser-test-harnesses](docs/development.md#browser-test-harnesses)
  (only when running Client browser harnesses).

## Stack & Tooling

- **Bun** for package management — never pnpm, npm, or yarn.
- **Tauri v2**, **Vite**, **Svelte**, **TypeScript** (strict), **Rust**, **SQLite**.
- Common checks (full list in `docs/development.md`):

  ```bash
  bun run check
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  bun run check:windows   # Windows clippy via cargo-xwin; when not on Windows
  ```

  Run the relevant checks before considering a task done; run all of them for
  cross-cutting changes.

## Durable Invariants

These hold everywhere and override any habit:

- **Rust owns all authority** — money, time, points, sessions, routing. The
  frontend and devices are thin clients.
- **The server computes** `seconds_added` and any authority value. Never trust a
  device- or frontend-supplied time, amount, or id.
- **Money is integer cents; authority durations are integer seconds.** No floating
  point for money or time. Remaining time never goes below zero. **Kill Report
  Age**, diagnostic **Client clock difference**, and the **Forwarded Diagnostic
  device capture timestamp** are narrow protocol-only integer-millisecond
  exceptions; none changes balances or timers.
- **Any money/time/transaction change runs inside a SQLite transaction.** SQL
  lives in db/repository modules or migrations, never in UI command handlers.
- **The frontend never writes payment/session records** directly — go through a
  Rust Tauri command or HTTP handler.

## Code Conventions

- Informational content in the Widget must render inline, never as a `fixed`
  overlay. Transactional dialogs (forms, confirmations) may use `fixed inset-0`
  — they already occlude the widget, which is acceptable for a focused task.
  All `fixed` overlays in the Widget view tree must go through the
  `WidgetOverlay` component.
- Frontend: API calls in `src/lib/api.ts`, shared types in `src/lib/types.ts`,
  formatting in `src/lib/format.ts`. Keep components UI-only; prefer readable
  components over clever abstractions; avoid extra state-management libraries.
- Web Admin action outcomes report via toast; load failures and standing advisories stay inline.
- Rust: `Result<T, AppError>` for fallible paths; no `unwrap()`/`expect()` in
  production paths; explicit domain types; `cargo fmt --all` before finishing.
- SQLite: migrations for schema changes; never edit released migrations (add new
  ones); foreign keys on; WAL mode; index report/transaction queries.
- Prefer small incremental changes and editing existing files over rewrites or
  parallel implementations. Inspect the existing structure before inventing a
  pattern.

## Security

- Never commit secrets, Wi-Fi passwords, device keys, or test credentials.
- Keep simulator mode clearly separate from real coin-device mode; real
  coin-device events require a device secret, with HMAC verification before they
  are treated as production-ready.
- Validate all input at the Rust boundary.
- Do not expose admin mutation endpoints to the public internet.

## Agent skills

Optional references: `bunx skills add dchuk/claude-code-tauri-skills` and `bunx skills add huntabyte/shadcn-svelte --skill shadcn-svelte`; read relevant skills before Tauri/UI work, and follow project rules over generic examples.

## Tauri

- Keep capabilities minimal; do not enable shell, process, or broad filesystem
  access unless the task explicitly requires it. When adding a plugin, update
  capabilities and document why.
- Prefer typed Tauri commands for desktop-only operations and the admin HTTP API
  for LAN client/device communication. Keep sidecars out of the MVP.

## Testing & Docs

- Add or update tests when changing pricing, add/subtract-time behavior, DB
  mutations, transaction logging, API payload types, or timer behavior.
- Update the relevant `docs/` file (and add an ADR for a new decision) when
  changing architecture, APIs, schema, or hardware behavior.

## Files

- Keep this file concise; put long details in `docs/`, durable project-wide rules
  in `AGENTS.md`, and personal/local notes in a gitignored `CLAUDE.local.md`.

## Quality Bar

A task is complete only when the behavior is implemented, the code follows the
architecture rules, the relevant checks pass, and docs are updated when behavior
or setup changed — not merely when code was written.

## Do Not Do Without Explicit Approval

Replace Bun, Svelte, or SQLite; add cloud sync, payment-provider integrations,
telemetry, invasive OS restrictions, or a large UI framework; change the
license; or add production dependencies unrelated to the requested task.
