/**
 * Clipboard write with a fallback for the plain-HTTP LAN origin, where the
 * modern Clipboard API may be unavailable. Keeps bundle-copy actions usable
 * in that deployment instead of silently losing the text.
 */
export async function copyTextToClipboard(text: string): Promise<void> {
  if (navigator.clipboard) {
    await navigator.clipboard.writeText(text);
    return;
  }

  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  const copied = document.execCommand("copy");
  textarea.remove();
  if (!copied) {
    throw new Error("clipboard access is unavailable in this browser");
  }
}
