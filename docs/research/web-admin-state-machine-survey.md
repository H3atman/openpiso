# Web Admin Pure-Reducer State Machine Survey

## Summary

The best first candidate is the Web Admin login flow: it has a compact set of explicit phases, a timer-driven lockout, and comparatively little UI to rewire. The richer candidates are Account Import, App Update, and Backup/Restore, where a reducer could make consequential transitions clearer but would touch substantially more UI and asynchronous work.

ADR-0089 prescribes pure reducers for **`apps/client` only**; it explicitly says that scope is limited for now. It is a useful house-style example, not a requirement to migrate Web Admin. Frontend standards say to keep components UI-only, avoid a new state-management library, and use the existing approach outside `apps/client`. Therefore, adopt a reducer selectively where it clarifies a real flow—not as a blanket conversion of every `$state` variable. (`CODING_STANDARDS.md:3-15`; `docs/adr/0089-client-ui-state-machines.md:12-21,29-31`)

Effort tiers below are relative estimates, not measured LOC. Side effects remain outside any proposed reducer: API calls, timers, polling, browser storage, notifications, and focus management belong in a thin runner or component adapter.

## Method and sources

Read the repository instructions, frontend standards, glossary, ADR index, ADR-0089, ADR-0024, and Web Admin ADRs governing routing, Notifications, Owner Sessions, and App Updates. Then surveyed Web Admin pages, components, reactive state and effects, polling/timer sites, API use, and the existing test-file inventory. No tests, typechecks, or builds were run.

The standards say components should be UI-only, API calls should live in `src/lib/api.ts`, shared types in `src/lib/types.ts`, and formatting in `src/lib/format.ts`; they also reject adding a state-management library. (`CODING_STANDARDS.md:3-15`)

The house-style example is the Client Spin reducer: a discriminated union models its states and events, and `reduceSpin` uses an exhaustive switch. Its Svelte runner owns `$state`, performs API calls and timers, and dispatches result events. (`apps/client/src/lib/machines/spin.ts:37-55,100-134`; `apps/client/src/lib/machines/spin.svelte.ts:1-4,21-39,41-51`) No direct Client Spin reducer test file was found in the Client source tree.

ADR-0024 makes the Svelte app the sole owner surface; ADR-0051 makes real History API paths authoritative and keeps login as a URL-preserving gate. (`docs/adr/0024-web-admin-sole-owner-surface.md:22-36`; `docs/adr/0051-web-admin-url-routing.md:25-57,79-85`)

For domain language, the glossary distinguishes an **Owner Session** from account sessions and describes a **Remote Control Session** as a separate owner-initiated capability; those distinctions matter when interpreting auth and UI state. (`docs/glossary.md:188-195,1085-1100`)

## Ranked candidates: least to most change

| Rank | Candidate | Main seams | Estimated scope | Tier |
|---:|---|---|---|---|
| 1 | Login / initial owner authentication | `LoginPage.svelte`; `api.ts`; parent `App.svelte` auth handoff | 1–3 files, ~60–120 LOC; login UI binds to phase-specific state | Small |
| 2 | Manual Adjustment submit flow | `AdjustmentsPage.svelte` | 1–2 files, ~30–60 LOC; small submit-state/UI adaptation | Small |
| 3 | Points Transfer approvals | `TransfersPage.svelte`; optional machine + focused test | 2–3 files, ~50–100 LOC; row actions adapt to per-request phase | Small–Medium |
| 4 | Diagnostics requests and lazy-loaded panels | `DiagnosticsPage.svelte`, harness/tests | 2–4 files, ~80–160 LOC; panels/buttons consume explicit states | Medium |
| 5 | Dashboard owner actions and polling | `DashboardPage.svelte`; dashboard tests | 2–4 files, ~100–200 LOC; preserve polling and snapshot semantics | Medium |
| 6 | Notifications page lifecycle and acknowledgements | `NotificationsPage.svelte`; possibly `App.svelte` poll; tests | 3–5 files, ~150–300 LOC; split into subflows | Large |
| 7 | Account Import preview/commit | `AccountsPage.svelte`; tests; machine module | 3–5 files, ~100–200 LOC; modal phases and commit results rewired | Large |
| 8 | App Update panel | `app-update.svelte`; tests | 3–5 files, ~180–350 LOC; substantial panel/state wiring | Large |
| 9 | Backup and Restore | `BackupPage.svelte`; harness and tests; separate schedule/catalog machines | 4–7 files, ~250–500 LOC; several forms and confirmations rewired | Huge |
| 10 | Accounts administration as a whole | `AccountsPage.svelte`; component extraction and tests | 5–10+ files, ~400–800+ LOC; major decomposition | Huge |

