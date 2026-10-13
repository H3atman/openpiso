import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";
import { syncKioskConfig } from "$lib/kiosk-config";
import { bootTheme, syncTheme } from "$lib/theme";
import { syncShopName } from "$lib/shop-name";

bootTheme();

async function start(): Promise<void> {
  await Promise.all([
    syncKioskConfig().catch(() => undefined),
    syncTheme().catch(() => undefined),
    syncShopName().catch(() => undefined),
  ]);
  mount(App, {
    target: document.getElementById("app")!,
  });
}

void start();
