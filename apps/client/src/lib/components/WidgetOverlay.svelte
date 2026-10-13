<script lang="ts">
  import { tick } from "svelte";

  interface Props {
    title: string;
    open: boolean;
    children?: import("svelte").Snippet;
  }

  let { title, open, children }: Props = $props();
  let dialog = $state<HTMLDivElement>();

  $effect(() => {
    if (!open) return;
    const previousFocus = document.activeElement;
    void tick().then(() => {
      dialog
        ?.querySelector<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
        )
        ?.focus();
    });
    return () => {
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) {
        previousFocus.focus();
      }
    };
  });

  function trapFocus(event: KeyboardEvent): void {
    if (event.key !== "Tab" || !dialog) return;
    const focusable = Array.from(
      dialog.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    );
    if (focusable.length === 0) {
      event.preventDefault();
      dialog.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable.at(-1);
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }
</script>

{#if open}
  <div
    bind:this={dialog}
    role="dialog"
    aria-modal="true"
    aria-label={title}
    tabindex="-1"
    class="fixed inset-0 z-20 grid place-items-center-safe overflow-y-auto bg-black/70 p-4"
    onkeydown={trapFocus}
  >
    {@render children?.()}
  </div>
{/if}
