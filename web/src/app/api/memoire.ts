import { Http, JsonText, Query } from './http';
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
  constructor(
    private readonly http: Http,
    /** The team every request names; null uses the session's active team. */
    readonly team: string | null = null,
  ) {}

  /** The same API bound to one team, so a view keeps acting on the team it shows. */
  forTeam(team: string | null): MemoireApi {
    return new MemoireApi(this.http, team);
  }

  private send<T>(method: string, path: string, body?: unknown, query?: Query): Promise<T> {
    return this.http.request<T>(method, path, body, query, this.team);
  }

  whoami(): Promise<Whoami> {
    return this.send('GET', '/v1/memoire/whoami');
  }

  list(state: 'active' | 'all', cursor?: string | null): Promise<Page<Database>> {
    return this.send('GET', '/v1/memoire/databases', undefined, {
      state,
      cursor,
      limit: 100,
    });
  }

  create(input: CreateDatabase): Promise<Database> {
    return this.send('POST', '/v1/memoire/databases', input);
  }

  get(name: string): Promise<Database> {
    return this.send('GET', db(name));
  }

  delete(name: string): Promise<void> {
    return this.send('DELETE', db(name));
  }

  extend(name: string, idleTtlSeconds?: number): Promise<Database> {
    const body = idleTtlSeconds === undefined ? {} : { idle_ttl_seconds: idleTtlSeconds };
    return this.send('POST', `${db(name)}/extend`, body);
  }

  /**
   * Runs a batch. `paramsJson`, the console's parameter text, goes into the
   * body verbatim for a single statement, so `9007199254740993` and `1.0`
   * reach SQLite as typed instead of as JavaScript numbers.
   */
  query(
    name: string,
    request: QueryRequest,
    paramsJson?: string,
  ): Promise<{ results: StatementResult[] }> {
    return this.send('POST', `${db(name)}/query`, queryBody(request, paramsJson));
  }

  schema(name: string): Promise<Schema> {
    return this.send('GET', `${db(name)}/schema`);
  }

  rows(name: string, table: string, limit: number, after?: string | null): Promise<RowPage> {
    return this.send('GET', `${db(name)}/tables/${encodeURIComponent(table)}/rows`, undefined, {
      limit,
      after,
    });
  }

  snapshots(name: string): Promise<Page<Snapshot>> {
    return this.send('GET', `${db(name)}/snapshots`);
  }

  restore(name: string, snapshotId: string): Promise<Database> {
    return this.send('POST', `${db(name)}/snapshots/${encodeURIComponent(snapshotId)}/restore`);
  }

  exportUrl(name: string): string {
    return `${db(name)}/export`;
  }
}

/** The query body, with raw parameter JSON spliced into its only statement. */
export function queryBody(request: QueryRequest, paramsJson?: string): QueryRequest | JsonText {
  if (paramsJson === undefined) return request;
  if (request.statements.length !== 1) throw new Error('raw parameters need exactly one statement');
  // Only a whole JSON array or object may be spliced in, never a fragment of the body.
  const parsed: unknown = JSON.parse(paramsJson);
  if (parsed === null || typeof parsed !== 'object')
    throw new Error('parameters must be an array or object');
  const marker = '"\\u0000params\\u0000"';
  const text = JSON.stringify({
    ...request,
    statements: [{ sql: request.statements[0].sql, params: '\u0000params\u0000' }],
  });
  return new JsonText(text.replace(marker, () => paramsJson.trim()));
}

function db(name: string): string {
  return `/v1/memoire/databases/${encodeURIComponent(name)}`;
}
