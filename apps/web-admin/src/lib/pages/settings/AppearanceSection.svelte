<script lang="ts">
  import { onMount } from "svelte";
  import { generateTheme, type GeneratedThemeTokens, type ThemeMode } from "@openpiso/ui";
  import * as Sheet from "../../components/ui/sheet";
  import { IsMobile } from "../../hooks/is-mobile.svelte";
  import * as api from "../../api";
  import { applyThemeRecord } from "../../theme";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Button } from "../../components/ui/button";
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

<div class="space-y-5">
  <div>
    <h3 class="text-lg font-semibold">Appearance</h3>
    <p class="text-sm text-muted-foreground">Preview a shop-wide accent and choose each surface mode before applying.</p>
  </div>
  {#if !themeDraft || !generatedTheme}
    <div class="rounded-lg border bg-card p-6 text-sm text-muted-foreground">Loading…</div>
  {:else}
    <section class="rounded-xl border bg-card p-5 shadow-sm">
      <div class="flex flex-wrap items-center gap-4">
        <label class="relative block size-16 shrink-0 cursor-pointer rounded-full border-4 border-background shadow ring-1 ring-border" style={`background:${themeDraft.accent_color}`} title="Choose accent color">
          <span class="sr-only">Choose accent color</span>
          <input id="theme-accent" type="color" class="absolute inset-0 cursor-pointer opacity-0" value={themeDraft.accent_color} oninput={(event) => updateAccent(event.currentTarget.value)} />
        </label>
        <div class="min-w-36">
          <div class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Shop accent</div>
          <div class="mt-1 font-mono text-lg font-semibold uppercase">{themeDraft.accent_color}</div>
          <button type="button" class="mt-1 text-xs font-medium text-primary underline-offset-4 hover:underline" onclick={() => updateAccent("#15803d")}>Reset to default green</button>
        </div>
        <div class="flex flex-1 flex-wrap gap-2 sm:justify-end">
          {#each themePresets as preset (preset.color)}
            <button type="button" class="group flex flex-col items-center gap-1 text-[11px] text-muted-foreground" title={preset.label} aria-label={`Use ${preset.label} accent`} onclick={() => updateAccent(preset.color)}>
              <span class="size-8 rounded-full border-2 shadow-sm transition-transform group-hover:scale-110" class:border-foreground={themeDraft.accent_color === preset.color} class:border-background={themeDraft.accent_color !== preset.color} style={`background:${preset.color}`}></span>
              {preset.label}
            </button>
          {/each}
        </div>
      </div>
    </section>

    <div class="grid gap-4 xl:grid-cols-3">
      {#each themeSurfaces as surface (surface.key)}
        {@const mode = themeDraft[surface.key]}
        {@const tokens = generatedTheme.tokens[mode]}
        <section class="overflow-hidden rounded-xl border bg-card shadow-sm">
          <header class="flex items-start justify-between gap-3 border-b p-4">
            <div><h4 class="font-semibold">{surface.label}</h4><p class="text-xs text-muted-foreground">{surface.description}</p></div>
            <div class="flex rounded-md bg-muted p-1" aria-label={`${surface.label} mode`}>
              {#each ["light", "dark"] as option}
                <button type="button" class="rounded px-2.5 py-1 text-xs font-medium capitalize" class:bg-background={mode === option} class:shadow-sm={mode === option} class:text-muted-foreground={mode !== option} onclick={() => updateSurfaceMode(surface.key, option as ThemeMode)}>{option}</button>
              {/each}
            </div>
          </header>
          <div class="p-4">
            <div class="h-52 overflow-hidden rounded-lg border text-[10px]" style={previewStyle(tokens, mode)}>
              {#if surface.key === "mode_web_admin"}
                <div class="flex h-full bg-[var(--preview-background)] text-[var(--preview-foreground)]">
                  <aside class="w-16 bg-[var(--preview-muted)] p-2"><div class="mb-3 h-3 rounded bg-[var(--preview-sidebar-primary)]"></div>{#each [1,2,3,4] as item}<div class="mb-2 h-2 rounded bg-[var(--preview-border)]"></div>{/each}</aside>
                  <main class="flex-1 p-3"><div class="mb-2 font-semibold">Dashboard</div><div class="rounded border border-[var(--preview-border)] p-2"><span class="text-[8px] opacity-60">Revenue</span><div class="text-base font-bold">₱2,480</div><div class="mt-3 flex h-14 items-end gap-1">{#each [35,55,42,78,62,90] as height, i}<div class="flex-1 rounded-t" style={`height:${height}%;background:var(--preview-chart-${(i % 5) + 1})`}></div>{/each}</div></div><button class="mt-3 rounded bg-[var(--preview-primary)] px-3 py-1 text-[var(--preview-primary-foreground)]">View report</button></main>
                </div>
              {:else if surface.key === "mode_kiosk"}
                <div class="h-full bg-[var(--preview-background)] p-3 text-[var(--preview-foreground)]"><div class="mb-3 flex justify-between"><b>SELECT A PC</b><span class="text-[var(--preview-primary)]">OPENPISO</span></div>{#each [["PC 01","1:24:08"],["PC 02","AVAILABLE"],["PC 03","42:16"]] as row, i}<div class="mb-2 flex items-center justify-between rounded border p-2" style={`border-color:${i === 1 ? "var(--preview-primary)" : "var(--preview-border)"}`}><b>{row[0]}</b><span class={i === 1 ? "text-[var(--preview-primary)]" : ""}>{row[1]}</span></div>{/each}<div class="mt-3 h-1.5 rounded bg-[var(--preview-primary)]"></div></div>
              {:else}
                <div class="flex h-full flex-col items-center justify-center bg-[var(--preview-background)] text-[var(--preview-foreground)]"><div class="mb-1 text-[8px] uppercase tracking-widest opacity-60">Remaining time</div><div class="text-3xl font-bold tabular-nums">01:24:08</div><button class="mt-5 rounded bg-[var(--preview-primary)] px-5 py-2 font-semibold text-[var(--preview-primary-foreground)]">INSERT COIN</button><span class="mt-2 text-[8px] opacity-60">or log in to your account</span></div>
              {/if}
            </div>
          </div>
        </section>
      {/each}
    </div>

    <div class="flex flex-wrap items-center gap-3">
      <Button type="button" disabled={themeBusy || !themeChanged} onclick={() => void openThemeConfirmation()}>Apply to shop…</Button>
      {#if themeChanged}<span class="text-sm text-amber-700 dark:text-amber-400">Changes not yet applied</span>{/if}
      {#if themeLoadError}<span class="text-sm text-destructive">{themeLoadError}</span>{/if}
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
      <div class="mt-4 rounded-lg border border-amber-500/40 bg-amber-500/10 p-3 text-sm text-amber-900 dark:text-amber-200"><strong>Live update:</strong> Client PCs will re-theme immediately, including during active sessions. Timers and sessions are not interrupted.</div>
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
    <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4" role="presentation" onclick={(event) => { if (event.target === event.currentTarget && !themeBusy) themeConfirmOpen = false; }}>
      <div class="w-full max-w-lg rounded-xl border bg-background p-6 shadow-2xl" role="dialog" aria-modal="true" aria-labelledby="theme-confirm-title">
        {@render themeDialogBody()}
      </div>
    </div>
  {/if}
{/if}
