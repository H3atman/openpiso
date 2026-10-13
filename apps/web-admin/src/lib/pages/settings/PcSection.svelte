<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../../api";
  import { reportActionFailure, reportActionSuccess } from "../../notifications";
  import { Input } from "../../components/ui/input";
  import { Button } from "../../components/ui/button";
  import ConfirmDialog from "../../components/ConfirmDialog.svelte";
  import type { PcSettings } from "../../types";

  let pcs = $state<PcSettings[]>([]);
  let pcsBusy = $state(false);
  let pcsLoadError = $state("");
  let newPcId = $state("");
  let newPcName = $state("");
  let newPcIp = $state("");
  let pcEditBusy = $state<string | null>(null);
  let pcEditValues = $state<Record<string, { name: string; visible: boolean; registeredIp: string; macAddress: string }>>({});
  let pcToRemove = $state<PcSettings | null>(null);
  let pcRemoveBusy = $state(false);

  async function loadPcs() {
    pcsBusy = true;
    pcsLoadError = "";
    try {
      pcs = await api.listPcSettings();
      const values: Record<string, { name: string; visible: boolean; registeredIp: string; macAddress: string }> = {};
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

<div class="rounded-lg border bg-card p-6">
  <h3 class="mb-4 text-lg font-semibold">PCs</h3>
  <p class="mb-4 text-sm text-muted-foreground">
    Pre-register each PC by number/name + the static IP it lives on.
    On the next boot, the client binds its MAC to this row; later
    boots identify by that MAC. Clear the MAC field to allow a
    hardware/NIC replacement to re-bind.
  </p>

  <div class="mb-6 grid gap-3 sm:grid-cols-[1fr_2fr_1.5fr_auto]">
    <Input
      id="new-pc-id"
      placeholder="pc-05"
      bind:value={newPcId}
      aria-label="New PC id"
    />
    <Input
      id="new-pc-name"
      placeholder="Counter 5"
      bind:value={newPcName}
      aria-label="New PC display name"
    />
    <Input
      id="new-pc-ip"
      placeholder="192.168.1.45"
      bind:value={newPcIp}
      aria-label="New PC static IP"
    />
    <Button
      type="button"
      disabled={pcsBusy || !newPcId.trim() || !newPcName.trim()}
      onclick={() => void registerPc()}
    >
      {pcsBusy ? "Adding…" : "Pre-register"}
    </Button>
  </div>

  {#if pcsLoadError}
    <div class="mb-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
      {pcsLoadError}
    </div>
  {/if}

  <div class="space-y-2">
    {#if pcs.length === 0}
      <p class="text-muted-foreground text-sm">No PCs yet — add one above.</p>
    {/if}
    {#each pcs as pc (pc.id)}
      <div class="grid items-start gap-2 rounded-md border p-3 sm:grid-cols-[1fr_2fr_1.5fr_1.5fr_auto_auto_auto]">
        <code class="bg-muted rounded px-2 py-1 font-mono text-xs">{pc.id}</code>
        <Input
          value={pcEditValues[pc.id]?.name ?? pc.name}
          oninput={(event) => {
            const target = event.currentTarget as HTMLInputElement;
            pcEditValues[pc.id] = {
              ...(pcEditValues[pc.id] ?? {
                name: pc.name,
                visible: pc.visible,
                registeredIp: pc.registered_ip ?? "",
                macAddress: pc.mac_address ?? "",
              }),
              name: target.value,
            };
          }}
          aria-label={`Name for ${pc.id}`}
        />
        <Input
          value={pcEditValues[pc.id]?.registeredIp ?? (pc.registered_ip ?? "")}
          oninput={(event) => {
            const target = event.currentTarget as HTMLInputElement;
            pcEditValues[pc.id] = {
              ...(pcEditValues[pc.id] ?? {
                name: pc.name,
                visible: pc.visible,
                registeredIp: pc.registered_ip ?? "",
                macAddress: pc.mac_address ?? "",
              }),
              registeredIp: target.value,
            };
          }}
          placeholder="registered IP"
          aria-label={`Registered IP for ${pc.id}`}
        />
        <Input
          value={pcEditValues[pc.id]?.macAddress ?? (pc.mac_address ?? "")}
          oninput={(event) => {
            const target = event.currentTarget as HTMLInputElement;
            pcEditValues[pc.id] = {
              ...(pcEditValues[pc.id] ?? {
                name: pc.name,
                visible: pc.visible,
                registeredIp: pc.registered_ip ?? "",
                macAddress: pc.mac_address ?? "",
              }),
              macAddress: target.value,
            };
          }}
          placeholder="(bound MAC)"
          aria-label={`MAC for ${pc.id}`}
        />
        <label class="flex items-center gap-2 self-center whitespace-nowrap text-sm">
          <input
            type="checkbox"
            class="rounded"
            checked={pcEditValues[pc.id]?.visible ?? pc.visible}
            onchange={(event) => {
              const target = event.currentTarget as HTMLInputElement;
              pcEditValues[pc.id] = {
                ...(pcEditValues[pc.id] ?? {
                  name: pc.name,
                  visible: pc.visible,
                  registeredIp: pc.registered_ip ?? "",
                  macAddress: pc.mac_address ?? "",
                }),
                visible: target.checked,
              };
            }}
            aria-label={`Visible in kiosk for ${pc.id}`}
          />
          Visible
        </label>
        <Button
          type="button"
          size="sm"
          disabled={pcEditBusy === pc.id}
          onclick={() => void savePcEdit(pc.id)}
        >
          {pcEditBusy === pc.id ? "Saving…" : "Save"}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="destructive"
          disabled={pcEditBusy === pc.id}
          onclick={() => (pcToRemove = pc)}
        >
          Remove
        </Button>
      </div>
    {/each}
  </div>
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
