<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  /**
   * A setting that is switched on or off: label and description on the left,
   * the control on the right. Give the Switch `id={id}` and
   * `aria-describedby="{id}-description"` so its name and help come from here.
   * `status` is optional visible state text ("On"/"Off") next to the control.
   */
  let {
    id,
    label,
    description,
    status,
    children,
    class: className,
  }: {
    id: string;
    label: string;
    description?: string | Snippet;
    status?: string;
    children: Snippet;
    class?: string;
  } = $props();
</script>

<div class={cn("flex items-start justify-between gap-4", className)}>
  <div class="min-w-0 flex-1">
    <label class="text-sm font-medium" for={id}>{label}</label>
    {#if typeof description === "string"}
      <p id="{id}-description" class="mt-0.5 max-w-prose text-sm text-muted-foreground">{description}</p>
    {:else if description}
      <div id="{id}-description" class="mt-0.5 max-w-prose text-sm text-muted-foreground">{@render description()}</div>
    {/if}
  </div>
  <div class="flex min-h-11 shrink-0 items-center gap-3 pt-0.5 md:min-h-0">
    {#if status}
      <span class="text-sm font-medium tabular-nums text-muted-foreground">{status}</span>
    {/if}
    {@render children()}
  </div>
</div>
