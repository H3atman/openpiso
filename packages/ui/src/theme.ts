export type ThemeMode = "light" | "dark";

export interface ThemeRecord {
  accent_color: string;
  mode_web_admin: ThemeMode;
  mode_kiosk: ThemeMode;
  mode_client: ThemeMode;
  version: number;
}

export type GeneratedThemeToken =
  | "primary"
  | "primary-foreground"
  | "ring"
  | "chart-1"
  | "chart-2"
  | "chart-3"
  | "chart-4"
  | "chart-5"
  | "sidebar-primary"
  | "sidebar-primary-foreground"
  | "sidebar-ring";

export type GeneratedThemeTokens = Record<GeneratedThemeToken, string>;

export interface GeneratedTheme {
  css: string;
  tokens: Record<ThemeMode, GeneratedThemeTokens>;
}

type Anchor = readonly [lightness: number, chroma: number];

const SHARED_ANCHORS = {
  "chart-1": [0.871, 0.15],
  "chart-2": [0.723, 0.219],
  "chart-3": [0.627, 0.194],
  "chart-4": [0.527, 0.154],
  "chart-5": [0.448, 0.119],
} as const satisfies Record<`chart-${1 | 2 | 3 | 4 | 5}`, Anchor>;

const MODE_ANCHORS = {
  light: {
    primary: [0.527, 0.154],
    "sidebar-primary": [0.627, 0.194],
    ringLightness: 0.708,
  },
  dark: {
    primary: [0.448, 0.119],
    "sidebar-primary": [0.723, 0.219],
    ringLightness: 0.556,
  },
} as const;

function hexToOklch(hex: string): { chroma: number; hue: number } {
  if (!/^#[0-9a-fA-F]{6}$/.test(hex)) throw new TypeError("accent_color must be a six-digit hex color");

  const channel = (offset: number): number => {
    const value = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  const red = channel(1);
  const green = channel(3);
  const blue = channel(5);
  const l = Math.cbrt(0.4122214708 * red + 0.5363325363 * green + 0.0514459929 * blue);
  const m = Math.cbrt(0.2119034982 * red + 0.6806995451 * green + 0.1073969566 * blue);
  const s = Math.cbrt(0.0883024619 * red + 0.2817188376 * green + 0.6299787005 * blue);
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const b = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  const chroma = Math.hypot(a, b);
  const hue = chroma < 1e-7 ? 0 : (Math.atan2(b, a) * 180) / Math.PI;

  return { chroma, hue: hue < 0 ? hue + 360 : hue };
}

function formatNumber(value: number): string {
  return Number(value.toFixed(6)).toString();
}

function color(lightness: number, chroma: number, hue: number): string {
  return `oklch(${formatNumber(lightness)} ${formatNumber(chroma)} ${formatNumber(hue)})`;
}

function scaledColor(anchor: Anchor, scale: number, hue: number): string {
  return color(anchor[0], Math.min(anchor[1] * scale, 0.25), hue);
}

function generateMode(mode: ThemeMode, scale: number, hue: number): GeneratedThemeTokens {
  const anchors = MODE_ANCHORS[mode];
  const primaryChroma = Math.min(anchors.primary[1] * scale, 0.25);
  const foreground = color(0.982, 0.018 * Math.min(scale, 1), hue);
  const ring = color(anchors.ringLightness, 0.6 * primaryChroma, hue);

  return {
    primary: color(anchors.primary[0], primaryChroma, hue),
    "primary-foreground": foreground,
    ring,
    "chart-1": scaledColor(SHARED_ANCHORS["chart-1"], scale, hue),
    "chart-2": scaledColor(SHARED_ANCHORS["chart-2"], scale, hue),
    "chart-3": scaledColor(SHARED_ANCHORS["chart-3"], scale, hue),
    "chart-4": scaledColor(SHARED_ANCHORS["chart-4"], scale, hue),
    "chart-5": scaledColor(SHARED_ANCHORS["chart-5"], scale, hue),
    "sidebar-primary": scaledColor(anchors["sidebar-primary"], scale, hue),
    "sidebar-primary-foreground": foreground,
    "sidebar-ring": ring,
  };
}

function cssBlock(selector: ":root" | ".dark", tokens: GeneratedThemeTokens): string {
  const declarations = Object.entries(tokens).map(([token, value]) => `  --${token}: ${value};`);
  return `${selector} {\n${declarations.join("\n")}\n}`;
}

export function generateTheme(record: ThemeRecord): GeneratedTheme {
  const pick = hexToOklch(record.accent_color);
  const scale = Math.min(Math.max(pick.chroma / 0.154, 0), 1.3);
  const tokens = {
    light: generateMode("light", scale, pick.hue),
    dark: generateMode("dark", scale, pick.hue),
  };

  return {
    css: `${cssBlock(":root", tokens.light)}\n\n${cssBlock(".dark", tokens.dark)}\n`,
    tokens,
  };
}
