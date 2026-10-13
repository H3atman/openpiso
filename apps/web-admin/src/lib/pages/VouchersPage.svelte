<script lang="ts">
  import { onMount } from "svelte";
  import * as api from "../api";
  import {
    formatMoney,
    formatDuration,
    formatDayDuration,
    formatDate,
    formatPoints,
    parsePesosToCents,
    parsePointsToCenti,
  } from "../format";
  import type {
    AdminSettings,
    EconomySettings,
    Voucher,
    VoucherBatchDetail,
    VoucherBatchSummary,
    VoucherTemplate,
  } from "../types";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import ResponsiveRows from "../components/ResponsiveRows.svelte";
  import { Badge } from "$lib/components/ui/badge";
  import { buildVoucherBatchCsv } from "../voucherCsv";
  import {
    buildVoucherDocument,
    type VoucherTokenValues,
  } from "../voucherTemplate";

  const SAMPLE_VALUES: VoucherTokenValues = {
    code: "DEMO-ABCD-2345",
    time: "02:00:00",
    points: "1.50",
    price: "₱50.00",
    shop_name: "OpenPiso Shop",
    expires_at: "Dec 31, 2027, 11:59 PM",
    prefix: "DEMO",
  };

  const BATCH_PAGE_SIZE = 100;
  const VOUCHER_PAGE_SIZE = 100;

  let tab = $state<"vouchers" | "batches" | "template">("vouchers");
  let vouchers = $state<Voucher[]>([]);
  let batches = $state<VoucherBatchSummary[]>([]);
  let selectedBatchId = $state("");
  let voucherStatus = $state<Voucher["status"] | "all">("active");
  let voucherOffset = $state(0);
  let hasMoreVouchers = $state(false);
  let codeSearchInput = $state("");
  let codeSearch = $state("");
  let hasMoreBatches = $state(false);
  let loadingMoreBatches = $state(false);
  let detail = $state<VoucherBatchDetail | null>(null);
  let voucherListError = $state("");
  let batchListError = $state("");
  let auxiliaryLoadError = $state("");
  let busy = $state(false);
  let voucherPrefix = $state("OP");
  let voucherSuffixLength = $state(8);
  let voucherDays = $state(0);
  let voucherHours = $state(0);
  let voucherMinutes = $state(5);
  let voucherSeconds = $state(0);
  let pointsValue = $state("0");
  let voucherPointsAuto = $state(true);
  let cashAmountPesos = $state("0");
  let createBusy = $state(false);

  let prefix = $state("");
  let suffixLength = $state(8);
  let count = $state(10);
  let batchDays = $state(0);
  let batchHours = $state(0);
  let batchMinutes = $state(0);
  let batchSeconds = $state(0);
  let batchPoints = $state("0");
  let batchPointsAuto = $state(true);
  let batchCashAmountPesos = $state("0");
  let expiresAt = $state("");
  let batchBusy = $state(false);

  let voucherTemplate = $state<VoucherTemplate | null>(null);
  let adminSettings = $state<AdminSettings | null>(null);
  let economySettings = $state<EconomySettings | null>(null);
  let templateHtml = $state("");
  let templateCss = $state("");
  let templateBusy = $state(false);
  let previewDocument = $derived(
    buildVoucherDocument({ html: templateHtml, css: templateCss }, [
      {
        ...SAMPLE_VALUES,
        shop_name: adminSettings?.shop_name ?? "OpenPiso Shop",
      },
    ]),
  );

  async function load() {
    busy = true;
    auxiliaryLoadError = "";
    const detailBatchId = detail?.id;
    const results = await Promise.allSettled([
      api.listVouchers(VOUCHER_PAGE_SIZE, {
        offset: voucherOffset,
        batchId: selectedBatchId,
        search: codeSearch,
        status: voucherStatus === "all" ? undefined : voucherStatus,
      }),
      api.listVoucherBatches(BATCH_PAGE_SIZE),
      detailBatchId
        ? api.getVoucherBatch(detailBatchId)
        : Promise.resolve(null),
      api.getVoucherTemplate(),
      api.getSettings(),
      api.getEconomySettings(),
    ]);
    if (results[0].status === "fulfilled") {
      vouchers = results[0].value;
      hasMoreVouchers = results[0].value.length === VOUCHER_PAGE_SIZE;
      voucherListError = "";
    } else {
      voucherListError = caughtMessage(results[0].reason);
    }
    if (results[1].status === "fulfilled") {
      batches = results[1].value;
      hasMoreBatches = results[1].value.length === BATCH_PAGE_SIZE;
      batchListError = "";
    } else {
      batchListError = caughtMessage(results[1].reason);
    }
    if (results[2].status === "fulfilled" && results[2].value)
      detail = results[2].value;
    if (results[3].status === "fulfilled") {
      voucherTemplate = results[3].value;
      templateHtml = results[3].value.html;
      templateCss = results[3].value.css;
    }
    if (results[4].status === "fulfilled") adminSettings = results[4].value;
    if (results[5].status === "fulfilled") {
      economySettings = results[5].value;
      refreshVoucherAutoPoints();
      refreshBatchAutoPoints();
    } else {
      economySettings = null;
      voucherPointsAuto = false;
      batchPointsAuto = false;
    }
    const failure = results
      .slice(2)
      .find((result) => result.status === "rejected");
    if (failure?.status === "rejected") {
      auxiliaryLoadError =
        failure.reason instanceof Error
          ? failure.reason.message
          : String(failure.reason);
    }
    busy = false;
  }

  function caughtMessage(caught: unknown): string {
    return caught instanceof Error ? caught.message : String(caught);
  }

  function durationToSeconds(
    days: number,
    hours: number,
    minutes: number,
    seconds: number,
  ): number | null {
    if (
      !Number.isInteger(days) ||
      !Number.isInteger(hours) ||
      !Number.isInteger(minutes) ||
      !Number.isInteger(seconds) ||
      days < 0 ||
      days > 99 ||
      hours < 0 ||
      hours > 23 ||
      minutes < 0 ||
      minutes > 59 ||
      seconds < 0 ||
      seconds > 59
    ) {
      return null;
    }
    return days * 86_400 + hours * 3_600 + minutes * 60 + seconds;
  }

  function autoPoints(cashAmount: string): string | null {
    const cashAmountCents = parsePesosToCents(cashAmount);
    if (
      cashAmountCents === null ||
      !economySettings ||
      economySettings.earn_cents_per_point <= 0
    ) {
      return null;
    }
    const pointsCenti =
      (BigInt(cashAmountCents) * 100n) /
      BigInt(economySettings.earn_cents_per_point);
    if (pointsCenti > BigInt(Number.MAX_SAFE_INTEGER)) return null;
    return formatPoints(Number(pointsCenti));
  }

  function refreshVoucherAutoPoints() {
    if (!voucherPointsAuto) return;
    const auto = autoPoints(cashAmountPesos);
    if (auto !== null) pointsValue = auto;
  }

  function refreshBatchAutoPoints() {
    if (!batchPointsAuto) return;
    const auto = autoPoints(batchCashAmountPesos);
    if (auto !== null) batchPoints = auto;
  }

  async function applyVoucherSearch() {
    codeSearch = codeSearchInput.trim();
    voucherOffset = 0;
    await load();
  }

  async function createVoucher() {
    const normalizedPrefix = voucherPrefix.trim().toUpperCase();
    const secondsValue = durationToSeconds(
      voucherDays,
      voucherHours,
      voucherMinutes,
      voucherSeconds,
    );
    const pointsValueCenti = parsePointsToCenti(pointsValue);
    const soldAmountCents = parsePesosToCents(cashAmountPesos);
    if (!/^[A-Z0-9]{2,8}$/.test(normalizedPrefix)) {
      reportActionFailure(
        "create",
        "Voucher",
        new Error("Prefix must be 2–8 letters or numbers"),
      );
      return;
    }
    if (
      !Number.isInteger(voucherSuffixLength) ||
      voucherSuffixLength < 6 ||
      voucherSuffixLength > 16
    ) {
      reportActionFailure(
        "create",
        "Voucher",
        new Error("Suffix length must be a whole number from 6 to 16"),
      );
      return;
    }
    if (secondsValue === null) {
      reportActionFailure(
        "create",
        "Voucher",
        new Error("Time fields must use DD 0–99 and HH:mm:ss in range"),
      );
      return;
    }
    if (soldAmountCents === null) {
      reportActionFailure(
        "create",
        "Voucher",
        new Error(
          "Cash amount must be a non-negative peso amount with at most two decimals",
        ),
      );
      return;
    }
    if (pointsValueCenti === null) {
      reportActionFailure(
        "create",
        "Voucher",
        new Error("Points value must be a number with at most two decimals"),
      );
      return;
    }
    createBusy = true;
    try {
      await api.createVoucher({
        prefix: normalizedPrefix,
        suffix_length: voucherSuffixLength,
        seconds_value: secondsValue,
        points_value_centi: pointsValueCenti,
        sold_amount_cents: soldAmountCents,
        expires_at: null,
      });
      await load();
      reportActionSuccess("create", "Voucher");
    } catch (caught) {
      reportActionFailure("create", "Voucher", caught);
    } finally {
      createBusy = false;
    }
  }

  async function createBatch() {
    const normalizedPrefix = prefix.trim().toUpperCase();
    const pointsValueCenti = parsePointsToCenti(batchPoints);
    const secondsValue = durationToSeconds(
      batchDays,
      batchHours,
      batchMinutes,
      batchSeconds,
    );
    const cashAmountCents = parsePesosToCents(batchCashAmountPesos);
    if (!/^[A-Z0-9]{2,8}$/.test(normalizedPrefix)) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error("Prefix must be 2–8 letters or numbers"),
      );
      return;
    }
    if (
      !Number.isInteger(suffixLength) ||
      suffixLength < 6 ||
      suffixLength > 16
    ) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error("Suffix length must be a whole number from 6 to 16"),
      );
      return;
    }
    if (!Number.isInteger(count) || count < 1 || count > 1000) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error("Count must be a whole number from 1 to 1000"),
      );
      return;
    }
    if (secondsValue === null) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error("Time fields must use DD 0–99 and HH:mm:ss in range"),
      );
      return;
    }
    if (pointsValueCenti === null || pointsValueCenti < 0) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error("Points must be non-negative with at most two decimals"),
      );
      return;
    }
    if (cashAmountCents === null) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error(
          "Cash amount must be a non-negative peso amount with at most two decimals",
        ),
      );
      return;
    }
    if (secondsValue === 0 && pointsValueCenti === 0) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error("Voucher Batch must include time or points"),
      );
      return;
    }
    const expiry = expiresAt ? new Date(expiresAt) : null;
    if (expiry && Number.isNaN(expiry.getTime())) {
      reportActionFailure(
        "create",
        "Voucher Batch",
        new Error("Expiry must be a valid date and time"),
      );
      return;
    }

    batchBusy = true;
    try {
      const created = await api.createVoucherBatch({
        prefix: normalizedPrefix,
        suffix_length: suffixLength,
        count,
        seconds_value: secondsValue,
        points_value_centi: pointsValueCenti,
        cash_amount_cents: cashAmountCents,
        expires_at: expiry?.toISOString() ?? null,
      });
      await load();
      await openBatch(created.id);
      reportActionSuccess("create", "Voucher Batch");
    } catch (caught) {
      reportActionFailure("create", "Voucher Batch", caught);
    } finally {
      batchBusy = false;
    }
  }

  async function loadMoreBatches() {
    loadingMoreBatches = true;
    try {
      const olderBatches = await api.listVoucherBatches(
        BATCH_PAGE_SIZE,
        batches.length,
      );
      batches = [...batches, ...olderBatches];
      hasMoreBatches = olderBatches.length === BATCH_PAGE_SIZE;
    } catch (caught) {
      batchListError = caughtMessage(caught);
    } finally {
      loadingMoreBatches = false;
    }
  }

  async function openBatch(id: string) {
    auxiliaryLoadError = "";
    try {
      detail = await api.getVoucherBatch(id);
    } catch (caught) {
      auxiliaryLoadError = caughtMessage(caught);
    }
  }

  async function exportBatch() {
    if (!detail) return;
    batchBusy = true;
    try {
      detail = await api.getVoucherBatch(detail.id);
      const url = URL.createObjectURL(
        new Blob([buildVoucherBatchCsv(detail)], {
          type: "text/csv;charset=utf-8",
        }),
      );
      const link = document.createElement("a");
      link.href = url;
      link.download = `vouchers-${detail.name}-${detail.created_at.slice(0, 10)}.csv`;
      link.click();
      URL.revokeObjectURL(url);
    } catch (caught) {
      reportActionFailure("export", "Voucher Batch", caught);
    } finally {
      batchBusy = false;
    }
  }

  async function saveTemplate(
    html = templateHtml,
    css = templateCss,
    action: "save" | "reset" = "save",
  ) {
    templateBusy = true;
    try {
      voucherTemplate = await api.updateVoucherTemplate({ html, css });
      templateHtml = voucherTemplate.html;
      templateCss = voucherTemplate.css;
      reportActionSuccess(action, "Voucher Template");
    } catch (caught) {
      reportActionFailure(action, "Voucher Template", caught);
    } finally {
      templateBusy = false;
    }
  }

  async function resetTemplate() {
    if (!voucherTemplate) return;
    await saveTemplate(
      voucherTemplate.default_html,
      voucherTemplate.default_css,
      "reset",
    );
  }

  function tokenValuesForCard(
    card: Voucher,
    batch: VoucherBatchDetail,
    shopName: string,
  ): VoucherTokenValues {
    return {
      code: card.code,
      time: formatDuration(card.seconds_value),
      points: formatPoints(card.points_value_centi),
      price: formatMoney(batch.cash_amount_cents),
      shop_name: shopName,
      expires_at: card.expires_at ? formatDate(card.expires_at) : "No expiry",
      prefix: batch.prefix,
    };
  }

  async function printBatch() {
    if (!detail) return;
    batchBusy = true;
    try {
      const [freshDetail, freshTemplate, freshSettings] = await Promise.all([
        api.getVoucherBatch(detail.id),
        api.getVoucherTemplate(),
        api.getSettings(),
      ]);
      detail = freshDetail;
      voucherTemplate = freshTemplate;
      adminSettings = freshSettings;
      templateHtml = freshTemplate.html;
      templateCss = freshTemplate.css;

      const printFrame = document.createElement("iframe");
      printFrame.title = `Print ${freshDetail.name} voucher batch`;
      printFrame.setAttribute("sandbox", "allow-same-origin allow-modals");
      printFrame.style.position = "fixed";
      printFrame.style.width = "1px";
      printFrame.style.height = "1px";
      printFrame.style.border = "0";
      printFrame.style.opacity = "0";
      const cleanup = () => printFrame.remove();
      printFrame.addEventListener(
        "load",
        () => {
          const printWindow = printFrame.contentWindow;
          if (!printWindow) {
            cleanup();
            reportActionFailure(
              "print",
              "Voucher Batch",
              new Error("Print window unavailable"),
            );
            return;
          }
          try {
            printWindow.addEventListener("afterprint", cleanup, { once: true });
            printWindow.focus();
            printWindow.print();
            window.setTimeout(cleanup, 60_000);
          } catch (caught) {
            cleanup();
            reportActionFailure("print", "Voucher Batch", caught);
          }
        },
        { once: true },
      );
      printFrame.srcdoc = buildVoucherDocument(
        freshTemplate,
        freshDetail.vouchers.map((card) =>
          tokenValuesForCard(card, freshDetail, freshSettings.shop_name),
        ),
      );
      document.body.append(printFrame);
    } catch (caught) {
      reportActionFailure("print", "Voucher Batch", caught);
    } finally {
      batchBusy = false;
    }
  }

  async function voidVoucher(voucher: Voucher) {
    const revenueWarning =
      voucher.kind === "sold"
        ? " Recorded revenue remains unchanged."
        : "";
    if (
      !window.confirm(
        `Void active voucher ${voucher.code}? This cannot be undone.${revenueWarning}`,
      )
    )
      return;
    busy = true;
    try {
      await api.voidVoucher(voucher.id);
      await load();
      reportActionSuccess("void", "Voucher");
    } catch (caught) {
      reportActionFailure("void", "Voucher", caught);
    } finally {
      busy = false;
    }
  }

  async function voidSelectedBatch() {
    const batch = batches.find((candidate) => candidate.id === selectedBatchId);
    if (
      !batch ||
      !window.confirm(
        `Void all active ${batch.name} (${batch.prefix}) cards? This cannot be undone.`,
      )
    )
      return;
    busy = true;
    try {
      await api.voidVoucherBatch(batch.id);
      await load();
      reportActionSuccess("void", "Voucher Batch");
    } catch (caught) {
      reportActionFailure("void", "Voucher Batch", caught);
    } finally {
      busy = false;
    }
  }

  async function voidBatch() {
    if (
      !detail ||
      !window.confirm(
        `Void all active ${detail.name} (${detail.prefix}) cards? This cannot be undone.`,
      )
    )
      return;
    batchBusy = true;
    try {
      await api.voidVoucherBatch(detail.id);
      await load();
      reportActionSuccess("void", "Voucher Batch");
    } catch (caught) {
      reportActionFailure("void", "Voucher Batch", caught);
    } finally {
      batchBusy = false;
    }
  }

  onMount(load);
