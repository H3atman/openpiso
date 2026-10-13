<script lang="ts">
	import * as Sidebar from "$lib/components/ui/sidebar/index.js";
	import * as Collapsible from "$lib/components/ui/collapsible/index.js";
	import ChevronRightIcon from "@lucide/svelte/icons/chevron-right";
	import type { NavItem } from "./app-sidebar.svelte";

	let {
		items,
		activeId,
		onSelect,
	}: {
		items: NavItem[];
		activeId: string;
		onSelect: (event: MouseEvent, id: string) => void;
	} = $props();

	const sidebar = Sidebar.useSidebar();

	function select(event: MouseEvent, id: string) {
		onSelect(event, id);
		if (
			event.button === 0 &&
			!event.ctrlKey &&
			!event.metaKey &&
			!event.shiftKey &&
			!event.altKey
		) {
			sidebar.setOpenMobile(false);
		}
	}

	// Group items by their `group` label while preserving first-seen order.
	// Ungrouped items (no label) render together in the leading section.
	const sections = $derived.by(() => {
		const map = new Map<string, NavItem[]>();
		for (const item of items) {
			const key = item.group ?? "";
			(map.get(key) ?? map.set(key, []).get(key)!).push(item);
		}
		return [...map.entries()].map(([label, groupItems]) => ({
			label,
			items: groupItems,
		}));
	});

	// A branch counts as active when it or one of its children is selected.
	function isBranchActive(item: NavItem): boolean {
		return (
			item.id === activeId ||
			(item.children?.some((child) => child.id === activeId) ?? false)
		);
	}

	// Explicit toggles win; otherwise a branch follows its active state.
	let openItems = $state<Record<string, boolean>>({});
</script>

{#each sections as section (section.label)}
	<Sidebar.Group>
		{#if section.label}
			<Sidebar.GroupLabel>{section.label}</Sidebar.GroupLabel>
		{/if}
		<Sidebar.Menu>
			{#each section.items as item (item.id)}
				<Sidebar.MenuItem>
					{#if item.children?.length}
						<Collapsible.Root
							class="group/collapsible"
							open={openItems[item.id] ?? isBranchActive(item)}
							onOpenChange={(open) => (openItems[item.id] = open)}
						>
							<Collapsible.Trigger>
								{#snippet child({ props })}
									<Sidebar.MenuButton
										{...props}
										isActive={isBranchActive(item)}
										tooltipContent={item.title}
									>
										<item.icon />
										<span>{item.title}</span>
										<ChevronRightIcon
											class="ms-auto transition-transform duration-200 group-data-[state=open]/collapsible:rotate-90"
										/>
									</Sidebar.MenuButton>
								{/snippet}
							</Collapsible.Trigger>
							<Collapsible.Content>
								<Sidebar.MenuSub>
									{#each item.children as sub (sub.id)}
										<Sidebar.MenuSubItem>
											<!-- Full-width, full-height hit area to match the top-level buttons. -->
											<Sidebar.MenuSubButton
												class="h-8 w-full"
												isActive={sub.id === activeId}
											>
												{#snippet child({ props })}
													<a
												{...props}
												href={sub.id}
												onclick={(event) => select(event, sub.id)}
											>
												<span>{sub.title}</span>
											</a>
												{/snippet}
											</Sidebar.MenuSubButton>
										</Sidebar.MenuSubItem>
									{/each}
								</Sidebar.MenuSub>
							</Collapsible.Content>
						</Collapsible.Root>
					{:else}
						<Sidebar.MenuButton
							isActive={isBranchActive(item)}
							tooltipContent={item.title}
						>
							{#snippet child({ props })}
								<a
									{...props}
									href={item.id}
									onclick={(event) => select(event, item.id)}
								>
									<item.icon />
									<span>{item.title}</span>
									{#if item.badge !== undefined}
										<Sidebar.MenuBadge class="group-data-[collapsible=icon]:!flex">{item.badge}</Sidebar.MenuBadge>
									{/if}
								</a>
							{/snippet}
						</Sidebar.MenuButton>
					{/if}
				</Sidebar.MenuItem>
			{/each}
		</Sidebar.Menu>
	</Sidebar.Group>
{/each}
