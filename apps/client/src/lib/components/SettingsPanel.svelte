<script lang="ts">
  // SettingsPanel — the owner-only backend settings (ADR-0027 §2, plan §8.2).
  // Rendered only after the OwnerPinGate unlocks, so everything here is behind
  // the PIN: the admin server URL form, the default View Mode, and the owner
  // Quit. The PC ID is server-owned (ADR-0031) and is shown read-only in the
  // footer, not edited here. There is no customer-facing Quit anywhere else.

  import { Button } from "$lib/components/ui/button";
  import SettingsForm from "$lib/components/SettingsForm.svelte";
  import type { ViewMode } from "$lib/types";

  interface Props {
    serverUrl: string;
    pcId: string | null;
    connected: boolean;
    busy: boolean;
    viewMode: ViewMode;
    launchAtStartup: boolean;
    disableWindowsSignOut: boolean;
    windowsPlatform?: boolean;
    onSaveAndConnect: (serverUrl: string) => Promise<void> | void;
    onDisconnect: () => Promise<void> | void;
    onSetViewMode: (mode: ViewMode) => Promise<void> | void;
    onSetLaunchAtStartup: (enabled: boolean) => Promise<void> | void;
    onSetWindowsSignOut: (enabled: boolean) => Promise<void> | void;
    onQuit: () => Promise<void> | void;
  }

  let {
    serverUrl,
    pcId,
    connected,
    busy,
    viewMode,
    launchAtStartup,
    disableWindowsSignOut,
    windowsPlatform = true,
    onSaveAndConnect,
    onDisconnect,
    onSetViewMode,
    onSetLaunchAtStartup,
    onSetWindowsSignOut,
    onQuit,
  }: Props = $props();

  let confirmingQuit = $state(false);
</script>

<div class="flex flex-col gap-5">
  <SettingsForm
    {serverUrl}
    {connected}
    {busy}
    {onSaveAndConnect}
    {onDisconnect}
  />

  <div
    class="text-muted-foreground flex items-center justify-between rounded-md border p-3 text-xs"
  >
    <span class="font-bold tracking-wider uppercase">PC id</span>
    <code class="font-mono text-sm" data-testid="settings-pc-id">
      {pcId ?? "— (registering)"}
    </code>
  </div>

  {#if windowsPlatform}
  <div class="flex flex-col gap-1.5">
    <span
      class="text-muted-foreground text-xs font-bold tracking-wider uppercase"
    >
      Launch at startup
    </span>
    <p class="text-muted-foreground text-xs">
      Start the Client when this Windows account signs in. Applies only to this account.
    </p>
    <button
      type="button"
      role="switch"
      aria-checked={launchAtStartup}
      data-testid="settings-launch-at-startup"
      class="flex items-center justify-between rounded-md border p-3 text-left text-sm"
      disabled={busy}
      onclick={() => void onSetLaunchAtStartup(!launchAtStartup)}
    >
      <span>{launchAtStartup ? "On" : "Off"}</span>
      <span class="text-muted-foreground text-xs">{launchAtStartup ? "Enabled" : "Disabled"}</span>
    </button>
  </div>

  <div class="flex flex-col gap-1.5">
    <span
      class="text-muted-foreground text-xs font-bold tracking-wider uppercase"
    >
      Disable Windows sign-out
    </span>
    <p class="text-muted-foreground text-xs">
      Removes Sign out and Switch user, and disables Win+L. Restart and Shut down stay available. Enabling asks for one Windows UAC consent.
    </p>
    <button
      type="button"
      role="switch"
      aria-checked={disableWindowsSignOut}
      data-testid="settings-disable-windows-sign-out"
      class="flex items-center justify-between rounded-md border p-3 text-left text-sm"
      disabled={busy}
      onclick={() => void onSetWindowsSignOut(!disableWindowsSignOut)}
    >
      <span>{disableWindowsSignOut ? "On" : "Off"}</span>
      <span class="text-muted-foreground text-xs">{disableWindowsSignOut ? "Protected" : "Stock Windows"}</span>
    </button>
  </div>
  {/if}

  <div class="flex flex-col gap-1.5">
    <span
      class="text-muted-foreground text-xs font-bold tracking-wider uppercase"
    >
      Default view
    </span>
    <p class="text-muted-foreground text-xs">
      Which float the customer resumes in once they have time.
    </p>
    <div class="flex gap-2">
      <Button
        type="button"
        size="sm"
        variant={viewMode === "widget" ? "default" : "outline"}
        disabled={busy}
        onclick={() => void onSetViewMode("widget")}
      >
        Widget
      </Button>
      <Button
        type="button"
        size="sm"
        variant={viewMode === "mini" ? "default" : "outline"}
        disabled={busy}
        onclick={() => void onSetViewMode("mini")}
      >
        Mini bar
      </Button>
    </div>
  </div>

  <div class="border-destructive/40 flex flex-col gap-2 rounded-md border p-3">
    <span
      class="text-muted-foreground text-xs font-bold tracking-wider uppercase"
    >
      Quit client
    </span>
    <p class="text-muted-foreground text-xs">
      Exits the OpenPiso client on this PC. Owner-only — the customer cannot
      reach this.
    </p>
    <p
      class="rounded-md border border-border bg-muted/40 p-2 text-xs
        text-muted-foreground"
      data-testid="quit-suppression-guidance"
    >
      Planned maintenance? Suppress this PC's monitoring in Web Admin first —
      quitting from here cannot suppress server-side PC Monitoring, so a
      stopped Client may truthfully raise a mismatch Alert.
    </p>
    {#if confirmingQuit}
      <div class="flex flex-wrap items-center gap-2">
        <span class="text-sm">Quit the client now?</span>
        <Button
          type="button"
          size="sm"
          variant="destructive"
          disabled={busy}
          onclick={() => void onQuit()}
        >
          Quit
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onclick={() => (confirmingQuit = false)}
        >
          Cancel
        </Button>
      </div>
    {:else}
      <div>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onclick={() => (confirmingQuit = true)}
        >
          Quit client…
        </Button>
      </div>
    {/if}
  </div>
</div>
