<script lang="ts">
  // TransferPointsDialog — owner-confirmed Points Transfer (ADR-0030).
  // The customer enters a recipient username and a point amount (0.01
  // precision, ADR-0038); the server creates a pending request and returns
  // a 6-digit code. This dialog then switches to a "show code" view so the
  // customer can read the code aloud to the owner. The owner enters the
  // code in web-admin to confirm the transfer.

  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import WidgetOverlay from "$lib/components/WidgetOverlay.svelte";
  import { formatPoints, parsePointsToCenti } from "$lib/format";

  interface Props {
    open: boolean;
    /** Sender's balance in integer centipoints (ADR-0038). */
    currentPointsCenti: number;
    busy: boolean;
    onSubmit: (toUsername: string, pointsCenti: number) => Promise<string>;
    onClose: () => void;
  }

  let { open, currentPointsCenti, busy, onSubmit, onClose }: Props = $props();

  let toUsername = $state("");
  let pointsInput = $state("");
  let localError = $state("");
  let requestCode = $state<string | null>(null);
  let sentPointsCenti = $state(0);

  // Reset on close so a later open starts a fresh request instead of
  // resurrecting a code that may already be resolved or expired.
  $effect(() => {
    if (open) return;
    toUsername = "";
    pointsInput = "";
    localError = "";
    requestCode = null;
    sentPointsCenti = 0;
  });

  const maxTransferableCenti = $derived(Math.max(0, currentPointsCenti));
  const pointsCenti = $derived(parsePointsToCenti(pointsInput));
  const submittable = $derived(
    pointsCenti !== null &&
      pointsCenti > 0 &&
      pointsCenti <= maxTransferableCenti,
  );

  async function handleSubmit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    localError = "";
    if (pointsCenti === null || pointsCenti <= 0) {
      localError = "Enter a positive amount with at most two decimals.";
      return;
    }
    if (pointsCenti > maxTransferableCenti) {
      localError = "You do not have enough points.";
      return;
    }
    try {
      requestCode = await onSubmit(toUsername, pointsCenti);
      sentPointsCenti = pointsCenti;
    } catch (caught) {
      localError = caught instanceof Error ? caught.message : String(caught);
    }
  }
</script>

<WidgetOverlay title="Transfer points" open={open}>
  <div class="bg-card text-card-foreground w-full max-w-sm rounded-md border p-5">
    <p
      class="text-muted-foreground mb-3 text-[0.72rem] font-bold tracking-widest uppercase"
    >
      Transfer points
    </p>

    {#if requestCode !== null}
      <div class="flex flex-col items-center gap-3 py-4">
        <p class="text-sm">
          Show this code to the shop owner:
        </p>
        <p
          class="font-mono text-4xl font-bold tracking-[0.3em]"
          aria-live="polite"
        >
          {requestCode}
        </p>
        <p class="text-muted-foreground text-[0.65rem]">
          The owner will enter this code to confirm your {formatPoints(
            sentPointsCenti,
          )}-point transfer
          to <strong>{toUsername}</strong>. The code expires in about 10 minutes.
        </p>
        <Button type="button" variant="outline" onclick={onClose}>
          Done
        </Button>
      </div>
    {:else}
      <form class="flex flex-col gap-3" onsubmit={(e) => void handleSubmit(e)}>
        <div class="flex flex-col gap-1.5">
          <label for="pt-transfer-to" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
            Recipient username
          </label>
          <Input
            id="pt-transfer-to"
            bind:value={toUsername}
            minlength={3}
            maxlength={32}
            placeholder="e.g. juan_02"
            required
          />
        </div>

        <div class="flex flex-col gap-1.5">
          <label for="pt-transfer-points" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
            Points to send
          </label>
          <Input
            id="pt-transfer-points"
            bind:value={pointsInput}
            type="text"
            inputmode="decimal"
            pattern={"[0-9]+([.][0-9]{1,2})?"}
            placeholder="e.g. 1.50"
            required
          />
          <p class="text-muted-foreground text-[0.65rem]">
            You have {formatPoints(currentPointsCenti)} points.
          </p>
        </div>

        {#if localError}
          <p class="text-destructive text-[0.65rem]" role="alert">
            {localError}
          </p>
        {/if}

        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" disabled={busy} onclick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !submittable}>
            {busy ? "Requesting…" : "Request transfer"}
          </Button>
        </div>
      </form>
    {/if}
  </div>
</WidgetOverlay>
