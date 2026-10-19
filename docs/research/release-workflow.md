# Release Workflow Review

## Summary

**Do not build installers inside the version-bump command.** Keep the bump as a reviewed version-and-notes commit plus tag; build and publish installers in GitHub Actions after the tag is pushed. That is already the repository’s design: `bun run version:patch|minor|major` prepares and pushes the bump and tag, and the `v*.*.*` tag starts the Windows release workflow. [`scripts/bump-version.ts:253-286`](../../scripts/bump-version.ts); [`.github/workflows/release.yml:3-5`](../../.github/workflows/release.yml)

The current release workflow is intentionally a single Windows job because both shipped installers are Windows NSIS packages and the Admin updater needs a signed Windows installer. It already separates building from publication: it builds first, uploads a complete draft, then publishes it. Keep that structure. [`.github/workflows/release.yml:15-20`](../../.github/workflows/release.yml); [`.github/workflows/release.yml:72-111`](../../.github/workflows/release.yml)

The most useful efficiency improvement is **not** adding more release automation: add a small, unprivileged PR-check workflow for fast validation, leave installer builds off PRs, and only add Bun dependency caching if its install time proves worth the complexity. Rust caching is already configured. [`.github/workflows/release.yml:57-67`](../../.github/workflows/release.yml); [`package.json:23-25`](../../package.json)

## Current-state inventory

### Actions and recent run history

There is **one** file in `.github/workflows/`: `release.yml`. No separate PR/CI workflow was found. The workflow runs on pushed `v*.*.*` tags; the release job is restricted to `H3atman/openpiso`, uses `windows-latest`, has job-scoped `contents: write`, and is serialized under `admin-stable-release` with cancellation disabled. [`.github/workflows/release.yml:1-20`](../../.github/workflows/release.yml)

It checks out without persisted credentials, sets up Bun 1.4.2, validates the release tag, notes, updater key and signing/Telemetry secrets, downloads the previous stable release metadata, configures the Windows Rust target, restores Rust cache, installs Bun dependencies, and runs the release-helper test. [`.github/workflows/release.yml:29-67`](../../.github/workflows/release.yml)

It builds the bundled web admin, then a signed Admin NSIS installer and a Client NSIS installer. Admin uses the updater signing key and Telemetry token; Client is manual-install only and has no updater signing environment. The workflow composes manifests and notes, creates or reuses a draft, uploads assets, then publishes the complete release as latest. [`.github/workflows/release.yml:69-111`](../../.github/workflows/release.yml)

