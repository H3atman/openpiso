<!-- Test-only wrapper: AppSidebar needs the provider supplied by App.svelte. -->
<script lang="ts">
  import * as Sidebar from "./ui/sidebar/index.js";
  import AppSidebar, { type NavItem } from "./app-sidebar.svelte";

  let {
    items,
    activeId,
    onSelect,
    onLogout = () => {},
  }: {
    items: NavItem[];
    activeId: string;
    onSelect: (event: MouseEvent, id: string) => void;
    onLogout?: () => void;
  } = $props();
</script>

<Sidebar.Provider>
  <Sidebar.Trigger />
  <AppSidebar
    {items}
    {activeId}
    {onSelect}
    {onLogout}
    user={{ name: "Owner", email: "Pisonet admin" }}
  />
</Sidebar.Provider>
