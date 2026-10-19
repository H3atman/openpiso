<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import * as api from "../api";
  import { ApiError, ThrottledError } from "../api";
  import BrandLogo from "../components/brand-logo.svelte";
  import {
    buttonLabel,
    canSubmit,
    initialLoginState,
    reduceLogin,
    showFirstRunBanner,
    showForgotHint,
    showSetupLink,
    type LoginEvent,
    type LoginState,
  } from "../machines/login";

  let { onLogin }: { onLogin: () => void } = $props();

  let current = $state.raw<LoginState>(initialLoginState);
  let pin = $state("");
  let confirmPin = $state("");
  let configuring = $derived("mode" in current && current.mode === "setup");
  let lockoutTimer: ReturnType<typeof setInterval> | undefined;
  let destroyed = false;

  function dispatch(event: LoginEvent) {
    if (destroyed) return;
    const previous = current;
    const next = reduceLogin(previous, event);
    if (next === previous) return;
    current = next;
    if (previous.phase === "locked-out" && next.phase !== "locked-out") {
      clearInterval(lockoutTimer);
    }
    if (previous.phase !== "locked-out" && next.phase === "locked-out") {
      lockoutTimer = setInterval(() => dispatch({ type: "lockout-tick" }), 1000);
    }
    if (next.phase === "authenticated") onLogin();
  }

  onMount(async () => {
    try {
      const session = await api.getSession();
      dispatch({
        type: "session-loaded",
        authenticated: session.authenticated,
        configured: session.configured,
      });
    } catch {
      dispatch({ type: "session-check-failed" });
    }
  });

  onDestroy(() => {
    destroyed = true;
    clearInterval(lockoutTimer);
  });

  async function handleSubmit(e: Event) {
    e.preventDefault();
    if (!canSubmit(current, !!pin)) return;
    dispatch({ type: "submit", pinsMatch: pin === confirmPin });
    if (current.phase !== "submitting") return;
    const mode = current.mode;
    try {
      if (mode === "setup") {
        await api.setupOwner(pin);
      } else {
        await api.ownerLogin(pin);
      }
      dispatch({ type: "submit-succeeded" });
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught);
      if (caught instanceof ThrottledError) {
        dispatch({ type: "throttled", retryAfterSeconds: caught.retryAfterSeconds });
      } else if (mode === "setup" && caught instanceof ApiError && caught.status === 409) {
        dispatch({ type: "setup-conflict", message });
      } else {
        dispatch({ type: "submit-failed", message });
      }
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

    {#if current.phase === "checking-session"}
      <p class="text-center text-sm text-muted-foreground">Checking sign-in…</p>
    {:else if current.phase !== "authenticated"}
      {#if showFirstRunBanner(current)}
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

        {#if current.phase === "locked-out"}
          <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            Too many attempts. Try again in {current.secondsLeft} second{current.secondsLeft === 1
              ? ""
              : "s"}.
          </div>
        {:else if current.phase === "ready" && current.error}
          <div class="mt-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
            {current.error}
          </div>
        {/if}

        <button
          type="submit"
          class="mt-6 w-full rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          disabled={!canSubmit(current, !!pin)}
        >
          {buttonLabel(current)}
        </button>
      </form>

      {#if showForgotHint(current)}
        <p class="mt-4 text-center text-xs text-muted-foreground">
          Forgot your PIN? Contact support or restart the server.
        </p>
      {/if}

      <!-- Only offer first-run setup on a never-configured box. On a configured
           box the setup route returns Conflict, so the button was a misleading
           dead-end that also advertised a resettable PIN to a bystander. -->
      {#if showSetupLink(current)}
        <div class="mt-2 text-center">
          <button
            type="button"
            class="text-xs text-muted-foreground underline hover:text-foreground"
            onclick={() => dispatch({ type: "choose-setup" })}
          >
            Set up a new PIN
          </button>
        </div>
      {/if}
    {/if}
  </div>
</div>
