// M3 / M6 (ADR-0023 / ADR-0024) kiosk surface — the only Tauri-facing
// types the customer Central Terminal needs. The desktop owner console
// was retired in M6; the owner data shapes (Dashboard, Account,
// Voucher, Wheel, Audit, Report, Economy, ManualAdjustment, …) now
// live on the `:8080/admin` HTTP API and are owned by the
// `apps/web-admin` workspace app.

export type PcStatus = "offline" | "online" | "in_use" | "locked";

export interface ShopStatus {
  open: boolean;
  next_change_at: string | null;
  reason: "schedule" | "override";
  last_call_active: boolean;
}

export interface Pc {
  id: string;
  name: string;
  status: PcStatus;
  remaining_seconds: number;
  guest_remaining_seconds: number;
  timer_mode: "guest" | "account";
  active_session: ActiveSession | null;
  shop_status?: ShopStatus;
  last_seen_at: string | null;
  clock_difference_ms?: number | null;
}

export interface ActiveSession {
  id: string;
  account_id: string;
  username: string;
  pc_id: string;
  remaining_seconds: number;
  points_balance_centi: number;
  claimable_guest_seconds: number;
}

/**
 * M3 kiosk surface: live snapshot of the active Terminal Selection
 * (ADR-0023). The Rust side broadcasts a fresh copy on every accepted
 * coin and on every close reason, so the kiosk can render the
 * coin-insert window in real time without trusting device-supplied
 * time or amount values.
 */
export interface SelectionState {
  pc_id: string;
  opened_at_unix_ms: number;
  inactivity_remaining_seconds: number;
  hard_cap_remaining_seconds: number;
  inserted_amount_cents: number;
  inserted_seconds: number;
}

/**
 * M3 kiosk surface: coin-source health. When `kind` is `out_of_order`
 * the kiosk refuses to open a Selection Window and renders an
 * operator-facing screen. The M2 simulator is always `available`;
 * the M4 serial source publishes `out_of_order` whenever the COM
 * handle is dead.
 */
export type KioskStatusKind = "available" | "out_of_order";

export interface KioskStatus {
  kind: KioskStatusKind;
  /** Short operator-facing reason. Empty when `kind` is `available`. */
  reason: string;
}

/** Live time-purchase rate mirrored to the kiosk for its "equivalent time"
 *  preview. The server stays authoritative for committed time;
 *  `seconds = inserted_cents * rate_seconds / rate_amount_cents`. */
export interface KioskConfig {
  rate_amount_cents: number;
  rate_seconds: number;
}

/** Whether the Central Terminal window is pinned above every other window
 *  (ADR-0056). Ephemeral and server-owned: it is never persisted, and the
 *  kiosk always boots `locked: true`. */
export interface KioskLockState {
  locked: boolean;
}
