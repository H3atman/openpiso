<script lang="ts">
  // OwnerPinGate — the PIN prompt that guards the Client's backend settings
  // (ADR-0027 §2, plan §6). The seated customer is untrusted, so the
  // settings panel (server URL, PC ID, view mode) must not be reachable
  // until the owner proves the shared PIN. Verification is server-side
  // (`verify_owner_pin` → `POST /api/owner/verify-pin`); this component owns
  // only the prompt and the unlock signal, never any authority.
  //
  // A wrong PIN clears the field and shows an inline error; a transport
  // failure is surfaced distinctly so the owner can tell "wrong PIN" apart
  // from "server unreachable". On success the parent swaps in the panel.

  import { onDestroy } from "svelte";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import { verifyOwnerPin } from "$lib/api";

  interface Props {
    onUnlock: () => Promise<void> | void;
    onCancel: () => void;
  }

  let { onUnlock, onCancel }: Props = $props();

  let pin = $state("");
  let busy = $state(false);
  let error = $state("");
  let active = true;

  onDestroy(() => {
    active = false;
  });

  async function submit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (busy) return;
    busy = true;
    error = "";
    try {
      const ok = await verifyOwnerPin(pin);
      if (!active) return;
      if (ok) {
        pin = "";
        await onUnlock();
      } else {
        pin = "";
        error = "Incorrect owner PIN.";
      }
    } catch (caught) {
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }
</script>

<form class="flex flex-col gap-4" onsubmit={submit}>
  <div class="flex flex-col gap-1.5">
    <label
      for="owner-pin"
      class="text-muted-foreground text-xs font-bold tracking-wider uppercase"
    >
      Owner PIN
    </label>
    <Input
      id="owner-pin"
      bind:value={pin}
      type="password"
      inputmode="numeric"
      autocomplete="off"
      placeholder="••••"
      required
    />
    <p class="text-muted-foreground text-xs">
      Backend settings are owner-only. Enter the shop's owner PIN to continue.
    </p>
  </div>

  {#if error}
    <p class="text-destructive text-sm" role="alert">{error}</p>
  {/if}

  <div class="flex justify-end gap-2">
    <Button type="button" variant="outline" onclick={onCancel} disabled={busy}>
      Cancel
    </Button>
    <Button type="submit" disabled={busy || pin.length === 0}>
      {busy ? "Verifying…" : "Unlock"}
    </Button>
  </div>
</form>
