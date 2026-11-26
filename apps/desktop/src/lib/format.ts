export function formatDuration(ms: number): string {
  if (ms < 1) return `${ms.toFixed(2)} ms`;
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)} s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m ${s}s`;
}

export function formatCount(n: number): string {
  return n.toLocaleString('en-US');
}

export function compactCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  return `${(n / 1_000_000_000).toFixed(1)}B`;
}

export function plural(n: number, one: string, many = one + 's'): string {
  return `${formatCount(n)} ${n === 1 ? one : many}`;
}

export function relativeTime(secs: number): string {
  const diff = Date.now() / 1000 - secs;
  if (diff < 45) return 'just now';
  if (diff < 3600) return `${Math.round(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.round(diff / 3600)}h ago`;
  if (diff < 86400 * 7) return `${Math.round(diff / 86400)}d ago`;
  return new Date(secs * 1000).toLocaleDateString();
}

/** A calendar date fixed in UTC, e.g. the end of the free launch period ("February 1, 2027"). */
export function formatDayUtc(secs: number): string {
  return new Date(secs * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric', timeZone: 'UTC' });
}

export function formatDate(secs: number): string {
  return new Date(secs * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

/** First line, limited length — for grid cells and lists. */
export function preview(v: string, max = 300): string {
  const s = v.length > max ? v.slice(0, max) + '…' : v;
  return s.replace(/\r?\n/g, ' ⏎ ');
}

export function initials(name: string): string {
  const words = name.replace(/[^\p{L}\p{N}\s_-]/gu, ' ').split(/[\s_-]+/).filter(Boolean);
  if (!words.length) return '?';
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return (words[0][0] + words[1][0]).toUpperCase();
}

export function uid(): string {
  return Math.random().toString(36).slice(2, 10) + Date.now().toString(36).slice(-4);
}

export function tryPrettyJson(v: string): string | null {
  const t = v.trim();
  if (!(t.startsWith('{') || t.startsWith('['))) return null;
  try {
    return JSON.stringify(JSON.parse(t), null, 2);
  } catch {
    return null;
  }
}
