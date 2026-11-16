<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  /**
   * One Settings card: a titled header (optional description and header-side
   * controls), a padded body, and an optional footer bar holding the card's
   * save action. Settings pages use one card per save boundary, so the button
   * in a card's footer always saves exactly what the card shows.
   */
  let {
    title,
    description,
    actions,
    footer,
    children,
    flush = false,
    class: className,
  }: {
    title: string;
    /** One or two sentences under the title; a snippet when it needs markup. */
    description?: string | Snippet;
    /** Header-side status or controls (a toggle, a pill, an Add button). */
    actions?: Snippet;
    /** Footer bar content: an optional hint, then the card's buttons. */
    footer?: Snippet;
    children: Snippet;
    /** Drop body padding for edge-to-edge lists. */
    flush?: boolean;
    class?: string;
  } = $props();

  const uid = $props.id();
  const headingId = `settings-card-${uid}`;
</script>

<section class={cn("min-w-0 rounded-lg border bg-card", className)} aria-labelledby={headingId}>
  <header class="flex flex-wrap items-start justify-between gap-x-6 gap-y-3 border-b px-4 py-4 md:px-6">
    <div class="min-w-0 flex-1 basis-60">
      <h3 id={headingId} class="leading-snug font-semibold">{title}</h3>
      {#if typeof description === "string"}
        <p class="mt-1 max-w-prose text-sm text-muted-foreground">{description}</p>
      {:else if description}
        <div class="mt-1 max-w-prose text-sm text-muted-foreground">{@render description()}</div>
      {/if}
    </div>
    {#if actions}
      <div class="flex flex-wrap items-center gap-3">{@render actions()}</div>
    {/if}
  </header>

  <div class={flush ? "" : "p-4 md:p-6"}>
    {@render children()}
  </div>

  {#if footer}
    <!-- Buttons fill the row on phones so the save action is a full-width target. -->
    <footer
      class="flex flex-wrap items-center justify-end gap-x-4 gap-y-3 rounded-b-lg border-t bg-muted/40 px-4 py-3 empty:hidden md:px-6 max-md:*:data-[slot=button]:flex-1"
    >
      {@render footer()}
    </footer>
  {/if}
</section>

<style>
  @media (width < 768px) {
    /* Beat app.css's unlayered button minimum within Settings only. The
       switch keeps its normal track height and its invisible tap area. */
    section :global([data-slot="switch"]) {
      min-height: 0;
    }

    /* Both normal and small tracks retain a centred 44px tap area. */
    section :global([data-slot="switch"])::after {
      top: 50%;
      bottom: auto;
      height: 44px;
      transform: translateY(-50%);
    }
  }
</style>
