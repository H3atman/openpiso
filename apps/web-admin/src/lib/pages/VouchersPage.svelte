<script lang="ts">
  import { onMount, tick } from "svelte";
  import * as api from "../api";
  import BanIcon from "@lucide/svelte/icons/ban";
  import DownloadIcon from "@lucide/svelte/icons/download";
  import LayersIcon from "@lucide/svelte/icons/layers";
  import PlayIcon from "@lucide/svelte/icons/play";
  import PrinterIcon from "@lucide/svelte/icons/printer";
  import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
  import SearchIcon from "@lucide/svelte/icons/search";
  import TicketIcon from "@lucide/svelte/icons/ticket";
  import TriangleAlertIcon from "@lucide/svelte/icons/triangle-alert";
  import { Button } from "$lib/components/ui/button";
  import { Input } from "$lib/components/ui/input";
  import ConfirmDialog from "../components/ConfirmDialog.svelte";
  import DurationFields from "../components/DurationFields.svelte";
  import LoadErrorNotice from "../components/LoadErrorNotice.svelte";
  import {
    formatMoney,
    formatDuration,
    formatProseDuration,
    formatDate,
    formatDateShort,
    formatPoints,
    parsePesosToCents,
    parsePointsToCenti,
  } from "../format";
  import type {
    AdminSettings,
    EconomySettings,
    RenderResult,
    Voucher,
    VoucherBatchDetail,
    VoucherBatchSummary,
    VoucherTemplate,
    VoucherTemplateInput,
  } from "../types";
  import { reportActionFailure, reportActionSuccess } from "../notifications";
  import ResponsiveRows from "../components/ResponsiveRows.svelte";
  import { Badge } from "$lib/components/ui/badge";
  import { buildVoucherBatchCsv } from "../voucherCsv";
  import {
    buildVoucherDocument,
    runVoucherFrame,
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

  // One preview card per price tier boundary (the default script tiers at ₱50).
  const SAMPLE_PRICES = ["₱5.00", "₱20.00", "₱50.00", "₱100.00"];

  const BATCH_PAGE_SIZE = 100;
  const VOUCHER_PAGE_SIZE = 100;

  const pageTabs = [
    { key: "vouchers", label: "Vouchers" },
    { key: "batches", label: "Batches" },
    { key: "template", label: "Template" },
  ] as const;
  let tab = $state<(typeof pageTabs)[number]["key"]>("vouchers");

  // Shared control classes, matching the Input primitive's height and focus ring.
  const SELECT_CLASS =
    "block min-h-11 w-full rounded-lg border border-input bg-background px-2.5 text-base text-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none md:h-8 md:min-h-0 md:text-sm dark:bg-input/30 dark:scheme-dark";
  const EDITOR_CLASS =
    "block h-[26rem] w-full resize-y rounded-lg border border-input bg-background p-3 font-mono text-xs leading-relaxed focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none max-md:h-80 dark:bg-input/30";

  const STATUS_STYLE: Record<Voucher["status"], { label: string; pill: string; dot: string }> = {
    active: { label: "Active", pill: "text-foreground", dot: "bg-emerald-500" },
    redeemed: { label: "Redeemed", pill: "border-transparent bg-secondary text-secondary-foreground", dot: "bg-primary" },
    voided: { label: "Voided", pill: "border-dashed text-muted-foreground", dot: "bg-muted-foreground/60" },
  };

  /** Arrow/Home/End movement across a tablist; the new tab is selected and focused. */
  function tabKeydown<K extends string>(
    event: KeyboardEvent,
    keys: readonly K[],
    current: K,
    select: (key: K) => void,
    idPrefix: string,
  ) {
    const last = keys.length - 1;
    const index = keys.indexOf(current);
    const next =
      event.key === "ArrowRight" ? (index === last ? 0 : index + 1)
      : event.key === "ArrowLeft" ? (index === 0 ? last : index - 1)
      : event.key === "Home" ? 0
      : event.key === "End" ? last
      : -1;
    if (next < 0) return;
    event.preventDefault();
    select(keys[next]);
    document.getElementById(`${idPrefix}${keys[next]}`)?.focus();
  }

  /** The kind a cash amount implies (sold when above zero), or null when unparseable. */
  function cashKind(pesos: string): { kind: Voucher["kind"]; cents: number } | null {
    const cents = parsePesosToCents(pesos);
    return cents === null ? null : { kind: cents > 0 ? "sold" : "promo", cents };
  }

  /** What the owner is about to void; the ConfirmDialog is open while set. */
  type VoidTarget =
    | { kind: "voucher"; voucher: Voucher }
    | { kind: "batch"; id: string; name: string; prefix: string };
  let voidTarget = $state<VoidTarget | null>(null);
  let voidBusy = $state(false);
  let detailHeading = $state<HTMLElement>();
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
  let templateScript = $state("");
  const editorTabs = [
    { key: "html", label: "Card HTML" },
    { key: "css", label: "Template CSS" },
    { key: "script", label: "Template Script" },
  ] as const;
  let editorTab = $state<(typeof editorTabs)[number]["key"]>("html");
  // The Template Script as of the last Run click; a fresh object per click so
  // running the same text again re-renders. The preview never runs unrun text.
  let scriptRun = $state<{ script: string } | null>(null);
  let templateBusy = $state(false);
  let previewFrame = $state<HTMLIFrameElement>();
  let previewStatus = $state<"rendering" | RenderResult["type"]>("rendering");
  let previewMessage = $state("");
  let previewHeight = $state<number | null>(null);
  let previewSeq = $state(0);
  let previewRenders = 0;
  let templateDirty = $derived(
    voucherTemplate !== null &&
      (templateHtml !== voucherTemplate.html ||
        templateCss !== voucherTemplate.css ||
        templateScript !== voucherTemplate.script),
  );
  // Whether the preview shows the Template Script as it is now in the editor.
  let scriptState = $derived(
    scriptRun === null ? "not-run" : scriptRun.script === templateScript ? "applied" : "stale",
  );
  let filtersActive = $derived(
    selectedBatchId !== "" || voucherStatus !== "active" || codeSearch !== "",
  );
  let voucherCashKind = $derived(cashKind(cashAmountPesos));
  let batchCashKind = $derived(cashKind(batchCashAmountPesos));
  // Cleaning can fail (a card loses its code); that is shown like any render error.
  let previewDocument = $derived.by<
    { srcdoc: string; error?: undefined } | { error: string; srcdoc?: undefined }
  >(() => {
    try {
      return {
        srcdoc: buildVoucherDocument(
          {
            html: templateHtml,
            css: templateCss,
            script: scriptRun?.script,
          },
          SAMPLE_PRICES.map((price) => ({
            ...SAMPLE_VALUES,
            price,
            shop_name: adminSettings?.shop_name ?? "OpenPiso Shop",
          })),
        ),
      };
    } catch (caught) {
      return { error: caughtMessage(caught) };
    }
  });

  // Re-render the sandboxed preview frame whenever the template changes.
  $effect(() => {
    const frame = previewFrame;
    const built = previewDocument;
    if (!frame) return;
    previewSeq = ++previewRenders;
    previewStatus = "rendering";
    previewMessage = "";
    if (built.error !== undefined) {
      frame.srcdoc = "";
      previewStatus = "error";
      previewMessage = built.error;
      return;
    }
    return runVoucherFrame(frame, built.srcdoc, {
      onResult(result) {
        previewStatus = result.type;
        if (result.type === "ready") previewHeight = result.height;
        else previewMessage = result.message;
      },
    });
  });

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
      templateScript = results[3].value.script;
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
    parts: VoucherTemplateInput = {
      html: templateHtml,
      css: templateCss,
      script: templateScript,
    },
    action: "save" | "reset" = "save",
  ) {
    templateBusy = true;
    try {
      voucherTemplate = await api.updateVoucherTemplate(parts);
      templateHtml = voucherTemplate.html;
      templateCss = voucherTemplate.css;
      templateScript = voucherTemplate.script;
      reportActionSuccess(action, "Voucher Template");
    } catch (caught) {
      reportActionFailure(action, "Voucher Template", caught);
    } finally {
      templateBusy = false;
    }
  }

  async function resetTemplate() {
    if (!voucherTemplate) return;
    scriptRun = null;
    await saveTemplate(
      {
        html: voucherTemplate.default_html,
        css: voucherTemplate.default_css,
        script: voucherTemplate.default_script,
      },
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
      adminSettings = freshSettings;

      // The saved template is for the print document only; the editor keeps its unsaved text.
      // Build first: a card that loses its code throws before any frame exists.
      const srcdoc = buildVoucherDocument(
        freshTemplate,
        freshDetail.vouchers.map((card) =>
          tokenValuesForCard(card, freshDetail, freshSettings.shop_name),
        ),
        { print: true },
      );

      // The frame is cross-origin, so it calls its own print() once ready.
      const printFrame = document.createElement("iframe");
      printFrame.title = `Print ${freshDetail.name} voucher batch`;
      printFrame.setAttribute("sandbox", "allow-scripts allow-modals");
      printFrame.dataset.voucherPrint = "";
      printFrame.dataset.renderStatus = "rendering";
      printFrame.style.position = "fixed";
      printFrame.style.width = "1px";
      printFrame.style.height = "1px";
      printFrame.style.border = "0";
      printFrame.style.opacity = "0";
      let stop = () => {};
      const cleanup = () => {
        stop();
        printFrame.remove();
      };
      document.body.append(printFrame);
      stop = runVoucherFrame(printFrame, srcdoc, {
        onResult(result) {
          printFrame.dataset.renderStatus = result.type;
          if (result.type === "ready") {
            window.setTimeout(cleanup, 60_000);
            return;
          }
          cleanup();
          reportActionFailure(
            "print",
            "Voucher Batch",
            new Error(result.message),
          );
        },
        onAfterPrint: cleanup,
      });
    } catch (caught) {
      reportActionFailure("print", "Voucher Batch", caught);
    } finally {
      batchBusy = false;
    }
  }

  function askVoidSelectedBatch() {
    const batch = batches.find((candidate) => candidate.id === selectedBatchId);
    if (batch)
      voidTarget = { kind: "batch", id: batch.id, name: batch.name, prefix: batch.prefix };
  }

  /** Runs the void the ConfirmDialog asked about; the outcome is a toast either way. */
  async function confirmVoid() {
    const target = voidTarget;
    if (!target) return;
    const subject = target.kind === "voucher" ? "Voucher" : "Voucher Batch";
    voidBusy = true;
    try {
      if (target.kind === "voucher") await api.voidVoucher(target.voucher.id);
      else await api.voidVoucherBatch(target.id);
      voidTarget = null;
      await load();
      reportActionSuccess("void", subject);
    } catch (caught) {
      voidTarget = null;
      reportActionFailure("void", subject, caught);
    } finally {
      voidBusy = false;
    }
  }

  /** Opens a batch from the list and moves focus to its detail (it sits below the list on narrow screens). */
  async function selectBatch(id: string) {
    await openBatch(id);
    if (detail?.id !== id) return;
    await tick();
    detailHeading?.focus();
  }

  async function clearVoucherFilters() {
    selectedBatchId = "";
    voucherStatus = "active";
    codeSearchInput = "";
    codeSearch = "";
    voucherOffset = 0;
    await load();
  }

  function batchLabel(v: Voucher): string {
    return v.batch_name && v.batch_prefix
      ? `${v.batch_name} · ${v.batch_prefix}`
      : "Single voucher";
  }

  function redeemedPercent(batch: VoucherBatchSummary): number {
    return batch.count > 0 ? Math.round((batch.redeemed_count / batch.count) * 100) : 0;
  }

  onMount(load);
