<script lang="ts">
  // TransferTimeDialog — immediate Time Transfer to another account
  // by Username (ADR-0030). The server debits the sender and credits
  // the recipient atomically; this dialog collects the recipient and
  // the number of seconds and surfaces the server's error on failure.
  // The caller provides the current balance so the form can cap the
  // input and show a preview.

  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { formatProseDuration } from "$lib/format";
  import WidgetOverlay from "$lib/components/WidgetOverlay.svelte";

  interface Props {
    open: boolean;
    currentSeconds: number;
    busy: boolean;
    onSubmit: (toUsername: string, seconds: number) => Promise<void>;
    onClose: () => void;
  }

  let { open, currentSeconds, busy, onSubmit, onClose }: Props = $props();

  let toUsername = $state("");
  let seconds = $state(0);
  let localError = $state("");

  const maxTransferable = $derived(Math.max(0, currentSeconds));

  async function handleSubmit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    localError = "";
    if (seconds <= 0) {
      localError = "Seconds must be positive.";
      return;
    }
    if (seconds > maxTransferable) {
      localError = "You do not have enough time.";
      return;
    }
    try {
      await onSubmit(toUsername, seconds);
    } catch (caught) {
      localError = caught instanceof Error ? caught.message : String(caught);
    }
  }
</script>

<WidgetOverlay title="Transfer time" open={open}>
  <div class="bg-card text-card-foreground w-full max-w-sm rounded-md border p-5">
    <p
      class="text-muted-foreground mb-3 text-[0.72rem] font-bold tracking-widest uppercase"
    >
      Transfer time
    </p>

    <form class="flex flex-col gap-3" onsubmit={(e) => void handleSubmit(e)}>
      <div class="flex flex-col gap-1.5">
        <label for="transfer-to" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
          Recipient username
        </label>
        <Input
          id="transfer-to"
          bind:value={toUsername}
          minlength={3}
          maxlength={32}
          placeholder="e.g. juan_02"
          required
        />
      </div>

      <div class="flex flex-col gap-1.5">
        <label for="transfer-seconds" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
          Seconds to send
        </label>
        <Input
          id="transfer-seconds"
          bind:value={seconds}
          type="number"
          min="1"
          max={maxTransferable}
          step="1"
          required
        />
        <p class="text-muted-foreground text-[0.65rem]">
          You have {formatProseDuration(currentSeconds)} portable time.
        </p>
        {#if seconds > 0}
          <p class="text-muted-foreground text-[0.65rem]" data-transfer-preview>
            Sending {formatProseDuration(seconds)}
          </p>
        {/if}
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
        <Button type="submit" disabled={busy || seconds <= 0 || seconds > maxTransferable}>
          {busy ? "Sending…" : "Send time"}
        </Button>
      </div>
    </form>
  </div>
</WidgetOverlay>
