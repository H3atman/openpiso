<script lang="ts">
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
</script>

<div class="mt-3 flex flex-wrap items-center justify-between gap-3 border-t pt-3">
  <p class="text-xs text-muted-foreground">
    Showing {start}–{end} of {page.total} {pageName}s
  </p>
  <div class="flex items-center gap-2">
    <Button
      variant="outline"
      size="sm"
      disabled={busy || page.offset === 0}
      onclick={onPrevious}
    >
      Previous {pageName} page
    </Button>
    <Button
      variant="outline"
      size="sm"
      disabled={busy || !page.has_more}
      onclick={onNext}
    >
      Next {pageName} page
    </Button>
  </div>
</div>
