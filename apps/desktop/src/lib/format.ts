export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

const timeFmt = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
const dayFmt = new Intl.DateTimeFormat(undefined, { weekday: "long", month: "long", day: "numeric" });
const shortDayFmt = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const fullFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** "Today", "Yesterday", or "Monday, November 9". */
export function dayLabel(d: Date): string {
  const diff = Math.round((startOfDay(new Date()) - startOfDay(d)) / 86_400_000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return dayFmt.format(d);
}

export const formatTime = (d: Date) => timeFmt.format(d);
export const formatShortDay = (d: Date) => shortDayFmt.format(d);
export const formatFull = (d: Date) => fullFmt.format(d);

/** "Today, 10:30 AM" */
export function formatWhen(iso: string | null | undefined): string {
  if (!iso) return "Never";
  const d = new Date(iso);
  return `${dayLabel(d)}, ${formatTime(d)}`;
}

export function formatRelative(iso: string): string {
  const ms = new Date(iso).getTime() - Date.now();
  const abs = Math.abs(ms);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (abs < 60_000) return ms >= 0 ? "in a moment" : "just now";
  if (abs < 3_600_000) return rtf.format(Math.round(ms / 60_000), "minute");
  if (abs < 86_400_000) return rtf.format(Math.round(ms / 3_600_000), "hour");
  return rtf.format(Math.round(ms / 86_400_000), "day");
}

export function formatMs(ms: number | null): string {
  return ms === null ? "—" : formatFull(new Date(ms));
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString()} ${n === 1 ? one : many}`;
}

export function baseName(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}