### 1. Login / initial owner authentication — Small

**Current shape.** `LoginPage.svelte` keeps `pin`, `confirmPin`, `error`, `busy`, `isSetup`, `configuring`, and `lockoutSeconds` as separate state, while also owning a lockout interval. On mount it reads session/configuration and chooses setup or login mode; submit validates the confirmation PIN, calls setup or login, and handles throttle errors. (`apps/web-admin/src/lib/pages/LoginPage.svelte:7-17,19-48,50-75`)

**Why a reducer could help.** Independent `configuring`, `isSetup`, `busy`, `error`, and `lockoutSeconds` flags permit combinations whose meaning is unclear—e.g. submission while setup status is still unknown, or a lockout/error coexisting with a pending request. The timer decrements state directly and clears the error at zero. (`apps/web-admin/src/lib/pages/LoginPage.svelte:19-47,50-75`)

**Possible machine.** States: `checking-session`, `login-idle`, `setup-idle`, `submitting-login`, `submitting-setup`, `locked-out`, `authenticated`, `session-check-error`. Events: `session-loaded`, `setup-selected`, `submit`, `pin-mismatch`, `login-succeeded`, `setup-succeeded`, `rejected`, `lockout-tick`. Keep the PIN out of machine state if practical; pass it as input to the submit effect, as the Client Spin runner does for its password. (`apps/client/src/lib/machines/spin.ts:47-55`; `apps/client/src/lib/machines/spin.svelte.ts:78-81`)

**Scope, benefit, and risk.** One plain `.ts` reducer and one runner boundary in `LoginPage.svelte`; ~60–120 LOC. Coverage is indirect: `App.svelte.test.ts` exists, but there is no `LoginPage` test. Benefit: encode setup/login/lockout phases and make timer transitions testable. Risk: do not over-model PIN field editing or change the URL-preserving login gate. (`docs/adr/0051-web-admin-url-routing.md:79-85`; `apps/web-admin/src/App.svelte:291-298`)

### 2. Manual Adjustment submit flow — Small

**Current shape.** The page has `busy`/`error` for loading PCs and separate `createBusy` for submission. `createAdjustment` writes, reloads, reports success/failure, then clears the busy flag. `pcId`, delta, and reason are form data, not lifecycle state. (`apps/web-admin/src/lib/pages/AdjustmentsPage.svelte:8-16,18-48`)

**Why a reducer could help.** Loading can be loading/succeeded/failed instead of `busy` + `error` alongside data; submission can be idle/submitting/failed/succeeded. The UI hides the form when loading or failed, but the representation permits data + error + busy combinations. (`apps/web-admin/src/lib/pages/AdjustmentsPage.svelte:18-29,53-60`)

**Possible machine.** `load-started`, `loaded(pcs)`, `load-failed(error)`, `submit`, `submit-succeeded`, `submit-failed(error)`. Mutation and refresh stay outside the reducer.

**Scope, benefit, and risk.** ~30–60 LOC; no `AdjustmentsPage` test exists. Limited benefit: one mutation, already guarded by `createBusy`. Only worth it to establish the Web Admin pattern.

### 3. Points Transfer approvals — Small–Medium

**Current shape.** `TransfersPage.svelte` stores list `busy`/`error`, per-request `rowBusy`, and a 15-second refresh interval. Approve and reject mutate then reload; periodic and manual refresh can overlap those operations. (`apps/web-admin/src/lib/pages/TransfersPage.svelte:8-26,28-52,68-78`)

**Why a reducer could help.** `rowBusy` is a record of booleans separate from each request's presence in the refreshed list. A stale poll can race a mutation/reload and briefly show a pending item after its action completed. The reducer can't make server mutations atomic, but it can make accepted result transitions explicit.

**Possible machine.** List lifecycle (`list-loading`, `list-ready`, `list-failed`) separate from row actions: `approve`/`reject` → `acting(id, action)` → `action-succeeded` / `action-failed`, plus `poll-result`. Discard stale responses by request identity only if evidence warrants it.

