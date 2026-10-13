<!-- Test-only wrapper: the update row shares App.svelte's sidebar provider. -->
<script lang="ts">
  import * as Sidebar from "./ui/sidebar/index.js";
  import AppUpdate from "./app-update.svelte";
</script>

<Sidebar.Provider>
  <AppUpdate />
</Sidebar.Provider>
