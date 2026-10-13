<script lang="ts">
  // Backend settings: admin server URL only. The PC ID is server-owned
  // (ADR-0031) and is shown read-only in the footer once the server
  // has adopted one — the owner no longer types it. Validation stays
  // in Rust (set_client_config) and we just surface the server's error.

  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";

  interface Props {
    serverUrl: string;
    connected: boolean;
    busy: boolean;
    onSaveAndConnect: (serverUrl: string) => Promise<void> | void;
    onDisconnect: () => Promise<void> | void;
    showDisconnect?: boolean;
  }

  let {
    serverUrl = $bindable(),
    connected,
    busy,
    onSaveAndConnect,
    onDisconnect,
    showDisconnect = true,
  }: Props = $props();

  async function handleSubmit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    await onSaveAndConnect(serverUrl);
  }
</script>

<form class="flex flex-col gap-4" onsubmit={handleSubmit}>
  <div class="flex flex-col gap-1.5">
    <label for="server-url" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
      Admin server URL
    </label>
    <Input
      id="server-url"
      bind:value={serverUrl}
      type="url"
      placeholder="http://192.168.1.10:3210"
      required
    />
  </div>
  <div class="flex flex-wrap gap-2">
    <Button type="submit" disabled={busy}>
      {connected ? "Reconnect" : "Save and connect"}
    </Button>
    {#if showDisconnect}
      <Button
        type="button"
        variant="outline"
        disabled={busy || !connected}
        onclick={() => void onDisconnect()}
      >
        Disconnect
      </Button>
    {/if}
  </div>
</form>