**Scope, benefit, and risk.** 2–3 files, ~50–100 LOC; no `TransfersPage` test. Benefit: clear per-row mutation ownership. Risk: server polling remains the source of truth; optimistic reconciliation would be a separate behavior change.

### 4. Diagnostics requests and lazy-loaded panels — Medium

**Current shape.** `DiagnosticsPage.svelte` has tabs plus distinct PC-list, Support Bundle, Diagnostic Request, Coin Debug, Coin Debug Window, and Host Diagnostics state. It lazy-loads data as tabs open; the Coin Debug Window uses a generation counter so an in-flight read can't overwrite a newer result. (`apps/web-admin/src/lib/pages/DiagnosticsPage.svelte:10-27,37-68,70-101,103-168`)

**Why a reducer could help.** Independent `busy`, `loaded`, and error flags across panels, with at least one hand-rolled stale-response guard. The Support Bundle snapshots `pcId` before awaiting and stores the result under `bundlePcId`. (`apps/web-admin/src/lib/pages/DiagnosticsPage.svelte:113-129`)

**Possible machine.** Small per-panel machines, not one page-wide union: `bundle-idle` → `bundle-requesting(pcId, window)` → `bundle-ready(pcId, markdown)` / `bundle-failed`; similarly `coin-window-loading`/`ready`/`failed`. Tabs stay plain UI state.

**Scope, benefit, and risk.** 2–4 files, ~80–160 LOC; `DiagnosticsPage.svelte.test.ts` exists. Benefit: explicit async result ownership, testable stale-result behavior. Risk: a mega-machine would multiply unrelated axes. ADR-0081: Diagnostics Mode is a visibility flag, not a security boundary—the machine must not make it one. (`docs/adr/README.md:89`)

### 5. Dashboard owner actions and polling — Medium

**Current shape.** `DashboardPage.svelte` polls every five seconds and holds a snapshot plus error. Separate state tracks per-PC and all-PC monitoring checks, Owner Sign-Out target/busy, and monitoring suppression/cancellation targets, PIN, error, and busy. Targets are intentionally snapshotted so a refresh can't redirect a mutation to a different customer or PC. (`apps/web-admin/src/lib/pages/DashboardPage.svelte:27-45,47-66,68-110,113-144,146-199,201-208,230-237`)

**Why a reducer could help.** Suppression holds two target variables (`suppressionTarget`, `cancellingSuppression`) sharing PIN/error/busy flags, so both can be set at once. Sign-Out and suppression have different confirmation semantics and shouldn't share a page-wide `busy`. (`apps/web-admin/src/lib/pages/DashboardPage.svelte:36-66,146-179`)

**Possible machine.** Sign-out: `closed` → `confirming(target)` → `submitting(target)` → `closed` / `failed(target, error)`. Suppression: `closed` → `confirming(target, duration)` → `submitting(target, PIN)` → `closed` / `failed`. Polling stays an external effect dispatching snapshot events.

**Scope, benefit, and risk.** 2–4 files, ~100–200 LOC; `DashboardPage.svelte.test.ts` exists. Benefit: dialog exclusivity and explicit target snapshots. Risk: preserve session-ID semantics; Owner Sign-Out has durable lifecycle meaning under ADR-0068. (`docs/adr/README.md:76`)

### 6. Notifications page lifecycle and acknowledgements — Large

**Current shape.** Alerts, Notices, acknowledged history, independent load-more flows, acknowledgement, bulk acknowledgement confirmation, Support Bundle requests, and Rejection Credit launch. Flags include `loading`, `error`, `historyError`, `showHistory`, `historyLoaded`, several busy flags, `busyAlertId`, `bulkBusy`, `showBulkConfirm`, plus per-collection pagination flags. (`apps/web-admin/src/lib/pages/NotificationsPage.svelte:34-50,73-80`)

`load()` uses `Promise.allSettled`, updates Alerts and Notices independently, and marks Notices seen only after the Notice list succeeds. Bulk acknowledgement refreshes alerts and reloads history even after failure, because a batch may partially acknowledge. (`apps/web-admin/src/lib/pages/NotificationsPage.svelte:119-164,316-339`)

**Why a reducer could help.** History visibility/loading/failure/loaded are separate flags. Requests overlap—e.g. Alert refresh after acknowledgement vs. the root 15-second poll updating the banner. (`apps/web-admin/src/lib/pages/NotificationsPage.svelte:41-50,235-279`; `apps/web-admin/src/App.svelte:191-220`; `docs/adr/0058-audit-derived-web-admin-notifications.md:151-153,241-243`)

