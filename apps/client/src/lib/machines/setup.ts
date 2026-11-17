// First-setup flow for a Client with no saved server URL (ADR-0090 §2,
// ADR-0089). Pure state machine; no I/O. The runner in `setup.svelte.ts`
// broadcasts on entering `searching`, waits on entering `waiting`, and saves
// the config on entering `saving`, then dispatches the result events.
//
//   searching --found[1]--> saving --saved--> saved
//   searching --found[n]--> choosing --pick--> saving
//   searching --found[0]--> waiting --retry--> searching
//   searching | waiting | choosing --enterManually--> manual --submit--> saving
//   manual | choosing --search--> searching
//   saving --saveFailed--> manual (error shown, address kept for editing)
//
// Discovery results and retry timers that arrive after the operator moved
// on (manual entry, saving) are ignored.

import type { DiscoveredServer } from "../types";

export type SetupState =
  | { kind: "searching" }
  /** None found; the runner searches again after a short delay. */
  | { kind: "waiting" }
  | { kind: "choosing"; servers: DiscoveredServer[] }
  | { kind: "manual"; address: string; error: string }
  | { kind: "saving"; url: string }
  /** Config saved; the connect path and the Lock take over. */
  | { kind: "saved" };

export type SetupEvent =
  | { type: "found"; servers: DiscoveredServer[] }
  | { type: "retry" }
  | { type: "pick"; url: string }
  | { type: "enterManually" }
  | { type: "submit"; address: string }
  | { type: "search" }
  | { type: "saved" }
  | { type: "saveFailed"; error: string };

export const initialSetupState: SetupState = { kind: "searching" };

function found(servers: DiscoveredServer[]): SetupState {
  if (servers.length === 0) return { kind: "waiting" };
  if (servers.length === 1) return { kind: "saving", url: servers[0].url };
  return { kind: "choosing", servers };
}

export function reduceSetup(state: SetupState, event: SetupEvent): SetupState {
  switch (event.type) {
    case "found":
      return state.kind === "searching" ? found(event.servers) : state;
    case "retry":
      return state.kind === "waiting" ? { kind: "searching" } : state;
    case "pick":
      return state.kind === "choosing" && state.servers.some((server) => server.url === event.url)
        ? { kind: "saving", url: event.url }
        : state;
    case "enterManually":
      return state.kind === "searching" || state.kind === "waiting" || state.kind === "choosing"
        ? { kind: "manual", address: "", error: "" }
        : state;
    case "submit":
      return state.kind === "manual" ? { kind: "saving", url: event.address } : state;
    case "search":
      return state.kind === "manual" || state.kind === "choosing" ? { kind: "searching" } : state;
    case "saved":
      return state.kind === "saving" ? { kind: "saved" } : state;
    case "saveFailed":
      return state.kind === "saving" ? { kind: "manual", address: state.url, error: event.error } : state;
    default: {
      const unreachable: never = event;
      return unreachable;
    }
  }
}
