<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  /**
   * A labelled Settings control: visible label, the control (children), and
   * an optional hint underneath. Give the control `id={id}` and
   * `aria-describedby="{id}-hint"` so the hint is read with it.
   */
  let {
    id,
    label,
    hint,
    children,
    class: className,
  }: {
    /** The control's id; the label points at it. */
    id: string;
    label: string;
    hint?: string;
    children: Snippet;
    class?: string;
  } = $props();
</script>

<div class={cn("grid content-start gap-1.5", className)}>
  <label class="text-sm font-medium" for={id}>{label}</label>
  {@render children()}
  {#if hint}
    <p id="{id}-hint" class="text-xs text-muted-foreground">{hint}</p>
  {/if}
</div>
