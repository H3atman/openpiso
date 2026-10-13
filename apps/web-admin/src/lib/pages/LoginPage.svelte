<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import * as api from "../api";
  import { ThrottledError } from "../api";
  import BrandLogo from "../components/brand-logo.svelte";

  let { onLogin }: { onLogin: () => void } = $props();

  let pin = $state("");
  let confirmPin = $state("");
  let error = $state("");
  let busy = $state(false);
  let isSetup = $state(false);
  let configuring = $state(false);
  // Remaining lockout seconds while rate-limited; 0 when not throttled.
  let lockoutSeconds = $state(0);
  let lockoutTimer: ReturnType<typeof setInterval> | undefined;

  onMount(async () => {
    try {
      const session = await api.getSession();
      if (session.authenticated) {
        onLogin();
        return;
      }
      isSetup = session.configured;
      // First run (no PIN configured) → make "set up a PIN" the primary action,
      // so the obvious button creates the PIN instead of attempting a login.
      configuring = !session.configured;
    } catch {
      isSetup = false;
    }
  });

  onDestroy(() => clearInterval(lockoutTimer));

  function startLockout(seconds: number) {
    lockoutSeconds = seconds;
    clearInterval(lockoutTimer);
    lockoutTimer = setInterval(() => {
      lockoutSeconds -= 1;
      if (lockoutSeconds <= 0) {
        clearInterval(lockoutTimer);
        lockoutSeconds = 0;
        error = "";
      }
    }, 1000);
  }

  async function handleSubmit(e: Event) {
    e.preventDefault();
    if (lockoutSeconds > 0) return;
    error = "";
    busy = true;
    try {
      if (configuring) {
        if (pin !== confirmPin) {
          error = "PINs do not match.";
          busy = false;
          return;
        }
        await api.setupOwner(pin);
      } else {
        await api.ownerLogin(pin);
      }
      onLogin();
    } catch (caught) {
      if (caught instanceof ThrottledError) {
        startLockout(caught.retryAfterSeconds);
      }
      error = caught instanceof Error ? caught.message : String(caught);
    } finally {
      busy = false;
    }
  }
</script>

<div class="flex min-h-screen items-center justify-center">
  <div class="w-full max-w-sm rounded-lg border bg-card p-8 shadow-sm">
    <div class="mb-6 text-center">
      <h1 class="flex justify-center">
        <BrandLogo variant="horizontal" class="h-10 w-auto" />
        <span class="sr-only">OpenPiso</span>
      </h1>
      <p class="mt-1 text-sm text-muted-foreground">Owner Admin Panel</p>
    </div>

    {#if configuring && !isSetup}
      <div class="mb-6 rounded-md bg-muted p-4 text-sm">
        First-time setup: create your Web Admin PIN to get started.<br />
        Also used as the Owner PIN for Client settings. Change separately in Security.
      </div>
    {/if}

    <form onsubmit={handleSubmit}>
      <div class="space-y-4">
        <div>
          <label for="pin" class="block text-sm font-medium">
            {configuring ? "New PIN" : "PIN"}
          </label>
          <input
            id="pin"
            type="password"
            class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            placeholder={configuring ? "Enter new PIN" : "Enter your PIN"}
            bind:value={pin}
            required
            autocomplete={configuring ? "new-password" : "current-password"}
          />
        </div>

        {#if configuring}
          <div>
            <label for="confirm-pin" class="block text-sm font-medium">Confirm PIN</label>
            <input
              id="confirm-pin"
              type="password"
              class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
              placeholder="Confirm PIN"
              bind:value={confirmPin}
              required
              autocomplete="new-password"
            />
          </div>
        {/if}
      </div>

      {#if lockoutSeconds > 0}
        <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
          Too many attempts. Try again in {lockoutSeconds} second{lockoutSeconds === 1
            ? ""
            : "s"}.
        </div>
      {:else if error}
        <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
          {error}
        </div>
      {/if}

      <button
        type="submit"
        class="mt-6 w-full rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
        disabled={busy || !pin || lockoutSeconds > 0}
      >
        {busy ? "Please wait…" : configuring ? "Set PIN" : "Sign in"}
      </button>
    </form>

    {#if isSetup && !configuring}
      <p class="mt-4 text-center text-xs text-muted-foreground">
        Forgot your PIN? Contact support or restart the server.
      </p>
    {/if}

    <!-- Only offer first-run setup on a never-configured box. On a configured
         box the setup route returns Conflict, so the button was a misleading
         dead-end that also advertised a resettable PIN to a bystander. -->
    {#if !configuring && !isSetup}
      <div class="mt-2 text-center">
        <button
          type="button"
          class="text-xs text-muted-foreground underline hover:text-foreground"
          onclick={() => {
            configuring = true;
            error = "";
          }}
        >
          Set up a new PIN
        </button>
      </div>
    {/if}
  </div>
</div>
