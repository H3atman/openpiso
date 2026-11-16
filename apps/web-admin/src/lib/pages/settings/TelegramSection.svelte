<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import ConfirmDialog from "../../components/ConfirmDialog.svelte";
  import { Button } from "../../components/ui/button";
  import { Input } from "../../components/ui/input";
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import StatePill from "../../components/settings/StatePill.svelte";
  import CheckIcon from "@lucide/svelte/icons/check";
  import UnplugIcon from "@lucide/svelte/icons/unplug";
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

  const connected = $derived(Boolean(status?.platform_supported && status.connected && status.destination));

  // The single confirmed destination when exactly one private chat was found,
  // or the manually typed fallback id otherwise.
  const readyToConnect = $derived(
    Boolean(pendingBot) &&
      (useManualChatId
        ? manualChatId.trim().length > 0
        : detectedChats.length === 1),
  );
</script>

{#snippet pinField(id: string)}
  <div class="mt-4 grid gap-1.5">
    <label class="text-sm font-medium text-foreground" for={id}>Current Web Admin PIN</label>
    <Input
      {id}
      type="password"
      autocomplete="current-password"
      placeholder="Enter current PIN"
      aria-invalid={pinError ? true : undefined}
      aria-describedby={pinError ? `${id}-error` : undefined}
      bind:value={pin}
    />
    {#if pinError}
      <p id="{id}-error" class="text-xs text-destructive">{pinError}</p>
    {/if}
  </div>
{/snippet}

{#snippet step(number: number, title: string, done: boolean)}
  <span
    class="flex size-6 shrink-0 items-center justify-center rounded-full border text-xs font-semibold tabular-nums {done
      ? 'border-primary bg-primary text-primary-foreground'
      : 'bg-background text-muted-foreground'}"
    aria-hidden="true"
  >
    {#if done}<CheckIcon class="size-3.5" />{:else}{number}{/if}
  </span>
  <p class="font-medium text-foreground">{title}{#if done}<span class="sr-only"> (done)</span>{/if}</p>
{/snippet}

<SettingsCard
  title={connected ? "Connected bot" : "Connect a bot"}
  description="Alerts go to one private Telegram chat that you choose."
>
  {#snippet actions()}
    {#if status?.platform_supported}
      <StatePill on={connected}>{connected ? "Connected" : "Not connected"}</StatePill>
    {/if}
  {/snippet}

  {#if loadError}
    <Notice tone="danger">{loadError}</Notice>
  {/if}

  {#if status}
    <ul class="mb-6 grid gap-2 text-sm text-muted-foreground md:grid-cols-3 md:gap-4">
      <li class="flex gap-2">
        <CheckIcon class="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400" aria-hidden="true" />
        Outbound only: the bot can never query or control OpenPiso.
      </li>
      <li class="flex gap-2">
        <CheckIcon class="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400" aria-hidden="true" />
        The token is stored protected and never shown again.
      </li>
      <li class="flex gap-2">
        <CheckIcon class="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400" aria-hidden="true" />
        Excluded from Backups: after a restore, you reconnect here.
      </li>
    </ul>

    {#if !status.platform_supported}
      <Notice>
        <p>
          Telegram requires a Windows Admin. This installation cannot protect the
          bot token and will never manufacture results.
        </p>
      </Notice>
    {:else if status.connected && status.destination}
      <div class="space-y-6 border-t pt-6">
        <dl class="grid gap-4 text-sm sm:grid-cols-2">
          <div class="min-w-0">
            <dt class="text-muted-foreground">Bot</dt>
            <dd class="mt-1 truncate font-medium">@{status.destination.bot_username}</dd>
          </div>
          <div class="min-w-0">
            <dt class="text-muted-foreground">Chat</dt>
            <dd class="mt-1 font-medium">
              {status.destination.chat_title}
              <span class="font-normal text-muted-foreground tabular-nums">(chat {status.destination.chat_id})</span>
            </dd>
          </div>
        </dl>
        {#if status.last_test}
          <p class="text-sm text-muted-foreground">
            Last successful test: {new Date(status.last_test.verified_at).toLocaleString()}
          </p>
        {/if}
        {#if status.delivery}
          {@const delivery = status.delivery}
          <div class="space-y-1 rounded-lg border bg-muted/40 px-4 py-3 text-sm" data-testid="delivery-health">
            <p class="font-medium">Delivery</p>
            <p class="text-muted-foreground">
              {delivery.last_sent_at
                ? `Last message delivered: ${new Date(delivery.last_sent_at).toLocaleString()}`
                : "No message delivered yet."}
            </p>
            <p class={delivery.expired_count > 0 ? "text-destructive" : "text-muted-foreground"}>
              {deliveryStateLine(delivery)}
            </p>
            {#if delivery.last_error}
              <p class="text-destructive">
                {`Last error (${new Date(delivery.last_error_at ?? "").toLocaleString()}): ${delivery.last_error}`}
              </p>
            {/if}
          </div>
        {/if}
      </div>
    {:else}
      <ol class="space-y-6 border-t pt-6 text-sm">
        <li class="grid grid-cols-[1.5rem_minmax(0,1fr)] gap-x-3 gap-y-1">
          {@render step(1, "Create a bot", Boolean(pendingBot))}
          <p class="col-start-2 text-muted-foreground">
            Message <strong class="text-foreground">@BotFather</strong> on Telegram and create a bot
            (<em>/newbot</em>), then copy the token it gives you.
          </p>
        </li>
        <li class="grid grid-cols-[1.5rem_minmax(0,1fr)] gap-x-3 gap-y-1">
          {@render step(2, "Message your bot", false)}
          <p class="col-start-2 text-muted-foreground">
            Send <em>/start</em> to your new bot from your own account. It must be a private chat sent within the
            last ten minutes.
          </p>
        </li>
        <li class="grid grid-cols-[1.5rem_minmax(0,1fr)] gap-x-3 gap-y-1">
          {@render step(3, "Paste the token", Boolean(pendingBot))}
          <div class="col-start-2 mt-1 space-y-3">
            {#if pendingBot}
              <p class="flex flex-wrap items-center gap-x-3 gap-y-1">
                <span>Token verified for <strong>@{pendingBot}</strong>.</span>
                <Button
                  variant="link"
                  class="h-auto px-0"
                  onclick={() => {
                    pendingBot = "";
                    detectedChats = [];
                  }}
                >
                  Use a different token
                </Button>
              </p>
            {:else}
              <form
                class="flex max-w-xl gap-2 max-sm:flex-col"
                onsubmit={(event) => {
                  event.preventDefault();
                  void verifyToken();
                }}
              >
                <label class="sr-only" for="telegram-token">Bot token</label>
                <Input
                  id="telegram-token"
                  type="password"
                  autocomplete="off"
                  placeholder="Paste your BotFather token"
                  class="flex-1"
                  bind:value={tokenInput}
                />
                <Button type="submit" variant="outline" disabled={busy || !tokenInput.trim()}>Verify token</Button>
              </form>
              {#if setupError}
                <Notice tone="danger">{setupError}</Notice>
              {/if}
            {/if}
          </div>
        </li>
        <li class="grid grid-cols-[1.5rem_minmax(0,1fr)] gap-x-3 gap-y-1" class:opacity-60={!pendingBot}>
          {@render step(4, "Find your chat and connect", false)}
          {#if pendingBot}
            <div class="col-start-2 mt-1 space-y-3">
              <Button variant="outline" onclick={detect} disabled={detectBusy}>
                {detectBusy ? "Checking for messages…" : "Look for my message"}
              </Button>
              {#if detectionError}
                <Notice tone="danger">{detectionError}</Notice>
              {/if}
              {#if detectedChats.length > 0}
                <div class="rounded-lg border bg-muted/40 px-4 py-3">
                  <p class="mb-2 font-medium">Private chats that messaged your bot:</p>
                  <ul class="space-y-1">
                    {#each detectedChats as chat (chat.chat_id)}
                      <li>
                        {chat.title} <span class="text-muted-foreground tabular-nums">({chat.chat_id})</span>
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
              <label class="flex min-h-11 w-fit items-center gap-2 text-sm md:min-h-0">
                <input type="checkbox" class="size-4 accent-primary" bind:checked={useManualChatId} />
                Enter chat ID manually
              </label>
              {#if useManualChatId}
                <Input
                  type="text"
                  inputmode="numeric"
                  placeholder="Numeric private chat ID"
                  aria-label="Chat ID"
                  class="max-w-56 tabular-nums"
                  bind:value={manualChatId}
                />
              {/if}
              <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
                <Button onclick={askConnect} disabled={!readyToConnect || busy}>Send test &amp; connect</Button>
                {#if !readyToConnect}
                  <span class="text-xs text-muted-foreground">
                    Look for your message first{useManualChatId ? " or type your chat ID" : ""}.
                  </span>
                {/if}
              </div>
              {#if setupError}
                <Notice tone="danger">{setupError}</Notice>
              {/if}
            </div>
          {:else}
            <p class="col-start-2 text-muted-foreground">Available once the token is verified.</p>
          {/if}
        </li>
      </ol>
    {/if}
  {:else if !loadError}
    <p class="text-sm text-muted-foreground">Loading Telegram state…</p>
  {/if}

  {#snippet footer()}
    {#if connected}
      <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">
        Deletes the token and this chat. Local PC Monitoring keeps running.
      </p>
      <Button
        variant="outline"
        class="text-destructive hover:bg-destructive/10 hover:text-destructive"
        onclick={askDisconnect}
        disabled={busy}
      >
        <UnplugIcon aria-hidden="true" />
        Disconnect
      </Button>
    {/if}
  {/snippet}
</SettingsCard>

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
    {@render pinField("telegram-connect-pin")}
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
    {@render pinField("telegram-disconnect-pin")}
  </ConfirmDialog>
{/if}
