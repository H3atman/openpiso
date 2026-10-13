<script lang="ts">
  // Central Terminal kiosk — grid view (the customer-facing surface).
  //
  // Layout: a small header (PC summary + acceptor health), a large
  // hero panel that mirrors the typed PC number (or a giant em-dash
  // when nothing is typed), and a 5-column grid of every PC. Each
  // tile carries a status-colored emphasized border with the time
  // on the clock and the account username (if any) — the customer
  // can see at a glance which PCs are free, busy, locked, or
  // offline without scrolling.

  import type { Pc } from "$lib/types";
  import { formatDuration } from "$lib/format";
  import { isAvailablePc, pcDisplayNumber, statusTone } from "$lib/kiosk";
  import PcTile from "$lib/components/kiosk/pc-tile.svelte";

  interface Props {
    pcs: Pc[];
    typedNumber: string;
    typedFlash?: boolean;
  }

  let { pcs, typedNumber, typedFlash = false }: Props = $props();

  const matchedPc = $derived.by(() => {
    if (typedNumber === "") return null;
    const padded = typedNumber.padStart(2, "0");
    return pcs.find((pc) => pcDisplayNumber(pc) === padded) ?? null;
  });
  const matchedAvailable = $derived(isAvailablePc(matchedPc));
  const matchedTone = $derived(
    matchedPc ? statusTone(matchedPc.status) : null,
  );
  const matchedUsername = $derived(matchedPc?.active_session?.username ?? null);
</script>

<div class="flex h-full w-full flex-col gap-4">
  <div class="flex flex-1 flex-col items-stretch gap-4 overflow-hidden">
    <section
      class="bg-card text-card-foreground ring-border relative flex flex-1 flex-col items-center justify-center overflow-hidden rounded-3xl p-8 ring-1"
    >
      {#if !matchedPc}
        <div class="text-center">
          <p
            class="text-muted-foreground text-[0.65rem] font-bold tracking-[0.25em] uppercase"
          >
            {typedNumber === "" ? "No PC selected" : "No PC matched"}
          </p>
          <div
            class="mt-3 font-mono text-[12rem] leading-none font-bold tracking-tighter tabular-nums"
            class:text-zinc-300={typedNumber === ""}
            class:dark:text-zinc-700={typedNumber === ""}
            class:text-foreground={typedNumber !== ""}
            aria-live="polite"
            aria-label={typedNumber === "" ? "Waiting for PC number" : `Typed ${typedNumber}`}
          >
            {typedNumber || "—"}
          </div>
          <p class="text-muted-foreground mt-4 text-lg">
            {typedNumber === ""
              ? "Type a PC number on the keypad"
              : `No PC ${typedNumber} on this terminal`}
          </p>
        </div>
      {:else if matchedTone}
        <div class="flex w-full flex-col items-center gap-6">
          <div class="flex items-center gap-3">
            <span
              class="inline-block h-3 w-3 rounded-full {matchedTone.dot}"
              aria-hidden="true"
            ></span>
            <span
              class="text-[0.7rem] font-bold tracking-[0.3em] uppercase {matchedTone.text}"
            >
              {matchedTone.label}
            </span>
          </div>
          <div
            class="font-mono text-[12rem] leading-none font-bold tracking-tighter tabular-nums"
            class:text-foreground={matchedAvailable}
            class:text-muted-foreground={!matchedAvailable}
            aria-label="Selected PC number"
          >
            {pcDisplayNumber(matchedPc)}
          </div>
          <div class="flex items-center gap-8 text-center">
            <div>
              <p
                class="text-muted-foreground text-[0.6rem] font-bold tracking-[0.25em] uppercase"
              >
                Time on clock
              </p>
              <p
                class="mt-1 font-mono text-3xl font-bold tracking-tight tabular-nums"
              >
                {formatDuration(matchedPc.remaining_seconds)}
              </p>
            </div>
            {#if matchedUsername}
              <div class="bg-border h-10 w-px" aria-hidden="true"></div>
              <div>
                <p
                  class="text-muted-foreground text-[0.6rem] font-bold tracking-[0.25em] uppercase"
                >
                  Account
                </p>
                <p class="mt-1 text-lg font-semibold">{matchedUsername}</p>
              </div>
            {:else}
              <div class="bg-border h-10 w-px" aria-hidden="true"></div>
              <div>
                <p
                  class="text-muted-foreground text-[0.6rem] font-bold tracking-[0.25em] uppercase"
                >
                  Mode
                </p>
                <p class="mt-1 text-lg font-semibold">
                  {matchedPc.timer_mode === "account" ? "Account" : "Guest Time"}
                </p>
              </div>
            {/if}
          </div>
          {#if matchedAvailable}
            <div
              class="border-primary bg-primary/5 text-primary inline-flex items-center gap-2 rounded-full border-2 px-5 py-2 text-sm font-bold tracking-widest uppercase"
            >
              Press
              <kbd class="bg-primary text-primary-foreground rounded px-2 py-0.5 font-mono"
                >Enter</kbd
              >
              to open
            </div>
          {:else}
            <div
              class="border-destructive bg-destructive/10 text-destructive inline-flex items-center gap-2 rounded-full border-2 px-5 py-2 text-sm font-bold tracking-widest uppercase"
            >
              {matchedPc.status === "offline"
                ? "PC offline"
                : matchedPc.status === "locked"
                  ? "PC locked"
                  : "Unavailable"}
            </div>
          {/if}
        </div>
      {/if}
    </section>

    <section class="flex flex-shrink-0 flex-col gap-2">
      <p
        class="text-muted-foreground text-[0.6rem] font-bold tracking-[0.25em] uppercase"
      >
        All PCs
      </p>
      <div class="grid grid-cols-5 gap-3 p-2">
        {#each pcs as pc (pc.id)}
          <PcTile
            {pc}
            size="md"
            matched={typedNumber !== "" &&
              pcDisplayNumber(pc) === typedNumber.padStart(2, "0")}
          />
        {/each}
      </div>
    </section>
  </div>
</div>
