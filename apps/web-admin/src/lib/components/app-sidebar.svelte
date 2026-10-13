<script lang="ts" module>
	import type { Component } from "svelte";

	export interface NavChild {
		id: string;
		title: string;
	}

	export interface NavItem {
		id: string;
		title: string;
		// Optional section label; items without one render in the top ungrouped section.
		group?: string;
		// Sub-navigation rendered as a collapsible submenu under this item.
		children?: NavChild[];
		badge?: string | number;
		// `Component` once @lucide/svelte ships precise icon types.
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		icon: Component<any> | any;
	}
</script>

<script lang="ts">
	import NavMain from "./nav-main.svelte";
	import NavUser from "./nav-user.svelte";
	import AppUpdate from "./app-update.svelte";
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import BrandLogo from "./brand-logo.svelte";
	import type { ComponentProps } from "svelte";

	let {
		ref = $bindable(null),
		items,
		activeId,
		onSelect,
		onLogout,
		user,
		collapsible = "icon",
		...restProps
	}: ComponentProps<typeof Sidebar.Root> & {
		items: NavItem[];
		activeId: string;
		onSelect: (event: MouseEvent, id: string) => void;
		onLogout: () => void;
		user: { name: string; email: string };
	} = $props();
</script>

<AppUpdate>
	{#snippet children(updateRow)}
		<Sidebar.Root bind:ref {collapsible} {...restProps}>
			<Sidebar.Header>
				<Sidebar.Menu>
					<Sidebar.MenuItem>
						<Sidebar.MenuButton size="lg" class="cursor-default">
							{#snippet child({ props })}
								<div {...props}>
									<BrandLogo variant="symbol" class="size-8 shrink-0" />
									<div class="grid flex-1 text-start text-sm leading-tight">
										<span class="truncate font-semibold">OpenPiso</span>
										<span class="truncate text-xs">Admin Panel</span>
									</div>
								</div>
							{/snippet}
						</Sidebar.MenuButton>
					</Sidebar.MenuItem>
				</Sidebar.Menu>
			</Sidebar.Header>
			<Sidebar.Content>
				<NavMain {items} {activeId} {onSelect} />
			</Sidebar.Content>
			<Sidebar.Footer>
				{@render updateRow()}
				<NavUser {user} {onLogout} />
			</Sidebar.Footer>
			<Sidebar.Rail />
		</Sidebar.Root>
	{/snippet}
</AppUpdate>
