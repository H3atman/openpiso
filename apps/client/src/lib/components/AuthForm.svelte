<script lang="ts">
  // Customer auth form: toggles between Log in and Sign up. The
  // server owns the username/password rules and the active-session
  // binding; the form just collects input and surfaces the server's
  // error. Successful auth replaces the form with the AccountPanel.

  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";

  interface Props {
    busy: boolean;
    error: string;
    disabled?: boolean;
    disabledReason?: string;
    onAuthenticate: (
      mode: "login" | "signup",
      username: string,
      password: string,
    ) => Promise<void>;
    onSwitchMode: (mode: "login" | "signup") => void;
  }

  let {
    busy,
    error,
    disabled = false,
    disabledReason = "",
    onAuthenticate,
    onSwitchMode,
  }: Props = $props();

  let mode = $state<"login" | "signup">("login");
  let username = $state("");
  let password = $state("");

  async function handleSubmit(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    // The pre-refactor app left the password field intact after a submit
    // (a failed login keeps the typed value for an immediate retry), so the
    // form does not clear it here.
    await onAuthenticate(mode, username, password);
  }
</script>

<!-- Shared shop PCs: never offer the previous customer's credentials.
     autocomplete="off" plus generalAutofillEnabled=false in tauri.conf.json
     (WebView2 "Suggestions" ignores the attribute on its own). -->
<form class="flex flex-col gap-4" onsubmit={handleSubmit} autocomplete="off">
  <div class="flex flex-col gap-1.5">
    <label for="auth-username" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
      Username
    </label>
    <Input
      id="auth-username"
      bind:value={username}
      minlength={3}
      maxlength={32}
      required
      disabled={disabled || busy}
      autocomplete="off"
      spellcheck={false}
    />
  </div>
  <div class="flex flex-col gap-1.5">
    <label for="auth-password" class="text-muted-foreground text-xs font-bold tracking-wider uppercase">
      Password
    </label>
    <Input
      id="auth-password"
      bind:value={password}
      type="password"
      minlength={4}
      maxlength={128}
      required
      disabled={disabled || busy}
      autocomplete="off"
    />
  </div>
  {#if disabledReason}
    <p class="rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-sm text-amber-300" role="status">
      {disabledReason}
    </p>
  {/if}
  {#if error}
    <p class="text-destructive text-sm" role="alert" aria-live="assertive">
      {error}
    </p>
  {/if}

  <div class="flex flex-wrap gap-2">
    <Button type="submit" disabled={busy || disabled}>
      {mode === "login" ? "Log in" : "Sign up"}
    </Button>
    <Button
      type="button"
      variant="outline"
      disabled={busy || disabled}
      onclick={() => {
        mode = mode === "login" ? "signup" : "login";
        onSwitchMode(mode);
      }}
    >
      {mode === "login" ? "Create account" : "Use existing account"}
    </Button>
  </div>
</form>
