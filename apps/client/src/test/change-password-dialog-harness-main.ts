import { mount } from "svelte";
import ChangePasswordDialogHarness from "./ChangePasswordDialogHarness.svelte";
import "../app.css";

mount(ChangePasswordDialogHarness, {
  target: document.getElementById("app")!,
});
