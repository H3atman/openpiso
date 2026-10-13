export const settingsSections = [
  { id: "points", label: "Points" },
  { id: "coin-promo", label: "Coin Promo" },
  { id: "attendance", label: "Attendance" },
  { id: "wheel", label: "Wheel" },
  { id: "coin-device", label: "Coin Device" },
  { id: "shop", label: "Shop" },
  { id: "pcs", label: "PCs" },
  { id: "pc-monitoring", label: "PC Monitoring" },
  { id: "telegram", label: "Telegram Alerts" },
  { id: "autostart", label: "Autostart" },
  { id: "security", label: "Security" },
  { id: "appearance", label: "Appearance" },
] as const;

export type SettingsSection = (typeof settingsSections)[number]["id"];

export type Route =
  | { page: TopLevelPage }
  | { page: "settings"; section: SettingsSection }
  | { page: "not-found" };

const topLevelPages = [
  "dashboard",
  "vouchers",
  "adjustments",
  "transfers",
  "accounts",
  "reports",
  "audit",
  "backups",
  "notifications",
  "diagnostics",
] as const;

type TopLevelPage = (typeof topLevelPages)[number];

const LAST_ROUTE_MAX_AGE_MS = 24 * 60 * 60 * 1_000;

function isTopLevelPage(value: string): value is TopLevelPage {
  return topLevelPages.some((page) => page === value);
}

function isSettingsSection(value: string): value is SettingsSection {
  return settingsSections.some((section) => section.id === value);
}

export function parseRoute(path: string): Route {
  const segments = path.replace(/^\//, "").split("/");
  const [base, page = "", section = ""] = segments;
  if (base !== "admin") return { page: "not-found" };
  if (segments.length === 2 && isTopLevelPage(page)) return { page };
  if (page === "settings" && segments.length <= 3) {
    // The retired Support id redirects to Diagnostics (ADR-0081) before the
    // unknown-section fallback, so a stale link never lands somewhere
    // unrelated. The redirect is unconditional: it never sees Diagnostics
    // Mode — the flag-off stub on the destination handles that case.
    if (section === "support") return { page: "diagnostics" };
    return {
      page: "settings",
      section: isSettingsSection(section) ? section : "points",
    };
  }
  return { page: "not-found" };
}

export function resolveRoute(path: string): {
  route: Route;
  replacementPath?: string;
} {
  const route = parseRoute(path);
  const segments = path.replace(/^\//, "").split("/");
  if (
    segments[0] === "admin" &&
    segments[1] === "settings" &&
    segments.length <= 3
  ) {
    const canonicalPath = routePath(route);
    return canonicalPath === path
      ? { route }
      : { route, replacementPath: canonicalPath };
  }
  return { route };
}

export function restoreRoute(
  rawStored: string | null,
  now: number,
): Route | null {
  if (!rawStored) return null;

  try {
    const stored: unknown = JSON.parse(rawStored);
    if (typeof stored !== "object" || stored === null) return null;

    const { path, savedAt } = stored as Record<string, unknown>;
    if (typeof path !== "string" || typeof savedAt !== "number") return null;

    const age = now - savedAt;
    if (!Number.isFinite(savedAt) || age < 0 || age >= LAST_ROUTE_MAX_AGE_MS)
      return null;

    const route = parseRoute(path);
    return route.page === "not-found" || routePath(route) !== path
      ? null
      : route;
  } catch {
    return null;
  }
}

export function routePath(route: Route): string {
  switch (route.page) {
    case "settings":
      return `/admin/settings/${route.section}`;
    case "not-found":
      return "/admin/not-found";
    default:
      return `/admin/${route.page}`;
  }
}
