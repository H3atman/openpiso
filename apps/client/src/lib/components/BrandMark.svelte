<script lang="ts">
	import symbolLight from "$lib/assets/logo/openpiso-symbol-color.svg";
	import symbolDark from "$lib/assets/logo/openpiso-symbol-reversed.svg";

	/** Decorative OpenPiso symbol; swaps with the `dark` class set by the client theme. */
	let { class: className = "" }: { class?: string } = $props();
</script>

<img src={symbolLight} class="{className} pointer-events-none shrink-0 dark:hidden" alt="" aria-hidden="true" />
<img src={symbolDark} class="{className} pointer-events-none hidden shrink-0 dark:block" alt="" aria-hidden="true" />
