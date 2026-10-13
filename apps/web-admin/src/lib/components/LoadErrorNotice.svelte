<script lang="ts">
  // A failed read, stated where the missing content would have been, with a
  // retry in place. Load failures stay inline; action outcomes stay toasts.
  let {
    message,
    detail,
    busy = false,
    onRetry,
  }: {
    /** What could not be read, e.g. "Diagnostics Mode could not be read". */
    message: string;
    /** The server's or client's own words. */
    detail: string;
    busy?: boolean;
    onRetry: () => void;
  } = $props();
</script>

<div class="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
  <p class="max-md:break-words">{message}: {detail}</p>
  <button
    type="button"
    class="mt-2 rounded-md border px-3 py-1.5 text-sm font-medium hover:bg-destructive/10"
    disabled={busy}
    onclick={onRetry}
  >
    Retry
  </button>
</div>
