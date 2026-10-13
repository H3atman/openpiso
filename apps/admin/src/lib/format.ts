const pesoFormatter = new Intl.NumberFormat("en-PH", {
  style: "currency",
  currency: "PHP",
  minimumFractionDigits: 2,
});

export function formatMoney(cents: number): string {
  return pesoFormatter.format(cents / 100);
}

export function formatDuration(totalSeconds: number): string {
  const safeSeconds = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(safeSeconds / 3600);
  const minutes = Math.floor((safeSeconds % 3600) / 60);
  const seconds = safeSeconds % 60;
  return [hours, minutes, seconds]
    .map((value) => value.toString().padStart(2, "0"))
    .join(":");
}

export function formatTransactionTime(value: string): string {
  const normalized = value.includes("T")
    ? value
    : `${value.replace(" ", "T")}Z`;
  return new Intl.DateTimeFormat("en-PH", {
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(normalized));
}

export function formatShopClosingTime(value: string | null): string {
  if (!value) return "soon";
  const match = /T(\d{2}):(\d{2})/.exec(value);
  if (!match) return "soon";
  const hour = Number(match[1]);
  const period = hour >= 12 ? "PM" : "AM";
  return `${hour % 12 || 12}:${match[2]} ${period}`;
}

export function formatShopNextChange(value: string | null): string {
  if (!value) return "No scheduled status change.";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return `Next status change: ${new Intl.DateTimeFormat("en-PH", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date)}`;
}

export function statusLabel(status: string): string {
  return status
    .replace("_", " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}
