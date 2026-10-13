# Releasing Windows Installers and Admin App Updates

Windows x64, stable releases. See [ADR-0086](adr/0086-admin-app-update.md).
Admin has signed App Updates. The published Client installer includes Client Guard
and remains manual-install only (`perMachine`, UAC); diskless images remain manual.
Source is public under PolyForm Shield 1.0.0 (source-available, not OSI open
source); installed Admins read public releases anonymously.

## One-time maintainer setup

1. Publish Releases in **`H3atman/openpiso`**, the public source repository.
   The tag-triggered workflow uses the built-in `GITHUB_TOKEN`, with
   `permissions: contents: write` only on the release job. No publishing PAT
   is needed for normal releases. Keep Releases and assets public and preserve
   the updater signing key across releases. Existing installs need the
   [Bridge release](#bridge-release) below to change feeds.
2. Generate a Tauri updater minisign keypair from a trusted checkout:

   ```bash
   bun install --frozen-lockfile
   bun --cwd apps/admin tauri signer generate -w /secure/offline/openpiso-updater.key
   ```

   Use a strong password; write the files **outside the repository**. The command
   writes the private key and a `.pub` file. Copy the **public key file's contents**
   into `plugins.updater.pubkey` in
   `apps/admin/src-tauri/tauri.conf.json`, replacing the committed public key for your own distribution,
   and commit that public configuration before tagging. Never generate a new key
   for each release. CI validates key configuration; it does not provision keys.

3. For the bridge release only, keep `OPENPISO_RELEASES_TOKEN`: a fine-grained
   PAT owned by `H3atman`, selected repository **only** `openpiso-releases`,
   permission **Contents: Read and write** (Metadata read is implicit). It
   enables the optional legacy mirror; an unset secret skips that step. No PAT
   access to the source repository is needed.
4. In the **source repository**, Settings → Secrets and variables →
   Actions, configure these repository secrets:

   | Secret                               | Value                                            |
   | ------------------------------------ | ------------------------------------------------ |
   | `TAURI_SIGNING_PRIVATE_KEY`          | Complete private key file contents, not its path |
   | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Password used when generating the key            |
   | `OPENPISO_RELEASES_TOKEN`            | Legacy-repo PAT; bridge release only              |

   Enter secrets through GitHub's private form or an interactive secret-management
   tool, never command-line literals, committed files, logs or chat. An intentionally
   passwordless key uses an empty password, but a protected key is recommended.
   `GITHUB_TOKEN` is provided automatically; it cannot publish across repositories.

5. Back up the private key **and password** offline, separately from CI and the
   checkout. Keep the public key and key identifier with the backup. Verify recovery
   by signing a disposable installer with the backed-up key and checking it against
   the committed public key. Losing this key prevents updates to existing installs;
   compromise permits malicious installers. Restrict tag pushes and workflow edits
   to trusted maintainers; renew the bridge PAT if needed, not the signing identity.

Minisign is updater authenticity, **not Windows Authenticode**. SmartScreen may
still warn on sideloaded EXEs. Authenticode is outside this release process.

## Release flow

From a clean `main`, after the manual gate below:

```bash
bun run version:patch     # or version:minor / version:major
```

The script first fetches `origin/main`, fast-forwards `main` when behind, and refuses any local commits ahead of `origin/main` (push or rebase first).

The version command drafts notes (local `codex exec`, or conventional-commit
fallback), opens `$EDITOR`, commits every version plus
`docs/releases/vX.Y.Z.md`, tags `vX.Y.Z`, then asks before pushing. Review every
claim; never publish unreviewed AI text. `--yes` skips the push question. A tag
pushed from the source repo triggers `.github/workflows/release.yml` on
`windows-latest`; no manual asset upload is needed.

CI:

1. Checks tag/version agreement, reviewed notes, public key and required secrets.
2. Reads all public release pages and selects the newest **older stable** release
   numerically. Drafts and previews are excluded; a tag older than an already
   published stable release is refused so it cannot displace latest.
3. Downloads that release's `release.json`. Failure or missing/invalid schema
   metadata stops publication, rather than guessing a safe rollback boundary.
   The first release has no baseline and receives a conservative database warning.
4. Derives Schema Version from the registered migration table in
   `apps/admin/src-tauri/src/db/mod.rs`, verifies its contiguous migration numbers
   and agreement with `CURRENT_SCHEMA_VERSION`. Unregistered SQL files do not count.
   This is the highest `_openpiso_migrations.version` the tagged Admin can apply;
   no live shop database is opened by CI.
5. Builds web admin, then Admin via `tauri-action`, with minisign secrets, NSIS
   `currentUser`, and updater artifacts enabled. Then builds Client via the same
   action and target, with `perMachine` NSIS and its Guard service. Client has no
   updater artifacts and needs no updater signing environment. Both Cargo members
   share `target/x86_64-pc-windows-msvc/release/bundle/nsis`.
6. Composes the body from the committed notes. When schema rose, adds a deterministic
   database-change / no cross-schema rollback warning. Schema regression is refused.
   The same final notes appear in the body and both manifests.
7. Creates a **draft** in the source repo with `--verify-tag` (the trigger tag
   must already exist), uploads all five assets, then publishes
   as stable and marks latest. Drafts stay invisible to installed Admins. Publication
   is serialized across tags; a published release is never overwritten by a rerun.
8. If `OPENPISO_RELEASES_TOKEN` is non-empty, regenerates manifests in a separate
   output directory with `--repository "$LEGACY_RELEASE_REPOSITORY"`, then
   publishes the same signed installers and legacy manifests to the legacy feed.

Each `vX.Y.Z` release contains:

- `OpenPiso Central Terminal_X.Y.Z_x64-setup.exe`
- `OpenPiso Central Terminal_X.Y.Z_x64-setup.exe.sig`
- `OpenPiso Client_X.Y.Z_x64-setup.exe` (manual install, includes Guard; no `.sig`)
- `release.json`: `{ version, schema_version, notes, pub_date }`, version without `v`
- `latest.json`: Tauri `{ version, notes, pub_date, platforms }`, with
  `platforms["windows-x86_64"]` containing the `.sig` contents and the URL to that
  exact release's **Admin** EXE, never a mutable `latest/download` URL. Client
  must not be added to this single-platform Admin update manifest

`pub_date` is the CI composition time in UTC ISO 8601. Per-release manifests keep
Compatible Rollback pointed at the chosen signed version. JSON metadata itself is
not signed; the installer signature is checked against the embedded public key
before installation.

If upload fails, fix the cause and rerun the failed workflow: it can replace assets
on its **draft**, then publish the complete release. Never delete/recreate a
published version or mutate its installer/manifests. Fix a published mistake with
a new version. An older legacy release without `release.json` must be deliberately
bootstrapped with accurate schema metadata before using it as a baseline; do not
invent a Schema Version to get CI green. The source feed uses only source-repo
releases as its baseline, not legacy releases.

Focused helper verification:

```bash
bun test scripts/prepare-release.test.ts
# If installed:
actionlint .github/workflows/release.yml
```

## Bridge release

Admins ≤0.12.0 read `H3atman/openpiso-releases` and accept installer URLs only
under that repository's `/releases/download/` prefix. The first release from
this branch must bridge them to `H3atman/openpiso`; simply redirecting the old
manifest to source-repo assets would be rejected.

1. Bump to the next version and review its release notes through the normal
   release flow. Do not replace the updater signing key.
2. Keep `OPENPISO_RELEASES_TOKEN` set for **that one release**. CI publishes to
   the source repository with `GITHUB_TOKEN`, then mirrors the Admin installer,
   `.sig`, Client installer, `latest.json`, and `release.json` to the legacy
   repository. The second `prepare-release` invocation writes `release/legacy`
   using `--repository H3atman/openpiso-releases`; each manifest retains immutable
   `/releases/download/vX.Y.Z/...` URLs for its own repository.
3. Verify anonymously accessible assets in both repositories. On an isolated
   **0.12.0 Admin**, Check now must offer the bridge version; install it through
   App Update and verify relaunch, signature validation, and the new source feed.
   The new feed starts at this bridge release: Compatible Rollback no longer
   offers versions published only in the legacy repository.
4. After successful verification, delete `OPENPISO_RELEASES_TOKEN` and remove
   the optional legacy mirror step and `LEGACY_RELEASE_REPOSITORY` workflow env
   entry. Archive `openpiso-releases` on GitHub; **do not delete it**, so old
   download links and installs that have not yet taken the bridge remain usable.

If the mirror fails after source publication, finish the missing legacy draft
with the same signed artifacts and legacy manifests before retiring the secret.
Do not overwrite the already published source release; rerunning the entire
workflow deliberately refuses to overwrite it.

## Manual release gate (real Windows installation)

Automated checks do not verify NSIS relaunch, kiosk visibility or HKCU Autostart.
Run this on an isolated Windows test shop before publication; never on customers'
paid sessions. Keep screenshots and Admin/Client logs with the tested versions,
Schema Versions and installer hashes. For the first updater-enabled release, use
**two signed builds** and an isolated staging Release Feed/build to rehearse the
Owner flow; never redirect production shops to a test feed or replace their key.
The production tag must carry the permanent production public key and feed.

- [ ] Review owner-facing notes and migrations; check Schema Version against the
      previous release. Confirm the warning appears only for a schema increase (or
      the first release), and the public key is not the placeholder.
- [ ] Install the older signed updater-enabled Admin as the ordinary kiosk user.
      Enable Autostart. Start a Client session with known remaining paid time.
- [ ] From a phone on the LAN, sign in as Owner, open App Update above the Owner
      menu, Check now, read notes and running-PC warning. Ensure download progress
      works; unauthenticated/non-Owner requests cannot install.
- [ ] For a schema increase, Install is refused without acknowledgement and works
      with it. Afterward, different-schema releases are absent from Compatible Rollback.
      For same-schema updates, no schema acknowledgement is required.
- [ ] Install the **real signed candidate EXE** through the panel. Observe kiosk
      hide, passive progress, no UAC, Admin close and automatic relaunch; kiosk returns.
- [ ] Confirm the bundled web admin changed too, SQLite records and remaining paid
      time survive, Clients briefly lock and reconnect before Offline Shutdown Countdown
      expires, and Owner sees the success toast.
- [ ] Sign out/in or reboot: HKCU Autostart still launches Admin at the existing
      per-user path. Exercise a same-schema Compatible Rollback: exact target-version
      confirmation, signed install and relaunch succeed.
- [ ] With a disposable staged artifact, verify a mismatched/tampered signature is
      refused before install. Confirm a failed install leaves the old version and
      reports failure in Diagnostics/Owner result; capture evidence.
- [ ] Inspect the public release after CI: stable, latest, exactly the intended
      five assets, matching versions/schema/notes, anonymous download works. Verify a
      test shop detects this release before declaring the rollout complete.

## Offline sideloading / initial adoption

`bun run build:installers` remains the local offline path. It explicitly disables
Admin updater artifacts for that build, so it needs no signing secrets; it is **not**
the CI signed release path. Do not upload its unsigned EXE as an App Update.
For a local signed candidate, securely set the two signing environment variables,
build web admin, then run the Admin Tauri build without the offline override.

Existing 0.11.x shops without the updater need **one manual sideload** of an
updater-enabled installer first. Offline shops may keep sideloading; turn off
automatic checks on metered/restricted connections. See
[installation](installation.md#admin-app-update-adr-0086) and its Pre-Migration
Backup recovery procedure. App Updates never install Client or Client Guard;
download the Client installer from the same release and install it manually.

## Forking / your own releases

Forks do not publish through the upstream workflow: the release job is guarded
by `github.repository == 'H3atman/openpiso'`. To distribute a permitted fork:

1. Change `RELEASE_REPOSITORY` in `apps/admin/src-tauri/src/app_update.rs` and
   `scripts/prepare-release.ts`, and the workflow's `RELEASE_REPOSITORY` environment
   value. The browser fixture shares the TypeScript constant; documentation URLs
   and test expectations should reflect your host too.
2. Generate your own updater keypair as above; replace `plugins.updater.pubkey`.
   Never ship the upstream public key for installers signed with a different key.
3. Set your own signing secrets and releases-repository token; change the job's
   repository guard to your source repository. Do not reuse upstream secrets.

`bun run build:admin` requires `TAURI_SIGNING_PRIVATE_KEY` because
`createUpdaterArtifacts` is enabled (and the password variable for a protected
key). `bun run build:installers` disables Admin updater artifacts and works
without signing secrets; those unsigned installers are for manual installation,
not the Admin update feed. Observe [the license](../LICENSE), including Noncompete.
