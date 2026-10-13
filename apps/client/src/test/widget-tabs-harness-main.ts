import "../app.css";
import { mount } from "svelte";
import WidgetTabsHarness from "./WidgetTabsHarness.svelte";

mount(WidgetTabsHarness, { target: document.getElementById("app")! });