**Possible machine.** Split: (a) collection loading/pagination; (b) per-Alert ack + bulk-confirm (`idle` → `confirming(ids)` → `acknowledging(ids)` → `refreshing` → `idle` / `failed`); (c) history visibility/loading; (d) Support Bundle request.

**Scope, benefit, and risk.** 3–5 files, ~150–300 LOC; `NotificationsPage.svelte.test.ts` and `NotificationBanner.svelte.test.ts` exist. Must preserve ADR-0058: Notice watermark advances only after visible Notices load; alerts need explicit acknowledgement; bulk covers only rendered Alert IDs; root poll stays 15 s. (`docs/adr/0058-audit-derived-web-admin-notifications.md:103-127,151-170`)

### 7. Account Import preview/commit — Large

**Current shape.** Embedded in `AccountsPage.svelte`: `showImport`, `importBusy`, `importError`, `importPayload`, `importCommitted`, `importReport`. File handling parses then previews; confirm commits the same payload, marks committed, reloads Accounts, and reports. (`apps/web-admin/src/lib/pages/AccountsPage.svelte:56-64,343-403`)

**Why a reducer could help.** Flags permit "committed but payload/report missing", "closed while busy", or "committed but confirm still enabled"; some are guarded imperatively today. (`apps/web-admin/src/lib/pages/AccountsPage.svelte:352-360,388-403`)

**Possible machine.** `closed` → `selecting-file` → `previewing(payload)` → `preview-ready(payload, report)` → `committing(payload, report)` → `committed(report)`; errors return to a phase retaining payload/report for retry.

**Scope, benefit, and risk.** 3–5 files, ~100–200 LOC, inside a 1,200+ line page. `AccountsPage.svelte.test.ts` exists. Benefit: preview vs. commit clarity, no accidental repeat commits. Risk: keep server authority and the shared preview/commit plan semantics. (`docs/glossary.md:89-98`; `docs/adr/README.md:65`)

### 8. App Update panel — Large

**Current shape.** `app-update.svelte` combines `open`, update data, errors, acknowledge/check/toggle/install busy flags, `awaitingRestart`, install/schema confirmation, rollback target and typed confirmation, and a poll that switches between 15 s and 1 s. A `generation` counter ignores stale GETs. (`apps/web-admin/src/lib/components/app-update.svelte:22-44,73-110,113-169,172-219`)

**Why a reducer could help.** `busy` is derived from many flags and guards enforce exclusivity. `publish` conditionally resets confirmation/rollback fields based on server state, with transitions spread across a helper, async functions, and effects. (`apps/web-admin/src/lib/components/app-update.svelte:36-57,93-110,124-219`)

**Possible machine.** Server `AppUpdateState` stays data; panel phases: `idle`, `checking`, `toggling-auto-check`, `acknowledging-result`, `confirming-install`, `installing`, `confirming-rollback`, `rolling-back`, `awaiting-restart`, `failed`.

**Scope, benefit, and risk.** 3–5 files, ~180–350 LOC; `app-update.svelte.test.ts` and `AppUpdateHarness.svelte` exist. Benefit: one auditable model for exclusive operations and schema/rollback gates. Risk: preserve server gates (schema acknowledgement, rollback-version confirmation). (`docs/adr/0086-admin-app-update.md:44-53`)

### 9. Backup and Restore — Huge

**Current shape.** `BackupPage.svelte` covers catalog loading, backup create/delete, schedule load/edit/save, restore from catalog path or upload, verification/refusal, pending-restore status, and cancellation. Many schedule fields, separate loading/busy/errors, three candidate sources, `pendingRestore`, and blocking PCs. Confirm picks among three inputs, calls upload or stage, and maps `RestoreRejectedError` to blocking PCs. (`apps/web-admin/src/lib/pages/BackupPage.svelte:29-57,70-100,102-120,141-180,183-219,223-277`)

**Why a reducer could help.** Restore source is multiple nullable fields that can coexist; phase rules live in procedures and disabled buttons, not a type. (`apps/web-admin/src/lib/pages/BackupPage.svelte:49-57,183-219,223-269`)

