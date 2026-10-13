import { mount } from "svelte";
import App from "./App.svelte";
import { bootTheme } from "$lib/theme";
import "./app.css";

bootTheme();

mount(App, {
  target: document.getElementById("app")!,
});
