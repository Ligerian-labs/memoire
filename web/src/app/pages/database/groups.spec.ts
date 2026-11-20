import { Schema, SchemaTable } from '../../api/memoire';
import { groupSchema } from './groups';

function table(name: string, sql = `CREATE TABLE ${name} (x)`): SchemaTable {
  return { name, sql, without_rowid: false, columns: [], indexes: [] };
}

function schema(...tables: SchemaTable[]): Schema {
  return { tables, views: [] };
}

const names = (tables: SchemaTable[]) => tables.map((t) => t.name);
const shadows = (parent: string) =>
  ['config', 'content', 'data', 'docsize', 'idx'].map((s) => table(`${parent}_${s}`));

describe('groupSchema', () => {
  it('leaves a database without a store alone', () => {
    const groups = groupSchema(schema(table('notes'), table('tags')));
    expect(names(groups.tables)).toEqual(['notes', 'tags']);
    expect(groups.memory).toBeNull();
    expect(groups.ftsInternals.size).toBe(0);
  });

  it('separates the memory store and folds its index tables', () => {
    const groups = groupSchema(
      schema(
        table('kv'),
        table('memoire_memory_events'),
        table('memoire_memory_fts', 'CREATE VIRTUAL TABLE memoire_memory_fts USING fts5(title)'),
        ...shadows('memoire_memory_fts'),
        table('memoire_memory_memories'),
        table('memoire_memory_meta'),
        table('zeta'),
      ),
    );
    expect(names(groups.tables)).toEqual(['kv', 'zeta']);
    expect(names(groups.memory!.tables)).toEqual([
      'memoire_memory_events',
      'memoire_memory_fts',
      'memoire_memory_memories',
      'memoire_memory_meta',
    ]);
    expect(names(groups.memory!.internals)).toEqual([
      'memoire_memory_fts_config',
      'memoire_memory_fts_content',
      'memoire_memory_fts_data',
      'memoire_memory_fts_docsize',
      'memoire_memory_fts_idx',
    ]);
    expect([...groups.ftsInternals].sort()).toEqual(names(groups.memory!.internals));
  });

  it("keeps an application's FTS5 tables in place and marks its shadows", () => {
    const groups = groupSchema(
      schema(
        table('docs', 'CREATE VIRTUAL TABLE docs USING fts5(body)'),
        ...shadows('docs'),
        table('notes'),
      ),
    );
    expect(names(groups.tables)).toEqual([
      'docs',
      'docs_config',
      'docs_content',
      'docs_data',
      'docs_docsize',
      'docs_idx',
      'notes',
    ]);
    expect(groups.memory).toBeNull();
    expect(groups.ftsInternals.has('docs_data')).toBe(true);
    expect(groups.ftsInternals.has('docs')).toBe(false);
    expect(groups.ftsInternals.has('notes')).toBe(false);
  });

  it('only counts the shadow tables that exist', () => {
    // contentless and external-content tables have no _content table.
    const groups = groupSchema(
      schema(
        table('docs', "CREATE VIRTUAL TABLE docs USING fts5(body, content='')"),
        table('docs_config'),
        table('docs_data'),
        table('docs_idx'),
      ),
    );
    expect([...groups.ftsInternals]).toEqual(['docs_config', 'docs_data', 'docs_idx']);
  });

  it('does not take a table named like a shadow for one', () => {
    const groups = groupSchema(
      schema(
        table('foo_data'),
        table('bar', 'CREATE TABLE bar (x)'),
        table('bar_data'),
        table('memoire_memory_extra_data'),
      ),
    );
    expect(groups.ftsInternals.size).toBe(0);
    expect(names(groups.memory!.tables)).toEqual(['memoire_memory_extra_data']);
    expect(groups.memory!.internals).toEqual([]);
  });

  it('ignores other virtual table modules', () => {
    const groups = groupSchema(
      schema(
        table('v', 'CREATE VIRTUAL TABLE v USING fts4(body)'),
        table('v_data'),
        table('r', 'CREATE VIRTUAL TABLE r USING rtree(id, x0, x1)'),
        table('r_data'),
      ),
    );
    expect(groups.ftsInternals.size).toBe(0);
  });

  it('reads the statement in any case, spacing and quoting', () => {
    const cases: [string, string][] = [
      ['a', 'create virtual table a using FTS5(x)'],
      ['b', 'CREATE VIRTUAL TABLE IF NOT EXISTS b USING fts5 (x)'],
      ['c d', 'CREATE VIRTUAL TABLE "c d"\n  USING\tfts5(x)'],
      ['e', 'CREATE VIRTUAL TABLE main.[e] USING fts5(x)'],
      ['f', 'CREATE VIRTUAL TABLE `f` USING fts5'],
      ['G', 'CREATE VIRTUAL TABLE G USING fts5(x)'],
    ];
    for (const [name, sql] of cases) {
      const groups = groupSchema(schema(table(name, sql), table(`${name.toLowerCase()}_data`)));
      expect([...groups.ftsInternals], sql).toEqual([`${name.toLowerCase()}_data`]);
    }
  });

  it('does not read fts5 out of a column list', () => {
    const groups = groupSchema(
      schema(table('t', 'CREATE TABLE t (note TEXT) -- using fts5'), table('t_data')),
    );
    expect(groups.ftsInternals.size).toBe(0);
  });
});
