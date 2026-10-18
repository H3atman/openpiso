# Coding standards

## Frontend

- Keep components UI-only. Put API calls in `src/lib/api.ts`, shared types in `src/lib/types.ts`, and formatting in `src/lib/format.ts` within each app.
- Do not add a state-management library. In `apps/client`, hand-rolled state machines (below) are the state-management approach; elsewhere use the existing approach.

### Client UI state (apps/client)

Any new or edited client UI flow must be a state machine ([ADR-0089](docs/adr/0089-client-ui-state-machines.md)):

- Discriminated-union `State` and `Event` types, and a pure `reduce(state, event): State` with an exhaustive `switch` (`never` check) and no I/O.
- Side effects (Tauri `invoke`, `api.ts` calls, timers) live in a thin runner that owns a `$state` holding the machine state and dispatches result events.
- Machine modules are plain `.ts` under `src/lib/machines/`, not inside components.
- No ad-hoc `$state` booleans or request counters for flow state.

## Rust

- Use `Result<T, AppError>` for fallible paths and explicit domain types.
- Propagate or handle production errors; never use `unwrap()` or `expect()` in production paths.
- Format Rust with `cargo fmt --all` before finishing Rust changes.

## SQLite

- Keep SQL (and its Atomic transactions) in db/repository modules or migrations, out of UI command handlers.
- Add migrations for schema changes; never edit released migrations.
- Enable foreign keys and WAL mode; index report and transaction queries.

## Authority boundaries

- Validate all input at the Rust boundary.
- Separate simulator mode from real coin-device mode. Require a device secret and verify HMAC before treating real coin-device events as production-ready.
- Keep admin mutation endpoints LAN-only; never expose them to the public internet.

## Tauri

- Use typed Tauri commands for desktop-only operations and the admin HTTP API for LAN client/device communication.
- Grant minimal capabilities. Enable shell, process, or broad filesystem access only when explicitly required by the task.
- When adding a plugin, update capabilities and document why each grant is needed.
- Before Tauri or UI work, read relevant available skills; [optional skill references](docs/development.md#optional-agent-skills) are subordinate to project rules.

## Verification and documentation

- Add or update tests for pricing, add/subtract-time behavior, DB mutations, transaction logging, API payload types, and timer behavior changes.
- Run relevant checks from [development](docs/development.md); run all common checks for cross-cutting changes. Report failures and exact reasons checks could not run.
- For Client browser harnesses, follow [browser test harnesses](docs/development.md#browser-test-harnesses).
- Update the relevant `docs/` file for architecture, API, schema, hardware, behavior, or setup changes; record new architecture decisions in an ADR.
