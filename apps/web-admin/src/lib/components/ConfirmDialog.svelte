<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import { Button } from "./ui/button";

  /**
   * The owner-facing "are you sure?" dialog: a title, a short explanation, and
   * a Cancel/Confirm pair. Transactional dialogs may use a `fixed` overlay in
   * Web Admin — the `WidgetOverlay` rule in AGENTS.md is Client-only.
   *
   * Deliberately not used by the restore, theme, and account-import dialogs:
   * those carry scrolling bodies and a mobile Sheet variant, and bending this
   * one to cover them would cost more than the duplication saves.
   */
  interface Props {
    /** Question in the heading, e.g. "Sign out juan from PC 02?". */
    title: string;
    /** Body copy — what the confirm will actually do. */
    children: Snippet;
    confirmLabel: string;
    /** Confirm label while the request is in flight, e.g. "Signing out…". */
    busyLabel: string;
    /** Blocks confirm, cancel, backdrop, and Escape while the action runs. */
    busy?: boolean;
    /** Red confirm button for anything destructive or irreversible. */
    destructive?: boolean;
    cancelLabel?: string;
    onconfirm: () => void;
    oncancel: () => void;
  }

  let {
    title,
    children,
    confirmLabel,
    busyLabel,
    busy = false,
    destructive = false,
    cancelLabel = "Cancel",
    onconfirm,
    oncancel,
  }: Props = $props();

  // Plain-HTTP LAN origin is not a secure context, so `crypto.randomUUID` is
  // undefined there; `$props.id()` is the crypto-free per-instance id.
  const instanceId = $props.id();
  const titleId = `confirm-dialog-title-${instanceId}`;

  function cancel() {
    if (busy) return;
    oncancel();
  }

  let panel: HTMLDivElement;
  const FOCUSABLE =
    "button:not(:disabled), input:not(:disabled):not([type=hidden]), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex='-1'])";

  // Move focus into the dialog on open (first field, else the safe Cancel),
  // and hand it back to whatever opened the dialog when it closes.
  onMount(() => {
    const opener = document.activeElement;
    const field = panel.querySelector<HTMLElement>(
      "input:not(:disabled):not([type=hidden]), select:not(:disabled), textarea:not(:disabled)",
    );
    (field ?? panel.querySelector<HTMLElement>("[data-dialog-cancel]") ?? panel).focus();
    return () => {
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  });

  /** Keeps Tab and Shift+Tab cycling inside the panel while it is open. */
  function containTab(event: KeyboardEvent) {
    const focusable = [...panel.querySelectorAll<HTMLElement>(FOCUSABLE)];
    const first = focusable[0];
    const last = focusable.at(-1);
    if (!first || !last) return;
    const active = document.activeElement;
    const outside = !panel.contains(active);
    if (event.shiftKey && (active === first || active === panel || outside)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (active === last || outside)) {
      event.preventDefault();
      first.focus();
    }
  }
</script>

<svelte:window
  onkeydown={(event) => {
    if (event.defaultPrevented) return;
    if (event.key === "Tab") {
      containTab(event);
      return;
    }
    if (event.key !== "Escape" || busy) return;
    cancel();
    // Claim a handled Escape so stacked overlays do not close together.
    event.preventDefault();
  }}
/>

<!-- Backdrop click cancels; the panel itself swallows the click. Escape and
     Tab are bound on the window so they work wherever focus currently is. -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
  onclick={cancel}
>
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    bind:this={panel}
    class="max-h-[calc(100dvh-2rem)] w-full max-w-md overflow-y-auto rounded-lg border bg-card p-6 shadow-lg outline-none"
    role="dialog"
    aria-modal="true"
    aria-labelledby={titleId}
    tabindex="-1"
    onclick={(event) => event.stopPropagation()}
  >
    <h3 class="text-lg font-semibold max-md:break-words" id={titleId}>{title}</h3>
    <div class="mt-2 text-sm text-muted-foreground">
      {@render children()}
    </div>
    <div class="mt-5 flex justify-end gap-2">
      <Button data-dialog-cancel type="button" variant="outline" disabled={busy} onclick={cancel}>
        {cancelLabel}
      </Button>
      <Button
        type="button"
        variant={destructive ? "destructive" : "default"}
        disabled={busy}
        onclick={onconfirm}
      >
        {busy ? busyLabel : confirmLabel}
      </Button>
    </div>
  </div>
</div>
