<script lang="ts">
  import * as api from "../api";
  import type { EconomySettings } from "../types";
  import AutostartSection from "./settings/AutostartSection.svelte";
  import CoinDeviceSection from "./settings/CoinDeviceSection.svelte";
  import CoinPromoSection from "./settings/CoinPromoSection.svelte";
  import AttendanceSection from "./settings/AttendanceSection.svelte";
  import WheelSection from "./settings/WheelSection.svelte";
  import PointsSection from "./settings/PointsSection.svelte";
  import PcSection from "./settings/PcSection.svelte";
  import ClientRecoveryEscalationSection from "./settings/ClientRecoveryEscalationSection.svelte";
  import PcMonitoringSection from "./settings/PcMonitoringSection.svelte";
  import TelegramSection from "./settings/TelegramSection.svelte";
  import ShopSection from "./settings/ShopSection.svelte";
  import SecuritySection from "./settings/SecuritySection.svelte";
  import AppearanceSection from "./settings/AppearanceSection.svelte";
  import ChevronRightIcon from "@lucide/svelte/icons/chevron-right";
  import { IsMobile } from "../hooks/is-mobile.svelte";
  import { settingsSections, type SettingsSection } from "../routing";
  import { assertUnreachable } from "../utils";

  let {
    section = "points",
    // True when the current address is the bare Settings path, which
    // ADR-0065 renders as a section index below 768px.
    bare = false,
    onSelectSection,
    // Diagnostics Mode is owned by the root shell (ADR-0081), so Security and
    // Coin Device read one value for this browser rather than each holding a
    // private copy that can go stale.
    diagnosticsMode = null,
    diagnosticsModeError = "",
    onDiagnosticsModeChange,
    onRetryDiagnosticsMode,
  }: {
    section?: SettingsSection;
    bare?: boolean;
    onSelectSection?: (section: SettingsSection) => void;
    diagnosticsMode?: boolean | null;
    diagnosticsModeError?: string;
    onDiagnosticsModeChange: (enabled: boolean) => void;
    onRetryDiagnosticsMode: () => void;
  } = $props();
  let prefetchedEconomy = $state<EconomySettings | null>(null);
  const isMobile = new IsMobile();

  async function notifyShopRateSaved() {
    try {
      prefetchedEconomy = await api.getEconomySettings();
    } catch {
      // The Shop Rate save already committed. Points will retry its own load
      // when the owner opens that section.
      prefetchedEconomy = null;
    }
  }

  function consumePrefetchedEconomy() {
    prefetchedEconomy = null;
  }
  const tab = $derived(section);
  const showIndex = $derived(bare && isMobile.current);
  const sectionLabel = $derived(
    settingsSections.find((entry) => entry.id === tab)?.label ?? "Settings",
  );
</script>

<div class="space-y-6">
  {#if showIndex}
    <h2 class="text-2xl font-bold tracking-tight">Settings</h2>

    <nav class="divide-y overflow-hidden rounded-lg border bg-card" aria-label="Settings sections">
      {#each settingsSections as entry}
        <button
          type="button"
          class="flex w-full items-center justify-between gap-2 px-4 py-3 text-left font-medium hover:bg-accent focus-visible:bg-accent"
          onclick={() => onSelectSection?.(entry.id)}
        >
          {entry.label}
          <ChevronRightIcon class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        </button>
      {/each}
    </nav>
  {:else}
    <h2 class="text-2xl font-bold tracking-tight">{sectionLabel}</h2>

  <div class="min-w-0">
  {#if tab === "points"}
    <PointsSection
      initialEconomy={prefetchedEconomy}
      onInitialEconomyConsumed={consumePrefetchedEconomy}
    />

  {:else if tab === "coin-promo"}
    <CoinPromoSection />

  {:else if tab === "attendance"}
    <AttendanceSection />

  {:else if tab === "wheel"}
    <WheelSection />

  {:else if tab === "coin-device"}
    <CoinDeviceSection {diagnosticsMode} />

  {:else if tab === "shop"}
    <ShopSection onShopRateSaved={notifyShopRateSaved} />

  {:else if tab === "pcs"}
    <PcSection />

  {:else if tab === "pc-monitoring"}
    <div class="space-y-6">
      <PcMonitoringSection />
      <ClientRecoveryEscalationSection />
    </div>

  {:else if tab === "telegram"}
    <TelegramSection />

  {:else if tab === "autostart"}
    <AutostartSection />

  {:else if tab === "security"}
    <SecuritySection
      {diagnosticsMode}
      {diagnosticsModeError}
      onChange={onDiagnosticsModeChange}
      onRetry={onRetryDiagnosticsMode}
    />

  {:else if tab === "appearance"}
    <AppearanceSection />
  {:else}
    <!-- A Settings section with no branch above narrows to `never` here, so
         adding one without rendering it fails to compile rather than showing
         an empty section. -->
    {assertUnreachable(tab)}
  {/if}
  </div>
  {/if}
</div>
