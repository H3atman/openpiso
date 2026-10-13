<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    closeSelection,
    getKioskLock,
    getKioskStatus,
    getShopStatus,
    getSelection,
    injectSimulatorPulses,
    listPcsKiosk,
    openSelection,
    setKioskLock,
  } from "./lib/api";
  import {
    formatDuration,
    formatMoney,
    formatShopClosingTime,
    formatShopNextChange,
  } from "./lib/format";
  import { equivalentTimeSeconds } from "./lib/kiosk";
  import { kioskConfig } from "$lib/kiosk-config";
  import { Button } from "$lib/components/ui/button";
  import { Card, CardContent } from "$lib/components/ui/card";
  import KioskGrid from "$lib/components/kiosk/grid.svelte";
  import type { KioskConfig, KioskStatus, Pc, SelectionState, ShopStatus } from "./lib/types";
  import { shopName } from "$lib/shop-name";
  import symbolColor from "$lib/assets/logo/openpiso-symbol-color.svg";
  import symbolReversed from "$lib/assets/logo/openpiso-symbol-reversed.svg";
  import {
    canToggleKioskLock,
    insertScreenStateLine,
    isAvailablePc,
    kioskLockSignal,
    unavailabilityReason,
  } from "$lib/kiosk";

  // The Central Terminal is a numpad/physical-keyboard surface. The
  // flow is grid → insert (plus out-of-order). ADR-0025 removed the
  // confirm screen; commit happens on Enter from the grid. The
  // success screen was later removed: per ADR-0023 each coin
  // commits immediately, so by the time the customer presses Enter
  // on the insert screen the time is already credited and visible
  // on the grid — the window just closes and we return to the grid.
  // The on-screen Done/Cancel button label is context-sensitive but
  // both branches invoke the same close path.
  type Screen = "grid" | "insert";

  // --- top-level state -------------------------------------------------
  let pcs = $state<Pc[]>([]);
  let shopStatus = $state<ShopStatus | null>(null);
  let typedNumber = $state("");
  let screen = $state<Screen>("grid");
  let status = $state<KioskStatus>({ kind: "available", reason: "" });
  let selection = $state<SelectionState | null>(null);
  let busy = $state(false);
  let transientError = $state("");
  // Set by `closeAndReturnToGrid` so the `selection-state` listener
  // can tell a kiosk-initiated close (Enter / Esc on the insert
  // screen) from a server-initiated auto-close (inactivity / hard
  // cap, which navigates straight back to the grid because the
  // customer walked away). Cleared on the release-and-return path.
  let kioskInitiatedClose = $state(false);

  // Local display mirrors for the inactivity and hard-cap meters. The
  // server only pushes `selection-state` on open and on coin insert
  // (and on close), so without a 1-second local tick the numbers
  // freeze between pushes. The server stays authoritative — the tick
  // is purely cosmetic and never triggers a close. Every
  // `selection-state` push resets the mirrors to the authoritative
  // server values, so a coin insert (which extends the inactivity
  // timer server-side) snaps the on-screen number back up.
  let inactivityDisplay = $state(0);

  // Local display mirror for the PC's `remaining_seconds` on the
  // insert screen ("Current time"). The server broadcasts
  // `pc-updated` once per second for any active PC, so the value is
  // already near-real-time, but the gap between broadcasts (event
  // handler latency, broadcast channel jitter) can make the clock
  // appear to freeze for a beat. A 1-second local tick bridges the
  // gap so the customer sees a smooth, real-time clock. The server
  // stays authoritative — every `pc-updated` push resets this
  // mirror via the effect below, so a coin insert (which bumps the
  // time) snaps the on-screen clock back up.
  let insertRemainingDisplay = $state(0);

  // Auto-delete the typed PC number when the customer presses Enter
  // on a PC that can't take a selection (offline, locked, or not in
  // the roster). The flash state gives the kiosk a brief
  // "shake + red border" visual cue; cleared after ~400ms by the
  // timer set in `revertTypedNumber`.
  let typedFlash = $state(false);
  let flashTimer: number | undefined;

  // --- kiosk lock (ADR-0056) -------------------------------------------
  // The window boots always-on-top. The owner releases it by
  // double-clicking the Shop Name and entering the owner PIN; Rust owns
  // both the PIN check and the window flag, so everything here is display
  // and input plumbing. `kioskLocked` mirrors the server's answer and is
  // never assumed — every path assigns it from a command result.
  let kioskLocked = $state(true);
  // The PIN prompt. The kiosk never opens it while a Selection Window is
  // open (see `canToggleKioskLock`).
  let unlockOpen = $state(false);
  let unlockPin = $state("");
  let unlockError = $state("");
  let unlockBusy = $state(false);
  let unlockInput = $state<HTMLInputElement | null>(null);
  // The prompt self-closes after this long without a keystroke so a
  // walked-away owner never leaves a PIN box facing the next customer.
  const UNLOCK_IDLE_MS = 30_000;
  let unlockIdleTimer: number | undefined;

  const lockSignal = $derived(kioskLockSignal(kioskLocked));

  function closeUnlockPrompt(): void {
    unlockOpen = false;
    unlockPin = "";
    unlockError = "";
    if (unlockIdleTimer !== undefined) {
      window.clearTimeout(unlockIdleTimer);
      unlockIdleTimer = undefined;
    }
  }

  function restartUnlockIdleTimer(): void {
    if (unlockIdleTimer !== undefined) window.clearTimeout(unlockIdleTimer);
    unlockIdleTimer = window.setTimeout(closeUnlockPrompt, UNLOCK_IDLE_MS);
  }

  // The hidden owner gesture. Locked → prompt for the PIN. Unlocked →
  // re-engage straight away: tightening the kiosk is the safe direction,
  // so it needs no credential.
  function onShopNameDoubleClick(): void {
    if (busy || unlockOpen) return;
    if (!canToggleKioskLock(screen, selection !== null)) return;
    if (!kioskLocked) {
      void applyKioskLock(true);
      return;
    }
    unlockOpen = true;
    unlockPin = "";
    unlockError = "";
    restartUnlockIdleTimer();
  }

  async function applyKioskLock(locked: boolean, pin?: string): Promise<void> {
    if (unlockBusy) return;
    unlockBusy = true;
    try {
      const next = await setKioskLock(locked, pin);
      kioskLocked = next.locked;
      closeUnlockPrompt();
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught);
      // A failed unlock keeps the prompt open with the server's reason
      // (invalid PIN, or a retry-after once the terminal is throttled);
      // a failed re-lock has no prompt to hold it, so it goes inline.
      if (unlockOpen) {
        unlockError = message;
        restartUnlockIdleTimer();
      } else {
        transientError = message;
      }
    } finally {
      unlockBusy = false;
    }
  }

  function submitUnlock(): void {
    if (!unlockOpen || unlockBusy) return;
    void applyKioskLock(false, unlockPin);
  }

  // Enter submits from the PIN box. Escape is deliberately *not* handled
  // here: it is handled on the window, so it still closes the prompt after
  // focus has moved to a button or been dropped by the disabled input.
  function onUnlockKeydown(event: KeyboardEvent): void {
    restartUnlockIdleTimer();
    if (event.key === "Enter") {
      event.preventDefault();
      submitUnlock();
    }
  }

  // DEV-ONLY coin simulator. `import.meta.env.DEV` is `true` under
  // `bun run dev:admin` and statically `false` in production builds, so
  // the whole control (and its dependency on the debug-only
  // `dev_inject_coin` command) is tree-shaken out of the shipped bundle.
  // `devInjecting` is a short re-entrancy guard so rapid clicks don't
  // stack injections before the 500ms server poll commits the batch.
  const isDev = import.meta.env.DEV;
  let devInjecting = $state(false);

  // Inject simulated coin pulses into the active Selection Window. The
  // server poll closes the batch and commits it through the real
  // `record_coin_event` path; the resulting `selection-state` and
  // `pc-updated` events refresh the UI, so there is nothing to apply here.
  async function devInsertCoin(pulseCount: number): Promise<void> {
    if (!isDev || devInjecting || busy || !selection) return;
    devInjecting = true;
    transientError = "";
    try {
      await injectSimulatorPulses(pulseCount);
    } catch (caught) {
      transientError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      devInjecting = false;
    }
  }

  function revertTypedNumber(reason: string): void {
    transientError = reason;
    typedNumber = "";
    typedFlash = true;
    if (flashTimer !== undefined) window.clearTimeout(flashTimer);
    flashTimer = window.setTimeout(() => {
      typedFlash = false;
      flashTimer = undefined;
    }, 420);
  }

  // --- derived ---------------------------------------------------------
  const typedPc = $derived(
    typedNumber.trim() === "" ? null : findPc(typedNumber),
  );
  const insertPc = $derived(selection ? findPc(selection.pc_id) : null);
  const insertStateLine = $derived(
    insertPc
      ? insertScreenStateLine(insertPc, insertRemainingDisplay)
      : null,
  );
  const showOutOfOrder = $derived(status.kind === "out_of_order");
  // Shop state is resolved by Rust and carried on the existing PC update
  // broadcast. The kiosk only chooses the closed render from that value.
  const showShopClosed = $derived(
    screen === "grid" && shopStatus !== null && !shopStatus.open,
  );
  const showLastCall = $derived(
    screen === "grid" && shopStatus?.last_call_active === true,
  );
  // Insert-screen urgency (last 5 s of the 15 s inactivity window).
  // The server is authoritative — `inactivityDisplay` is the
  // locally-ticked mirror that the server resets on every accepted
  // coin. While `closed` is true, the server has either just hit 0
  // or already auto-closed; the kiosk should bounce back to the
  // grid on the next `selection-state` push.
  const insertUrgent = $derived(
    screen === "insert" &&
      inactivityDisplay > 0 &&
      inactivityDisplay <= 5,
  );
  const insertClosed = $derived(
    screen === "insert" && inactivityDisplay === 0,
  );
  // Preserve the authoritative committed portion when a Shop Rate changes
  // during an open Selection Window. Only money inserted after the event is
  // quoted with the new rate, so earlier coins are never retroactively priced.
  let previewRate = $state<KioskConfig | null>(null);
  let previewBaselineAmountCents = $state(0);
  let previewBaselineSeconds = $state(0);

  function rebaseEquivalentTimePreview(current: SelectionState | null): void {
    previewBaselineAmountCents = current?.inserted_amount_cents ?? 0;
    previewBaselineSeconds = current?.inserted_seconds ?? 0;
  }

  const equivalentTimePreview = $derived.by(() => {
    if (!selection || !previewRate) return null;
    const amountSinceRateChange = selection.inserted_amount_cents - previewBaselineAmountCents;
    if (amountSinceRateChange < 0) return selection.inserted_seconds;
    return previewBaselineSeconds + equivalentTimeSeconds(amountSinceRateChange, previewRate);
  });

  function findPc(idOrNumber: string): Pc | null {
    const trimmed = idOrNumber.trim();
    if (!trimmed) return null;
    const padded = trimmed.padStart(2, "0");
    const candidates = [`pc-${padded}`, padded, `pc-${trimmed}`, trimmed];
    return (
      pcs.find((pc) => candidates.includes(pc.id)) ??
      pcs.find((pc) => pc.id === trimmed) ??
      null
    );
  }

  function isDigit(value: string): boolean {
    return value.length === 1 && value >= "0" && value <= "9";
  }

  function appendDigit(value: string): void {
    if (busy) return;
    if (!isDigit(value)) return;
    if (typedNumber.length >= 3) return;
    transientError = "";
    typedNumber = `${typedNumber}${value}`;
  }

  function backspaceDigit(): void {
    if (busy) return;
    transientError = "";
    typedNumber = typedNumber.slice(0, -1);
  }

  function clearTypedNumber(): void {
    if (busy) return;
    transientError = "";
    typedNumber = "";
  }

  // --- selection lifecycle --------------------------------------------
  // Open a Selection Window for `pc`. With no confirm screen, this
  // is the kiosk's only commit point on the grid — any failure
  // (terminal in use, hidden PC, unknown PC, server restart) is
  // surfaced inline as `transientError` and the customer stays on
  // the grid. The error message from the server is the canonical
  // string (e.g. "terminal in use").
  async function selectPc(pc: Pc): Promise<void> {
    if (busy) return;
    busy = true;
    transientError = "";
    try {
      selection = await openSelection(pc.id);
      rebaseEquivalentTimePreview(null);
      typedNumber = "";
      screen = "insert";
    } catch (caught) {
      transientError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }

  // The insert-screen Enter and Escape both close the Selection
  // Window and return to the grid. Per ADR-0023 each coin commits
  // immediately, so by the time the customer presses Enter the
  // time is already credited and visible on the grid — there is no
  // success screen to show. The on-screen Done/Cancel button label
  // is context-sensitive (Done if money has landed, Cancel
  // otherwise) so the customer sees an honest "I changed my mind"
  // affordance when nothing has been inserted, but both branches
  // invoke the same `close` call. Escape is the always-available
  // safety hatch that rescues a stuck terminal even with money in.
  // This also fixes the prior bug where the "Cancel" path only
  // cleared the typed number locally and left the server selection
  // open until the 15 s inactivity timeout.

  async function closeAndReturnToGrid(): Promise<void> {
    if (busy) return;
    busy = true;
    kioskInitiatedClose = true;
    try {
      await closeSelection("enter");
    } catch (caught) {
      // The server may have already auto-closed the window (the
      // inactivity or hard-cap timer fired between the user pressing
      // Enter/Esc and our call reaching Rust). That's fine — we
      // just want to return to the grid.
      transientError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      selection = null;
      typedNumber = "";
      screen = "grid";
      busy = false;
      kioskInitiatedClose = false;
    }
  }

  function applyPcUpdate(updatedPc: Pc): void {
    if (updatedPc.shop_status) shopStatus = updatedPc.shop_status;
    let mutated = false;
    pcs = pcs.map((pc) => {
      if (pc.id !== updatedPc.id) return pc;
      mutated = true;
      return updatedPc;
    });
    if (!mutated && updatedPc) {
      pcs = [...pcs, updatedPc];
    }
  }

  // --- effects ---------------------------------------------------------
  // Reconcile the inactivity mirror with every server-pushed
  // `selection-state` (which arrives on open and per-coin insert).
  // Without this, the local tick would drift between pushes; with
  // it, a coin insert (which extends the inactivity timer
  // server-side) snaps the on-screen number back up.
  $effect(() => {
    inactivityDisplay = selection
      ? selection.inactivity_remaining_seconds
      : 0;
  });

  // 1-second local tick: purely cosmetic smoothing between server
  // pushes. The server is still the only thing that can close the
  // window — the tick never triggers a close. Active only while on
  // the insert screen with an open selection.
  $effect(() => {
    if (screen !== "insert" || !selection) return;
    const interval = window.setInterval(() => {
      inactivityDisplay = Math.max(0, inactivityDisplay - 1);
    }, 1000);
    return () => window.clearInterval(interval);
  });

  // Reconcile the "Current time" mirror with every server-pushed
  // `pc-updated` (the per-second timer broadcast for any active PC).
  // `insertPc` is derived from `selection` + `pcs`, and the server
  // replaces the matching entry in `pcs` on every tick, so this
  // effect snaps the on-screen clock back to the authoritative
  // server value on each push — a coin insert that bumps the time
  // lands here as an upward jump before the local tick continues.
  $effect(() => {
    insertRemainingDisplay = insertPc ? insertPc.remaining_seconds : 0;
  });

  // 1-second local tick on the "Current time" display so the clock
  // reads as a real-time countdown between server pushes. The tick
  // is purely cosmetic; every server `pc-updated` resets the mirror
  // (see the effect above), so a coin insert that adds time snaps
  // the clock back up before the tick resumes. Active only while
  // on the insert screen with a selected PC that still has time.
  $effect(() => {
    if (screen !== "insert" || !insertPc) return;
    if (insertPc.remaining_seconds <= 0) return;
    const interval = window.setInterval(() => {
      insertRemainingDisplay = Math.max(0, insertRemainingDisplay - 1);
    }, 1000);
    return () => window.clearInterval(interval);
  });

  // Focus the PIN box as soon as the prompt opens: the kiosk has no
  // pointer-driven text entry habit, and the owner should be able to type
  // the PIN straight after the double-click.
  $effect(() => {
    if (unlockOpen) unlockInput?.focus();
  });

  // --- lifecycle -------------------------------------------------------
  let unlistenPc: (() => void) | undefined;
  let unlistenSelection: (() => void) | undefined;
  let unlistenKiosk: (() => void) | undefined;
  let unsubscribeRate: (() => void) | undefined;
  let mounted = true;

  onMount(() => {
    window.addEventListener("keydown", handleWindowKeydown);
    unsubscribeRate = kioskConfig.subscribe((nextRate) => {
      if (!nextRate) return;
      if (
        previewRate?.rate_amount_cents === nextRate.rate_amount_cents &&
        previewRate.rate_seconds === nextRate.rate_seconds
      ) {
        return;
      }
      rebaseEquivalentTimePreview(selection);
      previewRate = nextRate;
    });
    void (async () => {
      try {
        const [
          initialPcs,
          initialStatus,
          initialShopStatus,
          initialSelection,
          initialLock,
        ] = await Promise.all([
          listPcsKiosk(),
          getKioskStatus(),
          getShopStatus(),
          getSelection(),
          getKioskLock(),
        ]);
        if (!mounted) return;
        pcs = initialPcs;
        shopStatus = initialShopStatus;
        status = initialStatus;
        selection = initialSelection;
        rebaseEquivalentTimePreview(initialSelection);
        kioskLocked = initialLock.locked;
        if (initialSelection) {
          screen = "insert";
        }
      } catch (caught) {
        transientError = caught instanceof Error ? caught.message : String(caught);
      }
    })();

    void listen<Pc>("pc-updated", (event) => {
      applyPcUpdate(event.payload);
    }).then((cleanup) => {
      if (mounted) {
        unlistenPc = cleanup;
      } else {
        cleanup();
      }
    });

    void listen<SelectionState | null>("selection-state", (event) => {
      const next = event.payload;
      const wasOpen = selection !== null;
      const isClosed = next === null;
      selection = next;
      // Each pushed total follows a committed Coin Batch. Rebase to its
      // authoritative seconds so integer truncation is never recomputed over
      // several coins as though they were one purchase.
      rebaseEquivalentTimePreview(next);
      if (isClosed && wasOpen && !kioskInitiatedClose) {
        // The server auto-closed the window (inactivity / hard cap)
        // while the customer was on the insert screen — the customer
        // walked away. The kiosk returns to the grid so the next
        // customer can pick a PC. The kiosk-initiated close paths
        // (Enter / Escape on the insert screen) set
        // `kioskInitiatedClose`, so this branch is skipped for them.
        if (screen === "insert") {
          screen = "grid";
          typedNumber = "";
        }
      } else if (next !== null && !wasOpen) {
        // The server opened a new window (e.g. after a server
        // restart that lost the previous kiosk session, or an
        // external client triggered the open). Surface it on the
        // insert screen.
        screen = "insert";
      }
    }).then((cleanup) => {
      if (mounted) {
        unlistenSelection = cleanup;
      } else {
        cleanup();
      }
    });

    void listen<KioskStatus>("kiosk-status", (event) => {
      status = event.payload;
    }).then((cleanup) => {
      if (mounted) {
        unlistenKiosk = cleanup;
      } else {
        cleanup();
      }
    });

    // Re-fetch the PC list every 5 seconds as a safety net for any
    // update path that bypasses the live `pc-updated` event (e.g. a
    // server restart that drops the broadcast channel).
    const refreshTimer = window.setInterval(() => {
      void listPcsKiosk()
        .then((fresh) => {
          if (!mounted) return;
          pcs = fresh;
          const nextShopStatus = fresh.find((pc) => pc.shop_status)?.shop_status;
          if (nextShopStatus) shopStatus = nextShopStatus;
        })
        .catch(() => {
          // Swallow transient failures; the next tick will retry.
        });
    }, 5_000);

    return () => {
      window.clearInterval(refreshTimer);
    };
  });

  onDestroy(() => {
    mounted = false;
    window.removeEventListener("keydown", handleWindowKeydown);
    if (unlockIdleTimer !== undefined) window.clearTimeout(unlockIdleTimer);
    if (flashTimer !== undefined) window.clearTimeout(flashTimer);
    unlistenPc?.();
    unlistenSelection?.();
    unlistenKiosk?.();
    unsubscribeRate?.();
  });

  // --- keyboard --------------------------------------------------------
  // Grid: digits build the number, Backspace edits, Enter opens the
  //       Selection Window, Escape clears the typed number.
  // Insert: Enter and Escape both close the Selection Window and
  //         return to the grid (the Done/Cancel label is
  //         context-sensitive but the action is the same). Backspace
  //         is a no-op on insert (the typed number is irrelevant
  //         once a window is open).
  //
  // PROTOTYPE — the grid's Enter-on-unavailable-PC branch now
  // auto-deletes the typed number (and briefly flashes the display)
  // instead of leaving a stale number + a toast. The customer sees
  // the number vanish and can start over; no stale "Pick a PC
  // number first" message to dismiss.
  function handleWindowKeydown(event: KeyboardEvent): void {
    // The unlock prompt owns the keyboard while it is open: the digits the
    // owner types are PIN characters, not a PC number, and Enter belongs to
    // the PIN box. Escape stays on the window so it closes the prompt even
    // once focus has left the input — clicking a button, or the input being
    // disabled mid-check, must not strand the owner in the dialog.
    if (unlockOpen) {
      if (event.key === "Escape") {
        event.preventDefault();
        closeUnlockPrompt();
      }
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      if (screen === "grid") {
        if (typedNumber === "") {
          revertTypedNumber("Type a PC number first.");
        } else if (typedPc && isAvailablePc(typedPc)) {
          void selectPc(typedPc);
        } else {
          revertTypedNumber(unavailabilityReason(typedPc, typedNumber));
        }
      } else if (screen === "insert") {
        void closeAndReturnToGrid();
      }
      return;
    }
    if (event.key === "Backspace" || event.key === "Delete") {
      event.preventDefault();
      if (screen === "grid") {
        backspaceDigit();
      }
      // On insert, Backspace is intentionally a no-op: the typed
      // number is irrelevant once a window is open, and the
      // customer-facing safety hatch for a stuck terminal is Esc.
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      if (screen === "grid") {
        clearTypedNumber();
      } else if (screen === "insert") {
        void closeAndReturnToGrid();
      }
      return;
    }
    if (isDigit(event.key) && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      if (screen === "grid") {
        appendDigit(event.key);
      }
    }
  }

  // Visual affordance: "Cancel" / "Done" labels on the insert screen
  // depend on whether money has landed. The behavior is the same
  // (close the window, return to the grid), but the label tells
  // the customer an honest story: "Cancel" when nothing has been
  // inserted, "Done" when the inserted time is already credited
  // and pressing Enter just finalizes.
  const insertDoneLabel = $derived(
    selection && selection.inserted_amount_cents > 0 ? "Done (Enter)" : "Cancel (Enter)",
  );
</script>

<svelte:head>
  <title>{$shopName} — Kiosk</title>
</svelte:head>

<main
  class="bg-background text-foreground flex h-screen w-screen flex-col overflow-hidden p-6"
  aria-label={`${$shopName} Kiosk`}
>
  <header class="flex flex-shrink-0 items-center justify-between pb-3">
    <div class="flex items-center gap-3">
      <span
        class={`block h-3.5 w-3.5 shrink-0 rounded-full ${lockSignal.dot}`}
        aria-hidden="true"
      ></span>
      <!-- Decorative Open Frame symbol; the Shop Name below is the label. -->
      <img src={symbolColor} alt="" class="block h-6 w-6 shrink-0 dark:hidden" />
      <img src={symbolReversed} alt="" class="hidden h-6 w-6 shrink-0 dark:block" />
      <div class="min-w-0">
        <!-- The Shop Name carries the hidden owner gesture (ADR-0056):
             double-click to release or re-engage the Kiosk Lock. It stays
             deliberately undiscoverable — no button role, no cursor change,
             no tooltip, no keyboard equivalent — so a customer poking at
             the screen never finds it. The PIN behind it, not the obscurity,
             is what actually guards the release. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <h1
          class="text-2xl font-semibold tracking-tight select-none"
          ondblclick={onShopNameDoubleClick}
        >
          {$shopName}
        </h1>
        <p class="text-muted-foreground text-sm" aria-live="polite">
          {lockSignal.label}
          <span class="sr-only">— {lockSignal.description}</span>
        </p>
      </div>
    </div>
  </header>

  {#if transientError}
    <div
      class="border-destructive/40 bg-destructive/10 text-destructive mb-3 flex flex-shrink-0 items-center justify-between gap-4 rounded-md border p-3"
      role="alert"
    >
      <span class="text-sm">{transientError}</span>
      <Button
        variant="outline"
        size="sm"
        type="button"
        onclick={() => (transientError = "")}
      >
        Back
      </Button>
    </div>
  {/if}

  <section class="flex flex-1 flex-col overflow-hidden">
    {#if showOutOfOrder}
      <Card
        class="border-destructive/40 bg-destructive/10 text-destructive flex flex-1 flex-col items-center justify-center"
      >
        <CardContent class="max-w-xl space-y-3 text-center">
          <p class="text-xs font-bold tracking-widest uppercase">Coin acceptor</p>
          <h2 class="text-3xl font-semibold tracking-tight">Out of order</h2>
          <p class="text-base">
            {status.reason ||
              "The coin acceptor is unavailable. Please ask the shop staff for assistance."}
          </p>
          <p class="font-semibold">
            Please come back shortly — the shop staff has been notified.
          </p>
        </CardContent>
      </Card>
    {:else if showShopClosed}
      <Card class="flex flex-1 flex-col items-center justify-center">
        <CardContent class="max-w-xl space-y-4 text-center">
          <p class="text-muted-foreground text-xs font-bold tracking-widest uppercase">
            Shop Hours
          </p>
          <h2 class="text-4xl font-semibold tracking-tight">Shop Closed</h2>
          <p class="text-lg">The shop is closed and coin intake is unavailable.</p>
          <p class="text-muted-foreground text-base">
            {formatShopNextChange(shopStatus?.next_change_at ?? null)}
          </p>
        </CardContent>
      </Card>
    {:else if screen === "grid"}
      {#if showLastCall}
        <div
          class="border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300 mb-3 flex-shrink-0 rounded-md border p-3 text-center text-base font-semibold"
          role="status"
          aria-live="polite"
        >
          Closing at {formatShopClosingTime(shopStatus?.next_change_at ?? null)} — last chance to insert coins.
        </div>
      {/if}
      <div
        class="flex flex-1 flex-col overflow-hidden"
        class:animate-[shake_0.18s_ease-in-out]={typedFlash}
      >
        <KioskGrid pcs={pcs} {typedNumber} {typedFlash} />
      </div>
    {:else if screen === "insert" && insertPc && selection}
      <div class="flex flex-1 flex-col overflow-hidden">
        <main class="flex flex-1 flex-col items-center justify-center px-8">
          <p
            class="bg-primary/10 text-primary inline-flex items-center gap-2 rounded-full px-5 py-1.5 text-lg font-semibold tracking-tight"
          >
            For {insertPc.name}
          </p>

          <div
            class="insert-pill mt-6 inline-flex items-center gap-2 rounded-full border-2 px-5 py-1.5 text-sm font-bold tracking-[0.3em] uppercase"
            class:border-primary={!insertUrgent && !insertClosed}
            class:text-primary={!insertUrgent && !insertClosed}
            class:border-destructive={insertUrgent || insertClosed}
            class:text-destructive={insertUrgent || insertClosed}
            class:insert-pill-anim={!insertUrgent && !insertClosed}
            class:urgent-pill={insertUrgent || insertClosed}
          >
            <span
              class:insert-arrow-anim={!insertUrgent && !insertClosed}
              class:urgent-arrow={insertUrgent || insertClosed}
              aria-hidden="true">↓</span>
            Insert Coin
            <span
              class:insert-arrow-anim={!insertUrgent && !insertClosed}
              class:urgent-arrow={insertUrgent || insertClosed}
              aria-hidden="true">↓</span>
          </div>

          <p
            class="mt-10 text-base font-medium tracking-wide sm:text-lg"
            class:text-muted-foreground={!insertUrgent && !insertClosed}
            class:text-destructive={insertUrgent || insertClosed}
          >
            {#if insertClosed}
              Window closing…
            {:else if insertUrgent}
              Closing — insert now!
            {:else}
              Please insert a coin
            {/if}
          </p>

          <p
            class="font-mono mt-4 text-[10rem] leading-none font-bold tracking-tighter tabular-nums sm:text-[16rem]"
            class:text-primary={!insertUrgent && !insertClosed}
            class:text-destructive={insertUrgent || insertClosed}
            class:urgent-shake={insertUrgent || insertClosed}
            aria-live="polite"
            aria-label="Seconds remaining in the inactivity window"
          >
            {inactivityDisplay}
          </p>

          <p
            class="text-muted-foreground mt-4 text-sm tracking-[0.25em] uppercase"
          >
            {#if insertClosed}
              Inactivity window expired
            {:else}
              seconds to insert another coin
            {/if}
          </p>

          <div
            class="border-border bg-card text-card-foreground mt-10 inline-flex items-center gap-3 rounded-full border px-5 py-2"
          >
            {#if insertStateLine}
              <span class="text-base font-semibold" aria-live="polite">
                {insertStateLine}
              </span>
            {:else}
              <span
                class="text-muted-foreground text-[0.6rem] font-bold tracking-[0.3em] uppercase"
              >
                Current time
              </span>
              <span
                class="font-mono text-xl font-bold tabular-nums"
                aria-label="Time remaining on the PC"
              >
                {formatDuration(insertRemainingDisplay)}
              </span>
            {/if}
          </div>
        </main>

        <div
          class="border-border bg-card text-card-foreground flex flex-shrink-0 items-stretch divide-x divide-border border-y"
        >
          <div class="flex flex-1 flex-col items-center justify-center px-10 py-6">
            <p
              class="text-muted-foreground text-xs font-bold tracking-[0.3em] uppercase"
            >
              Inserted
            </p>
            <p class="mt-2 font-mono text-3xl font-bold tabular-nums">
              {formatMoney(selection.inserted_amount_cents)}
            </p>
          </div>
          <div class="flex flex-1 flex-col items-center justify-center px-10 py-6">
            <p
              class="text-muted-foreground text-xs font-bold tracking-[0.3em] uppercase"
            >
              Added
            </p>
            <p class="mt-2 font-mono text-3xl font-bold tabular-nums">
              +{formatDuration(selection.inserted_seconds)}
            </p>
          </div>
          <div class="flex flex-1 flex-col items-center justify-center px-10 py-6">
            <p
              class="text-muted-foreground text-xs font-bold tracking-[0.3em] uppercase"
            >
              Equivalent time
            </p>
            <p
              class="mt-2 font-mono text-3xl font-bold tabular-nums"
              aria-live="polite"
            >
              {equivalentTimePreview === null
                ? "—"
                : `+${formatDuration(equivalentTimePreview)}`}
            </p>
          </div>
        </div>

        {#if isDev}
          <!-- DEV-ONLY: simulated coin acceptor. Stripped from production
               builds via `import.meta.env.DEV`. Each button injects the
               pulse count that maps to that peso value in every active
               default map (1 = ₱1, 5 = ₱5, 10 = ₱10). The fresh ₱20
               candidate is inactive until physical Apply. -->
          <div
            class="border-amber-500/50 bg-amber-500/10 mt-4 flex flex-shrink-0 items-center gap-3 rounded-lg border-2 border-dashed px-4 py-3"
          >
            <span
              class="text-[0.6rem] font-bold tracking-[0.25em] text-amber-600 uppercase dark:text-amber-400"
            >
              Dev · Simulate coin
            </span>
            <div class="flex flex-1 items-center justify-end gap-2">
              {#each [1, 5, 10] as pesos (pesos)}
                <Button
                  variant="outline"
                  size="sm"
                  type="button"
                  disabled={busy || devInjecting}
                  onclick={() => void devInsertCoin(pesos)}
                >
                  +₱{pesos}
                </Button>
              {/each}
            </div>
          </div>
        {/if}

        <footer class="flex flex-shrink-0 items-center gap-3 py-5">
          <Button
            variant="outline"
            class="h-14 flex-1 text-base"
            type="button"
            disabled={busy}
            onclick={() => void closeAndReturnToGrid()}
          >
            {busy ? "Closing…" : "Release (Esc)"}
          </Button>
          <Button
            class="h-14 flex-1 text-base"
            type="button"
            disabled={busy}
            onclick={() => void closeAndReturnToGrid()}
          >
            {busy ? "Closing…" : insertDoneLabel}
          </Button>
        </footer>
      </div>
    {/if}
  </section>

  <footer
    class="text-muted-foreground mt-3 flex flex-shrink-0 items-center justify-between border-t pt-2 text-sm"
  >
    <span>
      Press
      <kbd
        class="text-foreground border-border rounded border bg-muted px-1.5 py-0.5 font-mono text-xs"
        >Enter</kbd
      >
      to continue
    </span>
  </footer>

  <!-- Kiosk Lock release prompt (ADR-0056). A transactional dialog, so a
       `fixed inset-0` overlay is appropriate: the owner is doing one
       focused thing and the kiosk beneath is deliberately occluded. It is
       only ever reachable from the grid, never over a live Selection
       Window. -->
  {#if unlockOpen}
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6"
      role="dialog"
      aria-modal="true"
      aria-label="Kiosk Lock"
    >
      <Card class="w-full max-w-sm">
        <CardContent class="space-y-4 p-6">
          <div>
            <p
              class="text-muted-foreground text-xs font-bold tracking-[0.3em] uppercase"
            >
              Kiosk Lock
            </p>
            <h2 class="mt-1 text-xl font-semibold tracking-tight">
              Enter owner PIN to unlock
            </h2>
            <p class="text-muted-foreground mt-1 text-sm">
              Releases always-on-top so other windows can cover the kiosk.
            </p>
          </div>
          <input
            bind:this={unlockInput}
            bind:value={unlockPin}
            type="password"
            inputmode="numeric"
            autocomplete="off"
            aria-label="Owner PIN"
            disabled={unlockBusy}
            oninput={restartUnlockIdleTimer}
            onkeydown={onUnlockKeydown}
            class="border-border bg-background focus:border-primary h-12 w-full rounded-md border px-3 font-mono text-lg tracking-[0.4em] outline-none"
          />
          {#if unlockError}
            <p class="text-destructive text-sm" role="alert">{unlockError}</p>
          {/if}
          <div class="flex gap-3">
            <Button
              variant="outline"
              class="flex-1"
              type="button"
              disabled={unlockBusy}
              onclick={closeUnlockPrompt}
            >
              Cancel (Esc)
            </Button>
            <Button
              class="flex-1"
              type="button"
              disabled={unlockBusy}
              onclick={submitUnlock}
            >
              {unlockBusy ? "Checking…" : "Unlock"}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  {/if}
</main>

<style>
  /* Insert-screen coin prompt. The pill breathes on a back-out
     easing (slight overshoot at the apex), and the arrows bob
     downward in sync to suggest gravity. In the last 5 s of the
     inactivity window the pill, the arrows, and the giant timer
     all swap to the red `urgent-shake` keyframe so the customer
     feels the window closing across the shop. Disabled under
     `prefers-reduced-motion`. */
  .insert-pill-anim {
    transform-origin: center;
    animation: insert-pill-bounce 1.3s cubic-bezier(0.34, 1.56, 0.64, 1) infinite;
  }
  .urgent-pill {
    animation: urgent-shake 0.45s ease-in-out infinite;
  }
  .insert-arrow-anim {
    display: inline-block;
    animation: insert-arrow-bounce 1.3s cubic-bezier(0.34, 1.56, 0.64, 1) infinite;
  }
  .urgent-arrow {
    display: inline-block;
    animation: urgent-shake 0.45s ease-in-out infinite;
  }
  .urgent-shake {
    animation: urgent-shake 0.45s ease-in-out infinite;
  }
  @keyframes insert-pill-bounce {
    0%,
    100% {
      transform: scale(1);
    }
    50% {
      transform: scale(1.07);
    }
  }
  @keyframes insert-arrow-bounce {
    0%,
    100% {
      transform: translateY(0);
    }
    50% {
      transform: translateY(3px);
    }
  }
  @keyframes urgent-shake {
    0%,
    100% {
      transform: translateX(0);
    }
    20% {
      transform: translateX(-8px);
    }
    40% {
      transform: translateX(8px);
    }
    60% {
      transform: translateX(-5px);
    }
    80% {
      transform: translateX(5px);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .insert-pill-anim,
    .urgent-pill,
    .insert-arrow-anim,
    .urgent-arrow,
    .urgent-shake {
      animation: none;
    }
  }
</style>
