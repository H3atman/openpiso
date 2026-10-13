<script lang="ts">
  import * as api from "../../api";
  import { Switch } from "../../components/ui/switch";
  import LoadErrorNotice from "../../components/LoadErrorNotice.svelte";
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
  <section class="rounded-lg border bg-card p-6">
    <h3 class="mb-4 text-lg font-semibold">Diagnostics Mode</h3>

    {#if diagnosticsEnabled !== null}
      <p class="mb-4 text-sm text-muted-foreground">
        Reveals the top-level <strong>Diagnostics</strong> page — the Support
        Bundle, the Diagnostic Request, the Coin Debug Bundle and Coin Debug
        Window, and Run Records — on every device signed in to this console.
        It changes visibility only: evidence is captured whether it is on or
        off, and every change is recorded in the Audit Log.
      </p>

      <div class="flex items-center gap-3">
        <Switch
          checked={diagnosticsEnabled}
          onCheckedChange={(value) => toggleDiagnosticsMode(value === true)}
          disabled={diagnosticsBusy}
        />
        <span class="text-sm font-medium">
          {diagnosticsEnabled ? "On" : "Off"}
        </span>
      </div>
    {:else if diagnosticsModeError}
      <!-- Only with no value at all: a transient failure over a known value
           must not put a read failure above a working switch. -->
      <LoadErrorNotice
        message="Diagnostics Mode could not be read"
        detail={diagnosticsModeError}
        {onRetry}
      />
    {:else}
      <div class="text-sm text-muted-foreground">Loading Diagnostics Mode…</div>
    {/if}
  </section>

  <section class="rounded-lg border bg-card p-6">
    <h3 class="mb-4 text-lg font-semibold">Owner PIN (Client)</h3>
    <p class="mb-4 text-sm text-muted-foreground">
      The Owner PIN gates the Client's backend settings and Quit. It is
      stored <strong>hashed</strong> server-side and is write-only — the
      current value cannot be read back. Setting a new PIN overwrites the
      previous one immediately on every Client on its next verify.
    </p>

    <div class="space-y-4">
      <div>
        <label class="block text-sm font-medium" for="new-pin">New PIN</label>
        <input
          id="new-pin"
          type="password"
          autocomplete="new-password"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          placeholder="Enter new PIN"
          bind:value={newPin}
        />
      </div>
      <div>
        <label class="block text-sm font-medium" for="confirm-new-pin">Confirm new PIN</label>
        <input
          id="confirm-new-pin"
          type="password"
          autocomplete="new-password"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          placeholder="Confirm new PIN"
          bind:value={confirmNewPin}
        />
        <p class="mt-1 text-xs text-muted-foreground">
          4–128 characters. The current PIN is never asked for and cannot be displayed.
        </p>
      </div>
    </div>

    <button
      class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
      type="button"
      disabled={ownerPinBusy || !newPin || !confirmNewPin}
      onclick={saveOwnerPin}
    >
      {ownerPinBusy ? "Saving…" : "Set owner PIN"}
    </button>
  </section>

  <section class="rounded-lg border bg-card p-6">
    <h3 class="mb-4 text-lg font-semibold">Web Admin PIN (this console)</h3>
    <p class="mb-4 text-sm text-muted-foreground">
      Change the PIN used to sign in to this owner console. Enter the current PIN to authorize the change.
    </p>

    <div class="space-y-4">
      <div>
        <label class="block text-sm font-medium" for="current-web-admin-pin">Current PIN</label>
        <input
          id="current-web-admin-pin"
          type="password"
          autocomplete="current-password"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          placeholder="Enter current PIN"
          bind:value={currentWebAdminPin}
        />
      </div>
      <div>
        <label class="block text-sm font-medium" for="new-web-admin-pin">New PIN</label>
        <input
          id="new-web-admin-pin"
          type="password"
          autocomplete="new-password"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          placeholder="Enter new PIN"
          bind:value={newWebAdminPin}
        />
      </div>
      <div>
        <label class="block text-sm font-medium" for="confirm-new-web-admin-pin">Confirm new PIN</label>
        <input
          id="confirm-new-web-admin-pin"
          type="password"
          autocomplete="new-password"
          class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
          placeholder="Confirm new PIN"
          bind:value={confirmNewWebAdminPin}
        />
        <p class="mt-1 text-xs text-muted-foreground">4–128 characters.</p>
      </div>
    </div>

    <button
      class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
      type="button"
      disabled={webAdminPinBusy || !currentWebAdminPin || !newWebAdminPin || !confirmNewWebAdminPin}
      onclick={rotateWebAdminPin}
    >
      {webAdminPinBusy ? "Saving…" : "Change Web Admin PIN"}
    </button>
  </section>
</div>