</script>

<div class="space-y-6">
  <div class="flex items-center justify-between">
    <h2 class="text-2xl font-bold tracking-tight">Vouchers</h2>
    <button
      class="rounded-md border bg-card px-3 py-1.5 text-sm hover:bg-accent"
      type="button"
      onclick={load}>Refresh</button
    >
  </div>

  <div
    class="flex gap-1 border-b"
    role="tablist"
    aria-label="Voucher management"
  >
    {#each [["vouchers", "Vouchers"], ["batches", "Batches"], ["template", "Template"]] as item}
      <button
        type="button"
        role="tab"
        aria-selected={tab === item[0]}
        class="border-b-2 px-4 py-2 text-sm font-medium {tab === item[0]
          ? 'border-primary text-foreground'
          : 'border-transparent text-muted-foreground'}"
        onclick={() => (tab = item[0] as typeof tab)}>{item[1]}</button
      >
    {/each}
  </div>

  {#if auxiliaryLoadError}<div
      class="rounded-md bg-destructive/10 p-3 text-sm text-destructive"
    >
      {auxiliaryLoadError}
    </div>{/if}

  {#if tab === "vouchers"}
    <div class="rounded-lg border bg-card p-6">
      <h3 class="mb-4 text-lg font-semibold">Create Voucher</h3>
      <div class="space-y-4">
        <div class="grid gap-4 sm:grid-cols-2">
          <div>
            <label class="block text-sm font-medium" for="voucher-prefix">Prefix</label>
            <input
              id="voucher-prefix"
              required
              minlength="2"
              maxlength="8"
              pattern="[A-Za-z0-9]+"
              class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm uppercase"
              bind:value={voucherPrefix}
            />
          </div>
          <div>
            <label class="block text-sm font-medium" for="voucher-suffix">Suffix length</label>
            <input
              id="voucher-suffix"
              required
              min="6"
              max="16"
              type="number"
              class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
              bind:value={voucherSuffixLength}
            />
          </div>
        </div>
        <div>
          <span class="block text-sm font-medium">Time (DD:HH:mm:ss)</span>
          <div class="mt-1 grid grid-cols-4 gap-2">
            <label class="text-xs text-muted-foreground">Days
              <input min="0" max="99" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={voucherDays} />
            </label>
            <label class="text-xs text-muted-foreground">Hours
              <input min="0" max="23" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={voucherHours} />
            </label>
            <label class="text-xs text-muted-foreground">Minutes
              <input min="0" max="59" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={voucherMinutes} />
            </label>
            <label class="text-xs text-muted-foreground">Seconds
              <input min="0" max="59" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={voucherSeconds} />
            </label>
          </div>
        </div>
        <div class="grid gap-4 sm:grid-cols-2">
          <div>
            <label class="block text-sm font-medium" for="voucher-cash"
              >Cash amount (pesos)</label
            >
            <input
              id="voucher-cash"
              type="text"
              inputmode="decimal"
              placeholder="0.00"
              class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
              value={cashAmountPesos}
              oninput={(event) => {
                cashAmountPesos = event.currentTarget.value;
                refreshVoucherAutoPoints();
              }}
            />
            <p class="mt-1 text-xs text-muted-foreground">
              {parsePesosToCents(cashAmountPesos) === null
                ? "Enter a non-negative peso amount"
                : parsePesosToCents(cashAmountPesos)! > 0
                  ? `Sold · ${formatMoney(parsePesosToCents(cashAmountPesos)!)}`
                  : "Promo"}
            </p>
          </div>
          <div>
            <div class="flex items-center justify-between gap-2">
              <label class="block text-sm font-medium" for="voucher-points"
                >Points Value</label
              >
              {#if voucherPointsAuto}
                <span class="text-xs text-muted-foreground">Auto from earn rate</span>
              {:else if economySettings}
                <button type="button" class="text-xs text-primary hover:underline" onclick={() => {
                  voucherPointsAuto = true;
                  refreshVoucherAutoPoints();
                }}>Reset to auto</button>
              {:else}
                <span class="text-xs text-muted-foreground">Custom</span>
              {/if}
            </div>
            <input
              id="voucher-points"
              type="text"
              inputmode="decimal"
              placeholder="e.g. 1.50"
              class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
              value={pointsValue}
              oninput={(event) => {
                voucherPointsAuto = false;
                pointsValue = event.currentTarget.value;
              }}
            />
            {#if !economySettings}
              <p class="mt-1 text-xs text-destructive">Auto points unavailable; enter a custom value.</p>
            {/if}
          </div>
        </div>
      </div>


      <button
        class="mt-6 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
        type="button"
        disabled={createBusy}
        onclick={createVoucher}
      >
        {createBusy ? "Creating…" : "Create Voucher"}
      </button>
    </div>

    <div class="rounded-lg border bg-card">
      <div class="space-y-3 border-b px-4 py-3">
        <h3 class="font-semibold">All Vouchers</h3>
        <div class="grid gap-3 sm:grid-cols-3">
          <div>
            <label class="block text-sm font-medium" for="voucher-batch-filter"
              >Batch</label
            >
            <select
              id="voucher-batch-filter"
              class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
              bind:value={selectedBatchId}
              onchange={async () => {
                voucherOffset = 0;
                await load();
              }}
            >
              <option value="">All batches and single vouchers</option>
              {#each batches as batch (batch.id)}
                <option value={batch.id}
                  >{batch.name} · {batch.prefix} · {formatDate(
                    batch.created_at,
                  )} · {batch.count} cards</option
                >
              {/each}
            </select>
          </div>
          <div>
            <label class="block text-sm font-medium" for="voucher-status-filter"
              >Status</label
            >
            <select
              id="voucher-status-filter"
              class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm"
              bind:value={voucherStatus}
              onchange={async () => {
                voucherOffset = 0;
                await load();
              }}
            >
              <option value="active">Active</option>
              <option value="redeemed">Redeemed</option>
              <option value="voided">Voided</option>
              <option value="all">All</option>
            </select>
          </div>
          <form
            onsubmit={(event) => {
              event.preventDefault();
              applyVoucherSearch();
            }}
          >
            <label class="block text-sm font-medium" for="voucher-code-search"
              >Code search</label
            >
            <div class="mt-1 flex gap-2">
              <input
                id="voucher-code-search"
                class="min-w-0 flex-1 rounded-md border bg-background px-3 py-2 text-sm"
                placeholder="Type any code fragment"
                bind:value={codeSearchInput}
              />
              <button
                type="submit"
                class="rounded-md border px-3 py-2 text-sm hover:bg-accent"
                >Search</button
              >
              {#if codeSearch}
                <button
                  type="button"
                  class="rounded-md border px-3 py-2 text-sm hover:bg-accent"
                  onclick={() => {
                    codeSearchInput = "";
                    codeSearch = "";
                    voucherOffset = 0;
                    load();
                  }}>Clear</button
                >
              {/if}
            </div>
          </form>
        </div>
        {#if selectedBatchId}
          <div class="flex items-center justify-between gap-3 border-t pt-3">
            <p class="text-xs text-muted-foreground">
              Voids all active vouchers in the selected batch; redeemed cards remain history.
            </p>
            <button
              type="button"
              class="shrink-0 rounded-md bg-destructive px-3 py-1.5 text-sm text-destructive-foreground disabled:opacity-50"
              disabled={busy}
              onclick={voidSelectedBatch}>Void batch</button
            >
          </div>
        {/if}
      </div>
      {#if busy}
        <div class="p-4 text-center text-sm text-muted-foreground">
          Loading…
        </div>
      {:else if voucherListError}
        <div class="p-4 text-sm text-destructive">{voucherListError}</div>
      {:else if vouchers.length === 0}
        <div class="p-4 text-center text-sm text-muted-foreground">
          No vouchers yet.
        </div>
      {:else}
        <div class="overflow-x-hidden md:overflow-x-auto">
          <table class="w-full table-fixed text-sm md:table-auto">
            <thead class="max-md:hidden">
              <tr class="border-b text-left text-muted-foreground">
                <th class="px-4 py-3 font-medium">Code</th>
                <th class="px-4 py-3 font-medium">Batch</th>
                <th class="px-4 py-3 font-medium">Kind</th>
                <th class="px-4 py-3 font-medium">Status</th>
                <th class="px-4 py-3 font-medium">Value</th>
                <th class="px-4 py-3 font-medium">Created</th>
                <th class="px-4 py-3 font-medium">Redeemed</th>
                <th class="px-4 py-3 font-medium">Action</th>
              </tr>
            </thead>
            <tbody class="divide-y">
              <ResponsiveRows rows={vouchers}>
                {#snippet row(v)}
                  <tr class="hover:bg-muted/50">
                    <td class="px-4 py-3 font-mono text-xs">{v.code}</td>
                    <td class="px-4 py-3">
                      {v.batch_name && v.batch_prefix
                        ? `${v.batch_name} · ${v.batch_prefix}`
                        : "—"}
                    </td>
                    <td class="px-4 py-3">
                      <Badge
                        variant={v.kind === "sold" ? "default" : "secondary"}
                      >
                        {v.kind}
                      </Badge>
                    </td>
                    <td class="px-4 py-3">{v.status}</td>
                    <td class="px-4 py-3">
                      {formatDuration(v.seconds_value)}
                      {#if v.points_value_centi > 0}
                        · +{formatPoints(v.points_value_centi)} pts
                      {/if}
                    </td>
                    <td class="px-4 py-3 text-muted-foreground"
                      >{formatDate(v.created_at)}</td
                    >
                    <td class="px-4 py-3 text-muted-foreground">
                      {v.redeemed_at ? formatDate(v.redeemed_at) : "—"}
                    </td>
                    <td class="px-4 py-3">
                      {#if v.status === "active"}
                        <button
                          type="button"
                          class="rounded-md border border-destructive px-2 py-1 text-xs text-destructive hover:bg-destructive/10 disabled:opacity-50"
                          disabled={busy}
                          onclick={() => voidVoucher(v)}>Void</button
                        >
                      {:else}
                        <span class="text-xs text-muted-foreground">—</span>
                      {/if}
                    </td>
                  </tr>
                {/snippet}
                {#snippet card(v)}
                  <tr>
                    <td colspan="8" class="p-2">
                      <article
                        class="rounded-md border bg-muted/30 p-4"
                        aria-label={`Voucher ${v.code}`}
                      >
                        <div class="flex items-start justify-between gap-3">
                          <div class="min-w-0">
                            <h4 class="font-mono text-xs font-semibold break-all">
                              {v.code}
                            </h4>
                            <div class="mt-1 flex flex-wrap items-center gap-2">
                              <Badge
                                variant={v.kind === "sold" ? "default" : "secondary"}
                              >
                                {v.kind}
                              </Badge>
                              <span class="text-xs text-muted-foreground"
                                >{v.status}</span
                              >
                            </div>
                          </div>
                          {#if v.status === "active"}
                            <button
                              type="button"
                              class="shrink-0 rounded-md border border-destructive px-2 py-1 text-xs text-destructive hover:bg-destructive/10 disabled:opacity-50"
                              disabled={busy}
                              onclick={() => voidVoucher(v)}>Void</button
                            >
                          {/if}
                        </div>
                        <dl class="mt-4 grid gap-3 sm:grid-cols-2">
                          <div>
                            <dt class="text-xs text-muted-foreground"
                              >Batch</dt
                            >
                            <dd class="mt-1">
                              {v.batch_name && v.batch_prefix
                                ? `${v.batch_name} · ${v.batch_prefix}`
                                : "—"}
                            </dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground"
                              >Value</dt
                            >
                            <dd class="mt-1">
                              {formatDuration(v.seconds_value)}
                              {#if v.points_value_centi > 0}
                                · +{formatPoints(v.points_value_centi)} pts
                              {/if}
                            </dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground"
                              >Created</dt
                            >
                            <dd class="mt-1 text-muted-foreground"
                              >{formatDate(v.created_at)}</dd
                            >
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground"
                              >Redeemed</dt
                            >
                            <dd class="mt-1 text-muted-foreground">
                              {v.redeemed_at ? formatDate(v.redeemed_at) : "—"}
                            </dd>
                          </div>
                        </dl>
                      </article>
                    </td>
                  </tr>
                {/snippet}
              </ResponsiveRows>
            </tbody>
          </table>
        </div>
        <div class="flex items-center justify-between border-t px-4 py-3 text-sm">
          <span class="text-muted-foreground">Showing {voucherOffset + 1}–{voucherOffset + vouchers.length}</span>
          <div class="flex gap-2">
            <button
              type="button"
              class="rounded-md border px-3 py-1.5 hover:bg-accent disabled:opacity-50"
              disabled={voucherOffset === 0}
              onclick={async () => {
                voucherOffset = Math.max(0, voucherOffset - VOUCHER_PAGE_SIZE);
                await load();
              }}>Previous</button
            >
            <button
              type="button"
              class="rounded-md border px-3 py-1.5 hover:bg-accent disabled:opacity-50"
              disabled={!hasMoreVouchers}
              onclick={async () => {
                voucherOffset += VOUCHER_PAGE_SIZE;
                await load();
              }}>Next</button
            >
          </div>
        </div>
      {/if}
    </div>
  {:else if tab === "batches"}
    <div
      class="grid gap-6 xl:grid-cols-[minmax(20rem,0.8fr)_minmax(28rem,1.2fr)]"
    >
      <div class="space-y-6">
        <form
          class="rounded-lg border bg-card p-6"
          onsubmit={(event) => {
            event.preventDefault();
            createBatch();
          }}
        >
          <h3 class="mb-4 text-lg font-semibold">Generate Batch</h3>
          <div class="grid gap-4 sm:grid-cols-2">
            <div>
              <label class="text-sm font-medium" for="batch-prefix"
                >Prefix</label
              >
              <input
                id="batch-prefix"
                required
                minlength="2"
                maxlength="8"
                pattern="[A-Za-z0-9]+"
                class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm uppercase"
                bind:value={prefix}
              />
            </div>
            <div>
              <label class="text-sm font-medium" for="batch-suffix"
                >Suffix length</label
              >
              <input
                id="batch-suffix"
                required
                min="6"
                max="16"
                type="number"
                class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm"
                bind:value={suffixLength}
              />
            </div>
            <div>
              <label class="text-sm font-medium" for="batch-count">Count</label>
              <input
                id="batch-count"
                required
                min="1"
                max="1000"
                type="number"
                class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm"
                bind:value={count}
              />
            </div>
            <div class="sm:col-span-2">
              <span class="text-sm font-medium">Time (DD:HH:mm:ss)</span>
              <div class="mt-1 grid grid-cols-4 gap-2">
                <label class="text-xs text-muted-foreground">Days
                  <input min="0" max="99" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={batchDays} />
                </label>
                <label class="text-xs text-muted-foreground">Hours
                  <input min="0" max="23" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={batchHours} />
                </label>
                <label class="text-xs text-muted-foreground">Minutes
                  <input min="0" max="59" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={batchMinutes} />
                </label>
                <label class="text-xs text-muted-foreground">Seconds
                  <input min="0" max="59" type="number" class="mt-1 block w-full rounded-md border bg-background px-3 py-2 text-sm" bind:value={batchSeconds} />
                </label>
              </div>
            </div>
            <div>
              <label class="text-sm font-medium" for="batch-cash"
                >Cash amount (pesos/card)</label
              >
              <input
                id="batch-cash"
                required
                type="text"
                inputmode="decimal"
                placeholder="0.00"
                class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm"
                value={batchCashAmountPesos}
                oninput={(event) => {
                  batchCashAmountPesos = event.currentTarget.value;
                  refreshBatchAutoPoints();
                }}
              />
              <p class="mt-1 text-xs text-muted-foreground">
                {parsePesosToCents(batchCashAmountPesos) === null
                  ? "Enter a non-negative peso amount"
                  : parsePesosToCents(batchCashAmountPesos)! > 0
                    ? `Sold · ${formatMoney(parsePesosToCents(batchCashAmountPesos)!)}`
                    : "Promo"}
              </p>
            </div>
            <div>
              <div class="flex items-center justify-between gap-2">
                <label class="text-sm font-medium" for="batch-points"
                  >Points</label
                >
                {#if batchPointsAuto}
                  <span class="text-xs text-muted-foreground">Auto</span>
                {:else if economySettings}
                  <button type="button" class="text-xs text-primary hover:underline" onclick={() => {
                    batchPointsAuto = true;
                    refreshBatchAutoPoints();
                  }}>Reset to auto</button>
                {:else}
                  <span class="text-xs text-muted-foreground">Custom</span>
                {/if}
              </div>
              <input
                id="batch-points"
                required
                inputmode="decimal"
                class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm"
                value={batchPoints}
                oninput={(event) => {
                  batchPointsAuto = false;
                  batchPoints = event.currentTarget.value;
                }}
              />
              {#if !economySettings}
                <p class="mt-1 text-xs text-destructive">Auto points unavailable; enter a custom value.</p>
              {/if}
            </div>
            <div class="sm:col-span-2">
              <label class="text-sm font-medium" for="batch-expiry"
                >Expiry (optional)</label
              >
              <input
                id="batch-expiry"
                type="datetime-local"
                class="mt-1 w-full rounded-md border bg-background px-3 py-2 text-sm"
                bind:value={expiresAt}
              />
            </div>
          </div>
          <button
            class="mt-5 rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground disabled:opacity-50"
            disabled={batchBusy}
          >
            {batchBusy ? "Working…" : "Generate Batch"}
          </button>
        </form>

        <div class="rounded-lg border bg-card">
          <div class="border-b px-4 py-3 font-semibold">Batches</div>
          {#if busy}
            <div class="p-4 text-sm text-muted-foreground">Loading…</div>
          {:else if batchListError}
            <div class="p-4 text-sm text-destructive">{batchListError}</div>
          {:else if batches.length === 0}
            <div class="p-4 text-sm text-muted-foreground">No batches yet.</div>
          {:else}
            <div class="divide-y">
              {#each batches as batch (batch.id)}
                <button
                  type="button"
                  class="block w-full p-4 text-left hover:bg-muted/50"
                  onclick={() => openBatch(batch.id)}
                >
                  <div class="flex justify-between gap-4">
                    <strong>{batch.name}</strong>
                    <span>{batch.redeemed_count}/{batch.count} redeemed</span>
                  </div>
                  <div class="mt-1 text-xs text-muted-foreground">
                    {batch.prefix} · {formatDayDuration(
                      batch.seconds_value,
                    )} · {formatPoints(
                      batch.points_value_centi,
                    )} pts ·
                    {batch.kind === "sold"
                      ? formatMoney(batch.cash_amount_cents)
                      : "Promo"} ·
                    {formatDate(batch.created_at)}{batch.voided_at
                      ? " · Voided"
                      : ""}
                  </div>
                </button>
              {/each}
            </div>
            {#if hasMoreBatches}
              <button
                type="button"
                class="w-full border-t px-4 py-3 text-sm hover:bg-muted/50 disabled:opacity-50"
                disabled={loadingMoreBatches}
                onclick={loadMoreBatches}
              >
                {loadingMoreBatches ? "Loading…" : "Load older batches"}
              </button>
            {/if}
          {/if}
        </div>
      </div>

      <div class="rounded-lg border bg-card">
        {#if !detail}
          <div class="p-8 text-center text-sm text-muted-foreground">
            Select a batch to view its cards.
          </div>
        {:else}
          <div
            class="flex flex-wrap items-start justify-between gap-4 border-b p-4"
          >
            <div>
              <h3 class="text-lg font-semibold">{detail.name} batch</h3>
              <p class="text-sm text-muted-foreground">
                {detail.prefix} · {detail.redeemed_count}/{detail.count} redeemed ·
                {detail.kind} · {formatDayDuration(detail.seconds_value)} · created
                {formatDate(detail.created_at)}
              </p>
            </div>
            <div class="flex flex-wrap gap-2">
              <button
                type="button"
                class="rounded-md border px-3 py-1.5 text-sm hover:bg-accent disabled:opacity-50"
                disabled={batchBusy}
                onclick={printBatch}>Print</button
              >
              <button
                type="button"
                class="rounded-md border px-3 py-1.5 text-sm hover:bg-accent disabled:opacity-50"
                disabled={batchBusy}
                onclick={exportBatch}>Export CSV</button
              >
              {#if !detail.voided_at}
                <button
                  type="button"
                  class="rounded-md bg-destructive px-3 py-1.5 text-sm text-destructive-foreground disabled:opacity-50"
                  disabled={batchBusy}
                  onclick={voidBatch}>Void batch</button
                >
              {/if}
            </div>
          </div>
          {#if detail.voided_at}
            <div
              class="m-4 rounded-md bg-destructive/10 p-3 text-sm text-destructive"
            >
              Voided {formatDate(detail.voided_at)}. Redeemed cards remain valid
              history.
            </div>
          {/if}
          <div class="max-h-[42rem] overflow-x-hidden overflow-y-auto md:overflow-x-auto">
            <table class="w-full table-fixed text-sm md:table-auto">
              <thead class="sticky top-0 bg-card max-md:hidden">
                <tr class="border-b text-left text-muted-foreground">
                  <th class="px-4 py-3">Code</th>
                  <th class="px-4 py-3">Status</th>
                  <th class="px-4 py-3">Redeemed by</th>
                  <th class="px-4 py-3">Redeemed</th>
                </tr>
              </thead>
              <tbody class="divide-y">
                <ResponsiveRows rows={detail.vouchers}>
                  {#snippet row(v)}
                    <tr>
                      <td class="px-4 py-3 font-mono text-xs">{v.code}</td>
                      <td class="px-4 py-3">{v.status}</td>
                      <td class="px-4 py-3">{v.redeemed_by_username ?? "—"}</td
                      >
                      <td class="px-4 py-3 text-muted-foreground">
                        {v.redeemed_at ? formatDate(v.redeemed_at) : "—"}
                      </td>
                    </tr>
                  {/snippet}
                  {#snippet card(v)}
                    <tr>
                      <td colspan="4" class="p-2">
                        <article
                          class="rounded-md border bg-muted/30 p-4"
                          aria-label={`Voucher ${v.code}`}
                        >
                          <h4 class="font-mono text-xs font-semibold break-all">
                            {v.code}
                          </h4>
                          <dl class="mt-4 grid gap-3 sm:grid-cols-3">
                            <div>
                              <dt class="text-xs text-muted-foreground"
                                >Status</dt
                              >
                              <dd class="mt-1">{v.status}</dd>
                            </div>
                            <div>
                              <dt class="text-xs text-muted-foreground"
                                >Redeemed by</dt
                              >
                              <dd class="mt-1 break-words">{v.redeemed_by_username ?? "—"}</dd>
                            </div>
                            <div>
                              <dt class="text-xs text-muted-foreground"
                                >Redeemed</dt
                              >
                              <dd class="mt-1 text-muted-foreground">
                                {v.redeemed_at ? formatDate(v.redeemed_at) : "—"}
                              </dd>
                            </div>
                          </dl>
                        </article>
                      </td>
                    </tr>
                  {/snippet}
                </ResponsiveRows>
              </tbody>
            </table>
          </div>
        {/if}
      </div>
    </div>
  {:else}
    <div class="space-y-6">
      <div class="rounded-lg border bg-card p-6">
        <div class="flex flex-wrap items-start justify-between gap-4">
          <div>
            <h3 class="text-lg font-semibold">Voucher Template</h3>
            <p class="mt-1 text-sm text-muted-foreground">
              HTML + CSS repeated once per card. JavaScript is disabled.
            </p>
          </div>
          <div class="flex gap-2">
            <button
              type="button"
              class="rounded-md border px-3 py-2 text-sm hover:bg-accent disabled:opacity-50"
              disabled={templateBusy || !voucherTemplate}
              onclick={resetTemplate}>Reset to default</button
            >
            <button
              type="button"
              class="rounded-md bg-primary px-3 py-2 text-sm font-medium text-primary-foreground disabled:opacity-50"
              disabled={templateBusy || !voucherTemplate}
              onclick={() => saveTemplate()}
              >{templateBusy ? "Saving…" : "Save template"}</button
            >
          </div>
        </div>
        <p class="mt-4 text-xs text-muted-foreground">
          Tokens: <code>{"{{code}}"}</code>, <code>{"{{time}}"}</code>,
          <code>{"{{points}}"}</code>, <code>{"{{price}}"}</code>,
          <code>{"{{shop_name}}"}</code>, <code>{"{{expires_at}}"}</code>,
          <code>{"{{prefix}}"}</code>. Put <code>@page</code>, grid, and
          page-break rules in CSS.
        </p>
        <div class="mt-5 grid gap-5 xl:grid-cols-2">
          <div>
            <label class="text-sm font-medium" for="voucher-template-html"
              >Card HTML</label
            >
            <textarea
              id="voucher-template-html"
              class="mt-1 min-h-80 w-full rounded-md border bg-background p-3 font-mono text-xs"
              spellcheck="false"
              bind:value={templateHtml}
            ></textarea>
          </div>
          <div>
            <label class="text-sm font-medium" for="voucher-template-css"
              >Template CSS</label
            >
            <textarea
              id="voucher-template-css"
              class="mt-1 min-h-80 w-full rounded-md border bg-background p-3 font-mono text-xs"
              spellcheck="false"
              bind:value={templateCss}
            ></textarea>
          </div>
        </div>
      </div>
      <div class="rounded-lg border bg-card p-6">
        <h3 class="mb-4 text-lg font-semibold">Live Preview</h3>
        <iframe
          title="Voucher template live preview"
          sandbox="allow-same-origin"
          srcdoc={previewDocument}
          class="h-[32rem] w-full rounded-md border bg-white"
        ></iframe>
      </div>
    </div>
  {/if}
</div>
