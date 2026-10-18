import { invoke } from "@tauri-apps/api/core";
import type { ThemeRecord } from "@openpiso/ui";
import type {
  AccountAuth,
  AccountLoginError,
  AttendanceClaimResponse,
  AttendanceMonth,
  AttendanceSummary,
  ClientConfig,
  CoinPromoMenu,
  PasswordChangeError,
  RedemptionTierMenu,
  PointsTransferCode,
  SetClientConfigResponse,
  SetLaunchAtStartupResponse,
  SetWindowsSignOutResponse,
  TimerState,
  ViewMode,
  WheelSettings,
  WheelSpinResult,
} from "./types";

/** Show Tauri while keeping the native startup placeholder above it. */
export function prepareStartupHandoff(): Promise<void> {
  return invoke("prepare_startup_handoff");
}

/** Retire the native placeholder after the visible Tauri window paints. */
export function frontendReady(): Promise<void> {
  return invoke("frontend_ready");
}

export function getClientConfig(): Promise<ClientConfig> {
  return invoke<ClientConfig>("get_client_config");
}

export function isWindowsPlatform(): Promise<boolean> {
  return invoke<boolean>("is_windows_platform");
}

export function getTheme(): Promise<ThemeRecord | null> {
  return invoke<ThemeRecord | null>("get_theme");
}

/**
 * Whether a saved client config was loaded at startup. `false` on a fresh or
 * corrupt install, where the client runs on the localhost default no owner
 * chose — the Lock then shows the un-gated "set up this PC" form.
 */
export function isClientConfigured(): Promise<boolean> {
  return invoke<boolean>("is_client_configured");
}

export function setClientConfig(
  serverUrl: string,
  viewMode?: ViewMode,
): Promise<SetClientConfigResponse> {
  return invoke<SetClientConfigResponse>("set_client_config", {
    serverUrl,
    viewMode: viewMode ?? null,
  });
}

export function setLaunchAtStartup(
  enabled: boolean,
): Promise<SetLaunchAtStartupResponse> {
  return invoke<SetLaunchAtStartupResponse>("set_launch_at_startup", {
    enabled,
  });
}

export function setWindowsSignOut(
  enabled: boolean,
): Promise<SetWindowsSignOutResponse> {
  return invoke<SetWindowsSignOutResponse>("set_windows_sign_out", {
    enabled,
  });
}

export function connectToServer(): Promise<void> {
  return invoke("connect_to_server");
}

export function disconnectFromServer(): Promise<void> {
  return invoke("disconnect_from_server");
}

/** Pause the Offline Shutdown Countdown only after the owner PIN is unlocked. */
export function pauseOfflineShutdown(): Promise<void> {
  return invoke("pause_offline_shutdown");
}

/** Resume the Offline Shutdown Countdown when owner settings close. */
export function resumeOfflineShutdown(): Promise<void> {
  return invoke("resume_offline_shutdown");
}

export function getTimerState(): Promise<TimerState> {
  return invoke<TimerState>("get_timer_state");
}

export function getAttendanceSummary(): Promise<AttendanceSummary> {
  return invoke<AttendanceSummary>("get_attendance_summary");
}

export function getAttendanceMonth(earningMonth: string): Promise<AttendanceMonth> {
  return invoke<AttendanceMonth>("get_attendance_month", { earningMonth });
}

export function claimAttendance(
  requestId: string,
  earningMonth: string,
): Promise<AttendanceClaimResponse> {
  return invoke<AttendanceClaimResponse>("claim_attendance", {
    requestId,
    earningMonth,
  });
}

export function signUpAccount(
  username: string,
  password: string,
): Promise<AccountAuth> {
  return invoke<AccountAuth>("sign_up_account", { username, password });
}

export function loginAccount(
  username: string,
  password: string,
): Promise<AccountAuth> {
  return invoke<AccountAuth>("login_account", { username, password });
}

