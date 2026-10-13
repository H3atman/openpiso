import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { generateTheme, type ThemeRecord } from "@openpiso/ui";
import { getTheme } from "$lib/api";

const CACHE_KEY = "openpiso.theme-record";
const STYLE_ID = "openpiso-theme";

export function applyThemeRecord(record: ThemeRecord): void {
  const { css } = generateTheme(record);
  let style = document.getElementById(STYLE_ID);
  if (!style) {
    style = document.createElement("style");
    style.id = STYLE_ID;
    document.head.appendChild(style);
  }
  style.textContent = css;
  document.documentElement.classList.toggle("dark", record.mode_client === "dark");
  localStorage.setItem(CACHE_KEY, JSON.stringify(record));
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

export async function listenForThemeUpdates(): Promise<UnlistenFn> {
  const unlisten = await listen<ThemeRecord>("theme-updated", (event) =>
    applyThemeRecord(event.payload),
  );
  const current = await getTheme();
  if (current) applyThemeRecord(current);
  return unlisten;
}
