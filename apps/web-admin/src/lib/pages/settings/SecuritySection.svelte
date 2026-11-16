<script lang="ts">
  import * as api from "../../api";
  import { Switch } from "../../components/ui/switch";
  import LoadErrorNotice from "../../components/LoadErrorNotice.svelte";
  import { Button } from "../../components/ui/button";
  import { Input } from "../../components/ui/input";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import SettingsField from "../../components/settings/SettingsField.svelte";
  import ToggleRow from "../../components/settings/ToggleRow.svelte";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";

  // Shop-wide Diagnostics Mode (ADR-0081): the visibility flag for the
  // Diagnostics page. Server-stored, so one flip means every device; the
  // change is audited. It is visibility only, never a security boundary.
  // The value and its load failure belong to the root shell, so this section,
  // the sidebar, the Diagnostics page, and Coin Device never disagree.
  let {
    diagnosticsMode = null,
    diagnosticsModeError = "",
    onChange,
    onRetry,
  }: {
    diagnosticsMode?: boolean | null;
    diagnosticsModeError?: string;
    // Required: a successful write reaches the screen only through the owner
    // of the value, so a caller that omits this would show a stale switch.
    onChange: (enabled: boolean) => void;
    onRetry: () => void;
  } = $props();

  // Shown only while a write is in flight, so the switch reflects the
  // decision at once and falls back to the shared value either way.
  let pendingDiagnostics = $state<boolean | null>(null);
  let diagnosticsBusy = $state(false);
  const diagnosticsEnabled = $derived(pendingDiagnostics ?? diagnosticsMode);

  async function toggleDiagnosticsMode(value: boolean) {
    if (diagnosticsBusy || diagnosticsEnabled === null) return;
    pendingDiagnostics = value;
    diagnosticsBusy = true;
    try {
      const stored = (await api.setDiagnosticsMode(value)).enabled;
      // Publish the server's answer to the browser's one value immediately;
      // other browsers converge on their next poll.
      onChange(stored);
      reportActionSuccess("save", "Diagnostics Mode");
    } catch (caught) {
      reportActionFailure("save", "Diagnostics Mode", caught);
    } finally {
      pendingDiagnostics = null;
      diagnosticsBusy = false;
    }
  }

  // Owner PIN (write-only: the current value is never read back, ADR-0027)
  let newPin = $state("");
  let confirmNewPin = $state("");
  let ownerPinBusy = $state(false);

  // Web Admin PIN rotation requires the current credential (ADR-0050).
  let currentWebAdminPin = $state("");
  let newWebAdminPin = $state("");
  let confirmNewWebAdminPin = $state("");
  let webAdminPinBusy = $state(false);

  async function saveOwnerPin() {
    if (newPin !== confirmNewPin) {
      reportActionFailure("save", "Owner PIN", new Error("PINs do not match"));
      return;
    }
    ownerPinBusy = true;
    try {
      await api.setOwnerPin({ new_pin: newPin });
      newPin = "";
      confirmNewPin = "";
      reportActionSuccess("save", "Owner PIN");
    } catch (caught) {
      reportActionFailure("save", "Owner PIN", caught);
    } finally {
      ownerPinBusy = false;
    }
  }

  async function rotateWebAdminPin() {
    if (newWebAdminPin !== confirmNewWebAdminPin) {
      reportActionFailure("save", "Web Admin PIN", new Error("PINs do not match"));
      return;
    }
    webAdminPinBusy = true;
    try {
      await api.rotateWebAdminPin({
        current_pin: currentWebAdminPin,
        new_pin: newWebAdminPin,
      });
      currentWebAdminPin = "";
      newWebAdminPin = "";
      confirmNewWebAdminPin = "";
      reportActionSuccess("save", "Web Admin PIN");
    } catch (caught) {
      reportActionFailure("save", "Web Admin PIN", caught);
    } finally {
      webAdminPinBusy = false;
    }
  }
</script>

