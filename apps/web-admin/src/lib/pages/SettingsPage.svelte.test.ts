// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", () => ({
  getEconomySettings: vi.fn(async () => ({
    earn_cents_per_point: 5000,
    rate_amount_cents: 100,
    rate_seconds: 600,
    effective_bonus_basis_points: 0,
    unsafe_bonus_threshold_basis_points: 5000,
    unsafe_bonus: false,
  })),
  getRedemptionTiers: vi.fn(async () => [
    { id: 1, points: 10, seconds: 600 },
    { id: 2, points: 50, seconds: 3600 },
  ]),
}));

vi.mock("../theme", () => ({ applyThemeRecord: vi.fn() }));

import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { createMatchMedia } from "../../test/match-media";
import SettingsPageHarness from "./SettingsPageHarness.svelte";
import { settingsSections } from "../routing";

function setMatchMedia(matches: boolean): void {
  vi.stubGlobal("matchMedia", createMatchMedia(matches));
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Settings section index (ADR-0065)", () => {
  it("lists every Settings section below 768px on the bare path", () => {
    setMatchMedia(true);
    render(SettingsPageHarness, {
      props: { section: "points", bare: true, onSelectSection: vi.fn() },
    });

    const entries = screen.getAllByRole("button");
    expect(entries.map((entry) => entry.textContent?.trim())).toEqual(
      settingsSections.map((section) => section.label),
    );
  });

  it("reports the chosen section when one is picked", async () => {
    setMatchMedia(true);
    const user = userEvent.setup();
    const onSelectSection = vi.fn();
    render(SettingsPageHarness, {
      props: { section: "points", bare: true, onSelectSection },
    });

    await user.click(screen.getByRole("button", { name: "Shop" }));

    expect(onSelectSection).toHaveBeenCalledWith("shop");
  });

  it("renders the resolved section instead of the index at 768px and above", () => {
    setMatchMedia(false);
    render(SettingsPageHarness, {
      props: { section: "points", bare: true, onSelectSection: vi.fn() },
    });

    expect(screen.queryByRole("button", { name: "Shop" })).toBeNull();
    expect(screen.getByRole("heading", { name: "Points" })).toBeTruthy();
  });
});
