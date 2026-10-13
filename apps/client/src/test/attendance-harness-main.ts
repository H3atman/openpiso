import "../app.css";
import { mount } from "svelte";
import AttendanceHarness from "./AttendanceHarness.svelte";

mount(AttendanceHarness, { target: document.getElementById("app")! });
