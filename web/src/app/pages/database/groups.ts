import { Schema, SchemaTable } from '../../api/memoire';

/** `memoire memory` keeps its store in tables with this prefix. */
export const MEMORY_PREFIX = 'memoire_memory_';

/** The tables FTS5 creates beside each of its virtual tables to hold the index. */
const FTS5_SHADOWS = ['config', 'content', 'data', 'docsize', 'idx'];

// CREATE VIRTUAL TABLE [IF NOT EXISTS] [schema.]name USING fts5, names bare or quoted.
const NAME = String.raw`(?:"(?:[^"]|"")*"|\`(?:[^\`]|\`\`)*\`|\[[^\]]*\]|'(?:[^']|'')*'|[^\s.("'\`\[]+)`;
const FTS5 = new RegExp(
  String.raw`^\s*create\s+virtual\s+table\s+(?:if\s+not\s+exists\s+)?(?:${NAME}\s*\.\s*)?${NAME}\s+using\s+fts5\b`,
  'i',
);

export interface SchemaGroups {
  /** The application's own tables, in the schema's order. */
  tables: SchemaTable[];
  /** The memory store's tables, null when the database holds none. */
  memory: { tables: SchemaTable[]; internals: SchemaTable[] } | null;
  /** Names of FTS5 shadow tables anywhere in the schema. */
  ftsInternals: ReadonlySet<string>;
}

/** Splits a schema's tables into the application's, the memory store's and FTS5's own. */
export function groupSchema(schema: Schema): SchemaGroups {
  // SQLite compares names without case.
  const byName = new Map(schema.tables.map((t) => [t.name.toLowerCase(), t.name]));
  const ftsInternals = new Set<string>();
  for (const table of schema.tables) {
    if (!FTS5.test(table.sql)) continue;
    for (const suffix of FTS5_SHADOWS) {
      const name = byName.get(`${table.name}_${suffix}`.toLowerCase());
      if (name) ftsInternals.add(name);
    }
  }
  const tables: SchemaTable[] = [];
  const store: SchemaTable[] = [];
  const internals: SchemaTable[] = [];
  for (const table of schema.tables) {
    if (!table.name.startsWith(MEMORY_PREFIX)) tables.push(table);
    else if (ftsInternals.has(table.name)) internals.push(table);
    else store.push(table);
  }
  const memory = store.length || internals.length ? { tables: store, internals } : null;
  return { tables, memory, ftsInternals };
}
