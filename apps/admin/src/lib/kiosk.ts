// Central Terminal kiosk helpers — visual tones for the PC grid and
// the auto-delete "reason" string the kiosk shows when the
// customer presses Enter on a number that can't be committed.

import { formatDuration } from "$lib/format";
import type { KioskConfig, Pc, PcStatus } from "$lib/types";

/** Compute the display-only equivalent time for the current Shop Rate. */
export function equivalentTimeSeconds(
  amountCents: number,
  rate: KioskConfig,
): number {
  if (
    !Number.isSafeInteger(amountCents) ||
    amountCents < 0 ||
    !Number.isSafeInteger(rate.rate_amount_cents) ||
    rate.rate_amount_cents <= 0 ||
    !Number.isSafeInteger(rate.rate_seconds) ||
    rate.rate_seconds <= 0
  ) {
    return 0;
  }

  const numerator = BigInt(amountCents) * BigInt(rate.rate_seconds);
  return Number(numerator / BigInt(rate.rate_amount_cents));
}

/** Build the optional live-state line shown after an in-use PC is selected. */
export function insertScreenStateLine(
  pc: Pc,
  remainingSeconds = pc.remaining_seconds,
): string | null {
  if (pc.status !== "in_use") return null;

  const usernameSuffix = pc.active_session
    ? ` · Logged in: ${pc.active_session.username}`
    : "";
  return `PC ${pcDisplayNumber(pc)} · In use · Remaining Time: ${formatDuration(remainingSeconds)}${usernameSuffix}`;
}

/** A PC is available when reachable and not owner-locked. Existing
 *  Remaining Time is not an unavailability signal: customers may top up
 *  both Guest Time and an Active Session. */
export function isAvailablePc(pc: Pc | null | undefined): boolean {
  if (!pc) return false;
  return pc.status === "online" || pc.status === "in_use";
}

/** Strip the `pc-` prefix and zero-pad to 2 digits. PCs are seeded
 *  as `pc-01`, `pc-02`, …; the customer types `01`. */
export function pcDisplayNumber(pc: Pc | null | undefined): string {
  if (!pc) return "";
  return pc.id.replace(/^pc-/, "").padStart(2, "0");
}

/** Map a PC status to a small, kiosk-readable label and tone. The
 *  tones drive the per-PC tile's status dot, emphasized border, and
 *  background tint. */
export interface StatusTone {
  label: string;
  /** Tailwind class set for the dot / stripe. */
  dot: string;
  stripe: string;
  /** Solid border color (the emphasized 2px tile border). */
  border: string;
  /** Subtle background tint, keyed off the status color. */
  tint: string;
  text: string;
}

export function statusTone(status: PcStatus): StatusTone {
  switch (status) {
    case "online":
      return {
        label: "Available",
        dot: "bg-emerald-500",
        stripe: "bg-emerald-500",
        border: "border-emerald-500/70",
        tint: "bg-emerald-500/5",
        text: "text-emerald-600 dark:text-emerald-400",
      };
    case "in_use":
      return {
        label: "In use — top-up OK",
        dot: "bg-amber-500",
        stripe: "bg-amber-500",
        border: "border-amber-500/70",
        tint: "bg-amber-500/5",
        text: "text-amber-600 dark:text-amber-400",
      };
    case "locked":
      return {
        label: "Locked",
        dot: "bg-rose-500",
        stripe: "bg-rose-500",
        border: "border-rose-500/70",
        tint: "bg-rose-500/5",
        text: "text-rose-600 dark:text-rose-400",
      };
    case "offline":
    default:
      return {
        label: "Offline",
        dot: "bg-zinc-400",
        stripe: "bg-zinc-400",
        border: "border-zinc-300 dark:border-zinc-700",
        tint: "bg-zinc-500/5",
        text: "text-zinc-500 dark:text-zinc-400",
      };
  }
}

/** Header signal for the Kiosk Lock (ADR-0056). Locked is the normal,
 *  expected state, so it gets the calm filled dot the header already had;
 *  unlocked is the anomaly the owner needs to notice, so it goes hollow and
 *  muted rather than alarming — a customer must not read it as an error. */
export interface KioskLockSignal {
  /** Subtitle under the Shop Name. */
  label: string;
  /** Tailwind classes for the header dot. */
  dot: string;
  /** Screen-reader description of the current state. */
  description: string;
}

export function kioskLockSignal(locked: boolean): KioskLockSignal {
  return locked
    ? {
        label: "Kiosk · Locked",
        dot: "bg-primary ring-4 ring-primary/20",
        description: "Kiosk is pinned above other windows",
      }
    : {
        label: "Kiosk · Unlocked",
        dot: "bg-transparent border-2 border-muted-foreground/60",
        description: "Kiosk is not pinned above other windows",
      };
}

/** Whether the hidden unlock gesture on the Shop Name may fire.
 *
 *  Grid only: while a Selection Window is open a customer is feeding coins
 *  against a live server-side inactivity timer, and no owner gesture may
 *  steal focus or key input from it. */
export function canToggleKioskLock(
  screen: "grid" | "insert",
  hasSelection: boolean,
): boolean {
  return screen === "grid" && !hasSelection;
}

/** Pick the "reason a typed number is not committable" message.
 *  Used to show a brief shake + auto-delete feedback when the
 *  customer presses Enter on a bad number. */
export function unavailabilityReason(
  pc: Pc | null | undefined,
  raw: string,
): string {
  if (!raw.trim()) return "Type a PC number first.";
  if (!pc) return `No PC ${raw.trim().padStart(2, "0")} on this terminal.`;
  if (pc.status === "offline") return `${pc.name} is offline.`;
  if (pc.status === "locked") return `${pc.name} is locked.`;
  return `${pc.name} is not available.`;
}
