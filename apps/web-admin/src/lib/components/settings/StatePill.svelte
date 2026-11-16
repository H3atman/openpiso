<script lang="ts">
  import type { Snippet } from "svelte";
  import { Badge } from "$lib/components/ui/badge";

  /** An on/off status pill in the Dashboard's style: the label carries the
   *  meaning, the dot only reinforces it. */
  let { on, children }: { on: boolean; children: Snippet } = $props();
</script>

<Badge variant="outline" class="gap-1.5 text-foreground">
  <span class="size-1.5 rounded-full {on ? 'bg-emerald-500' : 'bg-muted-foreground/60'}" aria-hidden="true"></span>
  {@render children()}
</Badge>
