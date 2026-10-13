<script lang="ts">
  import { reportActionFailure, reportActionSuccess } from "../notifications";

  let {
    bundle,
    filename,
    name,
  }: {
    bundle: string;
    /** Full download name for the .md file, e.g. `support-bundle-pc-01.md`. */
    filename: string;
    /** Artifact name for the preview region, the download control, and the toasts. */
    name: string;
  } = $props();

  let downloadBusy = $state(false);

  const verdict = $derived(extractVerdict(bundle));

  /** The Verdict section's body, so the failure shape is readable without
   *  scrolling the preview or downloading anything. Sections are joined by
   *  `\n\n---\n\n` (the builder's separator), which the slice below strips. */
  function extractVerdict(doc: string): string | null {
    const heading = "## Verdict";
    const start = doc.indexOf(heading);
    if (start === -1) return null;
    const body = doc.slice(start + heading.length);
    const next = body.search(/^## /m);
    const sliced = (next === -1 ? body : body.slice(0, next)).trim();
    return sliced.replace(/\n-{3,}$/, "").trim();
  }

  async function downloadBundle() {
    if (downloadBusy) return;
    downloadBusy = true;
    try {
      const url = URL.createObjectURL(new Blob([bundle], { type: "text/markdown" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      link.click();
      URL.revokeObjectURL(url);
      reportActionSuccess("download", name);
    } catch (caught) {
      reportActionFailure("download", name, caught);
    } finally {
      downloadBusy = false;
    }
  }
</script>

{#if verdict}
  <div class="rounded-md border bg-muted/50 p-3">
    <p class="text-sm font-semibold">Verdict</p>
    <p class="mt-1 text-sm max-md:break-words">{verdict}</p>
  </div>
{/if}

<div class="mt-3 flex items-center gap-3">
  <button
    type="button"
    class="rounded-md border bg-card px-4 py-2 text-sm font-medium hover:bg-accent disabled:opacity-50"
    disabled={downloadBusy}
    onclick={downloadBundle}
  >
    {downloadBusy ? "Downloading…" : `Download ${name}`}
  </button>
  <span class="text-xs text-muted-foreground">Saves the bundle as a .md file.</span>
</div>

<section
  class="mt-3 max-h-96 overflow-auto rounded-md border bg-muted/40 p-4"
  aria-label={`${name} preview`}
>
  <pre class="font-mono text-xs whitespace-pre-wrap">{bundle}</pre>
</section>
