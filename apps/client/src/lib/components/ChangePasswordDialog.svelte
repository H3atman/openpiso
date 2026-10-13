<script lang="ts">
  // ChangePasswordDialog — customer self-service password rotation
  // (ADR-0037). Verifies the old password, sets the new one,
  // and keeps the active session token valid. Reuses the signup
  // length rule (validated server-side). Wrong old password is a
  // generic unauthorized error.

  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import WidgetOverlay from "$lib/components/WidgetOverlay.svelte";

  interface Props {
    open: boolean;
    busy: boolean;
    onSubmit: (oldPassword: string, newPassword: string) => Promise<void>;
    onClose: () => void;
  }

  let { open, busy, onSubmit, onClose }: Props = $props();

  let oldPassword = $state("");
  let newPassword = $state("");
  let confirmPassword = $state("");
  let localError = $state("");
  let success = $state(false);
  let submitting = $state(false);
  const disabled = $derived(busy || submitting);

  async function handleSubmit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (disabled) return;
    submitting = true;
    localError = "";
    success = false;
    if (newPassword !== confirmPassword) {
      localError = "New passwords do not match.";
      submitting = false;
      return;
    }
    try {
      await onSubmit(oldPassword, newPassword);
      success = true;
      oldPassword = "";
      newPassword = "";
      confirmPassword = "";
    } catch (caught) {
      localError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      submitting = false;
    }
  }
</script>

<WidgetOverlay title="Change password" open={open}>
  <div class="bg-card text-card-foreground w-full max-w-sm rounded-md border p-5">
    <p
      class="text-muted-foreground mb-3 text-[0.72rem] font-bold tracking-widest uppercase"
    >
      Change password
    </p>

    {#if success}
      <div class="flex flex-col items-center gap-3 py-4">
        <p class="text-sm text-emerald-600 dark:text-emerald-400">
          Password changed successfully. Your session is still active.
        </p>
        <Button type="button" variant="outline" onclick={onClose}>
          Close
        </Button>
      </div>
    {:else}
      <form class="flex flex-col gap-3" onsubmit={(e) => void handleSubmit(e)}>
        <div class="flex flex-col gap-1.5">
          <label for="old-password" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
            Current password
          </label>
          <Input
            id="old-password"
            bind:value={oldPassword}
            type="password"
            minlength={4}
            maxlength={128}
            required
          />
        </div>

        <div class="flex flex-col gap-1.5">
          <label for="new-password" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
            New password
          </label>
          <Input
            id="new-password"
            bind:value={newPassword}
            type="password"
            minlength={4}
            maxlength={128}
            required
          />
        </div>

        <div class="flex flex-col gap-1.5">
          <label for="confirm-password" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
            Confirm new password
          </label>
          <Input
            id="confirm-password"
            bind:value={confirmPassword}
            type="password"
            minlength={4}
            maxlength={128}
            required
          />
        </div>

        {#if localError}
          <p class="text-destructive text-[0.65rem]" role="alert">
            {localError}
          </p>
        {/if}

        <div class="flex justify-end gap-2">
          <Button type="button" variant="outline" disabled={disabled} onclick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={disabled}>
            {disabled ? "Saving…" : "Change password"}
          </Button>
        </div>
      </form>
    {/if}
  </div>
</WidgetOverlay>