/** Map the structured Account login lockout into customer-facing copy. */
export function accountLoginErrorMessage(error: unknown): string {
  const details: AccountLoginError =
    typeof error === "object" && error !== null ? error : {};
  if (details.code === "lockout") {
    const seconds = details.retry_after_seconds;
    if (typeof seconds === "number" && Number.isFinite(seconds) && seconds > 0) {
      const roundedSeconds = Math.ceil(seconds);
      return `Too many failed login attempts. Please wait ${roundedSeconds} second${roundedSeconds === 1 ? "" : "s"} before trying again.`;
    }
    return "Too many failed login attempts. Please wait before trying again.";
  }
  if (details.code === "message" && typeof details.message === "string") {
    return details.message;
  }
  return error instanceof Error ? error.message : String(error);
}

export function getAccountAuth(): Promise<AccountAuth | null> {
  return invoke<AccountAuth | null>("get_account_auth");
}

export function claimGuestTime(): Promise<AccountAuth> {
  return invoke<AccountAuth>("claim_guest_time");
}

export function redeemVoucher(code: string): Promise<AccountAuth> {
  return invoke<AccountAuth>("redeem_voucher", { code });
}

export function getRedemptionTiers(): Promise<RedemptionTierMenu> {
  return invoke<RedemptionTierMenu>("get_redemption_tiers");
}

export function getCoinPromoMenu(): Promise<CoinPromoMenu> {
  return invoke<CoinPromoMenu>("get_coin_promo_menu");
}

export function redeemPoints(tierId: number, version: number): Promise<AccountAuth> {
  return invoke<AccountAuth>("redeem_points", { tierId, version });
}

/**
 * `password` is sent only when the customer was prompted (ADR-0088). Rejects
 * with an `Error` whose message is the server's raw error code or text
 * (`password_required`, `invalid_password`, a throttle message, ...); the spin
 * machine classifies it.
 */
export async function spinWheel(password?: string): Promise<WheelSpinResult> {
  try {
    return await invoke<WheelSpinResult>("spin_wheel", { password });
  } catch (caught) {
    // The command rejects with the server's body text, e.g. {"error":"invalid_password"}.
    const text = caught instanceof Error ? caught.message : String(caught);
    let code = text;
    try {
      const body: unknown = JSON.parse(text);
      if (typeof body === "object" && body !== null && "error" in body && typeof body.error === "string") {
        code = body.error;
      }
    } catch {
      // Not JSON: the text itself is the message.
    }
    throw new Error(code);
  }
}

export function getWheelEnabled(): Promise<boolean> {
  return invoke<boolean>("get_wheel_enabled");
}

export function getWheelSettings(): Promise<WheelSettings> {
  return invoke<WheelSettings>("get_wheel_settings");
}

export function logoutAccount(): Promise<void> {
  return invoke("logout_account");
}

/** Continue preserved PC-bound Guest Time after the neutral Tamper Kick Lock. */
export function continueGuestSession(): Promise<void> {
  return invoke("continue_guest_session");
}

// ---------------------------------------------------------------------------
// Window modes (ADR-0029, plan §5).
// ---------------------------------------------------------------------------

/**
 * Reconfigure the main window into the Fullscreen Lock: borderless-maximized,
 * undecorated, always-on-top. Does not touch `view_mode` in `ClientConfig`
 * because the lock is derived state, not a stored preference.
 */
export function enterFullscreenMode(): Promise<void> {
  return invoke("enter_fullscreen_mode");
}

/**
 * Reconfigure the main window into the floating Widget (320×480, draggable,
 * always-on-top) and persist `view_mode = "widget"` so the next session
 * resumes in the same shape.
 */
export function enterWidgetMode(): Promise<void> {
  return invoke("enter_widget_mode");
}

/**
 * Reconfigure the main window into the Mini Bar (100×30, always-on-top,
 * top-center of the current monitor's work area) and persist
 * `view_mode = "mini"`.
 */
export function enterMiniMode(): Promise<void> {
  return invoke("enter_mini_mode");
}

/**
 * Read the cached shop name. Refreshed on connect / reconnect in phase 2;
 * the frontend reads it on mount and after each `timer-updated` event so the
 * Lock header reflects the owner-edited value.
 */
