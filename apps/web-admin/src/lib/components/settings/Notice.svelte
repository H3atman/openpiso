<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLAttributes } from "svelte/elements";
  import CircleAlertIcon from "@lucide/svelte/icons/circle-alert";
  import InfoIcon from "@lucide/svelte/icons/info";
  import TriangleAlertIcon from "@lucide/svelte/icons/triangle-alert";
  import { cn } from "$lib/utils";

  /**
   * An inline Settings message: a load failure (`danger`), a standing
   * advisory (`warning`), or a neutral fact about this installation (`info`).
   * Action outcomes are toasts, never a Notice. Pass `role` when the message
   * appears or changes while the owner is on the page.
   */
  let {
    tone = "info",
    title,
    children,
    class: className,
    ...restProps
  }: HTMLAttributes<HTMLDivElement> & {
    tone?: "info" | "warning" | "danger";
    title?: string;
    children: Snippet;
  } = $props();

  const TONES = {
    info: { box: "border-border bg-muted/50 text-foreground", icon: InfoIcon, iconClass: "text-muted-foreground" },
    warning: {
      box: "border-amber-200 bg-amber-50 text-amber-900 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-100",
      icon: TriangleAlertIcon,
      iconClass: "",
    },
    danger: { box: "border-destructive/30 bg-destructive/10 text-destructive", icon: CircleAlertIcon, iconClass: "" },
  } as const;
  const toneStyle = $derived(TONES[tone]);
  const Icon = $derived(toneStyle.icon);
</script>

<div class={cn("flex gap-3 rounded-lg border px-4 py-3 text-sm", toneStyle.box, className)} {...restProps}>
  <Icon class={cn("mt-0.5 size-4 shrink-0", toneStyle.iconClass)} aria-hidden="true" />
  <div class="min-w-0 flex-1 space-y-1 max-md:break-words">
    {#if title}
      <p class="font-semibold">{title}</p>
    {/if}
    {@render children()}
  </div>
</div>
