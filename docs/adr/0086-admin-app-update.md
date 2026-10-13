# ADR-0086: Owner-initiated Admin App Update from public signed GitHub releases

- Status: Proposed; source-privacy decision superseded by [ADR-0087](0087-source-available-license.md)
- Date: 2026-10-13
- Deciders: shop owner (product)
- Relates to: [ADR-0083](0083-install-telemetry.md) (the other sanctioned
  outbound call), [ADR-0085](0085-migration-version-guard.md) (why rollback is
  schema-bounded), [ADR-0054](0054-backup-and-restore.md) (Pre-Migration
  Backups), [ADR-0024](0024-web-admin-sole-owner-surface.md),
  [ADR-0045](0045-offline-shutdown-countdown.md).

## Context

Admin installs are updated by hand-copying an NSIS installer. There is no CI,
no release host, and no updater. The source repository was initially private;
a separate public feed avoids shipping repository tokens to installed Admins.
Source publication is now governed by ADR-0087.

## Decision

- **Updater:** `tauri-plugin-updater`, Admin only. Clients keep their manual
  path. Stable releases only; no release channel.
- **Host (amended 2026-10-14):** CI publishes Releases in the **public source**
  repository (`H3atman/openpiso`), using the built-in `GITHUB_TOKEN` with
  job-scoped Contents write permission: the Admin NSIS installer,
  its minisign `.sig`, a per-release Tauri `latest.json`, and a `release.json`
  carrying `version`, `schema_version`, `notes`, and `pub_date`. Source is
  published under PolyForm Shield 1.0.0 (ADR-0087). The same release also
  includes the Client NSIS installer with its Guard service, for manual
  installation only (`perMachine`, UAC required). Client is never included
  in `latest.json` and has no updater.
- **Signing:** the Admin updater installer is minisign-signed; the public key ships in the
  Admin config. The private key lives in GitHub secrets with an offline backup.
  Losing it strands every installed Admin.
- **Egress:** the second sanctioned outbound call. Admin anonymously lists
  public releases on startup and every 6 hours, and on demand. Owners can turn
  automatic checks off in the update panel; the default is on. Nothing about
  the shop is sent.
- **Compatible Rollback:** only older stable releases whose `schema_version`
  equals the database's are offered. Crossing a schema boundary backwards
  stays a Pre-Migration Backup restore (ADR-0085). The new feed starts at the
  bridge release; versions published only in the legacy repository are no
  longer offered, even when their Schema Version matches.
- **Gates:** an App Update that raises the Schema Version requires an explicit
  "no rollback below this version" acknowledgement. A Compatible Rollback
  requires typing the target version. Running sessions produce a warning
  with the PC count, not a block.
- **Surface:** owner-authenticated web-admin routes on `:8080` only, never on
  `:3210`. Rust checks, verifies, gates, and installs; the browser shows
  state and sends intents.
- **Install:** NSIS `passive` in the existing `currentUser` mode (no UAC, and
  the HKCU autostart stays valid). The kiosk window is hidden first. A restart
  marker lets the next start report success or failure.
- **Restart impact:** Clients lose the server briefly and return to the
  Fullscreen Lock under the existing Offline Shutdown Countdown. Time is
  server-authoritative and preserved. A planned-restart notice is deferred.
- **Release notes:** drafted locally by `bun run version:*` (AI draft with a
  conventional-commit fallback), reviewed by the maintainer, committed to
  `docs/releases/vX.Y.Z.md`, and used by CI as the release body. CI adds a
  database-change warning when the Schema Version rose.

## Considered Options

- Publish source: originally deferred; superseded by ADR-0087.
- Embed a read-only token: rejected (secret on the client).
- Unrestricted downgrade or auto-restore of backups: rejected (loses money
  records written since the update).
- Block installs while sessions run: rejected; a short lock is acceptable
  because time is preserved.
- AI notes generated in CI: rejected (needs an API key secret, and publishes
  unreviewed text).

## Consequences

- Shops without internet never see updates; sideloading the NSIS installer
  stays supported.
- Every release must record its Schema Version, so CI derives it from the
  migration table.
- The signing key stays unchanged. A feed change requires a bridge: installs
  ≤0.12.0 hardcode `H3atman/openpiso-releases` and validate its asset URL prefix.
  Publish the first source-repo release to both repositories, regenerating the
  legacy `latest.json` with immutable legacy-repo download URLs. That signed
  installer switches the Admin to the source feed. After verifying a 0.12.0
  Admin can update, remove the optional mirror step and its PAT secret, then
  archive (never delete) the legacy repository so old downloads still work.
  See [Bridge release](../releasing.md#bridge-release).