<div class="space-y-6">
  <SettingsCard
    title="Diagnostics Mode"
    description="Visibility only: evidence is captured whether it is on or off, and every change is recorded in the Audit Log."
  >
    {#if diagnosticsEnabled !== null}
      <ToggleRow
        id="diagnostics-mode"
        label="Show the Diagnostics page"
        status={diagnosticsEnabled ? "On" : "Off"}
      >
        {#snippet description()}
          Reveals the top-level <strong class="font-medium text-foreground">Diagnostics</strong> page — the
          Support Bundle, the Diagnostic Request, the Coin Debug Bundle and Coin Debug Window, and Run
          Records — on every device signed in to this console.
        {/snippet}
        <Switch
          id="diagnostics-mode"
          aria-describedby="diagnostics-mode-description"
          checked={diagnosticsEnabled}
          onCheckedChange={(value) => toggleDiagnosticsMode(value === true)}
          disabled={diagnosticsBusy}
        />
      </ToggleRow>
    {:else if diagnosticsModeError}
      <!-- Only with no value at all: a transient failure over a known value
           must not put a read failure above a working switch. -->
      <LoadErrorNotice
        message="Diagnostics Mode could not be read"
        detail={diagnosticsModeError}
        {onRetry}
      />
    {:else}
      <p class="text-sm text-muted-foreground">Loading Diagnostics Mode…</p>
    {/if}
  </SettingsCard>

  <div class="grid gap-6 xl:grid-cols-2 xl:gap-y-0">
    <SettingsCard
      title="Owner PIN (Client)"
      description="Gates the Client's backend settings and Quit."
      class="xl:row-span-3 xl:grid xl:grid-rows-subgrid"
    >
      <form
        id="owner-pin-form"
        aria-label="Owner PIN (Client)"
        class="grid max-w-md gap-4"
        onsubmit={(event) => {
          event.preventDefault();
          if (!ownerPinBusy && newPin && confirmNewPin) void saveOwnerPin();
        }}
      >
        <p class="text-sm text-muted-foreground">
          Stored <strong class="font-medium text-foreground">hashed</strong> server-side and write-only — the
          current value cannot be read back. A new PIN overwrites the previous one immediately on every
          Client on its next verify.
        </p>
        <SettingsField id="new-pin" label="New PIN">
          <Input id="new-pin" type="password" autocomplete="new-password" placeholder="Enter new PIN" aria-describedby="confirm-new-pin-hint" bind:value={newPin} />
        </SettingsField>
        <SettingsField
          id="confirm-new-pin"
          label="Confirm new PIN"
          hint="4–128 characters. The current PIN is never asked for and cannot be displayed."
        >
          <Input
            id="confirm-new-pin"
            type="password"
            autocomplete="new-password"
            placeholder="Confirm new PIN"
            aria-describedby="confirm-new-pin-hint"
            bind:value={confirmNewPin}
          />
        </SettingsField>
      </form>

      {#snippet footer()}
        <Button type="submit" form="owner-pin-form" disabled={ownerPinBusy || !newPin || !confirmNewPin}>
          {ownerPinBusy ? "Saving…" : "Set owner PIN"}
        </Button>
      {/snippet}
    </SettingsCard>

    <SettingsCard
      title="Web Admin PIN (this console)"
      description="The PIN used to sign in to this owner console. Enter the current PIN to authorize the change."
      class="xl:row-span-3 xl:grid xl:grid-rows-subgrid"
    >
      <form
        id="web-admin-pin-form"
        aria-label="Web Admin PIN (this console)"
        class="grid max-w-md gap-4"
        onsubmit={(event) => {
          event.preventDefault();
          if (!webAdminPinBusy && currentWebAdminPin && newWebAdminPin && confirmNewWebAdminPin) {
            void rotateWebAdminPin();
          }
        }}
      >
        <SettingsField id="current-web-admin-pin" label="Current PIN">
          <Input
            id="current-web-admin-pin"
            type="password"
            autocomplete="current-password"
            placeholder="Enter current PIN"
            bind:value={currentWebAdminPin}
          />
        </SettingsField>
        <SettingsField id="new-web-admin-pin" label="New PIN">
          <Input
            id="new-web-admin-pin"
            aria-describedby="confirm-new-web-admin-pin-hint"
            type="password"
            autocomplete="new-password"
            placeholder="Enter new PIN"
            bind:value={newWebAdminPin}
          />
        </SettingsField>
        <SettingsField id="confirm-new-web-admin-pin" label="Confirm new PIN" hint="4–128 characters.">
          <Input
            id="confirm-new-web-admin-pin"
            type="password"
            autocomplete="new-password"
            placeholder="Confirm new PIN"
            aria-describedby="confirm-new-web-admin-pin-hint"
            bind:value={confirmNewWebAdminPin}
          />
        </SettingsField>
      </form>

      {#snippet footer()}
        <Button
          type="submit"
          form="web-admin-pin-form"
          disabled={webAdminPinBusy || !currentWebAdminPin || !newWebAdminPin || !confirmNewWebAdminPin}
        >
          {webAdminPinBusy ? "Saving…" : "Change Web Admin PIN"}
        </Button>
      {/snippet}
    </SettingsCard>
  </div>
</div>
