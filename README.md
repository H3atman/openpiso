# OpenPiso

Local-first LAN coinslot and session control for Pisonet shops. Rust owns money,
time, sessions, and SQLite; shop operation does not require cloud services.

- **Admin** — Central Terminal, coin input, database, timers, LAN API (`:3210`).
- **Web admin** — owner dashboard served by Admin (`:8080/admin`).
- **Client** — per-PC timer, heartbeat, lock screen; Windows Client Guard service.

[Download Windows installers](https://github.com/H3atman/openpiso/releases/latest).
Admin supports signed, owner-initiated updates; Client installation is manual.

## License

[PolyForm Shield 1.0.0](LICENSE): source-available, **not OSI open source**.
Free to use, including running a shop, and to inspect/adapt for permitted purposes.
No providing competing products, including selling/reselling it as a competing
substitute. The full license controls, including its exceptions.
See [third-party notices](THIRD-PARTY-NOTICES.md).

## Requirements

- Bun, Rust stable, [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/).
- Windows deployment: WebView2; C++ Build Tools for builds. Client Guard installation
  requires administrator approval. Serial coin input needs a configured acceptor.
- PCs on the same trusted LAN; do not expose Admin mutation endpoints publicly.

## Develop / build

```bash
bun install --frozen-lockfile
bun run dev:admin        # builds web admin, then starts Admin
bun run dev:client       # separate terminal; point Client at Admin's LAN address
bun run dev:web-admin    # optional owner UI hot reload
bun run check
bun run build:web-admin
bun run build:installers # Windows NSIS installers; no updater signing secrets
```

`bun run build:admin` enables updater artifacts and needs your own
`TAURI_SIGNING_PRIVATE_KEY`; see [releasing](docs/releasing.md) for signed builds
and fork configuration. On non-Windows hosts, installer builds use cargo-xwin.
Never commit signing keys or shop records.

Telemetry is compiled in only when the builder supplies their own
`OPENPISO_POSTHOG_TOKEN`. Plain builds send nothing. Token-bearing builds send
anonymous daily Admin summaries; details in [development](docs/development.md)
and [ADR-0083](docs/adr/0083-install-telemetry.md).

## Documentation

[Installation](docs/installation.md) · [Development](docs/development.md) ·
[Architecture](docs/architecture.md) · [API](docs/api.md) ·
[Database](docs/database.md) · [Hardware](docs/hardware.md) ·
[Glossary](docs/glossary.md) · [Decisions](docs/adr/README.md)

[Contributing](CONTRIBUTING.md) · [Security reporting](SECURITY.md)
