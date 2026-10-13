<script lang="ts">
  // Shared status banner used by the Lock for both the error banner
  // (kind = "error") and the low-time warning banner (kind = "warning").
  // The classes mirror the destructive / amber tones the shadcn-svelte
  // theme already ships with, so the Lock picks up the shared theme
  // without per-instance color overrides. `role` is forwarded so the
  // call site can choose `alert` vs `status`.

  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";

  type Kind = "error" | "warning";

  interface Props {
    kind: Kind;
    role?: "alert" | "status";
    "aria-live"?: "assertive" | "polite";
    class?: string;
    children: Snippet;
  }

  let {
    kind,
    role = kind === "error" ? "alert" : "status",
    "aria-live": ariaLive = kind === "error" ? "assertive" : "polite",
    class: className,
    children,
  }: Props = $props();

  const tones: Record<Kind, string> = {
    error:
      "border-destructive/40 bg-destructive/10 text-destructive",
    warning:
      "border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300",
  };
</script>

<div
  class={cn(
    "rounded-md border p-3 text-sm",
    tones[kind],
    className,
  )}
  role={role}
  aria-live={ariaLive}
>
  {@render children()}
</div>
