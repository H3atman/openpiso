# OpenPiso

Local-first LAN coinslot and session control for Pisonet shops; SQLite is the source of truth.

- `apps/admin`: Tauri app owning SQLite, timers, the coin pipeline, the client/device API (`:3210`), and the web admin server (`:8080`).
- `apps/web-admin`: Svelte owner web app, the sole owner surface (ADR-0024).
- `apps/client`: per-PC Tauri app for timer, heartbeat, and lock screen.
- `crates/openpiso-core`: shared Rust domain logic.

## Invariants

- **Authority**: Rust owns money, time, points, sessions, and routing; frontends and devices are thin clients. The server computes `seconds_added` and all authority values; never trust device/frontend-supplied time, amount, or id.
- Money uses integer cents; authority durations use integer seconds; remaining time stays nonnegative.
  Kill Report Age, diagnostic Client clock difference, and the Forwarded Diagnostic device capture timestamp are protocol-only integer-millisecond exceptions; none changes balances or timers.
- **Atomic**: every money/time/transaction change runs inside a SQLite transaction.
- Frontends use Rust Tauri commands or HTTP handlers; never write payment/session records directly.
- Use Bun exclusively for package management.
- Keep secrets outside commits; never commit Wi-Fi passwords, device keys, or test credentials.

## Approval required

Obtain explicit approval before replacing Bun, Svelte, or SQLite; adding cloud sync,
payment-provider integrations, telemetry, invasive OS restrictions, a large UI framework,
or production dependencies unrelated to the task; or changing the license.

## Read by branch

- **Code or code review** → [CODING_STANDARDS.md](CODING_STANDARDS.md), matching area sections.
- **Domain language** → [docs/glossary.md](docs/glossary.md), for code, schema, API, and UI terminology.
- **Design or behavior changes** → [docs/adr/README.md](docs/adr/README.md); read the governing ADRs before changing their area.
- **Setup, execution, or verification** → [docs/development.md](docs/development.md), for commands, run order, and layout.
- **API changes** → [docs/api.md](docs/api.md).
- **Schema changes** → [docs/database.md](docs/database.md).
- **Hardware changes** → [docs/hardware.md](docs/hardware.md).
