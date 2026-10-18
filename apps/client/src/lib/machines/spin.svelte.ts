// Runner for the spin machine (ADR-0089). Owns the `$state` and the only
// effects: fetching wheel settings in `open()`, calling the server on entering
// `requesting`, and the Spin Duration timer on entering `animating`. Hosts
// render `state` and call the methods below.

import { getWheelSettings, spinWheel } from "$lib/api";
import { classifySpinError, initialSpinState, reduceSpin, type SpinEvent, type SpinState } from "./spin";

/** How long to wait after the wheel stops before showing the result text. */
const WAIT_BEFORE_REVEAL_MS = 400;

interface SpinHooks {
  /** Refresh balances: after every spin request settles and on close. */
  refresh: () => void;
  /** The wheel setting as last reported by the server. */
  onEnabled: (enabled: boolean) => void;
  /** Wheel settings could not be fetched. */
  onError: (message: string) => void;
}

export function createSpinMachine({ refresh, onEnabled, onError }: SpinHooks) {
  // Raw: states are replaced whole.
  let current = $state.raw<SpinState>(initialSpinState);
  let revealTimer: ReturnType<typeof setTimeout> | undefined;

  /** Apply an event; resolves to whether the state changed. */
  function dispatch(event: SpinEvent): boolean {
    const next = reduceSpin(current, event);
    if (next === current) return false;
    current = next;
    clearTimeout(revealTimer);
    if (next.kind === "animating") {
      revealTimer = setTimeout(
        () => dispatch({ type: "animationFinished" }),
        next.wheel.spinDuration * 1000 + WAIT_BEFORE_REVEAL_MS,
      );
    }
    return true;
  }

  async function request(password?: string): Promise<void> {
    try {
      dispatch({ type: "succeeded", result: await spinWheel(password) });
    } catch (caught) {
      const code = caught instanceof Error ? caught.message : String(caught);
      dispatch({ type: "rejected", failure: classifySpinError(code) });
    } finally {
      // The post-spin balance decides whether another spin is affordable.
      refresh();
    }
  }

  /** Fetch wheel settings and open the dialog when the wheel is enabled. */
  async function open(): Promise<void> {
    if (current.kind !== "closed") return;
    try {
      const settings = await getWheelSettings();
      onEnabled(settings.enabled);
      if (!settings.enabled) return;
      dispatch({
        type: "opened",
        wheel: {
          segments: settings.segments,
          spinDuration: settings.spin_duration,
          passwordRequired: settings.password_confirmation_required,
        },
      });
    } catch (caught) {
      onError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  /** Start a paid spin: asks for the password first when the setting is on. */
  function spin(): void {
    if (dispatch({ type: "spin" }) && current.kind === "requesting") void request();
  }

  /** Send the typed password with the paid spin. It is never stored. */
  function submitPassword(password: string): void {
    if (dispatch({ type: "submit", password })) void request(password);
  }

  /** Reveal the next pre-resolved free-spin landing; no server request. */
  function nextFreeSpin(): void {
    dispatch({ type: "nextFreeSpin" });
  }

  /** Close the dialog (refused mid-spin by the machine) and refresh balances. */
  function close(): void {
    if (dispatch({ type: "close" })) refresh();
  }

  return {
    get state() {
      return current;
    },
    open,
    spin,
    submitPassword,
    nextFreeSpin,
    close,
  };
}
