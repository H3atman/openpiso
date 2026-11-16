<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Input } from "../../components/ui/input";
  import { Button } from "../../components/ui/button";
  import { Switch } from "../../components/ui/switch";
  import { Badge } from "../../components/ui/badge";
  import ConfirmDialog from "../../components/ConfirmDialog.svelte";
  import Notice from "../../components/settings/Notice.svelte";
  import SettingsCard from "../../components/settings/SettingsCard.svelte";
  import SettingsField from "../../components/settings/SettingsField.svelte";
  import MonitorIcon from "@lucide/svelte/icons/monitor";
  import Trash2Icon from "@lucide/svelte/icons/trash-2";
  import type { PcSettings } from "../../types";

  let pcs = $state<PcSettings[]>([]);
  let pcsBusy = $state(false);
  let pcsLoadError = $state("");
  let newPcId = $state("");
  let newPcName = $state("");
  let newPcIp = $state("");
  let pcEditBusy = $state<string | null>(null);
  let pcEditValues = $state<Record<string, PcEdit>>({});
  let pcToRemove = $state<PcSettings | null>(null);
  let pcRemoveBusy = $state(false);

  type PcEdit = { name: string; visible: boolean; registeredIp: string; macAddress: string };

  /** The row's unsaved edits, or its saved values when it has none. */
  function draftFor(pc: PcSettings): PcEdit {
    return pcEditValues[pc.id] ?? {
      name: pc.name,
      visible: pc.visible,
      registeredIp: pc.registered_ip ?? "",
      macAddress: pc.mac_address ?? "",
    };
  }

  function editPc(pc: PcSettings, patch: Partial<PcEdit>) {
    pcEditValues[pc.id] = { ...draftFor(pc), ...patch };
  }

  function isDirty(pc: PcSettings): boolean {
    const draft = draftFor(pc);
    return draft.name !== pc.name ||
      draft.visible !== pc.visible ||
      draft.registeredIp !== (pc.registered_ip ?? "") ||
      draft.macAddress !== (pc.mac_address ?? "");
  }

  async function loadPcs() {
    pcsBusy = true;
    pcsLoadError = "";
    try {
      pcs = await api.listPcSettings();
      const values: Record<string, PcEdit> = {};
      for (const pc of pcs) {
        values[pc.id] = {
          name: pc.name,
          visible: pc.visible,
          registeredIp: pc.registered_ip ?? "",
          macAddress: pc.mac_address ?? "",
        };
      }
      pcEditValues = values;
    } catch (caught) {
      pcsLoadError = caught instanceof Error ? caught.message : String(caught);
    } finally {
      pcsBusy = false;
    }
  }

  async function registerPc(): Promise<void> {
    if (!newPcId.trim() || !newPcName.trim()) {
      reportActionFailure("register", "PC", new Error("PC id and display name are required"));
      return;
    }
    pcsBusy = true;
    try {
      await api.registerPc({
        id: newPcId.trim(),
        name: newPcName.trim(),
        registered_ip: newPcIp.trim() || null,
      });
      newPcId = "";
      newPcName = "";
      newPcIp = "";
      await loadPcs();
      reportActionSuccess("register", "PC");
    } catch (caught) {
      reportActionFailure("register", "PC", caught);
    } finally {
      pcsBusy = false;
    }
  }

  async function savePcEdit(id: string): Promise<void> {
    const values = pcEditValues[id];
    if (!values) return;
    pcEditBusy = id;
    try {
      await api.updatePcSettings(id, {
        name: values.name,
        visible: values.visible,
        registered_ip: values.registeredIp.trim() || null,
        mac_address: values.macAddress.trim() || null,
      });
      await loadPcs();
      reportActionSuccess("edit", "PC");
    } catch (caught) {
      reportActionFailure("edit", "PC", caught);
    } finally {
      pcEditBusy = null;
    }
  }

  async function confirmRemovePc(): Promise<void> {
    const target = pcToRemove;
    if (!target) return;
    pcRemoveBusy = true;
    try {
      await api.removePc(target.id);
      pcToRemove = null;
      await loadPcs();
      reportActionSuccess("remove", "PC");
    } catch (caught) {
      reportActionFailure("remove", "PC", caught);
      pcToRemove = null;
    } finally {
      pcRemoveBusy = false;
    }
  }

  onMount(() => {
    void loadPcs();
  });
</script>