export function getShopName(): Promise<string> {
  return invoke<string>("get_shop_name");
}

// ---------------------------------------------------------------------------
// Owner PIN gate (ADR-0027 §2, plan §6).
// ---------------------------------------------------------------------------

/**
 * Verify the owner PIN server-side (ADR-0027 §2). The PIN is compared against
 * the hashed `owner_pin` setting on the admin server; the real hash never
 * reaches this machine. Resolves `true` for a correct PIN and `false` for a
 * wrong one; rejects only on a transport/parse failure, so the caller can
 * tell "wrong PIN" apart from "server unreachable".
 */
export function verifyOwnerPin(pin: string): Promise<boolean> {
  return invoke<boolean>("verify_owner_pin", { pin });
}

/**
 * PIN-gated owner exit (ADR-0027 §2). Reachable only from the unlocked
 * settings panel — there is no customer-facing Quit. Notifies the admin
 * server of a clean disconnect, then exits the client.
 */
export function quitClient(): Promise<void> {
  return invoke("quit_client");
}

/**
 * Tell the app-level keyboard-hook hardening whether the Fullscreen Lock is
 * active (ADR-0028). While the Lock shows, escape chords (Win / Alt-Tab /
 * Alt-Esc / Ctrl-Esc / Alt-F4) are swallowed; during a paid session they pass
 * through. No-op unless a release Windows build installed the hook.
 */
export function setKioskLockActive(active: boolean): Promise<void> {
  return invoke("set_kiosk_lock_active", { active });
}

/**
 * Dev-only escape from the Fullscreen Lock window restrictions. Release Rust
 * builds reject this command, and the UI only renders its button in Vite dev
 * mode.
 */
export function devBypassScreenRestriction(): Promise<void> {
  return invoke("dev_bypass_screen_restriction");
}

// ---------------------------------------------------------------------------
// Account self-service actions (ADR-0030, plan §8).
// ---------------------------------------------------------------------------

/**
 * Immediate **Time Transfer** to another account by username (ADR-0030).
 * The server debits the caller and credits the recipient atomically;
 * returns the refreshed `AccountAuthState`.
 */
export function transferTime(
  toUsername: string,
  seconds: number,
): Promise<AccountAuth> {
  return invoke<AccountAuth>("transfer_time", { toUsername, seconds });
}

/**
 * Owner-confirmed **Points Transfer Request** (ADR-0030). Creates a pending
 * request and returns the single-use 6-digit code for the customer to read
 * to the owner. No points move until the owner confirms in web-admin.
 */
export function requestPointsTransfer(
  toUsername: string,
  pointsCenti: number,
): Promise<PointsTransferCode> {
  return invoke<PointsTransferCode>("request_points_transfer", {
    toUsername,
    pointsCenti,
  });
}

/** Password Change error variant to customer display copy (ADR-0037). */
const PASSWORD_CHANGE_COPY: Record<string, string> = {
  conflict:
    "Password could not be changed. Re-enter your current password and try again.",
  internal_error:
    "Password could not be changed. Try again.",
  other:
    "Password could not be changed. Try again.",
};

export function passwordChangeErrorMessage(error: unknown): string {
  const details: PasswordChangeError =
    typeof error === "object" && error !== null ? error : {};
  return details.code === "customer_message" &&
    typeof details.message === "string"
    ? details.message
    : PASSWORD_CHANGE_COPY[details.code ?? "other"] ??
        PASSWORD_CHANGE_COPY.other;
}

/**
 * Customer self-service **Password Change** (ADR-0037). Keeps the Account
 * Session and Active Session valid; returns structured error codes that
 * are mapped to display copy here, not in the Rust command layer.
 */
export function changeAccountPassword(
  oldPassword: string,
  newPassword: string,
): Promise<void> {
  return invoke<void>("change_account_password", {
    oldPassword,
    newPassword,
  }).catch((error: unknown) => {
    throw new Error(passwordChangeErrorMessage(error));
  });
}
