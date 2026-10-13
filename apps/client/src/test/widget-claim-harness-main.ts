import "../app.css";
import { mount } from "svelte";
import WidgetClaimHarness from "./WidgetClaimHarness.svelte";

mount(WidgetClaimHarness, { target: document.getElementById("app")! });