Recent public Actions history: the v0.12.1 run succeeded in about **13m 48s**, v0.12.2 failed in about **1m 39s**, and v0.12.3 succeeded in about **15m 04s**. The current release listing has v0.12.3 and v0.12.1, but not v0.12.2. [v0.12.1 run](https://github.com/H3atman/openpiso/actions/runs/38464514529); [v0.12.2 run](https://github.com/H3atman/openpiso/actions/runs/39087814218); [v0.12.3 run](https://github.com/H3atman/openpiso/actions/runs/39088390778); [`gh release list --limit 10` output](https://github.com/H3atman/openpiso/releases)

The published v0.12.3 assets include the Admin installer and its `.sig`, the Client installer, `latest.json`, and `release.json`. The updater manifest is for Admin; the Client remains manual-install-only. [`.github/workflows/release.yml:86-110`](../../.github/workflows/release.yml); [v0.12.3 assets](https://github.com/H3atman/openpiso/releases/tag/v0.12.3)

The workflow uses `swatinem/rust-cache@v2`, but does not explicitly cache Bun’s package dependency cache. `oven-sh/setup-bun` documents caching of the Bun executable; its README does not document dependency-cache behavior. [`.github/workflows/release.yml:33-35`](../../.github/workflows/release.yml); [`.github/workflows/release.yml:57-64`](../../.github/workflows/release.yml); [setup-bun](https://github.com/oven-sh/setup-bun); [rust-cache](https://github.com/Swatinem/rust-cache)

### Version bump and release procedure

The root package exposes `version:patch`, `version:minor`, `version:major`, and a separate local `build:installers` command. The bump script accepts all three increment types and updates versions in seven JSON manifests/config files plus `Cargo.toml`—**eight version-bearing source files total**—after checking that their existing versions agree. [`package.json:19-22`](../../package.json); [`scripts/bump-version.ts:5-5`](../../scripts/bump-version.ts); [`scripts/bump-version.ts:69-105`](../../scripts/bump-version.ts); [`Cargo.toml:13-16`](../../Cargo.toml)

The seven JSON files are the root and four workspace package manifests, plus both Tauri configs. Cargo’s workspace version is the eighth. The lockfiles are refreshed by the script, rather than serving as independently maintained version inputs. [`scripts/bump-version.ts:69-92`](../../scripts/bump-version.ts); [`scripts/bump-version.ts:156-173`](../../scripts/bump-version.ts)

The documented release procedure is to run the bump command from a clean `main`, review/edit release notes, then approve pushing `main` and the tag atomically. After the push, the script watches the `release.yml` run and checks the published release and assets. [`docs/releasing.md:55-83`](../../docs/releasing.md); [`scripts/bump-version.ts:247-286`](../../scripts/bump-version.ts); [`scripts/bump-version.ts:287-393`](../../scripts/bump-version.ts)

CI derives Schema Version from the Admin migration table and checks it against the current schema constant. It uses the prior stable release’s `release.json` as the compatibility baseline; missing or invalid prior schema metadata fails rather than guessing. [`docs/releasing.md:87-106`](../../docs/releasing.md); [`scripts/prepare-release.ts:140-182`](../../scripts/prepare-release.ts); [`scripts/prepare-release.ts:194-205`](../../scripts/prepare-release.ts)

The publish flow creates the GitHub Release as a draft after the installers and manifests are ready. It can replace assets on an existing draft after a failed upload, but refuses to overwrite a published release. [`docs/releasing.md:107-110`](../../docs/releasing.md); [`docs/releasing.md:132-138`](../../docs/releasing.md); [`.github/workflows/release.yml:97-111`](../../.github/workflows/release.yml)

`docs/releasing.md`, `docs/development.md`, and four release-note files for v0.12.0–v0.12.3 exist. `CONTRIBUTING.md` exists, but no release/version policy or release procedure was found there. The ADR index lists ADR-0086, and that ADR establishes the Admin updater, signed stable releases, Client manual-install path, Schema Version-aware rollback, and reviewed release notes. [`docs/adr/README.md:94-94`](../../docs/adr/README.md); [`docs/adr/0086-admin-app-update.md:19-60`](../../docs/adr/0086-admin-app-update.md); [`docs/releases/`](../../docs/releases/)

The current checked-in version is **0.12.3**. The repo supports patch/minor/major selection, but no project-specific criteria for choosing among them was found in the release docs, development docs, ADR-0086, or `CONTRIBUTING.md`. SemVer’s mandatory increment rules only apply once a project has reached 1.0.0; for 0.y.z it describes initial development and says anything may change. [SemVer](https://semver.org/); [`scripts/bump-version.ts:5-5`](../../scripts/bump-version.ts)

## Findings

### Which version increment?

At 0.12.3, use SemVer as a communication convention rather than treating the pre-1.0 increment rules as mandatory: use **patch** for a small compatible fix, **minor** for a meaningful feature or bundled set of changes, and reserve **major** for a deliberately signaled compatibility break or a 1.0 transition. SemVer explicitly allows change at any time in 0.y.z; the repository does not override that policy with its own release criteria. [SemVer](https://semver.org/); [`scripts/bump-version.ts:107-118`](../../scripts/bump-version.ts)

### Installer timing and runner cost

The observed successful releases took roughly 14–15 minutes on a single Windows runner; the failed v0.12.2 attempt ended much earlier. These timings are the full workflow run duration, not a per-step build profile. [v0.12.1 run](https://github.com/H3atman/openpiso/actions/runs/38464514529); [v0.12.2 run](https://github.com/H3atman/openpiso/actions/runs/39087814218); [v0.12.3 run](https://github.com/H3atman/openpiso/actions/runs/39088390778)

GitHub’s published hosted-runner rates list Windows 2-core x64 at $0.010/minute and Linux 2-core x64 at $0.006/minute. At the listed Windows rate, a 15-minute run is approximately $0.15 before billing details or any applicable free allowance; macOS is not needed for the current Windows-only release. [GitHub Actions billing](https://docs.github.com/en/billing/managing-billing-for-your-products/managing-billing-for-github-actions/about-billing-for-github-actions)

### Trigger and release safety

The current tag-push trigger is the right fit for the existing release command: one reviewed commit/tag is the release input, and the workflow builds from that immutable tag. GitHub supports tag filters on `push`. [GitHub workflow events](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows); [`.github/workflows/release.yml:3-5`](../../.github/workflows/release.yml)

A future workflow that creates a tag using `GITHUB_TOKEN` should not assume that tag push will start this release workflow: GitHub documents that events created by `GITHUB_TOKEN` generally do not trigger new workflow runs, except for specific dispatch events. The current local release command pushes through the maintainer’s configured Git credentials; the release workflow itself uses `GITHUB_TOKEN` to publish assets, not to create the triggering tag. [GitHub workflow events](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows); [`scripts/bump-version.ts:279-286`](../../scripts/bump-version.ts); [`.github/workflows/release.yml:98-111`](../../.github/workflows/release.yml)

The existing concurrency group’s `cancel-in-progress: false` is sensible: release jobs read the prior published release as the Schema Version baseline and publish stable releases in order. Enabling cancellation could interrupt a release partway through that sequence. GitHub documents that concurrency serializes jobs/workflows in a group and that `cancel-in-progress: true` cancels an active run when a new one arrives. [`.github/workflows/release.yml:10-13`](../../.github/workflows/release.yml); [`docs/releasing.md:87-110`](../../docs/releasing.md); [GitHub concurrency](https://docs.github.com/en/actions/using-jobs/using-concurrency)

## Options comparison

| Option | Assessment |
|---|---|
| **Build installers inside the version-bump command** | Not recommended. It couples a local edit/review/commit/tag command to a long Windows native build, signing secrets, and publication. The existing script already pushes the tag and watches CI, without building locally. [`scripts/bump-version.ts:247-286`](../../scripts/bump-version.ts); [`scripts/bump-version.ts:287-327`](../../scripts/bump-version.ts) |
| **Bump commit plus tag push starts release workflow** | **Recommended; already implemented.** Keeps the reviewed version and notes in Git, isolates signing/build dependencies in CI, and allows a failed pre-publication run to be rerun without creating a second version. [`docs/releasing.md:70-83`](../../docs/releasing.md); [`docs/releasing.md:132-138`](../../docs/releasing.md) |
| **Create a draft release first, then publish/build from it** | Current behavior already creates a draft for asset upload, but only after builds complete. Moving draft creation earlier adds lifecycle handling without avoiding the installer build; the current design keeps partial uploads invisible to the Admin feed. [`.github/workflows/release.yml:94-111`](../../.github/workflows/release.yml) |
| **Release-please/Changesets-style release PR** | Not recommended yet. The current script already synchronizes the version files, prepares reviewed notes, tags, pushes, and watches the release; another release PR/bot layer adds process without removing the Windows build or Schema Version safety checks. [`scripts/bump-version.ts:69-105`](../../scripts/bump-version.ts); [`docs/releasing.md:63-83`](../../docs/releasing.md) |

## Recommended workflow

Keep the existing release path:

```text
reviewed changes on main
  → bun run version:patch|minor|major
  → review release notes; bump script commits and tags
  → atomic push of main + vX.Y.Z
  → tag-triggered Windows build and release-helper checks
  → upload complete draft, then publish stable release
  → script verifies release/assets; maintainer completes the real Windows manual gate
```

The reviewed notes, tag, CI-generated Schema Version metadata, signed Admin updater artifacts, and Client installer all remain tied to one source revision. The existing draft-first publication boundary and serialization should stay. [`docs/releasing.md:70-110`](../../docs/releasing.md); [`docs/releasing.md:160-194`](../../docs/releasing.md)

Add a separate, inexpensive PR workflow on Linux for checks that do not require installer signing or Windows bundling. Use path filters only if the PR checks are not required—or provide an always-running aggregate check—because GitHub warns skipped path-filtered workflows can leave required checks pending. The repository already exposes `bun check`, `check:windows`, and release-helper tests; choose the appropriate focused checks after examining their package scripts. [`package.json:23-25`](../../package.json); [GitHub workflow syntax and path filters](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax); [`.github/workflows/release.yml:66-67`](../../.github/workflows/release.yml)

Keep `rust-cache` in the release job. Consider caching Bun’s dependency cache only after measuring `bun install`’s share of the 14–15 minute run; `setup-bun` caches the Bun executable, while GitHub’s cache action can cache dependency directories with keys based on lockfiles. Avoid caching secrets or relying on cache hits for correctness. [`.github/workflows/release.yml:33-35`](../../.github/workflows/release.yml); [`.github/workflows/release.yml:57-64`](../../.github/workflows/release.yml); [setup-bun](https://github.com/oven-sh/setup-bun); [GitHub dependency caching](https://docs.github.com/en/actions/using-workflows/caching-dependencies-to-speed-up-workflows)

Do not add a macOS/Windows matrix for the current release: the shipped targets are both Windows installers, and the current single runner already builds both. Do not build installers on PRs; reserve the signed, secret-bearing release build for an intentional tag. [`.github/workflows/release.yml:20-20`](../../.github/workflows/release.yml); [`.github/workflows/release.yml:72-92`](../../.github/workflows/release.yml)

## Migration steps

1. Keep the existing tag-triggered installer workflow and current release command; no release-flow migration is needed. [`docs/releasing.md:55-83`](../../docs/releasing.md)
2. Add a separate PR-check workflow using Linux and only the repository’s existing validation commands; explicitly decide whether path-filtered checks are required before using path filters. [`package.json:23-25`](../../package.json); [GitHub workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
3. Keep installer builds and the production signing/Telemetry secrets out of PR checks. The release job currently owns the signing and publication steps with scoped permissions. [`.github/workflows/release.yml:7-20`](../../.github/workflows/release.yml); [`.github/workflows/release.yml:37-45`](../../.github/workflows/release.yml)
4. Capture step-level durations on a later release; only then decide whether Bun dependency caching is worth maintaining. Rust caching is already present. [`.github/workflows/release.yml:57-67`](../../.github/workflows/release.yml); [GitHub dependency caching](https://docs.github.com/en/actions/using-workflows/caching-dependencies-to-speed-up-workflows)

## Risks and open questions

- **Published-release recovery:** the workflow intentionally refuses to overwrite a published release. A failed build or draft upload can be rerun and repaired on the draft; an already-published mistake should be corrected with a new version, consistent with the release guide. [`.github/workflows/release.yml:102-111`](../../.github/workflows/release.yml); [`docs/releasing.md:132-138`](../../docs/releasing.md)
- **Token-trigger behavior:** any future automation that pushes the tag using `GITHUB_TOKEN` must explicitly account for GitHub’s workflow-trigger limitation; the current human-driven atomic push is the trigger. [GitHub workflow events](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows)
- **Manual release gate remains necessary:** Actions validates and publishes assets, but docs require real Windows installation/update testing for installer behavior and updater compatibility. [`docs/releasing.md:160-194`](../../docs/releasing.md)
- **SemVer policy is informal pre-1.0:** decide whether to document a project-specific patch/minor/major convention; the current bump script offers the three choices but does not encode release-intent rules. [`scripts/bump-version.ts:5-5`](../../scripts/bump-version.ts); [SemVer](https://semver.org/)
- **Checks to run for an implementation:** inspect the relevant package scripts, then run the selected PR checks and `actionlint .github/workflows/release.yml` if the release workflow changes. The release docs list `bun test scripts/prepare-release.test.ts` and `actionlint` as focused workflow checks. Do not run installer builds as part of PR verification. [`docs/releasing.md:140-145`](../../docs/releasing.md); [`package.json:23-25`](../../package.json)

### Official sources

- [GitHub Actions workflow events and `GITHUB_TOKEN` behavior](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows)
- [GitHub Actions concurrency](https://docs.github.com/en/actions/using-jobs/using-concurrency)
- [GitHub Actions workflow syntax and path filters](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [GitHub Actions dependency caching](https://docs.github.com/en/actions/using-workflows/caching-dependencies-to-speed-up-workflows)
- [GitHub Actions billing](https://docs.github.com/en/billing/managing-billing-for-your-products/managing-billing-for-github-actions/about-billing-for-github-actions)
- [GitHub release asset uploads](https://docs.github.com/en/rest/releases/assets)
- [Tauri Action](https://github.com/tauri-apps/tauri-action)
- [Tauri v2 updater](https://v2.tauri.app/plugin/updater/)
- [Bun setup action](https://github.com/oven-sh/setup-bun)
- [Rust cache action](https://github.com/Swatinem/rust-cache)
- [Semantic Versioning](https://semver.org/)
