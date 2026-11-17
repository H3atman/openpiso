<script lang="ts">
  import ChevronLeftIcon from "@lucide/svelte/icons/chevron-left";
  import ChevronRightIcon from "@lucide/svelte/icons/chevron-right";
  import { Button } from "./ui/button";
  import type { AttendancePage } from "../types";

  let {
    page,
    pageName,
    busy,
    onPrevious,
    onNext,
  }: {
    page: AttendancePage<unknown>;
    pageName: string;
    busy: boolean;
    onPrevious: () => void;
    onNext: () => void;
  } = $props();

  let start = $derived(page.total === 0 ? 0 : page.offset + 1);
  let end = $derived(Math.min(page.total, page.offset + page.rows.length));
  // Buttons only when there is more than one page to move between.
  let paged = $derived(page.offset > 0 || page.has_more);
</script>

<!-- An empty page has its own empty state; the footer would only repeat it. -->
{#if page.total > 0}
  <div class="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 border-t px-4 py-2.5">
    <p class="text-xs text-muted-foreground tabular-nums">
      Showing {start}–{end} of {page.total} {page.total === 1 ? pageName : `${pageName}s`}
    </p>
    {#if paged}
      <!-- Visible text stays short for phones; the accessible name starts with
           it and names the table, so the two pagers stay distinguishable. -->
      <div class="flex gap-2 max-md:w-full">
        <Button
          variant="outline"
          size="sm"
          class="max-md:flex-1"
          aria-label="Previous {pageName} page"
          disabled={busy || page.offset === 0}
          onclick={onPrevious}
        >
          <ChevronLeftIcon aria-hidden="true" />
          Previous
        </Button>
        <Button
          variant="outline"
          size="sm"
          class="max-md:flex-1"
          aria-label="Next {pageName} page"
          disabled={busy || !page.has_more}
          onclick={onNext}
        >
          Next
          <ChevronRightIcon aria-hidden="true" />
        </Button>
      </div>
    {/if}
  </div>
{/if}
