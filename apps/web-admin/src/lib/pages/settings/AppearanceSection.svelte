<script lang="ts">
  import { onMount } from "svelte";
  import { generateTheme, type GeneratedThemeTokens, type ThemeMode } from "@openpiso/ui";
  import * as Sheet from "../../components/ui/sheet";
  import { IsMobile } from "../../hooks/is-mobile.svelte";
  import * as api from "../../api";
  import { applyThemeRecord } from "../../theme";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Button } from "../../components/ui/button";
  import Notice from "../../components/settings/Notice.svelte";
  import { Dialog } from "bits-ui";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import CheckIcon from "@lucide/svelte/icons/check";
  import type { ThemeRecord } from "../../types";

  let themeDraft = $state<ThemeRecord | null>(null);
  let appliedTheme = $state<ThemeRecord | null>(null);
  let themeBusy = $state(false);
  let themeLoadError = $state("");
  let themeConfirmOpen = $state(false);

  const isMobile = new IsMobile();

  const themePresets = [
    { label: "Green", color: "#15803d" },
    { label: "Blue", color: "#2563eb" },
    { label: "Purple", color: "#9333ea" },
    { label: "Rose", color: "#e11d48" },
    { label: "Orange", color: "#ea580c" },
    { label: "Teal", color: "#0d9488" },
  ] as const;

  type ThemeSurfaceKey = keyof Pick<ThemeRecord, "mode_web_admin" | "mode_kiosk" | "mode_client">;

  const themeSurfaces: {
    key: ThemeSurfaceKey;
    label: string;
    description: string;
  }[] = [
    { key: "mode_web_admin", label: "Web admin", description: "Owner dashboard and settings" },
    { key: "mode_kiosk", label: "Kiosk", description: "Kiosk — coin insertion and PC selection" },
    { key: "mode_client", label: "Client", description: "Customer lock screen and timer" },
  ];

  let generatedTheme = $derived(themeDraft ? generateTheme(themeDraft) : null);
  let themeChanged = $derived(
    Boolean(themeDraft && appliedTheme && (
      themeDraft.accent_color !== appliedTheme.accent_color ||
      themeDraft.mode_web_admin !== appliedTheme.mode_web_admin ||
      themeDraft.mode_kiosk !== appliedTheme.mode_kiosk ||
      themeDraft.mode_client !== appliedTheme.mode_client
    )),
  );

  function previewStyle(tokens: GeneratedThemeTokens, mode: ThemeMode): string {
    const background = mode === "dark" ? "oklch(0.145 0 0)" : "oklch(1 0 0)";
    const foreground = mode === "dark" ? "oklch(0.985 0 0)" : "oklch(0.145 0 0)";
    const muted = mode === "dark" ? "oklch(0.269 0 0)" : "oklch(0.97 0 0)";
    const border = mode === "dark" ? "oklch(1 0 0 / 10%)" : "oklch(0.922 0 0)";
    return Object.entries(tokens).map(([key, value]) => `--preview-${key}:${value}`).join(";") +
      `;--preview-background:${background};--preview-foreground:${foreground};--preview-muted:${muted};--preview-border:${border}`;
  }

  function updateAccent(color: string) {
    if (!themeDraft) return;
    themeDraft.accent_color = color.toLowerCase();
  }

  function updateSurfaceMode(key: ThemeSurfaceKey, mode: ThemeMode) {
    if (!themeDraft) return;
    themeDraft[key] = mode;
  }

  async function loadTheme() {
    try {
      const theme = await api.getTheme();
      appliedTheme = { ...theme };
      themeDraft = { ...theme };
    } catch (caught) {
      themeLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function openThemeConfirmation() {
    if (!themeDraft) return;
    themeLoadError = "";
    try {
      appliedTheme = { ...(await api.getTheme()) };
      themeConfirmOpen = true;
    } catch (caught) {
      themeLoadError = caught instanceof Error ? caught.message : String(caught);
    }
  }

  async function saveTheme() {
    if (!themeDraft) return;
    themeBusy = true;
    try {
      const theme = await api.updateTheme({
        ...themeDraft,
        accent_color: themeDraft.accent_color.toLowerCase(),
      });
      appliedTheme = { ...theme };
      themeDraft = { ...theme };
      applyThemeRecord(theme);
      themeConfirmOpen = false;
      reportActionSuccess("save", "Theme");
    } catch (caught) {
      reportActionFailure("save", "Theme", caught);
    } finally {
      themeBusy = false;
    }
  }

  onMount(() => {
    loadTheme();
  });
</script>

<div class="space-y-6">
  {#if !themeDraft || !generatedTheme}
    {#if themeLoadError}
      <Notice tone="danger"><p>{themeLoadError}</p></Notice>
    {:else}
      <p class="rounded-lg border bg-card px-4 py-10 text-center text-sm text-muted-foreground">Loading appearance…</p>
    {/if}
  {:else}
    <SettingsCard title="Shop accent" description="One shop-wide colour. Preview it on each surface below before applying.">
      <div class="flex flex-wrap items-center gap-x-8 gap-y-5">
        <div class="flex items-center gap-4">
          <label
            class="relative block size-14 shrink-0 cursor-pointer rounded-full border-4 border-background shadow ring-1 ring-border focus-within:ring-3 focus-within:ring-ring/50"
            style={`background:${themeDraft.accent_color}`}
            title="Choose accent color"
          >
            <span class="sr-only">Choose accent color</span>
            <input
              id="theme-accent"
              type="color"
              class="absolute inset-0 cursor-pointer opacity-0"
              value={themeDraft.accent_color}
              oninput={(event) => updateAccent(event.currentTarget.value)}
            />
          </label>
          <div>
            <p class="font-mono text-base font-semibold uppercase">{themeDraft.accent_color}</p>
            <Button variant="link" class="h-auto px-0 text-xs text-foreground underline decoration-muted-foreground underline-offset-4 hover:decoration-foreground" onclick={() => updateAccent("#15803d")}>
              Reset to default green
            </Button>
          </div>
        </div>

        <div class="flex flex-wrap gap-1" role="group" aria-label="Accent presets">
          {#each themePresets as preset (preset.color)}
            {@const selected = themeDraft.accent_color === preset.color}
            <button
              type="button"
              class="flex min-w-14 flex-col items-center gap-1.5 rounded-lg px-1.5 py-2 text-xs text-muted-foreground outline-none hover:bg-accent focus-visible:ring-3 focus-visible:ring-ring/50"
              class:text-foreground={selected}
              aria-label={`Use ${preset.label} accent`}
              aria-pressed={selected}
              onclick={() => updateAccent(preset.color)}
            >
              <span
                class="flex size-8 items-center justify-center rounded-full text-white shadow-sm ring-offset-2 ring-offset-card"
                class:ring-2={selected}
                class:ring-foreground={selected}
                style={`background:${preset.color}`}
              >
                {#if selected}<CheckIcon class="size-4" aria-hidden="true" />{/if}
              </span>
              {preset.label}
            </button>
          {/each}
        </div>
      </div>
    </SettingsCard>

    <!-- Shared header/body tracks keep controls and previews aligned even
         when one surface description wraps onto an extra line. -->
    <div class="grid gap-6 xl:grid-cols-3 xl:gap-y-0">
      {#each themeSurfaces as surface (surface.key)}
        {@const mode = themeDraft[surface.key]}
        {@const tokens = generatedTheme.tokens[mode]}
        <SettingsCard
          title={surface.label}
          description={surface.description}
          class="xl:row-span-2 xl:grid xl:grid-rows-subgrid xl:[&>header]:flex-col xl:[&>header>div:first-child]:basis-auto"
        >
          {#snippet actions()}
            <div class="flex rounded-lg border bg-muted p-0.5" role="group" aria-label={`${surface.label} mode`}>
              {#each ["light", "dark"] as option}
                <button
                  type="button"
                  class="min-h-11 rounded-md px-3 text-xs font-medium capitalize outline-none focus-visible:ring-3 focus-visible:ring-ring/50 md:min-h-7"
                  class:bg-background={mode === option}
                  class:shadow-sm={mode === option}
                  class:text-foreground={mode === option}
                  class:text-muted-foreground={mode !== option}
                  aria-pressed={mode === option}
                  onclick={() => updateSurfaceMode(surface.key, option as ThemeMode)}
                >
                  {option}
                </button>
              {/each}
            </div>
          {/snippet}

          <!-- A picture of the surface, not part of the page: hidden from
               assistive tech and unreachable by keyboard. Its "buttons" are
               spans so the phone touch-size rule for real buttons cannot
               resize them and push the picture out of its frame. -->
          <div class="h-52 overflow-hidden rounded-lg border text-[10px]" style={previewStyle(tokens, mode)} aria-hidden="true" inert>
              {#if surface.key === "mode_web_admin"}
                <div class="flex h-full bg-[var(--preview-background)] text-[var(--preview-foreground)]">
                  <div class="w-16 shrink-0 bg-[var(--preview-muted)] p-2"><div class="mb-3 h-3 rounded bg-[var(--preview-sidebar-primary)]"></div>{#each [1,2,3,4] as item}<div class="mb-2 h-2 rounded bg-[var(--preview-border)]"></div>{/each}</div>
                  <div class="min-w-0 flex-1 p-3"><div class="mb-2 font-semibold">Dashboard</div><div class="rounded border border-[var(--preview-border)] p-2"><span class="text-[8px] opacity-60">Revenue</span><div class="text-base font-bold">₱2,480</div><div class="mt-3 flex h-12 items-end gap-1">{#each [35,55,42,78,62,90] as height, i}<div class="flex-1 rounded-t" style={`height:${height}%;background:var(--preview-chart-${(i % 5) + 1})`}></div>{/each}</div></div><span class="mt-3 inline-block rounded bg-[var(--preview-primary)] px-3 py-1 text-[var(--preview-primary-foreground)]">View report</span></div>
                </div>
              {:else if surface.key === "mode_kiosk"}
                <div class="h-full bg-[var(--preview-background)] p-3 text-[var(--preview-foreground)]"><div class="mb-3 flex justify-between"><b>SELECT A PC</b><span class="text-[var(--preview-primary)]">OPENPISO</span></div>{#each [["PC 01","1:24:08"],["PC 02","AVAILABLE"],["PC 03","42:16"]] as row, i}<div class="mb-2 flex items-center justify-between rounded border p-2" style={`border-color:${i === 1 ? "var(--preview-primary)" : "var(--preview-border)"}`}><b>{row[0]}</b><span class={i === 1 ? "text-[var(--preview-primary)]" : ""}>{row[1]}</span></div>{/each}<div class="mt-3 h-1.5 rounded bg-[var(--preview-primary)]"></div></div>
              {:else}
                <div class="flex h-full flex-col items-center justify-center bg-[var(--preview-background)] text-[var(--preview-foreground)]"><div class="mb-1 text-[8px] uppercase tracking-widest opacity-60">Remaining time</div><div class="text-3xl font-bold tabular-nums">01:24:08</div><span class="mt-5 inline-block rounded bg-[var(--preview-primary)] px-5 py-2 font-semibold text-[var(--preview-primary-foreground)]">INSERT COIN</span><span class="mt-2 text-[8px] opacity-60">or log in to your account</span></div>
              {/if}
          </div>
        </SettingsCard>
      {/each}
    </div>

    <div class="flex flex-wrap items-center justify-end gap-x-4 gap-y-3 rounded-lg border bg-card px-4 py-3 md:px-6">
      <p class="mr-auto text-xs text-muted-foreground max-md:basis-full">
        {#if themeChanged}
          <span class="font-medium text-amber-700 dark:text-amber-400">Changes not yet applied.</span>
        {/if}
        Applies the accent and all three surface modes together.
      </p>
      {#if themeLoadError}
        <p class="text-sm text-destructive max-md:basis-full" role="alert">{themeLoadError}</p>
      {/if}
      <Button type="button" class="max-md:flex-1" disabled={themeBusy || !themeChanged} onclick={() => void openThemeConfirmation()}>
        Apply to shop…
      </Button>
    </div>
  {/if}
</div>

{#if themeConfirmOpen && themeDraft && appliedTheme && generatedTheme}
  {#snippet themeDialogBody()}
    <!-- Re-narrows the nullable state: snippet closures do not inherit the outer {#if} narrowing. -->
    {#if themeConfirmOpen && themeDraft && appliedTheme && generatedTheme}
      {@const currentLightTokens = generateTheme(appliedTheme).tokens.light}
      <h3 id="theme-confirm-title" class="text-lg font-semibold">Apply theme to shop?</h3>
      <p class="mt-1 text-sm text-muted-foreground">Review the new shop-wide appearance before applying.</p>
      <div class="mt-5 flex items-center gap-3 rounded-lg border p-3"><span class="size-10 rounded-full border" style={`background:${appliedTheme.accent_color}`}></span><span class="text-muted-foreground">→</span><span class="size-10 rounded-full border" style={`background:${themeDraft.accent_color}`}></span><div class="ml-1 font-mono text-sm">{appliedTheme.accent_color} → {themeDraft.accent_color}</div></div>
      <div class="mt-4">
        <div class="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">Generated light ramp</div>
        <div class="grid grid-cols-[1fr_auto_1fr] items-center gap-3">
          <div class="flex overflow-hidden rounded-lg border" aria-label="Current light ramp">
            {#each ["chart-1", "chart-2", "chart-3", "chart-4", "chart-5"] as token}
              <div class="h-10 flex-1" style={`background:${currentLightTokens[token as keyof GeneratedThemeTokens]}`}></div>
            {/each}
          </div>
          <span class="text-muted-foreground">→</span>
          <div class="flex overflow-hidden rounded-lg border" aria-label="New light ramp">
            {#each ["chart-1", "chart-2", "chart-3", "chart-4", "chart-5"] as token}
              <div class="h-10 flex-1" style={`background:${generatedTheme.tokens.light[token as keyof GeneratedThemeTokens]}`}></div>
            {/each}
          </div>
        </div>
      </div>
      <div class="mt-4 grid grid-cols-3 gap-2">{#each themeSurfaces as surface}<div class="rounded-md bg-muted p-2 text-center"><div class="text-xs text-muted-foreground">{surface.label}</div><div class="text-sm font-semibold capitalize">{themeDraft[surface.key]}</div></div>{/each}</div>
      <Notice tone="warning" class="mt-4"><p><strong>Live update:</strong> Client PCs will re-theme immediately, including during active sessions. Timers and sessions are not interrupted.</p></Notice>
      <div class="mt-6 flex justify-end gap-3"><Button type="button" variant="outline" disabled={themeBusy} onclick={() => (themeConfirmOpen = false)}>Cancel</Button><Button type="button" disabled={themeBusy} onclick={() => void saveTheme()}>{themeBusy ? "Applying…" : "Apply to shop"}</Button></div>
    {/if}
  {/snippet}

  {#if isMobile.current}
    <Sheet.Root open onOpenChange={(open) => { if (!open && !themeBusy) themeConfirmOpen = false; }}>
      <Sheet.Content side="bottom" showCloseButton={false} aria-labelledby="theme-confirm-title" class="max-h-none gap-0 overflow-y-auto rounded-none p-4 data-[side=bottom]:h-dvh">
        {@render themeDialogBody()}
      </Sheet.Content>
    </Sheet.Root>
  {:else}
    <!-- bits-ui Dialog (the primitive under Sheet) supplies the focus trap,
         initial focus, Escape, and focus restoration. Escape and backdrop
         clicks are ignored while the theme is being applied. -->
    <Dialog.Root open onOpenChange={(open) => { if (!open && !themeBusy) themeConfirmOpen = false; }}>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/50" />
        <Dialog.Content
          aria-labelledby="theme-confirm-title"
          escapeKeydownBehavior={themeBusy ? "ignore" : "close"}
          interactOutsideBehavior={themeBusy ? "ignore" : "close"}
          class="fixed top-1/2 left-1/2 z-50 max-h-[calc(100dvh-2rem)] w-[calc(100%-2rem)] max-w-lg -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-lg border bg-card p-6 shadow-lg outline-none"
        >
          {@render themeDialogBody()}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  {/if}
{/if}
