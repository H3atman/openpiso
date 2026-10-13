import { toast } from "svelte-sonner";

const SUCCESS_DURATION_MS = 4_000;
const FAILURE_DURATION_MS = 8_000;

export type NotificationAction =
  | "save"
  | "reset"
  | "copy"
  | "register"
  | "edit"
  | "remove"
  | "create"
  | "import"
  | "void"
  | "export"
  | "print"
  | "adjust"
  | "approve"
  | "reject"
  | "complete"
  | "delete"
  | "acknowledge"
  | "credit"
  | "check"
  | "verify"
  | "detect"
  | "sign-out"
  | "suppress"
  | "cancel-suppression"
  | "request"
  | "place"
  | "download";

const actionCopy: Record<
  NotificationAction,
  { present: string; past: string }
> = {
  save: { present: "save", past: "saved" },
  reset: { present: "reset", past: "reset" },
  copy: { present: "copy", past: "copied" },
  register: { present: "register", past: "registered" },
  edit: { present: "update", past: "updated" },
  remove: { present: "remove", past: "removed" },
  create: { present: "create", past: "created" },
  import: { present: "import", past: "imported" },
  void: { present: "void", past: "voided" },
  export: { present: "export", past: "exported" },
  print: { present: "print", past: "printed" },
  adjust: { present: "adjust", past: "adjusted" },
  approve: { present: "approve", past: "approved" },
  reject: { present: "reject", past: "rejected" },
  complete: { present: "complete", past: "completed" },
  delete: { present: "delete", past: "deleted" },
  acknowledge: { present: "acknowledge", past: "acknowledged" },
  credit: { present: "credit", past: "credited" },
  check: { present: "check", past: "checked" },
  verify: { present: "verify", past: "verified" },
  detect: { present: "detect", past: "detected" },
  "sign-out": { present: "sign out", past: "signed out" },
  suppress: { present: "suppress", past: "suppressed" },
  "cancel-suppression": {
    present: "cancel suppression for",
    past: "suppression cancelled",
  },
  request: { present: "request", past: "requested" },
  place: { present: "place", past: "placed" },
  download: { present: "download", past: "downloaded" },
};

function caughtMessage(caught: unknown): string {
  if (caught instanceof Error) return caught.message;
  if (typeof caught === "string") return caught;
  try {
    return JSON.stringify(caught) ?? String(caught);
  } catch {
    return String(caught);
  }
}

export function reportAttendanceSettingsOutcome(outcome: "updated" | "unchanged"): void {
  toast.success(
    outcome === "updated" ? "Attendance settings updated" : "Attendance settings unchanged",
    { duration: SUCCESS_DURATION_MS },
  );
}

export function reportActionSuccess(
  action: NotificationAction,
  subject: string,
  variant?: "coin-debug-window-reset" | "default-password",
): void {
  const options =
    variant === "coin-debug-window-reset"
      ? {
          description: "Coin Debug Window reset",
          duration: SUCCESS_DURATION_MS,
        }
      : { duration: SUCCESS_DURATION_MS };
  const title =
    variant === "default-password"
      ? `${subject} ${actionCopy[action].past} with default password.`
      : `${subject} ${actionCopy[action].past}`;
  toast.success(title, options);
}

export function reportActionFailure(
  action: NotificationAction,
  subject: string,
  caught: unknown,
): void {
  toast.error(`Could not ${actionCopy[action].present} ${subject}`, {
    description: caughtMessage(caught),
    duration: FAILURE_DURATION_MS,
  });
}
