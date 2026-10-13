import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { generateTheme, type ThemeRecord } from "@openpiso/ui";
import { getTheme } from "./api";

const CACHE_KEY = "openpiso.theme-record";
const STYLE_ID = "openpiso-theme";

let appliedVersion: number | null = null;

export function applyThemeRecord(record: ThemeRecord): void {
  if (appliedVersion !== null && record.version < appliedVersion) return;

  let style = document.getElementById(STYLE_ID);
  if (!style) {
    style = document.createElement("style");
    style.id = STYLE_ID;
    document.head.appendChild(style);
  }
  style.textContent = generateTheme(record).css;
  document.documentElement.classList.toggle("dark", record.mode_kiosk === "dark");
  localStorage.setItem(CACHE_KEY, JSON.stringify(record));
  appliedVersion = record.version;
}

export function bootTheme(): void {
  const cached = localStorage.getItem(CACHE_KEY);
  if (!cached) return;
  try {
    applyThemeRecord(JSON.parse(cached) as ThemeRecord);
  } catch {
    localStorage.removeItem(CACHE_KEY);
  }
}

export async function syncTheme(): Promise<UnlistenFn> {
  const unlisten = await listen<ThemeRecord>("theme-updated", ({ payload }) => {
    applyThemeRecord(payload);
  });
  try {
    applyThemeRecord(await getTheme());
  } catch {
    // Keep the cached theme when SQLite startup sync is unavailable.
  }
  return unlisten;
}