</script>

{#snippet statusBadge(status: Voucher["status"])}
  {@const style = STATUS_STYLE[status]}
  <Badge variant="outline" class="gap-1.5 {style.pill}">
    <span class="size-1.5 rounded-full {style.dot}" aria-hidden="true"></span>
    {style.label}
  </Badge>
{/snippet}

{#snippet kindText(kind: Voucher["kind"])}
  <span class={kind === "sold" ? "text-foreground" : "text-muted-foreground"}>
    {kind === "sold" ? "Sold" : "Promo"}
  </span>
{/snippet}

{#snippet progress(batch: VoucherBatchSummary)}
  <span class="block h-1.5 overflow-hidden rounded-full bg-foreground/10" aria-hidden="true">
    <span class="block h-full rounded-full bg-primary" style:width="{redeemedPercent(batch)}%"></span>
  </span>
{/snippet}

{#snippet cashHint(id: string, kind: ReturnType<typeof cashKind>, perCard: boolean)}
  {#if kind === null}
    <p {id} class="mt-1.5 text-xs text-destructive">Enter a non-negative peso amount.</p>
  {:else if kind.kind === "sold"}
    <p {id} class="mt-1.5 text-xs text-muted-foreground">
      Sold · {formatMoney(kind.cents)}{perCard ? " per card" : ""}
    </p>
  {:else}
    <p {id} class="mt-1.5 text-xs text-muted-foreground">Promo · no cash collected</p>
  {/if}
{/snippet}

<div class="space-y-6">
  <div class="flex items-start justify-between gap-4">
    <div class="min-w-0">
      <h2 class="text-2xl font-bold tracking-tight">Vouchers</h2>
      <p class="text-sm text-muted-foreground">
        Create single Vouchers, generate printable Voucher Batches, and design the printed card.
      </p>
    </div>
    <Button variant="outline" onclick={load}>
      <RefreshCwIcon aria-hidden="true" />
      Refresh
    </Button>
  </div>

  <div
    class="flex gap-1 border-b"
    role="tablist"
    aria-label="Voucher management"
  >
    {#each pageTabs as item (item.key)}
      <button
        type="button"
        role="tab"
        id="vouchers-tab-{item.key}"
        aria-selected={tab === item.key}
        aria-controls={tab === item.key ? `vouchers-panel-${item.key}` : undefined}
        tabindex={tab === item.key ? 0 : -1}
        class="-mb-px border-b-2 px-4 py-2 text-sm font-medium whitespace-nowrap transition-colors focus-visible:rounded-t-md focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none {tab === item.key
          ? 'border-primary text-foreground'
          : 'border-transparent text-muted-foreground hover:text-foreground'}"
        onclick={() => (tab = item.key)}
        onkeydown={(event) =>
          tabKeydown(event, pageTabs.map((t) => t.key), tab, (key) => (tab = key), "vouchers-tab-")}
        >{item.label}</button
      >
    {/each}
  </div>

  {#if auxiliaryLoadError}
    <div
      class="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
      role="alert"
    >
      {auxiliaryLoadError}
    </div>
  {/if}

  <div
    id="vouchers-panel-{tab}"
    role="tabpanel"
    aria-labelledby="vouchers-tab-{tab}"
    class="space-y-6"
  >
  {#if tab === "vouchers"}
    <form
      class="rounded-lg border bg-card"
      aria-labelledby="create-voucher-heading"
      onsubmit={(event) => {
        event.preventDefault();
        createVoucher();
      }}
    >
      <div class="border-b px-4 py-3">
        <h3 id="create-voucher-heading" class="font-semibold">Create Voucher</h3>
        <p class="text-sm text-muted-foreground">
          One code to sell at the counter or give away. Cash above ₱0 makes it a sold Voucher.
        </p>
      </div>
      <div class="grid gap-x-4 gap-y-5 p-4 sm:grid-cols-2 lg:grid-cols-4">
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="voucher-prefix">Prefix</label>
          <Input
            id="voucher-prefix"
            required
            minlength={2}
            maxlength={8}
            pattern="[A-Za-z0-9]+"
            autocomplete="off"
            class="uppercase"
            aria-describedby="voucher-prefix-hint"
            bind:value={voucherPrefix}
          />
          <p id="voucher-prefix-hint" class="mt-1.5 text-xs text-muted-foreground">2–8 letters or numbers</p>
        </div>
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="voucher-suffix">Suffix length</label>
          <Input
            id="voucher-suffix"
            required
            min="6"
            max="16"
            type="number"
            inputmode="numeric"
            class="tabular-nums"
            aria-describedby="voucher-suffix-hint"
            bind:value={voucherSuffixLength}
          />
          <p id="voucher-suffix-hint" class="mt-1.5 text-xs text-muted-foreground">6–16 random characters</p>
        </div>
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="voucher-cash">Cash amount (₱)</label>
          <Input
            id="voucher-cash"
            type="text"
            inputmode="decimal"
            placeholder="0.00"
            class="tabular-nums"
            aria-describedby="voucher-cash-hint"
            aria-invalid={voucherCashKind === null ? true : undefined}
            value={cashAmountPesos}
            oninput={(event: Event & { currentTarget: HTMLInputElement }) => {
              cashAmountPesos = event.currentTarget.value;
              refreshVoucherAutoPoints();
            }}
          />
          {@render cashHint("voucher-cash-hint", voucherCashKind, false)}
        </div>
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="voucher-points">Points</label>
          <Input
            id="voucher-points"
            type="text"
            inputmode="decimal"
            placeholder="e.g. 1.50"
            class="tabular-nums"
            aria-describedby="voucher-points-hint"
            value={pointsValue}
            oninput={(event: Event & { currentTarget: HTMLInputElement }) => {
              voucherPointsAuto = false;
              pointsValue = event.currentTarget.value;
            }}
          />
          <p id="voucher-points-hint" class="mt-1.5 text-xs {economySettings ? 'text-muted-foreground' : 'text-destructive'}">
            {#if voucherPointsAuto}
              Auto from the earn rate
            {:else if economySettings}
              Custom value ·
              <button
                type="button"
                class="font-medium text-primary underline-offset-2 hover:underline focus-visible:underline focus-visible:outline-none"
                onclick={() => {
                  voucherPointsAuto = true;
                  refreshVoucherAutoPoints();
                }}>Reset to auto</button
              >
            {:else}
              Auto points unavailable; enter a custom value.
            {/if}
          </p>
        </div>
        <DurationFields
          idPrefix="voucher-time"
          class="sm:col-span-2"
          bind:days={voucherDays}
          bind:hours={voucherHours}
          bind:minutes={voucherMinutes}
          bind:seconds={voucherSeconds}
        />
      </div>
      <div class="flex flex-wrap items-center justify-end gap-3 border-t px-4 py-3">
        <Button type="submit" class="max-md:w-full" disabled={createBusy}>
          {createBusy ? "Creating…" : "Create Voucher"}
        </Button>
      </div>
    </form>

    <section class="rounded-lg border bg-card" aria-labelledby="all-vouchers-heading">
      <div class="space-y-3 border-b px-4 py-3">
        <h3 id="all-vouchers-heading" class="font-semibold">All Vouchers</h3>
        <div class="grid gap-3 md:grid-cols-[minmax(0,1fr)_minmax(0,16rem)_minmax(0,10rem)] md:items-end">
          <form
            class="min-w-0"
            role="search"
            onsubmit={(event) => {
              event.preventDefault();
              applyVoucherSearch();
            }}
          >
            <label class="mb-1.5 block text-xs font-medium text-muted-foreground" for="voucher-code-search"
              >Code search</label
            >
            <div class="flex gap-2">
              <div class="relative min-w-0 flex-1">
                <SearchIcon
                  class="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
                  aria-hidden="true"
                />
                <Input
                  id="voucher-code-search"
                  class="pl-8"
                  placeholder="Any part of a code"
                  autocomplete="off"
                  bind:value={codeSearchInput}
                />
              </div>
              <Button type="submit" variant="outline">Search</Button>
              {#if codeSearch}
                <Button
                  type="button"
                  variant="ghost"
                  onclick={() => {
                    codeSearchInput = "";
                    codeSearch = "";
                    voucherOffset = 0;
                    load();
                  }}>Clear</Button
                >
              {/if}
            </div>
          </form>
          <div class="min-w-0">
            <label class="mb-1.5 block text-xs font-medium text-muted-foreground" for="voucher-batch-filter"
              >Batch</label
            >
            <select
              id="voucher-batch-filter"
              class={SELECT_CLASS}
              bind:value={selectedBatchId}
              onchange={async () => {
                voucherOffset = 0;
                await load();
              }}
            >
              <option value="">All vouchers</option>
              {#each batches as batch (batch.id)}
                <option value={batch.id}
                  >{batch.name} · {batch.prefix} · {formatDate(
                    batch.created_at,
                  )} · {batch.count} cards</option
                >
              {/each}
            </select>
          </div>
          <div class="min-w-0">
            <label class="mb-1.5 block text-xs font-medium text-muted-foreground" for="voucher-status-filter"
              >Status</label
            >
            <select
              id="voucher-status-filter"
              class={SELECT_CLASS}
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
        </div>
        {#if selectedBatchId}
          <div class="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-dashed px-3 py-2">
            <p class="min-w-0 text-xs text-muted-foreground">
              Voiding this batch stops every active card in it; redeemed cards stay as history.
            </p>
            <Button
              type="button"
              variant="destructive"
              size="sm"
              class="max-md:w-full"
              disabled={busy || voidBusy}
              onclick={askVoidSelectedBatch}
            >
              <BanIcon aria-hidden="true" />
              Void batch
            </Button>
          </div>
        {/if}
      </div>
      {#if busy}
        <p class="px-4 py-10 text-center text-sm text-muted-foreground" role="status">
          Loading vouchers…
        </p>
      {:else if voucherListError}
        <div class="p-4">
          <LoadErrorNotice
            message="Vouchers could not be loaded"
            detail={voucherListError}
            onRetry={load}
          />
        </div>
      {:else if vouchers.length === 0}
        <div class="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <span class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
            <TicketIcon class="size-5" aria-hidden="true" />
          </span>
          {#if filtersActive}
            <p class="font-medium">No vouchers match these filters</p>
            <p class="max-w-sm text-sm text-muted-foreground">
              Try another code, batch, or status.
            </p>
            <Button variant="outline" class="mt-2" onclick={clearVoucherFilters}>Clear filters</Button>
          {:else}
            <p class="font-medium">No active vouchers</p>
            <p class="max-w-sm text-sm text-muted-foreground">
              Create one above, or generate a Voucher Batch on the Batches tab for printed cards.
            </p>
          {/if}
        </div>
      {:else}
        <div class="md:overflow-x-auto">
          <table class="w-full text-sm">
            <caption class="sr-only">Vouchers</caption>
            <thead class="max-md:hidden">
              <tr class="border-b text-left text-xs text-muted-foreground">
                <th scope="col" class="px-4 py-2 font-medium">Code</th>
                <th scope="col" class="px-3 py-2 font-medium">Kind</th>
                <th scope="col" class="px-3 py-2 font-medium">Status</th>
                <th scope="col" class="px-3 py-2 font-medium">Value</th>
                <th scope="col" class="px-3 py-2 font-medium">Created</th>
                <th scope="col" class="px-3 py-2 font-medium">Redeemed</th>
                <th scope="col" class="px-4 py-2 text-right font-medium"><span class="sr-only">Actions</span></th>
              </tr>
            </thead>
            <tbody class="divide-y">
              <ResponsiveRows rows={vouchers}>
                {#snippet row(v)}
                  <tr class="hover:bg-muted/50">
                    <th scope="row" class="px-4 py-2.5 text-left font-normal">
                      <span class="block font-mono text-sm font-medium whitespace-nowrap">{v.code}</span>
                      <span class="block text-xs text-muted-foreground">{batchLabel(v)}</span>
                    </th>
                    <td class="px-3 py-2.5">{@render kindText(v.kind)}</td>
                    <td class="px-3 py-2.5">{@render statusBadge(v.status)}</td>
                    <td class="px-3 py-2.5 tabular-nums">
                      <span class="block font-mono whitespace-nowrap">{formatDuration(v.seconds_value)}</span>
                      {#if v.points_value_centi > 0}
                        <span class="block text-xs whitespace-nowrap text-muted-foreground">+{formatPoints(v.points_value_centi)} pts</span>
                      {/if}
                    </td>
                    <td class="px-3 py-2.5 whitespace-nowrap text-muted-foreground tabular-nums">
                      {formatDate(v.created_at)}
                    </td>
                    <td class="px-3 py-2.5 tabular-nums">
                      {#if v.redeemed_at}
                        <span class="block whitespace-nowrap">{formatDate(v.redeemed_at)}</span>
                        {#if v.redeemed_by_username}
                          <span class="block text-xs text-muted-foreground">by {v.redeemed_by_username}</span>
                        {/if}
                      {:else}
                        <span class="text-muted-foreground" aria-hidden="true">—</span>
                        <span class="sr-only">Not redeemed</span>
                      {/if}
                    </td>
                    <td class="px-4 py-2.5 text-right">
                      {#if v.status === "active"}
                        <Button
                          variant="destructive"
                          size="sm"
                          aria-label="Void {v.code}"
                          disabled={busy || voidBusy}
                          onclick={() => (voidTarget = { kind: "voucher", voucher: v })}>Void</Button
                        >
                      {/if}
                    </td>
                  </tr>
                {/snippet}
                {#snippet card(v)}
                  <tr>
                    <td colspan="7" class="px-4 py-3">
                      <article aria-label={`Voucher ${v.code}`}>
                        <div class="flex items-start justify-between gap-3">
                          <div class="min-w-0">
                            <h4 class="font-mono text-sm font-semibold break-all">{v.code}</h4>
                            <p class="mt-0.5 text-xs text-muted-foreground">{batchLabel(v)}</p>
                          </div>
                          {@render statusBadge(v.status)}
                        </div>
                        <dl class="mt-3 grid grid-cols-2 gap-x-4 gap-y-2.5 text-sm">
                          <div>
                            <dt class="text-xs text-muted-foreground">Value</dt>
                            <dd class="tabular-nums">
                              <span class="font-mono">{formatDuration(v.seconds_value)}</span>
                              {#if v.points_value_centi > 0}
                                <span class="text-muted-foreground">· +{formatPoints(v.points_value_centi)} pts</span>
                              {/if}
                            </dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground">Kind</dt>
                            <dd>{@render kindText(v.kind)}</dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground">Created</dt>
                            <dd class="tabular-nums">{formatDate(v.created_at)}</dd>
                          </div>
                          <div>
                            <dt class="text-xs text-muted-foreground">Redeemed</dt>
                            <dd class="tabular-nums break-words">
                              {#if v.redeemed_at}
                                {formatDate(v.redeemed_at)}{v.redeemed_by_username ? ` by ${v.redeemed_by_username}` : ""}
                              {:else}
                                <span class="text-muted-foreground">Not yet</span>
                              {/if}
                            </dd>
                          </div>
                        </dl>
                        {#if v.status === "active"}
                          <Button
                            variant="destructive"
                            class="mt-3 w-full"
                            aria-label="Void {v.code}"
                            disabled={busy || voidBusy}
                            onclick={() => (voidTarget = { kind: "voucher", voucher: v })}>Void</Button
                          >
                        {/if}
                      </article>
                    </td>
                  </tr>
                {/snippet}
              </ResponsiveRows>
            </tbody>
          </table>
        </div>
        <div class="flex flex-wrap items-center justify-between gap-3 border-t px-4 py-3 text-sm">
          <span class="text-muted-foreground tabular-nums">Showing {voucherOffset + 1}–{voucherOffset + vouchers.length}</span>
          {#if voucherOffset > 0 || hasMoreVouchers}
            <div class="flex gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={voucherOffset === 0}
                onclick={async () => {
                  voucherOffset = Math.max(0, voucherOffset - VOUCHER_PAGE_SIZE);
                  await load();
                }}>Previous</Button
              >
              <Button
                variant="outline"
                size="sm"
                disabled={!hasMoreVouchers}
                onclick={async () => {
                  voucherOffset += VOUCHER_PAGE_SIZE;
                  await load();
                }}>Next</Button
              >
            </div>
          {/if}
        </div>
      {/if}
    </section>
  {:else if tab === "batches"}
    <form
      class="rounded-lg border bg-card"
      aria-labelledby="generate-batch-heading"
      onsubmit={(event) => {
        event.preventDefault();
        createBatch();
      }}
    >
      <div class="border-b px-4 py-3">
        <h3 id="generate-batch-heading" class="font-semibold">Generate Batch</h3>
        <p class="text-sm text-muted-foreground">
          Up to 1,000 Vouchers with the same value, ready to print or export.
        </p>
      </div>
      <div class="grid gap-x-4 gap-y-5 p-4 sm:grid-cols-2 lg:grid-cols-4">
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="batch-prefix">Prefix</label>
          <Input
            id="batch-prefix"
            required
            minlength={2}
            maxlength={8}
            pattern="[A-Za-z0-9]+"
            autocomplete="off"
            placeholder="e.g. REG"
            class="uppercase placeholder:normal-case"
            aria-describedby="batch-prefix-hint"
            bind:value={prefix}
          />
          <p id="batch-prefix-hint" class="mt-1.5 text-xs text-muted-foreground">2–8 letters or numbers</p>
        </div>
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="batch-suffix">Suffix length</label>
          <Input
            id="batch-suffix"
            required
            min="6"
            max="16"
            type="number"
            inputmode="numeric"
            class="tabular-nums"
            aria-describedby="batch-suffix-hint"
            bind:value={suffixLength}
          />
          <p id="batch-suffix-hint" class="mt-1.5 text-xs text-muted-foreground">6–16 random characters</p>
        </div>
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="batch-count">Count</label>
          <Input
            id="batch-count"
            required
            min="1"
            max="1000"
            type="number"
            inputmode="numeric"
            class="tabular-nums"
            aria-describedby="batch-count-hint"
            bind:value={count}
          />
          <p id="batch-count-hint" class="mt-1.5 text-xs text-muted-foreground">1–1,000 cards</p>
        </div>
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="batch-expiry">Expiry</label>
          <Input
            id="batch-expiry"
            type="datetime-local"
            class="dark:scheme-dark"
            aria-describedby="batch-expiry-hint"
            bind:value={expiresAt}
          />
          <p id="batch-expiry-hint" class="mt-1.5 text-xs text-muted-foreground">Optional; leave empty for no expiry</p>
        </div>
        <DurationFields
          idPrefix="batch-time"
          class="sm:col-span-2"
          bind:days={batchDays}
          bind:hours={batchHours}
          bind:minutes={batchMinutes}
          bind:seconds={batchSeconds}
        />
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="batch-cash">Cash per card (₱)</label>
          <Input
            id="batch-cash"
            required
            type="text"
            inputmode="decimal"
            placeholder="0.00"
            class="tabular-nums"
            aria-describedby="batch-cash-hint"
            aria-invalid={batchCashKind === null ? true : undefined}
            value={batchCashAmountPesos}
            oninput={(event: Event & { currentTarget: HTMLInputElement }) => {
              batchCashAmountPesos = event.currentTarget.value;
              refreshBatchAutoPoints();
            }}
          />
          {@render cashHint("batch-cash-hint", batchCashKind, true)}
        </div>
        <div class="min-w-0">
          <label class="mb-1.5 block text-sm font-medium" for="batch-points">Points</label>
          <Input
            id="batch-points"
            required
            inputmode="decimal"
            class="tabular-nums"
            aria-describedby="batch-points-hint"
            value={batchPoints}
            oninput={(event: Event & { currentTarget: HTMLInputElement }) => {
              batchPointsAuto = false;
              batchPoints = event.currentTarget.value;
            }}
          />
          <p id="batch-points-hint" class="mt-1.5 text-xs {economySettings ? 'text-muted-foreground' : 'text-destructive'}">
            {#if batchPointsAuto}
              Auto from the earn rate
            {:else if economySettings}
              Custom value ·
              <button
                type="button"
                class="font-medium text-primary underline-offset-2 hover:underline focus-visible:underline focus-visible:outline-none"
                onclick={() => {
                  batchPointsAuto = true;
                  refreshBatchAutoPoints();
                }}>Reset to auto</button
              >
            {:else}
              Auto points unavailable; enter a custom value.
            {/if}
          </p>
        </div>
      </div>
      <div class="flex flex-wrap items-center justify-end gap-3 border-t px-4 py-3">
        <Button type="submit" class="max-md:w-full" disabled={batchBusy}>
          {batchBusy ? "Working…" : "Generate Batch"}
        </Button>
      </div>
    </form>

    <div
      class="grid items-start gap-6 {detail || batches.length > 0
        ? 'xl:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]'
        : ''}"
    >
      <section class="min-w-0 rounded-lg border bg-card" aria-labelledby="batches-heading">
        <div class="flex items-baseline justify-between gap-3 border-b px-4 py-3">
          <h3 id="batches-heading" class="font-semibold">Batches</h3>
          {#if batches.length > 0}
            <p class="text-xs text-muted-foreground tabular-nums">{batches.length} shown</p>
          {/if}
        </div>
        {#if busy}
          <p class="px-4 py-10 text-center text-sm text-muted-foreground" role="status">Loading batches…</p>
        {:else if batchListError}
          <div class="p-4">
            <LoadErrorNotice
              message="Voucher Batches could not be loaded"
              detail={batchListError}
              onRetry={load}
            />
          </div>
        {:else if batches.length === 0}
          <div class="flex flex-col items-center gap-2 px-6 py-12 text-center">
            <span class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
              <LayersIcon class="size-5" aria-hidden="true" />
            </span>
            <p class="font-medium">No batches yet</p>
            <p class="max-w-sm text-sm text-muted-foreground">
              Generate one above to print a sheet of Voucher cards or export them as CSV.
            </p>
          </div>
        {:else}
          <ul class="divide-y">
            {#each batches as batch (batch.id)}
              {@const selected = detail?.id === batch.id}
              <li>
                <button
                  type="button"
                  aria-current={selected ? "true" : undefined}
                  class="block w-full px-4 py-3 text-left transition-colors hover:bg-muted/50 focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none focus-visible:ring-inset {selected
                    ? 'bg-muted/60 shadow-[inset_3px_0_0_var(--color-primary)]'
                    : ''}"
                  onclick={() => selectBatch(batch.id)}
                >
                  <span class="flex items-baseline justify-between gap-3">
                    <span class="min-w-0 truncate font-medium">{batch.name}</span>{" "}
                    <span class="shrink-0 text-sm text-muted-foreground tabular-nums"
                      ><span class="font-medium text-foreground">{batch.redeemed_count}</span>/{batch.count} redeemed</span
                    >
                  </span>
                  <span class="mt-2 block">{@render progress(batch)}</span>
                  <span class="mt-2 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs text-muted-foreground tabular-nums">
                    <span class="font-mono text-foreground">{batch.prefix}</span>
                    <span aria-hidden="true">·</span>
                    <span>{formatProseDuration(batch.seconds_value)}</span>
                    <span aria-hidden="true">·</span>
                    <span>{formatPoints(batch.points_value_centi)} pts</span>
                    <span aria-hidden="true">·</span>
                    <span>{batch.kind === "sold" ? formatMoney(batch.cash_amount_cents) : "Promo"}</span>
                    <span aria-hidden="true">·</span>
                    <span>{formatDate(batch.created_at)}</span>
                    {#if batch.voided_at}
                      {@render statusBadge("voided")}
                    {/if}
                  </span>
                </button>
              </li>
            {/each}
          </ul>
          {#if hasMoreBatches}
            <div class="border-t p-3">
              <Button
                variant="ghost"
                class="w-full"
                disabled={loadingMoreBatches}
                onclick={loadMoreBatches}
              >
                {loadingMoreBatches ? "Loading…" : "Load older batches"}
              </Button>
            </div>
          {/if}
        {/if}
      </section>

      {#if detail || batches.length > 0}
        <section
          class="min-w-0 rounded-lg border bg-card"
          aria-labelledby={detail ? "voucher-batch-detail-heading" : undefined}
          aria-label={detail ? undefined : "Batch detail"}
        >
          {#if !detail}
            <div class="flex flex-col items-center gap-2 px-6 py-12 text-center">
              <span class="flex size-10 items-center justify-center rounded-full bg-muted text-muted-foreground">
                <PrinterIcon class="size-5" aria-hidden="true" />
              </span>
              <p class="font-medium">No batch selected</p>
              <p class="max-w-sm text-sm text-muted-foreground">
                Choose a batch to see its cards, print them, or export a CSV.
              </p>
            </div>
          {:else}
            <div class="space-y-4 border-b p-4">
              <div class="flex flex-wrap items-start justify-between gap-3">
                <div class="min-w-0">
                  <h3
                    id="voucher-batch-detail-heading"
                    bind:this={detailHeading}
                    tabindex="-1"
                    class="text-lg font-semibold outline-none max-md:break-words"
                  >
                    {detail.name} batch
                  </h3>
                  <p class="text-sm text-muted-foreground">
                    <span class="font-mono">{detail.prefix}</span> · created {formatDate(detail.created_at)}
                  </p>
                </div>
                <div class="flex flex-wrap gap-2 max-md:w-full">
                  <Button variant="outline" class="max-md:flex-1" disabled={batchBusy} onclick={printBatch}>
                    <PrinterIcon aria-hidden="true" />
                    Print
                  </Button>
                  <Button variant="outline" class="max-md:flex-1" disabled={batchBusy} onclick={exportBatch}>
                    <DownloadIcon aria-hidden="true" />
                    Export CSV
                  </Button>
                  {#if !detail.voided_at}
                    <Button
                      variant="destructive"
                      class="max-md:flex-1"
                      disabled={batchBusy || voidBusy}
                      onclick={() => {
                        if (detail)
                          voidTarget = { kind: "batch", id: detail.id, name: detail.name, prefix: detail.prefix };
                      }}
                    >
                      <BanIcon aria-hidden="true" />
                      Void batch
                    </Button>
                  {/if}
                </div>
              </div>
              <dl class="grid grid-cols-2 gap-3 sm:grid-cols-4">
                <div class="rounded-md bg-muted/60 px-3 py-2.5">
                  <dt class="text-xs text-muted-foreground">Redeemed</dt>
                  <dd class="mt-0.5 font-semibold tabular-nums">{detail.redeemed_count} / {detail.count}</dd>
                  <dd class="mt-1.5">{@render progress(detail)}</dd>
                </div>
                <div class="rounded-md bg-muted/60 px-3 py-2.5">
                  <dt class="text-xs text-muted-foreground">Time per card</dt>
                  <dd class="mt-0.5 font-semibold tabular-nums">{formatProseDuration(detail.seconds_value)}</dd>
                  <dd class="text-xs text-muted-foreground tabular-nums">{formatPoints(detail.points_value_centi)} pts</dd>
                </div>
                <div class="rounded-md bg-muted/60 px-3 py-2.5">
                  <dt class="text-xs text-muted-foreground">Price per card</dt>
                  <dd class="mt-0.5 font-semibold tabular-nums">
                    {detail.kind === "sold" ? formatMoney(detail.cash_amount_cents) : "Promo"}
                  </dd>
                  <dd class="text-xs text-muted-foreground">{detail.kind === "sold" ? "Sold" : "No cash collected"}</dd>
                </div>
                <div class="rounded-md bg-muted/60 px-3 py-2.5">
                  <dt class="text-xs text-muted-foreground">Expires</dt>
                  <dd class="mt-0.5 font-semibold tabular-nums">
                    {detail.expires_at ? formatDateShort(detail.expires_at) : "Never"}
                  </dd>
                </div>
              </dl>
              {#if detail.voided_at}
                <p
                  class="flex gap-2 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive"
                  role="status"
                >
                  <BanIcon class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
                  <span>Voided {formatDate(detail.voided_at)}. Redeemed cards remain valid history.</span>
                </p>
              {/if}
            </div>
            <div class="max-h-[42rem] overflow-y-auto md:overflow-x-auto">
              <table class="w-full text-sm">
                <caption class="sr-only">Vouchers in the {detail.name} batch</caption>
                <thead class="sticky top-0 z-10 bg-card max-md:hidden">
                  <tr class="border-b text-left text-xs text-muted-foreground">
                    <th scope="col" class="px-4 py-2 font-medium">Code</th>
                    <th scope="col" class="px-3 py-2 font-medium">Status</th>
                    <th scope="col" class="px-3 py-2 font-medium">Redeemed by</th>
                    <th scope="col" class="px-4 py-2 font-medium">Redeemed</th>
                  </tr>
                </thead>
                <tbody class="divide-y">
                  <ResponsiveRows rows={detail.vouchers}>
                    {#snippet row(v)}
                      <tr class="hover:bg-muted/50">
                        <th scope="row" class="px-4 py-2.5 text-left font-mono text-sm font-medium whitespace-nowrap">{v.code}</th>
                        <td class="px-3 py-2.5">{@render statusBadge(v.status)}</td>
                        <td class="px-3 py-2.5 break-words">
                          {#if v.redeemed_by_username}
                            {v.redeemed_by_username}
                          {:else}
                            <span class="text-muted-foreground" aria-hidden="true">—</span>
                            <span class="sr-only">No one</span>
                          {/if}
                        </td>
                        <td class="px-4 py-2.5 whitespace-nowrap text-muted-foreground tabular-nums">
                          {#if v.redeemed_at}
                            {formatDate(v.redeemed_at)}
                          {:else}
                            <span aria-hidden="true">—</span>
                            <span class="sr-only">Not redeemed</span>
                          {/if}
                        </td>
                      </tr>
                    {/snippet}
                    {#snippet card(v)}
                      <tr>
                        <td colspan="4" class="px-4 py-3">
                          <article aria-label={`Voucher ${v.code}`}>
                            <div class="flex items-start justify-between gap-3">
                              <h4 class="min-w-0 font-mono text-sm font-semibold break-all">{v.code}</h4>
                              {@render statusBadge(v.status)}
                            </div>
                            {#if v.redeemed_at}
                              <p class="mt-1 text-xs text-muted-foreground break-words tabular-nums">
                                Redeemed {formatDate(v.redeemed_at)}{v.redeemed_by_username ? ` by ${v.redeemed_by_username}` : ""}
                              </p>
                            {/if}
                          </article>
                        </td>
                      </tr>
                    {/snippet}
                  </ResponsiveRows>
                </tbody>
              </table>
            </div>
          {/if}
        </section>
      {/if}
    </div>
  {:else}
    <section class="rounded-lg border bg-card" aria-labelledby="voucher-template-heading">
      <div class="flex flex-wrap items-start justify-between gap-4 px-4 py-3">
        <div class="min-w-0 max-w-2xl">
          <h3 id="voucher-template-heading" class="font-semibold">Voucher Template</h3>
          <p class="text-sm text-muted-foreground">
            Card HTML and Template CSS repeat once per card; the optional Template Script runs
            once per preview or print.
          </p>
        </div>
        <div class="flex flex-wrap items-center gap-2 max-md:w-full">
          <p class="text-xs font-medium text-amber-700 empty:hidden max-md:w-full dark:text-amber-300" role="status">
            {templateDirty ? "Unsaved changes" : ""}
          </p>
          <Button
            variant="outline"
            class="max-md:flex-1"
            disabled={templateBusy || !voucherTemplate}
            onclick={resetTemplate}>Reset to default</Button
          >
          <Button
            class="max-md:flex-1"
            disabled={templateBusy || !voucherTemplate}
            onclick={() => saveTemplate()}
            >{templateBusy ? "Saving…" : "Save template"}</Button
          >
        </div>
      </div>
      <div class="border-t px-4 py-3 text-xs leading-6 text-muted-foreground">
        <span class="font-medium text-foreground">Tokens</span>
        {#each ["code", "time", "points", "price", "shop_name", "expires_at", "prefix"] as token (token)}
          {" "}<code class="rounded-md bg-muted px-1.5 py-0.5 font-mono text-foreground">{`{{${token}}}`}</code>
        {/each}
        <span class="block">
          Put <code class="font-mono text-foreground">@page</code>, grid, and page-break rules in Template CSS.
        </span>
      </div>
    </section>

    <div class="grid items-start gap-6 xl:grid-cols-[minmax(0,5fr)_minmax(0,7fr)]">
      <section data-template-editors class="min-w-0 rounded-lg border bg-card" aria-label="Template editors">
        <div class="border-b p-2">
          <div
            role="tablist"
            aria-label="Template part"
            class="flex flex-wrap gap-1 rounded-lg bg-muted p-1"
          >
            {#each editorTabs as item (item.key)}
              <button
                type="button"
                role="tab"
                id="voucher-template-tab-{item.key}"
                aria-selected={editorTab === item.key}
                aria-controls="voucher-template-panel-{item.key}"
                tabindex={editorTab === item.key ? 0 : -1}
                class="flex-1 rounded-md px-2 py-1.5 text-sm font-medium whitespace-nowrap transition-colors focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none md:px-3 {editorTab === item.key
                  ? 'bg-background text-foreground shadow-sm dark:bg-input/50'
                  : 'text-muted-foreground hover:text-foreground'}"
                onclick={() => (editorTab = item.key)}
                onkeydown={(event) =>
                  tabKeydown(event, editorTabs.map((t) => t.key), editorTab, (key) => (editorTab = key), "voucher-template-tab-")}
                >{item.label}</button
              >
            {/each}
          </div>
        </div>
        <!-- All three editors stay mounted so unsaved edits survive tab switches;
             each tabpanel wraps its textarea so the textarea keeps its textbox role. -->
        <div
          id="voucher-template-panel-html"
          role="tabpanel"
          aria-labelledby="voucher-template-tab-html"
          class="p-4"
          hidden={editorTab !== "html"}
        >
          <textarea
            id="voucher-template-html"
            aria-label="Card HTML"
            class={EDITOR_CLASS}
            spellcheck="false"
            bind:value={templateHtml}
          ></textarea>
        </div>
        <div
          id="voucher-template-panel-css"
          role="tabpanel"
          aria-labelledby="voucher-template-tab-css"
          class="p-4"
          hidden={editorTab !== "css"}
        >
          <textarea
            id="voucher-template-css"
            aria-label="Template CSS"
            class={EDITOR_CLASS}
            spellcheck="false"
            bind:value={templateCss}
          ></textarea>
        </div>
        <div
          id="voucher-template-panel-script"
          role="tabpanel"
          aria-labelledby="voucher-template-tab-script"
          class="p-4"
          hidden={editorTab !== "script"}
        >
          <p
            role="note"
            class="mb-3 flex gap-2 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-900 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-100"
          >
            <TriangleAlertIcon class="mt-px size-3.5 shrink-0" aria-hidden="true" />
            <span>A Template Script can send voucher codes off this PC. Only paste code you trust.</span>
          </p>
          <textarea
            id="voucher-template-script"
            aria-label="Template Script"
            aria-describedby="voucher-template-script-hint"
            class={EDITOR_CLASS}
            spellcheck="false"
            placeholder="Optional JavaScript. Read card data from the cards, for example data-price=&quot;&#123;&#123;price&#125;&#125;&quot;."
            bind:value={templateScript}
          ></textarea>
          <p id="voucher-template-script-hint" class="mt-1.5 text-xs text-muted-foreground">
            Run applies it to the preview; print always runs the saved script.
          </p>
        </div>
      </section>
      <section
        data-template-preview
        class="min-w-0 rounded-lg border bg-card xl:sticky xl:top-4"
        aria-labelledby="voucher-preview-heading"
      >
        <div class="flex items-center justify-between gap-3 border-b px-4 py-2.5">
          <h3 id="voucher-preview-heading" class="font-semibold">Live Preview</h3>
          <Button
            variant="outline"
            size="sm"
            aria-describedby="voucher-preview-script-state"
            onclick={() => (scriptRun = { script: templateScript })}
          >
            <PlayIcon aria-hidden="true" />
            Run
          </Button>
        </div>
        <div class="p-3">
          <div
            data-voucher-render="preview"
            data-render-status={previewStatus}
            data-render-seq={previewSeq}
            class="rounded-md bg-neutral-200 p-3 dark:bg-neutral-700"
          >
            <iframe
              bind:this={previewFrame}
              title="Voucher template live preview"
              sandbox="allow-scripts allow-modals"
              style:height={previewHeight === null ? undefined : `${previewHeight}px`}
              class="w-full"
            ></iframe>
            {#if previewMessage}
              <p
                role="alert"
                data-render-error
                class="mt-3 rounded-md bg-card px-3 py-2 text-sm text-destructive"
              >
                {previewMessage}
              </p>
            {/if}
          </div>
        </div>
        <p
          id="voucher-preview-script-state"
          class="flex flex-wrap items-center gap-x-1.5 border-t px-4 py-2.5 text-xs text-muted-foreground"
          role="status"
        >
          <span>Sample cards at ₱5, ₱20, ₱50 and ₱100.</span>
          {#if scriptState === "applied"}
            <span class="font-medium text-foreground">Template Script applied.</span>
          {:else if scriptState === "stale"}
            <span class="font-medium text-amber-700 dark:text-amber-300">Template Script changed since the last Run.</span>
          {:else}
            <span>Template Script not run; press Run to apply it.</span>
          {/if}
        </p>
      </section>
    </div>
  {/if}
  </div>
</div>

{#if voidTarget}
  <ConfirmDialog
    title={voidTarget.kind === "voucher"
      ? `Void voucher ${voidTarget.voucher.code}?`
      : `Void the ${voidTarget.name} batch?`}
    confirmLabel={voidTarget.kind === "voucher" ? "Void voucher" : "Void batch"}
    busyLabel="Voiding…"
    busy={voidBusy}
    destructive
    onconfirm={confirmVoid}
    oncancel={() => (voidTarget = null)}
  >
    {#if voidTarget.kind === "voucher"}
      The code stops working immediately and stays in history. This cannot be undone.
      {#if voidTarget.voucher.kind === "sold"}
        Recorded revenue remains unchanged.
      {/if}
    {:else}
      Every active {voidTarget.prefix} card in this batch stops working immediately; redeemed
      cards stay as history. This cannot be undone.
    {/if}
  </ConfirmDialog>
{/if}
