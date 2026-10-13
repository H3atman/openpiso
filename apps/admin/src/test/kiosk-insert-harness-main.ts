// Browser harness for the Central Terminal insert screen.
//
// Mounts the real `App.svelte` behind a stubbed `__TAURI_INTERNALS__` so a
// headless Chrome driver can open a Selection Window and push
// `selection-state` events exactly the way the Rust `EventHub` does. The
// stub answers only the kiosk command surface (`src/lib/api.ts`); anything
// else rejects loudly so a missing command shows up as a harness failure
// rather than a silent empty screen.

import "../app.css";
import { mount } from "svelte";
import App from "../App.svelte";
import { syncKioskConfig } from "../lib/kiosk-config";
import type { Pc, SelectionState } from "../lib/types";

interface TauriEvent {
  event: string;
  id: number;
  payload: unknown;
}

type EventHandler = (event: TauriEvent) => void;

interface TauriInternals {
  transformCallback(callback: EventHandler): number;
  invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown>;
}

interface Harness {
  /** Emit a Tauri event the way the Rust `EventHub` does. */
  emit(event: string, payload: unknown): void;
  /** Simulate one accepted coin: bump the server-side totals, then push
   *  the `selection-state` and `pc-updated` events the commit produces. */
  insertCoin(amountCents: number, secondsAdded: number): void;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__: TauriInternals;
    __harness: Harness;
  }
}

const listeners = new Map<string, number[]>();
const callbacks = new Map<number, EventHandler>();
let nextCallbackId = 0;

const pc: Pc = {
  id: "pc-01",
  name: "PC 01",
  status: "online",
  remaining_seconds: 0,
  guest_remaining_seconds: 0,
  timer_mode: "guest",
  active_session: null,
  shop_status: {
    open: true,
    next_change_at: null,
    reason: "schedule",
    last_call_active: false,
  },
  last_seen_at: null,
};

let selection: SelectionState | null = null;

function emit(event: string, payload: unknown): void {
  for (const id of listeners.get(event) ?? []) {
    callbacks.get(id)?.({ event, id, payload });
  }
}

window.__TAURI_INTERNALS__ = {
  transformCallback(callback: EventHandler): number {
    const id = ++nextCallbackId;
    callbacks.set(id, callback);
    return id;
  },
  invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
    switch (cmd) {
      case "plugin:event|listen": {
        const event = String(args?.event);
        const id = Number(args?.handler);
        listeners.set(event, [...(listeners.get(event) ?? []), id]);
        return Promise.resolve(id);
      }
      case "plugin:event|unlisten":
        return Promise.resolve();
      case "list_pcs_kiosk":
        return Promise.resolve([pc]);
      case "get_kiosk_status":
        return Promise.resolve({ kind: "available", reason: "" });
      case "get_kiosk_config":
        return Promise.resolve({ rate_amount_cents: 100, rate_seconds: 300 });
      case "get_shop_status":
        return Promise.resolve(pc.shop_status);
      case "get_shop_name":
        return Promise.resolve("Harness Shop");
      // ADR-0056: the real kiosk boots locked and the overlay owns the
      // keyboard. This harness drives the grid, so it starts unlocked.
      case "get_kiosk_lock":
        return Promise.resolve({ locked: false });
      case "get_theme":
        return Promise.reject(new Error("no theme in harness"));
      case "get_selection":
        return Promise.resolve(selection);
      case "open_selection":
        selection = {
          pc_id: String(args?.pcId),
          opened_at_unix_ms: 0,
          inactivity_remaining_seconds: 15,
          hard_cap_remaining_seconds: 120,
          inserted_amount_cents: 0,
          inserted_seconds: 0,
        };
        return Promise.resolve(selection);
      case "close_selection":
        selection = null;
        return Promise.resolve(null);
      default:
        return Promise.reject(new Error(`unstubbed command: ${cmd}`));
    }
  },
};

window.__harness = {
  emit,
  insertCoin(amountCents: number, secondsAdded: number): void {
    if (!selection) throw new Error("no open selection to insert into");
    selection = {
      ...selection,
      inactivity_remaining_seconds: 15,
      inserted_amount_cents: selection.inserted_amount_cents + amountCents,
      inserted_seconds: selection.inserted_seconds + secondsAdded,
    };
    pc.status = "in_use";
    pc.remaining_seconds += secondsAdded;
    pc.guest_remaining_seconds += secondsAdded;
    emit("pc-updated", { ...pc });
    emit("selection-state", selection);
  },
};

void syncKioskConfig().then(() => {
  mount(App, { target: document.getElementById("app")! });
});
