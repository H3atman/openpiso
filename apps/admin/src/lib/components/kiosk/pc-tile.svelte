<script lang="ts">
  // Central Terminal kiosk — per-PC card. Renders a status-colored
  // emphasized border, the PC number, the time on the clock, and
  // the account username (with a user icon) if logged in — or the
  // mode label otherwise. Size variants ("sm" | "md" | "lg") let
  // the host dial the density up or down.

  import { User } from "@lucide/svelte";
  import type { Pc } from "$lib/types";
  import { formatDuration } from "$lib/format";
  import { isAvailablePc, pcDisplayNumber, statusTone } from "$lib/kiosk";

  interface Props {
    pc: Pc;
    matched?: boolean;
    size?: "sm" | "md" | "lg";
  }

  let { pc, matched = false, size = "md" }: Props = $props();

  const tone = $derived(statusTone(pc.status));
  const available = $derived(isAvailablePc(pc));
  const username = $derived(pc.active_session?.username ?? null);
  const modeLabel = $derived(
    pc.timer_mode === "account" ? (username ?? "Account") : "Guest",
  );

  const sizeClasses = $derived.by(() => {
    switch (size) {
      case "sm":
        return {
          wrapper: "gap-1 p-2",
          number: "text-2xl",
          time: "text-sm",
          mode: "text-[0.6rem] tracking-[0.18em]",
          icon: "h-2.5 w-2.5",
        };
      case "lg":
        return {
          wrapper: "gap-2.5 p-4",
          number: "text-5xl",
          time: "text-2xl",
          mode: "text-xs tracking-[0.22em]",
          icon: "h-3.5 w-3.5",
        };
      case "md":
      default:
        return {
          wrapper: "gap-1.5 p-3",
          number: "text-3xl",
          time: "text-base",
          mode: "text-[0.65rem] tracking-[0.2em]",
          icon: "h-3 w-3",
        };
    }
  });
</script>

<div
  class="bg-card text-card-foreground relative rounded-xl border-2 transition-colors outline-4 outline-offset-2 outline-transparent {tone.border} {tone.tint} before:pointer-events-none before:absolute before:inset-[3px] before:rounded-[8px] before:border-2 before:border-transparent before:content-['']"
  class:opacity-50={!available && !matched}
  class:border-primary={matched}
  class:outline-primary={matched}
  class:before:border-primary={matched}
  data-pc-id={pc.id}
>
  <div
    class="flex h-full w-full flex-col items-center justify-center overflow-hidden rounded-[10px] {sizeClasses.wrapper}"
  >
    <span
      class="font-mono font-bold leading-none tabular-nums {sizeClasses.number}"
      class:text-foreground={available}
      class:text-muted-foreground={!available}
    >
      {pcDisplayNumber(pc)}
    </span>

    <span
      class="font-mono font-bold leading-none tabular-nums {sizeClasses.time}"
      class:text-foreground={available || pc.remaining_seconds > 0}
      class:text-muted-foreground={!available && pc.remaining_seconds === 0}
    >
      {formatDuration(pc.remaining_seconds)}
    </span>

    <span
      class="text-muted-foreground inline-flex items-center gap-1 font-bold uppercase {sizeClasses.mode}"
    >
      {#if username}
        <User class="flex-shrink-0 {sizeClasses.icon}" aria-hidden="true" />
        <span class="truncate">{username}</span>
      {:else}
        {modeLabel}
      {/if}
    </span>

    <span
      class="absolute top-1.5 right-1.5 inline-block h-1.5 w-1.5 rounded-full {tone.dot}"
      aria-hidden="true"
    ></span>
  </div>
</div>
