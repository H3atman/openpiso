<script lang="ts">
  // WheelEntry — shared Spin-the-Wheel entry point.
  //
  // Used from both the Widget (compact variant) and the Fullscreen Lock
  // (card variant) to produce consistent visibility, labelling, and
  // enablement logic per issue #127.
  //
  // - Visible whenever wheelEnabled is true, regardless of who is looking.
  // - A guest sees "Log in to spin" and cannot open.
  // - A logged-in customer below the 1-point cost sees the exact shortfall
  //   and their current balance; the button is disabled.
  // - A customer who can afford a spin sees no reason text and can open.
  // - The entry always states the 1-point cost.
  // - Hidden entirely when wheelEnabled is false.

  import { Button } from "$lib/components/ui/button";
  import { formatPoints } from "$lib/format";
  import type { AccountAuth } from "$lib/types";

  interface Props {
    accountAuth: AccountAuth | null;
    busy: boolean;
    /** Whether the wheel is enabled in the wheel settings. */
    wheelEnabled: boolean;
    /** Called when the customer clicks the Spin button. */
    onOpen: () => void;
    /**
     * Visual variant:
     * - `"card"` — bordered rounded card (used on the lock screen / AccountPanel)
     * - `"compact"` — inline list entry (used in the widget Points tab)
     */
    variant?: "card" | "compact";
  }

  let {
    accountAuth,
    busy,
    wheelEnabled,
    onOpen,
    variant = "compact",
  }: Props = $props();

  const SPIN_COST_CENTI = 100;

  type EntryState = { show: boolean; reason: string; canOpen: boolean };

  const entryState: EntryState = $derived.by(() => {
    if (!wheelEnabled) return { show: false, reason: "", canOpen: false };

    if (!accountAuth) {
      return {
        show: true,
        reason: "Log in to spin",
        canOpen: false,
      };
    }

    const balance = accountAuth.account.points_balance_centi;
    if (balance < SPIN_COST_CENTI) {
      return {
        show: true,
        reason: `${formatPoints(SPIN_COST_CENTI - balance)} more points needed (balance: ${formatPoints(balance)})`,
        canOpen: false,
      };
    }

    return {
      show: true,
      reason: "",
      canOpen: true,
    };
  });

  const containerClass = $derived(
    variant === "card"
      ? "flex flex-col gap-2 rounded-md border border-border bg-card p-3"
      : "flex flex-col gap-1",
  );
</script>

{#if entryState.show}
  <div class={containerClass}>
    <p class="text-muted-foreground text-xs font-bold tracking-widest uppercase">
      Spin the Wheel
    </p>
    {#if variant === "card"}
      <p class="text-sm">Bet 1 point for a chance at bonus time.</p>
    {/if}
    {#if entryState.reason}
      <p class="text-xs text-muted-foreground">{entryState.reason}</p>
    {/if}
    <div class={variant === "card" ? "" : "flex flex-col"}>
      <Button
        type="button"
        class={variant === "card" ? "" : "w-full"}
        variant={variant === "card" ? "default" : "outline"}
        size="sm"
        disabled={busy || !entryState.canOpen}
        onclick={onOpen}
      >
        Spin for 1 point
      </Button>
    </div>
  </div>
{/if}
