<script lang="ts">
  import type { HTMLSelectAttributes } from "svelte/elements";
  import ChevronDownIcon from "@lucide/svelte/icons/chevron-down";
  import { cn } from "$lib/utils";

  /**
   * A native <select> styled to match the Input primitive (height, border,
   * focus ring, dark background). Native on purpose: phones get their own
   * picker and keyboard behaviour stays the browser's.
   */
  let { value = $bindable(), class: className, children, ...restProps }: HTMLSelectAttributes = $props();
</script>

<div class={cn("relative min-w-0", className)}>
  <select
    bind:value
    class="h-8 min-h-11 w-full min-w-0 appearance-none rounded-lg border border-input bg-transparent py-1 pr-8 pl-2.5 text-base transition-colors outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 md:min-h-0 md:text-sm dark:bg-input/30 [&_option]:bg-popover [&_option]:text-popover-foreground"
    {...restProps}
  >
    {@render children?.()}
  </select>
  <ChevronDownIcon
    class="pointer-events-none absolute top-1/2 right-2.5 size-4 -translate-y-1/2 text-muted-foreground"
    aria-hidden="true"
  />
</div>