<div class="space-y-6">
  <SettingsCard
    title="Pre-register a PC"
    description="Add each PC by id and display name, with the static IP it lives on. On its next boot the Client binds its MAC to this row; later boots identify it by that MAC."
  >
    <form
      id="pre-register-pc"
      class="grid gap-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1.5fr)_minmax(0,1.25fr)]"
      onsubmit={(event) => {
        event.preventDefault();
        void registerPc();
      }}
    >
      <SettingsField id="new-pc-id" label="PC id">
        <Input id="new-pc-id" placeholder="pc-05" autocomplete="off" bind:value={newPcId} aria-label="New PC id" />
      </SettingsField>
      <SettingsField id="new-pc-name" label="Display name">
        <Input id="new-pc-name" placeholder="Counter 5" autocomplete="off" bind:value={newPcName} aria-label="New PC display name" />
      </SettingsField>
      <SettingsField id="new-pc-ip" label="Static IP (optional)">
        <Input id="new-pc-ip" placeholder="192.168.1.45" autocomplete="off" inputmode="decimal" bind:value={newPcIp} aria-label="New PC static IP" />
      </SettingsField>
    </form>

    {#snippet footer()}
      <Button type="submit" form="pre-register-pc" disabled={pcsBusy || !newPcId.trim() || !newPcName.trim()}>
        {pcsBusy ? "Adding…" : "Pre-register"}
      </Button>
    {/snippet}
  </SettingsCard>

  <SettingsCard
    title="Registered PCs"
    description="Edit a PC and save its row. Clear the MAC to let a replacement network card re-bind on the next boot."
    flush
  >
    {#snippet actions()}
      {#if pcs.length > 0}
        <Badge variant="secondary" class="tabular-nums">{pcs.length} {pcs.length === 1 ? "PC" : "PCs"}</Badge>
      {/if}
    {/snippet}

    {#if pcsLoadError}
      <div class="px-4 pt-4 md:px-6">
        <Notice tone="danger">{pcsLoadError}</Notice>
      </div>
    {/if}

    {#if pcs.length === 0}
      {#if !pcsLoadError}
        <div class="flex flex-col items-center gap-2 px-4 py-10 text-center">
          <MonitorIcon class="size-6 text-muted-foreground" aria-hidden="true" />
          <p class="text-sm font-medium">{pcsBusy ? "Loading PCs…" : "No PCs yet"}</p>
          {#if !pcsBusy}
            <p class="text-sm text-muted-foreground">Pre-register one above and it appears here.</p>
          {/if}
        </div>
      {/if}
    {:else}
      <!-- Header and rows are separate grids, so every column but the
           display name is fixed: IP and MAC fit "255.255.255.255" and a full
           17-character MAC in monospace at 1280px. -->
      <div
        class="hidden grid-cols-[6rem_minmax(0,1fr)_9.5rem_11.5rem_4rem_10rem] items-center gap-3 border-b bg-muted/40 px-6 py-2 text-xs font-medium text-muted-foreground xl:grid"
        aria-hidden="true"
      >
        <span>PC id</span>
        <span>Display name</span>
        <span>Registered IP</span>
        <span>Bound MAC</span>
        <span>Visible</span>
        <span></span>
      </div>
      <ul class="divide-y">
        {#each pcs as pc (pc.id)}
          {@const draft = draftFor(pc)}
          {@const dirty = isDirty(pc)}
          <li
            class="grid gap-3 px-4 py-4 sm:grid-cols-2 md:px-6 xl:grid-cols-[6rem_minmax(0,1fr)_9.5rem_11.5rem_4rem_10rem] xl:items-center xl:py-3"
          >
            <div class="flex items-center justify-between gap-3 sm:col-span-2 xl:contents">
              <code class="w-fit rounded bg-muted px-2 py-1 font-mono text-xs xl:col-start-1 xl:row-start-1">{pc.id}</code>
              <label class="flex items-center gap-2 text-sm xl:col-start-5 xl:row-start-1">
                <span class="xl:sr-only">Visible</span>
                <Switch
                  size="sm"
                  checked={draft.visible}
                  onCheckedChange={(checked) => editPc(pc, { visible: checked })}
                  aria-label={`Visible in kiosk for ${pc.id}`}
                />
              </label>
            </div>
            <label class="grid gap-1 sm:col-span-2 xl:col-span-1 xl:col-start-2 xl:row-start-1">
              <span class="text-xs text-muted-foreground xl:hidden" aria-hidden="true">Display name</span>
              <Input
                value={draft.name}
                oninput={(event) => editPc(pc, { name: event.currentTarget.value })}
                aria-label={`Name for ${pc.id}`}
              />
            </label>
            <label class="grid gap-1 xl:col-start-3 xl:row-start-1">
              <span class="text-xs text-muted-foreground xl:hidden" aria-hidden="true">Registered IP</span>
              <Input
                value={draft.registeredIp}
                oninput={(event) => editPc(pc, { registeredIp: event.currentTarget.value })}
                placeholder="Not set"
                inputmode="decimal"
                class="font-mono"
                aria-label={`Registered IP for ${pc.id}`}
              />
            </label>
            <label class="grid gap-1 xl:col-start-4 xl:row-start-1">
              <span class="text-xs text-muted-foreground xl:hidden" aria-hidden="true">Bound MAC</span>
              <Input
                value={draft.macAddress}
                oninput={(event) => editPc(pc, { macAddress: event.currentTarget.value })}
                placeholder="Not bound yet"
                class="font-mono"
                aria-label={`MAC for ${pc.id}`}
              />
            </label>
            <div class="flex gap-2 sm:col-span-2 sm:justify-end xl:col-span-1 xl:col-start-6 xl:row-start-1 max-sm:*:flex-1">
              <Button
                size="sm"
                variant={dirty ? "default" : "outline"}
                disabled={pcEditBusy === pc.id}
                aria-label={`Save ${pc.name}`}
                onclick={() => void savePcEdit(pc.id)}
              >
                {pcEditBusy === pc.id ? "Saving…" : "Save"}
              </Button>
              <Button
                size="sm"
                variant="outline"
                class="text-destructive hover:bg-destructive/10 hover:text-destructive"
                disabled={pcEditBusy === pc.id}
                aria-label={`Remove ${pc.name}`}
                onclick={() => (pcToRemove = pc)}
              >
                <Trash2Icon aria-hidden="true" />
                Remove
              </Button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </SettingsCard>
</div>

{#if pcToRemove}
  <ConfirmDialog
    title="Remove {pcToRemove.name}?"
    confirmLabel="Remove"
    busyLabel="Removing…"
    busy={pcRemoveBusy}
    destructive
    onconfirm={() => void confirmRemovePc()}
    oncancel={() => (pcToRemove = null)}
  >
    This permanently deletes <code class="font-mono text-xs">{pcToRemove.id}</code>.
    Removal is allowed only for a PC with no coin/adjustment/audit history, no
    remaining time, and not currently online. A PC with history should be
    hidden instead. This cannot be undone.
  </ConfirmDialog>
{/if}
