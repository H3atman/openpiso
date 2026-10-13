<script lang="ts">
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";
  import ChevronDownIcon from "@lucide/svelte/icons/chevron-down";
  import { Button } from "$lib/components/ui/button/index.js";
  import { formatAuditSummary } from "$lib/format";
  import type { NotificationItem } from "$lib/types";

  let {
    alerts,
    hasMore = false,
    onNavigate,
  }: {
    alerts: NotificationItem[];
    hasMore?: boolean;
    onNavigate: () => void;
  } = $props();

  let expanded = $state(false);
  const countLabel = $derived(`${alerts.length}${hasMore ? "+" : ""}`);
</script>

{#if alerts.length > 0}
  <section
    class="mb-6 rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-3"
    aria-labelledby="active-alert-heading"
  >
    <div class="flex flex-wrap items-center gap-3">
      <CircleAlertIcon class="size-5 shrink-0 text-amber-700 dark:text-amber-400" />
      <div class="min-w-0 flex-1">
        <h2 id="active-alert-heading" class="font-semibold">
          {countLabel} active Alert{alerts.length === 1 && !hasMore ? "" : "s"}
        </h2>
        <p class="text-sm text-muted-foreground">Review incidents that need your attention.</p>
      </div>
      <div class="flex shrink-0 max-md:w-full max-md:justify-end items-center gap-2">
        <Button variant="outline" size="sm" onclick={onNavigate}>Review</Button>
        <Button
          variant="outline"
          size="sm"
          aria-expanded={expanded}
          aria-controls="active-alert-preview"
          onclick={() => (expanded = !expanded)}
        >
          {expanded ? "Hide" : "Preview"}
          <ChevronDownIcon class={expanded ? "size-3 rotate-180 transition-transform" : "size-3 transition-transform"} />
        </Button>
      </div>
    </div>
    {#if expanded}
      <div
        id="active-alert-preview"
        class="mt-3 max-h-36 space-y-1 overflow-y-auto border-t border-amber-600/20 pt-3 text-sm"
      >
        {#each alerts as alert (alert.id)}
          <p class="truncate">
            {formatAuditSummary(alert)}
          </p>
        {/each}
        {#if hasMore}
          <p class="text-xs text-muted-foreground">More Alerts are available in Notifications.</p>
        {/if}
      </div>
    {/if}
  </section>
{/if}
