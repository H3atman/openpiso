// Runner for the setup machine (ADR-0089, ADR-0090 §2). Owns the `$state`
// and the only effects: one discovery round on entering `searching`, the
// retry delay on entering `waiting`, and the save on entering `saving`. The
// Lock calls `start()` while the Client has no saved server URL and `stop()`
// once it has one.

import { discoverServers } from "$lib/api";
import { initialSetupState, reduceSetup, type SetupEvent, type SetupState } from "./setup";

/** Pause between discovery rounds when no server answered. */
const RETRY_DELAY_MS = 3_000;

interface SetupHooks {
  /** Save `url` as the server and connect; rejects with the save error. */
  save: (url: string) => Promise<void>;
}

export function createSetupMachine({ save }: SetupHooks) {
  // Raw: states are replaced whole.
  let current = $state.raw<SetupState>(initialSetupState);
  let running = false;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;

  function dispatch(event: SetupEvent): void {
    if (!running) return;
    const next = reduceSetup(current, event);
    if (next === current) return;
    current = next;
    enter(next);
  }

  function enter(state: SetupState): void {
    clearTimeout(retryTimer);
    switch (state.kind) {
      case "searching":
        void discoverServers()
          .then((servers) => dispatch({ type: "found", servers }))
          // A failed round (no network yet, socket error) counts as none found.
          .catch(() => dispatch({ type: "found", servers: [] }));
        return;
      case "waiting":
        retryTimer = setTimeout(() => dispatch({ type: "retry" }), RETRY_DELAY_MS);
        return;
      case "saving":
        void save(state.url)
          .then(() => dispatch({ type: "saved" }))
          .catch((caught) => dispatch({ type: "saveFailed", error: caught instanceof Error ? caught.message : String(caught) }));
        return;
      case "choosing":
      case "manual":
      case "saved":
        return;
      default: {
        const unreachable: never = state;
        return unreachable;
      }
    }
  }

  return {
    get state() {
      return current;
    },
    /** Begin searching from scratch. */
    start(): void {
      if (running) return;
      running = true;
      current = initialSetupState;
      enter(current);
    },
    /** Stop all effects; late results are dropped. */
    stop(): void {
      running = false;
      clearTimeout(retryTimer);
    },
    pick: (url: string) => dispatch({ type: "pick", url }),
    enterManually: () => dispatch({ type: "enterManually" }),
    submit: (address: string) => dispatch({ type: "submit", address }),
    search: () => dispatch({ type: "search" }),
  };
}
