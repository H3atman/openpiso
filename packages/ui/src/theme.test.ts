import { describe, expect, test } from "bun:test";
import { generateTheme, type GeneratedThemeTokens, type ThemeRecord } from "./theme";

const seed: ThemeRecord = {
  accent_color: "#15803d",
  mode_web_admin: "light",
  mode_kiosk: "light",
  mode_client: "light",
  version: 1,
};

const generatedTokenNames = [
  "primary",
  "primary-foreground",
  "ring",
  "chart-1",
  "chart-2",
  "chart-3",
  "chart-4",
  "chart-5",
  "sidebar-primary",
  "sidebar-primary-foreground",
  "sidebar-ring",
] as const;

function channels(value: string): [lightness: number, chroma: number, hue: number] {
  const match = /^oklch\(([\d.]+) ([\d.]+) ([\d.]+)\)$/.exec(value);
  if (!match) throw new Error(`Invalid generated color: ${value}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

function expectCommonHue(tokens: GeneratedThemeTokens, expected: number): void {
  for (const value of Object.values(tokens)) expect(channels(value)[2]).toBeCloseTo(expected, 6);
}

describe("generateTheme", () => {
  test("applies literal hex conversion and every anchor-snap formula to the seed in both modes", () => {
    const { tokens } = generateTheme(seed);

    expect(tokens).toEqual({
      light: {
        primary: "oklch(0.527 0.137103 150.069279)",
        "primary-foreground": "oklch(0.982 0.016025 150.069279)",
        ring: "oklch(0.708 0.082262 150.069279)",
        "chart-1": "oklch(0.871 0.133541 150.069279)",
        "chart-2": "oklch(0.723 0.19497 150.069279)",
        "chart-3": "oklch(0.627 0.172714 150.069279)",
        "chart-4": "oklch(0.527 0.137103 150.069279)",
        "chart-5": "oklch(0.448 0.105943 150.069279)",
        "sidebar-primary": "oklch(0.627 0.172714 150.069279)",
        "sidebar-primary-foreground": "oklch(0.982 0.016025 150.069279)",
        "sidebar-ring": "oklch(0.708 0.082262 150.069279)",
      },
      dark: {
        primary: "oklch(0.448 0.105943 150.069279)",
        "primary-foreground": "oklch(0.982 0.016025 150.069279)",
        ring: "oklch(0.556 0.063566 150.069279)",
        "chart-1": "oklch(0.871 0.133541 150.069279)",
        "chart-2": "oklch(0.723 0.19497 150.069279)",
        "chart-3": "oklch(0.627 0.172714 150.069279)",
        "chart-4": "oklch(0.527 0.137103 150.069279)",
        "chart-5": "oklch(0.448 0.105943 150.069279)",
        "sidebar-primary": "oklch(0.723 0.19497 150.069279)",
        "sidebar-primary-foreground": "oklch(0.982 0.016025 150.069279)",
        "sidebar-ring": "oklch(0.556 0.063566 150.069279)",
      },
    });
  });

  test("emits exactly the generated tokens in root and dark blocks", () => {
    const { css, tokens } = generateTheme(seed);

    expect(Object.keys(tokens.light).sort()).toEqual([...generatedTokenNames].sort());
    expect(Object.keys(tokens.dark).sort()).toEqual([...generatedTokenNames].sort());
    expect(css.startsWith(":root {\n")).toBe(true);
    expect(css).toContain("\n.dark {\n");
    for (const token of generatedTokenNames) {
      expect(css.match(new RegExp(`--${token}:`, "g"))?.length).toBe(2);
    }
    expect(css).not.toMatch(/--(?:background|foreground|destructive|accent|muted|card|border|input):/);
  });

  test.each(["#000000", "#808080", "#ffffff"])("turns neutral pick %s into a zero-chroma ramp", (accentColor) => {
    const { tokens } = generateTheme({ ...seed, accent_color: accentColor });

    for (const mode of [tokens.light, tokens.dark]) {
      for (const value of Object.values(mode)) expect(channels(value)).toEqual([channels(value)[0], 0, 0]);
    }
  });

  test.each([
    ["#010500", 136.393284],
    ["#f5fff1", 136.591821],
  ] as const)("ignores extreme pick lightness for %s while retaining its calculated hue", (accentColor, hue) => {
    const { light, dark } = generateTheme({ ...seed, accent_color: accentColor }).tokens;

    expect(channels(light.primary)[0]).toBe(0.527);
    expect(channels(dark.primary)[0]).toBe(0.448);
    expect(channels(light["primary-foreground"])[0]).toBe(0.982);
    expectCommonHue(light, hue);
    expectCommonHue(dark, hue);
  });

  test.each([
    ["#ff0000", 29.233885],
    ["#00ff00", 142.495339],
    ["#0000ff", 264.052021],
    ["#ff00ff", 328.363418],
  ] as const)("caps saturated pick %s and uses its hue for every token", (accentColor, hue) => {
    const { tokens } = generateTheme({ ...seed, accent_color: accentColor });

    expect(channels(tokens.light.primary)[1]).toBe(0.2002);
    expect(channels(tokens.light["chart-1"])[1]).toBe(0.195);
    expect(channels(tokens.light["chart-2"])[1]).toBe(0.25);
    expect(channels(tokens.light["primary-foreground"])[1]).toBe(0.018);
    expect(channels(tokens.light.ring)[1]).toBeCloseTo(0.6 * 0.2002, 6);
    expect(channels(tokens.dark.ring)[1]).toBeCloseTo(0.6 * (0.119 * 1.3), 6);
    expectCommonHue(tokens.light, hue);
    expectCommonHue(tokens.dark, hue);
  });

  test("does not let surface modes alter the two generated CSS palettes", () => {
    const changedModes = generateTheme({
      ...seed,
      mode_web_admin: "dark",
      mode_kiosk: "dark",
      mode_client: "dark",
    });

    expect(changedModes).toEqual(generateTheme(seed));
  });

  test.each(["15803d", "#15803", "#15803g", "#15803dff"])("rejects invalid accent %s", (accentColor) => {
    expect(() => generateTheme({ ...seed, accent_color: accentColor })).toThrow(TypeError);
  });
});
