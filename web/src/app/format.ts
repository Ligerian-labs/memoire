/** Formatting for dense tables: short, exact enough, never locale-dependent in tests. */
export function bytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return '—';
  if (n < 1024) return `${n} B`;
  const units = ['KiB', 'MiB', 'GiB'];
  let value = n / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

/** A whole-second duration as its largest one or two units: `1d 2h`, `45m`, `30s`. */
export function duration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  const parts: [number, string][] = [
    [Math.floor(s / 86400), 'd'],
    [Math.floor((s % 86400) / 3600), 'h'],
    [Math.floor((s % 3600) / 60), 'm'],
    [s % 60, 's'],
  ];
  const first = parts.findIndex(([v]) => v > 0);
  if (first === -1) return '0s';
  return parts
    .slice(first, first + 2)
    .filter(([v]) => v > 0)
    .map(([v, u]) => `${v}${u}`)
    .join(' ');
}

/** `in 3h 5m`, `12m ago`; `now` within five seconds. */
export function relative(iso: string | null | undefined, now: number): string {
  if (!iso) return '—';
  const delta = (Date.parse(iso) - now) / 1000;
  if (Math.abs(delta) < 5) return 'now';
  return delta > 0 ? `in ${duration(delta)}` : `${duration(-delta)} ago`;
}

/** `2026-11-19 10:05:12Z`: UTC, sortable, the same everywhere. */
export function timestamp(iso: string | null | undefined): string {
  if (!iso) return '—';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date
    .toISOString()
    .replace('T', ' ')
    .replace(/\.\d+Z$/, 'Z');
}

/** A team slug from its name: lowercase ASCII words joined by `-`. */
export function slugify(name: string): string {
  return name
    .normalize('NFKD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 63);
}

export const DATABASE_NAME = /^[a-z][a-z0-9-]{1,62}$/;
