<script lang="ts">
  import { marked } from "marked";
  import DOMPurify from "dompurify";

  let { notes }: { notes: string } = $props();
  // Release Feed text is untrusted. Permit prose only, without embedded media,
  // forms, styles or remote images that could make a shop send outbound requests.
  const html = $derived(
    DOMPurify.sanitize(marked.parse(notes, { async: false }), {
      ALLOWED_TAGS: [
        "p",
        "br",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "ul",
        "ol",
        "li",
        "strong",
        "em",
        "del",
        "blockquote",
        "pre",
        "code",
        "a",
        "hr",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
      ],
      ALLOWED_ATTR: ["href", "title"],
      ALLOW_DATA_ATTR: false,
      ALLOW_ARIA_ATTR: false,
    }),
  );
</script>

<div
  class="prose prose-sm max-w-none break-words"
  data-testid="app-update-notes"
>
  {@html html}
</div>

<style>
  .prose {
    --tw-prose-body: var(--foreground);
    --tw-prose-headings: var(--foreground);
    --tw-prose-links: var(--foreground);
    --tw-prose-bold: var(--foreground);
    --tw-prose-code: var(--foreground);
    --tw-prose-pre-code: var(--foreground);
    --tw-prose-pre-bg: var(--muted);
    --tw-prose-counters: var(--muted-foreground);
    --tw-prose-bullets: var(--muted-foreground);
    --tw-prose-quotes: var(--foreground);
    --tw-prose-quote-borders: var(--border);
    --tw-prose-hr: var(--border);
    --tw-prose-th-borders: var(--border);
    --tw-prose-td-borders: var(--border);
  }
</style>
