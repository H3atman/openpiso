import "../app.css";
import WidgetTierMenuHarness from "./WidgetTierMenuHarness.svelte";
import { mount } from "svelte";

mount(WidgetTierMenuHarness, { target: document.getElementById("app")! });