**Possible machine.** Restore source union (`catalog-entry`, `filesystem-path`, `uploaded-file`) and phases `closed`, `reviewing(source)`, `verifying(source)`, `staged(pendingRestore)`, `rejected(error, blockingPcs)`, `cancelling`. Catalog/schedule editing stay separate.

**Scope, benefit, and risk.** 4–7 files, ~250–500 LOC; `backupPage.svelte.test.ts` and `BackupPageHarness.svelte` exist. High-consequence: retain server fail-closed checks and next-launch staging (ADR-0054). The reducer is UI coordination, not restore authority. (`docs/adr/README.md:62`)

### 10. Accounts administration as a whole — Huge

**Current shape.** `AccountsPage.svelte` combines list loading, search/filter/sort/pagination, range selection, create, reset, delete, time/points adjustment, import, and Rejection Credit. Separate target/open/selected-ID/busy/error/form state for many dialogs. (`apps/web-admin/src/lib/pages/AccountsPage.svelte:31-63,168-185,250-276,278-340,343-510`)

**Why a reducer could help.** Dialog booleans and targets are independent, so incompatible dialogs can be open together; each mutation has its own busy flag. (`apps/web-admin/src/lib/pages/AccountsPage.svelte:105-119,153-166,168-219,250-276,278-299`)

**Possible machine.** Extract one flow first (Import, see #7). Broadly: a `dialog` union (`none`, `create`, `reset(target)`, `delete(ids)`, `adjust(target, kind)`, `credit-choices`, `credit(target)`) with local action phases. Search, sorting, and focus stay out.

**Scope, benefit, and risk.** 5–10+ files, ~400–800+ LOC, needs component extraction. Effectively a rewrite of a feature-dense page; incremental extraction is much safer.

## Not worth making state machines

- **Derived values.** Account filter/sort/pagination and selected counts are `$derived`, not lifecycles. (`apps/web-admin/src/lib/pages/AccountsPage.svelte:130-151,171-185`)
- **Simple presentation state.** Notification banner `expanded`; Diagnostics tab selection. (`apps/web-admin/src/lib/components/NotificationBanner.svelte:18-20,40-63`; `apps/web-admin/src/lib/pages/DiagnosticsPage.svelte:29-37`)
- **URL routing.** ADR-0051 makes History API paths authoritative with a small hand-written router; a machine must not compete with the URL. (`docs/adr/0051-web-admin-url-routing.md:25-57,103-111`; `apps/web-admin/src/lib/router.svelte.ts:16-24,27-71,74-99`)
- **Theme, sidebar navigation, static layout.** Derived from route and data. (`apps/web-admin/src/App.svelte:73-139`)
- **Settings forms as a blanket category.** Mostly one read/edit/save flow, already covered by focused tests (`SecuritySection`, `ShopSection`, `WheelSection`). Convert a section only on a concrete problem.

## Polling and async boundaries

Web Admin polls rather than using SSE/WebSocket: root (Theme, Notifications, Diagnostics Mode) every 15 s; Dashboard 5 s; Transfers 15 s; App Update 1 s or 15 s. (`apps/web-admin/src/App.svelte:209-220`; `apps/web-admin/src/lib/pages/DashboardPage.svelte:230-237`; `apps/web-admin/src/lib/pages/TransfersPage.svelte:68-78`; `apps/web-admin/src/lib/components/app-update.svelte:82-91`) No `EventSource`/`WebSocket` usage; ADR-0058 rejects SSE for Notifications. (`docs/adr/0058-audit-derived-web-admin-notifications.md:151-153,241-243`)

Reducers should take poll/API outcomes as events and never own intervals, fetches, toasts, history/storage, focus, or DOM state—matching the Client split. (`apps/client/src/lib/machines/spin.ts:1-4,100-134`; `apps/client/src/lib/machines/spin.svelte.ts:1-4,21-39`)

## Recommended first step

Prototype the login flow only. It's small enough to establish the reducer/runner seam without coupling to an owner page's data model, and it exercises async session discovery, mode selection, submission, throttle failure, and timer events. Plain `.ts` module, API/timer work outside the reducer, direct reducer tests for valid transitions and ignored events. This borrows the Client pattern without implying ADR-0089's `apps/client`-only scope applies to Web Admin; if the pattern sticks, widen ADR-0089 or write a Web Admin ADR. (`apps/web-admin/src/lib/pages/LoginPage.svelte:19-75`; `docs/adr/0089-client-ui-state-machines.md:14-21,29-31`)
