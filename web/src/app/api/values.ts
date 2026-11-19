/** SQLite values on the wire (docs/api.md, "Value encoding"). */
export type TaggedValue =
  | { type: 'integer'; value: string }
  | { type: 'real'; value: string }
  | { type: 'blob'; value: string };
export type Value = null | number | string | TaggedValue;

export type Param =
  | Value
  | boolean
  | { type: 'integer'; value: number | string }
  | { type: 'real'; value: number | string }
  | { type: 'text'; value: string };
export type Params = Param[] | Record<string, Param>;

export type CellKind = 'null' | 'number' | 'text' | 'blob' | 'big';
export interface Cell {
  kind: CellKind;
  text: string;
  /** Full value for a tooltip when `text` is shortened. */
  title?: string;
}

const MAX_TEXT = 200;

/** How the grid shows one value: never lossy for numbers, shortened for long text and blobs. */
export function cell(value: Value): Cell {
  if (value === null) return { kind: 'null', text: 'NULL' };
  if (typeof value === 'number') return { kind: 'number', text: String(value) };
  if (typeof value === 'string') {
    return value.length > MAX_TEXT
      ? { kind: 'text', text: value.slice(0, MAX_TEXT) + '…', title: value }
      : { kind: 'text', text: value };
  }
  if (value.type === 'blob') {
    const size = base64Size(value.value);
    const preview = value.value.length > 24 ? value.value.slice(0, 24) + '…' : value.value;
    return { kind: 'blob', text: `blob ${size} B ${preview}`, title: value.value };
  }
  return { kind: 'big', text: value.value };
}

export function base64Size(b64: string): number {
  const padding = b64.endsWith('==') ? 2 : b64.endsWith('=') ? 1 : 0;
  return Math.floor((b64.length * 3) / 4) - padding;
}

/** Plain text of a value, for copying a cell. */
export function plain(value: Value): string {
  if (value === null) return '';
  if (typeof value === 'number' || typeof value === 'string') return String(value);
  return value.value;
}

export class ParamsError extends Error {}

/**
 * Parses the console's parameter field: empty means none, otherwise a JSON
 * array (positional) or object (named). The gateway validates the values.
 */
export function parseParams(text: string): Params | undefined {
  const trimmed = text.trim();
  if (trimmed === '') return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    throw new ParamsError(
      'Parameters must be JSON: an array like [1, "a"] or an object like {"id": 1}.',
    );
  }
  if (Array.isArray(parsed)) return parsed as Param[];
  if (parsed !== null && typeof parsed === 'object') return parsed as Record<string, Param>;
  throw new ParamsError('Parameters must be a JSON array or object.');
}
