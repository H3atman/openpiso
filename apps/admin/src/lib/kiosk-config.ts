import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { writable } from "svelte/store";
import { getKioskConfig } from "./api";
import type { KioskConfig } from "./types";

/** The active Shop Rate used by the Central Terminal's display-only preview. */
export const kioskConfig = writable<KioskConfig | null>(null);

/** Listen before reading so an owner save cannot be lost during kiosk boot. */
export async function syncKioskConfig(): Promise<UnlistenFn> {
  let revision = 0;
  const unlisten = await listen<KioskConfig>("shop-rate-updated", ({ payload }) => {
    revision += 1;
    kioskConfig.set(payload);
  });

  const revisionBeforeRead = revision;
  const current = await getKioskConfig();
  if (revision === revisionBeforeRead) kioskConfig.set(current);
  return unlisten;
}
