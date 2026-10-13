# Contributing

Read [AGENTS.md](AGENTS.md), [development](docs/development.md), the glossary,
and relevant ADRs first. Use Bun (not npm, pnpm, or yarn), stable Rust, and
Tauri v2 prerequisites. Keep changes small, typed, local-first, and in scope.

Before a PR, run relevant focused tests and checks; cross-cutting changes need:

```bash
bun run check
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bun run check:windows # non-Windows hosts with cargo-xwin
```

Run memory-heavy checks one at a time. Include what changed, why, verification
commands/results, and any platform checks not run. Update docs with behavior,
API, or setup changes; add an ADR for architectural decisions. Never include
secrets, shop records, or personal deployment data. Discuss substantial changes
in an issue first. Report vulnerabilities privately, per [SECURITY.md](SECURITY.md).

Contributions are accepted under the project's [PolyForm Shield 1.0.0 license](LICENSE).
Only contribute code you have the right to license; retain third-party notices.
