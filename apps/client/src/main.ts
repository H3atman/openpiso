import { mount } from "svelte";
import App from "./App.svelte";
import { bootTheme, listenForThemeUpdates } from "$lib/theme";
import "./app.css";

bootTheme();
void listenForThemeUpdates();

try {
  mount(App, {
    target: document.getElementById("app")!,
  });
} finally {
  // The Svelte app owns the screen from here; drop the inlined boot splash.
  // In `finally` so a failed mount surfaces the broken app rather than
  // leaving a kiosk PC on a "Starting…" screen forever.
  document.getElementById("boot")?.remove();
}
