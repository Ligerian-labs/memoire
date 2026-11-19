import { Http } from './http';
import { Params, Value } from './values';

export type Kind = 'burstable' | 'persistent';
export type State = 'active' | 'expired';

export interface Database {
  id: string;
  name: string;
  kind: Kind;
  state: State;
  created_at: string;
  last_used_at: string | null;
  size_bytes: number | null;
  idle_ttl_seconds: number | null;
  expires_at: string | null;
  expires_at_max: string | null;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}

export interface Whoami {
  subject: string;
  kind: 'user' | 'api_key';
  team_id: string | null;
}

export interface CreateDatabase {
  name: string;
  kind: Kind;
  idle_ttl_seconds?: number;
}

export type Mode = 'read' | 'write';
export interface Statement {
  sql: string;
  params?: Params;
}
export interface QueryRequest {
  mode: Mode;
  transaction: boolean;
  timeout_ms?: number;
  statements: Statement[];
}
export interface Column {
  name: string;
  decltype: string | null;
}
export interface StatementResult {
  columns: Column[];
  rows: Value[][];
  rows_affected: number;
  last_insert_rowid: Value;
}

export interface SchemaColumn {
  name: string;
  type: string;
  not_null: boolean;
  default: string | null;
  primary_key: number;
  generated: boolean;
}
export interface SchemaIndex {
  name: string;
  unique: boolean;
  columns: (string | null)[];
  sql: string | null;
}
export interface SchemaTable {
  name: string;
  sql: string;
  without_rowid: boolean;
  columns: SchemaColumn[];
  indexes: SchemaIndex[];
}
export interface SchemaView {
  name: string;
  sql: string;
  columns: SchemaColumn[];
}
export interface Schema {
  tables: SchemaTable[];
  views: SchemaView[];
}

export interface RowPage {
  columns: Column[];
  rows: Value[][];
  next_after: string | null;
}

export interface Snapshot {
  id: string;
  created_at: string;
  size_bytes: number;
  reason: 'scheduled' | 'pre_restore';
}

/** memoire's routes under /v1/memoire (docs/api.md, "Routes"). */
export class MemoireApi {
  constructor(private readonly http: Http) {}

  whoami(): Promise<Whoami> {
    return this.http.request('GET', '/v1/memoire/whoami');
  }

  list(state: 'active' | 'all', cursor?: string | null): Promise<Page<Database>> {
    return this.http.request('GET', '/v1/memoire/databases', undefined, {
      state,
      cursor,
      limit: 100,
    });
  }

  create(input: CreateDatabase): Promise<Database> {
    return this.http.request('POST', '/v1/memoire/databases', input);
  }

  get(name: string): Promise<Database> {
    return this.http.request('GET', db(name));
  }

  delete(name: string): Promise<void> {
    return this.http.request('DELETE', db(name));
  }

  extend(name: string, idleTtlSeconds?: number): Promise<Database> {
    const body = idleTtlSeconds === undefined ? {} : { idle_ttl_seconds: idleTtlSeconds };
    return this.http.request('POST', `${db(name)}/extend`, body);
  }

  query(name: string, request: QueryRequest): Promise<{ results: StatementResult[] }> {
    return this.http.request('POST', `${db(name)}/query`, request);
  }

  schema(name: string): Promise<Schema> {
    return this.http.request('GET', `${db(name)}/schema`);
  }

  rows(name: string, table: string, limit: number, after?: string | null): Promise<RowPage> {
    return this.http.request(
      'GET',
      `${db(name)}/tables/${encodeURIComponent(table)}/rows`,
      undefined,
      { limit, after },
    );
  }

  snapshots(name: string): Promise<Page<Snapshot>> {
    return this.http.request('GET', `${db(name)}/snapshots`);
  }

  restore(name: string, snapshotId: string): Promise<Database> {
    return this.http.request(
      'POST',
      `${db(name)}/snapshots/${encodeURIComponent(snapshotId)}/restore`,
    );
  }

  exportUrl(name: string): string {
    return `${db(name)}/export`;
  }
}

function db(name: string): string {
  return `/v1/memoire/databases/${encodeURIComponent(name)}`;
}
