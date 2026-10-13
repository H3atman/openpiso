<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import ConfirmDialog from "../../components/ConfirmDialog.svelte";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import type {
    DetectedTelegramChat,
    TelegramDeliveryHealth,
    TelegramStatus,
  } from "../../types";

  let status = $state<TelegramStatus | null>(null);
  let loadError = $state("");

  // Guided-setup state. The verified-but-unconfirmed bot username comes back
  // from Admin after the token passes its Bot API check; nothing is stored
  // server-side until a test message succeeds.
  let tokenInput = $state("");
  let pendingBot = $state("");
  let detectBusy = $state(false);
  let detectedChats = $state<DetectedTelegramChat[]>([]);
  let detectionError = $state("");
  let useManualChatId = $state(false);
  let manualChatId = $state("");
  let setupError = $state("");
  let busy = $state(false);

  // PIN dialogs for connect and disconnect. The decision is snapshotted when
  // the dialog opens, like every other PIN-gated write here.
  let confirmConnect = $state(false);
  let confirmDisconnect = $state(false);
  let pin = $state("");
  let pinError = $state("");

  onMount(() => {
    void load();
  });

  async function load() {
    loadError = "";
    try {
      status = await api.getTelegramStatus();
    } catch (caught) {
      loadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  function resetSetup() {
    tokenInput = "";
    pendingBot = "";
    detectedChats = [];
    detectionError = "";
    useManualChatId = false;
    manualChatId = "";
    setupError = "";
  }

  async function verifyToken() {
    if (busy || !tokenInput.trim()) return;
    busy = true;
    setupError = "";
    try {
      const candidate = await api.submitTelegramToken(tokenInput.trim());
      pendingBot = candidate.bot_username;
      tokenInput = "";
      reportActionSuccess("verify", `Bot @${candidate.bot_username}`);
    } catch (caught) {
      setupError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("verify", "Telegram bot token", caught);
    } finally {
      busy = false;
    }
  }

  async function detect() {
    if (detectBusy || !pendingBot) return;
    detectBusy = true;
    detectionError = "";
    try {
      detectedChats = await api.detectTelegramChat();
      reportActionSuccess(
        "detect",
        detectedChats.length === 0
          ? "No recent Telegram chats"
          : `${detectedChats.length} Telegram ${detectedChats.length === 1 ? "chat" : "chats"}`,
      );
    } catch (caught) {
      detectionError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("detect", "Telegram chat", caught);
    } finally {
      detectBusy = false;
    }
  }

  function askConnect() {
    if (busy) return;
    const manualMode = useManualChatId && manualChatId.trim();
    if (!manualMode && detectedChats.length !== 1) return;
    pin = "";
    pinError = "";
    confirmConnect = true;
  }

  function cancelConnect() {
    if (busy) return;
    confirmConnect = false;
  }

  async function doConnect() {
    if (busy || !pin) return;
    busy = true;
    pinError = "";
    const chatId = useManualChatId ? manualChatId.trim() || undefined : undefined;
    try {
      status = await api.connectTelegram(chatId, pin);
      confirmConnect = false;
      resetSetup();
      reportActionSuccess("save", "Telegram");
    } catch (caught) {
      pinError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("save", "Telegram", caught);
    } finally {
      busy = false;
    }
  }

  function askDisconnect() {
    if (busy) return;
    pin = "";
    pinError = "";
    confirmDisconnect = true;
  }

  function cancelDisconnect() {
    if (busy) return;
    confirmDisconnect = false;
  }

  async function doDisconnect() {
    if (busy || !pin) return;
    busy = true;
    pinError = "";
    try {
      await api.disconnectTelegram(pin);
      confirmDisconnect = false;
      await load();
      reportActionSuccess("delete", "Telegram");
    } catch (caught) {
      pinError = caught instanceof Error ? caught.message : String(caught);
      reportActionFailure("delete", "Telegram", caught);
    } finally {
      busy = false;
    }
  }

  // The one-line queue state Settings shows: healthy, or which parts of the
  // queue are waiting/retrying/expired. Counts only — never message content.
  function deliveryStateLine(delivery: TelegramDeliveryHealth): string {
    const parts: string[] = [];
    if (delivery.queued_count > 0) parts.push(`${delivery.queued_count} queued`);
    if (delivery.retrying_count > 0) {
      parts.push(`${delivery.retrying_count} retrying after failed attempts`);
    }
    if (parts.length === 0 && delivery.expired_count === 0) {
      return "Delivery is healthy: nothing waiting.";
    }
    let line = parts.join(", ");
    if (delivery.expired_count > 0) {
      line += `${parts.length ? "; " : ""}${delivery.expired_count} expired unsent after 24 hours of retries`;
    }
    return `${line}. Local Alerts keep the full history.`;
  }

  // The single confirmed destination when exactly one private chat was found,
  // or the manually typed fallback id otherwise.
  const readyToConnect = $derived(
    Boolean(pendingBot) &&
      (useManualChatId
        ? manualChatId.trim().length > 0
        : detectedChats.length === 1),
  );
</script>

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">Telegram Alerts</h3>

  {#if loadError}
    <div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
      {loadError}
    </div>
  {/if}

  {#if status}
    <p class="mb-4 text-sm text-muted-foreground">
      Optionally receive PC Monitoring alerts in one private Telegram chat.
      Outbound only — the bot can never query or control OpenPiso, and the
      token is stored protected and never shown again. The whole integration
      is excluded from Backups: after a restore you reconnect here.
    </p>

    {#if !status.platform_supported}
      <div class="rounded-md bg-muted p-3 text-sm text-muted-foreground">
        Telegram requires a Windows Admin. This installation cannot protect the
        bot token and will never manufacture results.
      </div>
    {:else if status.connected && status.destination}
      <div class="space-y-3">
        <div class="text-sm">
          <span class="font-medium">Connected:</span>
          @{status.destination.bot_username} — {status.destination.chat_title}
          <span class="text-muted-foreground">(chat {status.destination.chat_id})</span>
        </div>
        {#if status.last_test}
          <div class="text-sm text-muted-foreground">
            Last successful test: {new Date(status.last_test.verified_at).toLocaleString()}
          </div>
        {/if}
        {#if status.delivery}
          <div class="rounded-md bg-muted p-3 text-sm" data-testid="delivery-health">
            <p class="font-medium">Delivery</p>
            <p class="text-muted-foreground">
              {status.delivery.last_sent_at
                ? `Last message delivered: ${new Date(status.delivery.last_sent_at).toLocaleString()}`
                : "No message delivered yet."}
            </p>
            <p
              class={status.delivery.expired_count > 0
                ? "text-destructive"
                : "text-muted-foreground"}
            >
              {deliveryStateLine(status.delivery)}
            </p>
            {#if status.delivery.last_error}
              <p class="mt-1 text-destructive">
                {`Last error (${new Date(status.delivery.last_error_at ?? "").toLocaleString()}): ${status.delivery.last_error}`}
              </p>
            {/if}
          </div>
        {/if}
        <div class="flex items-center gap-3">
          <button
            type="button"
            class="rounded-md border px-3 py-1.5 text-sm hover:bg-accent"
            onclick={askDisconnect}
            disabled={busy}
          >
            Disconnect
          </button>
          <span class="text-xs text-muted-foreground">
            Deleting the token and this chat. Local PC Monitoring keeps running.
          </span>
        </div>
      </div>
    {:else}
      <div class="space-y-4 text-sm">
        <ol class="list-decimal space-y-1 pl-5 text-muted-foreground">
          <li>
            Message <strong>@BotFather</strong> on Telegram and create a bot
            (<em>/newbot</em>), then copy the token it gives you.
          </li>
          <li>Send <em>/start</em> to your new bot from your own account. It must be a private chat sent within the last ten minutes.</li>
          <li>Paste the token below.</li>
        </ol>

        {#if pendingBot}
          <div class="rounded-md bg-muted p-3">
            Token verified for <strong>@{pendingBot}</strong>.
            <button
              type="button"
              class="ml-2 text-primary underline"
              onclick={() => {
                pendingBot = "";
                detectedChats = [];
              }}
            >
              Use a different token
            </button>
          </div>
          <button
            type="button"
            class="rounded-md border px-3 py-1.5 hover:bg-accent"
            onclick={detect}
            disabled={detectBusy}
          >
            {detectBusy ? "Checking for messages…" : "Look for my message"}
          </button>
          {#if detectionError}
            <div class="rounded-md bg-destructive/10 p-3 text-destructive">{detectionError}</div>
          {/if}
          {#if detectedChats.length > 0}
            <div class="rounded-md bg-muted p-3">
              <p class="mb-2 font-medium">Private chats that messaged your bot:</p>
              <ul class="space-y-1">
                {#each detectedChats as chat (chat.chat_id)}
                  <li>
                    {chat.title} <span class="text-muted-foreground">({chat.chat_id})</span>
                  </li>
                {/each}
              </ul>
              {#if detectedChats.length > 1}
                <p class="mt-2 text-xs text-muted-foreground">
                  More than one chat wrote to this bot, so enter your own chat ID below instead.
                </p>
              {/if}
            </div>
          {/if}
          <label class="flex items-center gap-2 text-xs text-muted-foreground">
            <input type="checkbox" bind:checked={useManualChatId} />
            Enter chat ID manually
          </label>
          {#if useManualChatId}
            <input
              type="text"
              inputmode="numeric"
              placeholder="Numeric private chat ID"
              class="block w-56 rounded-md border bg-background px-3 py-2"
              bind:value={manualChatId}
            />
          {/if}
          <div>
            <button
              type="button"
              class="rounded-md bg-primary px-3 py-1.5 font-medium text-primary-foreground disabled:opacity-50"
              onclick={askConnect}
              disabled={!readyToConnect || busy}
            >
              Send test &amp; connect
            </button>
            {#if !readyToConnect}
              <span class="ml-2 text-xs text-muted-foreground">
                Look for your message first{useManualChatId ? " or type your chat ID" : ""}.
              </span>
            {/if}
          </div>
          {#if setupError}
            <div class="rounded-md bg-destructive/10 p-3 text-destructive">{setupError}</div>
          {/if}
        {:else}
          <div class="flex items-center gap-2 max-md:flex-col max-md:items-start">
            <input
              id="telegram-token"
              type="password"
              autocomplete="off"
              placeholder="Paste your BotFather token"
              class="block w-80 rounded-md border bg-background px-3 py-2 max-md:w-full"
              bind:value={tokenInput}
            />
            <button
              type="button"
              class="rounded-md border px-3 py-1.5 hover:bg-accent"
              onclick={verifyToken}
              disabled={busy || !tokenInput.trim()}
            >
              Verify token
            </button>
          </div>
          {#if setupError}
            <div class="rounded-md bg-destructive/10 p-3 text-destructive">{setupError}</div>
          {/if}
        {/if}
      </div>
    {/if}
  {:else if !loadError}
    <div class="text-sm text-muted-foreground">Loading Telegram state…</div>
  {/if}
</div>

{#if confirmConnect}
  <ConfirmDialog
    title="Send Telegram test message?"
    confirmLabel="Test &amp; connect"
    busyLabel="Sending…"
    busy={busy}
    onconfirm={doConnect}
    oncancel={cancelConnect}
  >
    A test message will be sent to
    <strong>{useManualChatId ? `chat ${manualChatId.trim()}` : "your detected chat"}</strong>.
    The connection becomes active only if it arrives.
    <div class="mt-3">
      <label class="block text-sm font-medium" for="telegram-connect-pin">
        Current Web Admin PIN
      </label>
      <input
        id="telegram-connect-pin"
        type="password"
        autocomplete="current-password"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        placeholder="Enter current PIN"
        bind:value={pin}
      />
      {#if pinError}
        <p class="mt-1 text-xs text-destructive">{pinError}</p>
      {/if}
    </div>
  </ConfirmDialog>
{/if}

{#if confirmDisconnect}
  <ConfirmDialog
    title="Disconnect Telegram?"
    confirmLabel="Disconnect"
    busyLabel="Disconnecting…"
    busy={busy}
    destructive
    onconfirm={doDisconnect}
    oncancel={cancelDisconnect}
  >
    Deletes the protected token and this chat's delivery, including any
    pending deliveries. Local monitoring and Alert history are unaffected.
    <div class="mt-3">
      <label class="block text-sm font-medium" for="telegram-disconnect-pin">
        Current Web Admin PIN
      </label>
      <input
        id="telegram-disconnect-pin"
        type="password"
        autocomplete="current-password"
        class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
        placeholder="Enter current PIN"
        bind:value={pin}
      />
      {#if pinError}
        <p class="mt-1 text-xs text-destructive">{pinError}</p>
      {/if}
    </div>
  </ConfirmDialog>
{/if}
