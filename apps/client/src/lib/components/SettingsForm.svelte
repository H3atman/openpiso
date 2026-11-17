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
    /** Distinct id when a second form is on screen (the Lock's setup surface). */
    inputId?: string;
  }

  let {
    serverUrl = $bindable(),
    connected,
    busy,
    onSaveAndConnect,
    onDisconnect,
    showDisconnect = true,
    inputId = "server-url",
  }: Props = $props();

  async function handleSubmit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    await onSaveAndConnect(serverUrl);
  }
</script>

<form class="flex flex-col gap-4" onsubmit={handleSubmit}>
  <div class="flex flex-col gap-1.5">
    <label for={inputId} class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
      Admin server address
    </label>
    <!-- Plain text: Rust accepts a bare host or host:port as well as a URL. -->
    <Input
      id={inputId}
      bind:value={serverUrl}
      type="text"
      placeholder="192.168.1.10"
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
