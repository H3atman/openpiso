<script lang="ts" generics="T">
  import type { Snippet } from "svelte";
  import { IsMobile } from "$lib/hooks/is-mobile.svelte";

  interface Props {
    rows: readonly T[];
    row: Snippet<[T]>;
    card: Snippet<[T]>;
  }

  let { rows, row, card }: Props = $props();
  const isMobile = new IsMobile();
</script>

{#if isMobile.current}
  {#each rows as item}
    {@render card(item)}
  {/each}
{:else}
  {#each rows as item}
    {@render row(item)}
  {/each}
{/if}
