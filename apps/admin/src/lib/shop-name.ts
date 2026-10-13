import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { writable } from "svelte/store";
import { getShopName } from "./api";

const DEFAULT_SHOP_NAME = "OpenPiso Shop";

function nonBlankShopName(value: string): string {
  return value.trim() || DEFAULT_SHOP_NAME;
}

export const shopName = writable(DEFAULT_SHOP_NAME);

/** Listen before reading so an owner save cannot be lost during kiosk boot. */
export async function syncShopName(): Promise<UnlistenFn> {
  let revision = 0;
  const unlisten = await listen<string>("shop-name-updated", ({ payload }) => {
    revision += 1;
    shopName.set(nonBlankShopName(payload));
  });

  const revisionBeforeRead = revision;
  const current = await getShopName();
  if (revision === revisionBeforeRead) shopName.set(nonBlankShopName(current));
  return unlisten;
}
