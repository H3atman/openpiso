import { mount } from "svelte";
import AuthenticationRouteHarness from "./AuthenticationRouteHarness.svelte";
import FullscreenLockHarness from "./FullscreenLockHarness.svelte";
import "../app.css";

const Harness = new URLSearchParams(window.location.search).has("route-harness")
  ? AuthenticationRouteHarness
  : FullscreenLockHarness;

mount(Harness, {
  target: document.getElementById("app")!,
});
