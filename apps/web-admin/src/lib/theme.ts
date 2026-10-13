import { generateTheme, type ThemeRecord } from "@openpiso/ui";

const CACHE_KEY = "openpiso.theme-record";
const LEGACY_KEY = "openpiso.theme";
const STYLE_ID = "openpiso-theme";
const DARK_CLASS = "dark";

let appliedVersion: number | null = null;

/// Version of the record currently injected, or null when the static
/// theme.css defaults are in effect. The root-layout poller compares this
/// against the fetched record's `version` to skip redundant re-applies.
export function appliedThemeVersion(): number | null {
  return appliedVersion;
}

/// Generate CSS from the raw record and swap it into the single
/// `<style id="openpiso-theme">` element; drive the `dark` class from the
/// central `mode_web_admin`; cache the raw record for offline boots.
export function applyThemeRecord(record: ThemeRecord): void {
  const { css } = generateTheme(record);
  let style = document.getElementById(STYLE_ID);
  if (!style) {
    style = document.createElement("style");
    style.id = STYLE_ID;
    document.head.appendChild(style);
  }
  style.textContent = css;
  document.documentElement.classList.toggle(DARK_CLASS, record.mode_web_admin === "dark");
  localStorage.setItem(CACHE_KEY, JSON.stringify(record));
  appliedVersion = record.version;
}

/// Call before mount: apply the cached record so a reload with the server
/// unreachable keeps the last-applied theme. No cache → static theme.css
/// defaults (identical to the seed record).
export function bootTheme(): void {
  localStorage.removeItem(LEGACY_KEY);
  const raw = localStorage.getItem(CACHE_KEY);
  if (!raw) return;
  try {
    applyThemeRecord(JSON.parse(raw) as ThemeRecord);
  } catch {
    localStorage.removeItem(CACHE_KEY);
  }
}
