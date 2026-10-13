import { describe, expect, it } from "vitest";
import {
  parseRoute,
  resolveRoute,
  restoreRoute,
  routePath,
  settingsSections,
  type Route,
} from "./routing";

const routes: [string, Route][] = [
  ["/admin/dashboard", { page: "dashboard" }],
  ["/admin/vouchers", { page: "vouchers" }],
  ["/admin/adjustments", { page: "adjustments" }],
  ["/admin/transfers", { page: "transfers" }],
  ["/admin/accounts", { page: "accounts" }],
  ["/admin/reports", { page: "reports" }],
  ["/admin/audit", { page: "audit" }],
  ["/admin/backups", { page: "backups" }],
  ["/admin/notifications", { page: "notifications" }],
  ["/admin/diagnostics", { page: "diagnostics" }],
  ["/admin/settings/points", { page: "settings", section: "points" }],
  ["/admin/settings/coin-promo", { page: "settings", section: "coin-promo" }],
  ["/admin/settings/attendance", { page: "settings", section: "attendance" }],
  ["/admin/settings/wheel", { page: "settings", section: "wheel" }],
  ["/admin/settings/coin-device", { page: "settings", section: "coin-device" }],
  ["/admin/settings/shop", { page: "settings", section: "shop" }],
  ["/admin/settings/pcs", { page: "settings", section: "pcs" }],
  [
    "/admin/settings/pc-monitoring",
    { page: "settings", section: "pc-monitoring" },
  ],
  ["/admin/settings/autostart", { page: "settings", section: "autostart" }],
  ["/admin/settings/security", { page: "settings", section: "security" }],
  ["/admin/settings/appearance", { page: "settings", section: "appearance" }],
];

describe("routing vocabulary", () => {
  it.each(routes)("parses %s", (path, route) => {
    expect(parseRoute(path)).toEqual(route);
  });

  it("uses Points for bare or unknown Settings sections", () => {
    expect(parseRoute("/admin/settings")).toEqual({
      page: "settings",
      section: "points",
    });
    expect(parseRoute("/admin/settings/unknown")).toEqual({
      page: "settings",
      section: "points",
    });
  });

  it("redirects the retired Support section to Diagnostics before the fallback", () => {
    expect(parseRoute("/admin/settings/support")).toEqual({
      page: "diagnostics",
    });
    expect(resolveRoute("/admin/settings/support")).toEqual({
      route: { page: "diagnostics" },
      replacementPath: "/admin/diagnostics",
    });
    // Unconditional: routing never consults Diagnostics Mode (ADR-0081) — the
    // flag-off stub on the Diagnostics page handles the disabled case.
    expect(routePath(parseRoute("/admin/settings/support"))).toBe(
      "/admin/diagnostics",
    );
  });

  it("requests canonical replacement only for Settings fallbacks", () => {
    expect(resolveRoute("/admin/settings")).toEqual({
      route: { page: "settings", section: "points" },
      replacementPath: "/admin/settings/points",
    });
    expect(resolveRoute("/admin/settings/unknown")).toEqual({
      route: { page: "settings", section: "points" },
      replacementPath: "/admin/settings/points",
    });
    expect(resolveRoute("/admin/unknown")).toEqual({
      route: { page: "not-found" },
    });
  });

  it("returns not-found for paths outside the route vocabulary", () => {
    expect(parseRoute("/admin/unknown")).toEqual({ page: "not-found" });
    expect(parseRoute("/admin/dashboard/extra")).toEqual({ page: "not-found" });
    expect(parseRoute("/admin/settings/points/extra")).toEqual({
      page: "not-found",
    });
  });

  it.each(routes)("round-trips %s to its canonical path", (path) => {
    expect(routePath(parseRoute(path))).toBe(path);
  });

  it("restores a screen recorded less than 24 hours ago", () => {
    const now = 1_750_000_000_000;
    expect(
      restoreRoute(
        JSON.stringify({
          path: "/admin/settings/coin-device",
          savedAt: now - 86_399_999,
        }),
        now,
      ),
    ).toEqual({ page: "settings", section: "coin-device" });
  });

  it("does not restore an expired screen", () => {
    const now = 1_750_000_000_000;
    expect(
      restoreRoute(
        JSON.stringify({ path: "/admin/reports", savedAt: now - 86_400_000 }),
        now,
      ),
    ).toBeNull();
  });

  it.each([
    ["an absent value", null],
    ["a malformed value", "not json"],
    [
      "a no-longer-valid address",
      JSON.stringify({
        path: "/admin/retired-screen",
        savedAt: 1_750_000_000_000,
      }),
    ],
  ])("does not restore %s", (_case, rawStored) => {
    expect(restoreRoute(rawStored, 1_750_000_000_000)).toBeNull();
  });

  it("keeps Settings sections in owner-visible order", () => {
    expect(settingsSections).toEqual([
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
    ]);
  });
});
