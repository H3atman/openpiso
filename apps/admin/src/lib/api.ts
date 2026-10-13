import { invoke } from "@tauri-apps/api/core";
import type { ThemeRecord } from "@openpiso/ui";
import type {
  KioskConfig,
  KioskLockState,
  KioskStatus,
  Pc,
  SelectionState,
  ShopStatus,
} from "./types";

// ---------------------------------------------------------------------------
// Central Terminal kiosk surface (M3, ADR-0023; M6 owner-console retirement).
//
// These commands are unauthenticated: the kiosk is the customer-facing
// surface and the only data it reads is the public PC state. All
// authority (money, time, points) still lives in Rust — the kiosk never
// supplies `pc_id` for a coin, never trusts device-supplied time, and
// never mutates balances directly.
//
// M6 retired the desktop owner console. The owner surface (PIN login,
// dashboard, settings, vouchers, manual adjustments, reports, audit,
// account admin) is now reached only via the authenticated HTTP API on
// `http://<host>:8080/admin`; there is no Tauri command equivalent any
// more, by design (ADR-0024).
// ---------------------------------------------------------------------------

/** Live, enriched PC list for the Central Terminal kiosk. */
export function listPcsKiosk(): Promise<Pc[]> {
  return invoke<Pc[]>("list_pcs_kiosk");
}

/** Coin-source health for the Central Terminal kiosk. */
export function getKioskStatus(): Promise<KioskStatus> {
  return invoke<KioskStatus>("get_kiosk_status");
}

/** Live time-purchase rate for the kiosk's "equivalent time" preview.
 *  Read-only; the server still computes the authoritative `seconds_added`. */
export function getKioskConfig(): Promise<KioskConfig> {
  return invoke<KioskConfig>("get_kiosk_config");
}

/** Current shop theme read directly from the admin-owned database. */
export function getTheme(): Promise<ThemeRecord> {
  return invoke<ThemeRecord>("get_theme");
}

/** Current Shop Name read directly from the admin-owned database. */
export function getShopName(): Promise<string> {
  return invoke<string>("get_shop_name");
}

/** Current server-resolved Shop Status for the Central Terminal. */
export function getShopStatus(): Promise<ShopStatus> {
  return invoke<ShopStatus>("get_shop_status");
}

/** Open a Selection Window for `pcId`. The server owns the lock and
 *  asserts the set pin; a second open is rejected with
 *  `"terminal in use"`. */
export function openSelection(pcId: string): Promise<SelectionState> {
  return invoke<SelectionState>("open_selection", { pcId });
}

/** Close the active Selection Window. `reason` is one of
 *  `"enter"`, `"inactivity"`, `"hard_cap"`, `"manual"`, or
 *  `"shutdown"`; defaults to `"manual"` so a bare close from the
 *  kiosk still works. */
export function closeSelection(
  reason?: string,
): Promise<SelectionState | null> {
  return invoke<SelectionState | null>("close_selection", {
    reason: reason ?? null,
  });
}

/** Current Kiosk Lock state (ADR-0056). Always `locked: true` at boot. */
export function getKioskLock(): Promise<KioskLockState> {
  return invoke<KioskLockState>("get_kiosk_lock");
}

/** Engage or release the Kiosk Lock (ADR-0056).
 *
 *  Releasing requires the owner PIN, which Rust verifies — the kiosk only
 *  forwards the typed characters and never decides anything itself.
 *  Re-engaging needs no PIN because tightening the kiosk is always safe.
 *  Rejections surface as an `Error`: an invalid PIN, or a retry-after
 *  message once the terminal has been throttled. */
export function setKioskLock(
  locked: boolean,
  pin?: string,
): Promise<KioskLockState> {
  return invoke<KioskLockState>("set_kiosk_lock", {
    locked,
    pin: pin ?? null,
  });
}

/** Snapshot of the active Selection Window, or `null` if none. */
export function getSelection(): Promise<SelectionState | null> {
  return invoke<SelectionState | null>("get_selection");
}

/** DEV-ONLY coin-insert simulator. Injects `pulseCount` pulses into the
 *  active Selection Window's simulator source so the kiosk can be tested
 *  end-to-end without coin hardware. The backing Tauri command
 *  `dev_inject_coin` is compiled only in debug builds, so this must only be
 *  called behind an `import.meta.env.DEV` guard — invoking it in a release
 *  build rejects (the command does not exist). Valid pulse counts are the
 *  configured map (1 = ₱1, 5 = ₱5, 10 = ₱10, 20 = ₱20 on fresh installs);
 *  other counts fail-closed server-side. */
export function injectSimulatorPulses(
  pulseCount: number,
): Promise<SelectionState> {
  return invoke<SelectionState>("dev_inject_coin", { pulseCount });
}
