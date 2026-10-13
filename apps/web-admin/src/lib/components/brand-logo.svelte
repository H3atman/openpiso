<script lang="ts">
	import symbolLight from "$lib/assets/logo/openpiso-symbol-color.svg";
	import symbolDark from "$lib/assets/logo/openpiso-symbol-reversed.svg";
	import horizontalLight from "$lib/assets/logo/openpiso-horizontal-color.svg";
	import horizontalDark from "$lib/assets/logo/openpiso-horizontal-reversed.svg";

	/** Decorative OpenPiso mark; the caller supplies the accessible name. Swaps with the `dark` class. */
	let { variant, class: className = "" }: { variant: "symbol" | "horizontal"; class?: string } =
		$props();

	const sources = {
		symbol: { light: symbolLight, dark: symbolDark },
		horizontal: { light: horizontalLight, dark: horizontalDark },
	};
	const src = $derived(sources[variant]);
</script>

<img src={src.light} class="{className} dark:hidden" alt="" aria-hidden="true" />
<img src={src.dark} class="{className} hidden dark:block" alt="" aria-hidden="true" />
