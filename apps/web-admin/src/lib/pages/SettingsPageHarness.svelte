<!-- Test-only wrapper: SettingsPage renders tooltips, which need the provider
     that App.svelte supplies in the real tree, and it reads the Diagnostics
     Mode value the root shell owns (ADR-0081). This stands in for both. -->
<script lang="ts">
  import * as Tooltip from "../components/ui/tooltip";
  import SettingsPage from "./SettingsPage.svelte";
  import * as api from "../api";
  import type { SettingsSection } from "../routing";

  let {
    section,
    bare = false,
    onSelectSection,
  }: {
    section: SettingsSection;
    bare?: boolean;
    onSelectSection?: (section: SettingsSection) => void;
  } = $props();

  let diagnosticsMode = $state<boolean | null>(null);
  let diagnosticsModeError = $state("");

  async function loadDiagnosticsMode() {
    try {
      diagnosticsMode = (await api.getDiagnosticsMode()).enabled;
      diagnosticsModeError = "";
    } catch (caught) {
      diagnosticsModeError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  void loadDiagnosticsMode();
</script>

<Tooltip.Provider>
  <SettingsPage
    {section}
    {bare}
    {onSelectSection}
    {diagnosticsMode}
    {diagnosticsModeError}
    onDiagnosticsModeChange={(enabled) => {
      diagnosticsMode = enabled;
      diagnosticsModeError = "";
    }}
    onRetryDiagnosticsMode={loadDiagnosticsMode}
  />
</Tooltip.Provider>
