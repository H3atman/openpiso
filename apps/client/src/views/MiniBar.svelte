<!--
  MiniBar — the 100×30 Client view (ADR-0029 §4.1 / §8.2).

  Mounted by App.svelte's state machine when the customer has chosen
  the Mini Bar as their persisted `view_mode`. The whole bar is a drag
  region (`data-tauri-drag-region`) per the Tauri v2 custom-drag
  pattern (the corresponding Rust capability is
  `core:window:allow-start-dragging`, documented in
  `apps/client/src-tauri/capabilities/default.json`). Default opacity
  is `.3` so the bar is unobtrusive while the customer is using the
  PC; the bar becomes fully opaque on hover so the remaining time is
  always readable. A double-click (or Enter/Space) expands back to the
  Widget; a single press starts the window drag.

  The expand button does not change `view_mode` back to `"widget"`
  until the customer explicitly chose Widget last time — Mini is the
  only persisted preference that this view changes. (App.svelte
  performs the `set_client_config` write that the plan §7.2 calls for.)
-->
<script lang="ts">
  import { formatDuration } from "$lib/format";
  import type { TimerState, ViewMode } from "$lib/types";

  interface Props {
    timer: TimerState | null;
    onExpand: () => void;
  }

  let { timer, onExpand }: Props = $props();

  const remaining = $derived(timer?.remaining_seconds ?? 0);
</script>

<div
  class="group flex h-[30px] w-[100px] cursor-grab items-center justify-center rounded-md border border-border bg-card font-mono text-sm font-bold tabular-nums opacity-30 transition-opacity hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
  data-tauri-drag-region
  role="button"
  tabindex="0"
  aria-label="OpenPiso mini bar"
  title="Drag to move, double-click to expand"
  onmousedown={(event) => {
    // Expand must not use `click`: Tauri's drag-region script starts a
    // native window drag on mousedown, and the OS move loop swallows the
    // matching mouseup, so a `click` never fires on this element (bug #9).
    // The second press of a double-click (detail 2) is still delivered, and
    // this target-phase handler runs before the document-level drag script —
    // stopPropagation keeps that press from toggling maximize or dragging.
    if (event.button === 0 && event.detail === 2) {
      event.preventDefault();
      event.stopPropagation();
      onExpand();
    }
  }}
  onkeydown={(event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onExpand();
    }
  }}
>
  {formatDuration(remaining)}
</div>
