/** Merezha's error envelope (docs/api.md, "Errors"). */
export interface ErrorBody {
  code: string;
  classification?: 'permanent' | 'transient' | 'conflict';
  details?: { path: string; code: string }[];
}

/** What a `422 sql_error` tells about the statement SQLite refused. */
export interface SqlFailure {
  /** 0-based index in the batch. */
  statement: number;
  /** SQLite's primary result code, e.g. `sqlite_constraint`. */
  code: string;
  /** SQLite's message, from the `Memoire-Sql-Message` header; null when absent. */
  message: string | null;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly body: ErrorBody,
    readonly retryAfter: number | null = null,
    readonly sqlMessage: string | null = null,
  ) {
    super(body.code);
    this.name = 'ApiError';
  }
  get code(): string {
    return this.body.code;
  }

  /** The refused statement of a `sql_error`: its `details` entry and the message header. */
  get sql(): SqlFailure | null {
    if (this.body.code !== 'sql_error') return null;
    for (const detail of this.body.details ?? []) {
      const match = /^\/statements\/(\d+)\/sql$/.exec(detail.path);
      if (match)
        return { statement: Number(match[1]), code: detail.code, message: this.sqlMessage };
    }
    return null;
  }
}

export type Query = Record<string, string | number | null | undefined>;

/** A request body already serialized, sent as is (keeps number tokens JSON.parse would round). */
export class JsonText {
  constructor(readonly text: string) {}
}
const UNSAFE = new Set(['POST', 'PUT', 'PATCH', 'DELETE']);

/**
 * The one HTTP boundary. Same-origin requests carry the session cookie; state
 * changes also carry the CSRF token identity returned at sign-in. No retries:
 * every caller shows the failure and lets the person act on it.
 */
export class Http {
  csrf: string | null = null;
  /** Called with every failure before it is thrown, for app-wide reactions such as an ended session. */
  onError: ((error: ApiError) => void) | null = null;

  constructor(private readonly fetcher: typeof fetch) {}

  /**
   * `team`, when given, goes in `Merezha-Organization`: it wins over the
   * session's active team, which another tab may have switched.
   */
  async request<T>(
    method: string,
    path: string,
    body?: unknown,
    query?: Query,
    team?: string | null,
  ): Promise<T> {
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    if (UNSAFE.has(method) && this.csrf) headers['X-CSRF-Token'] = this.csrf;
    if (team) headers['Merezha-Organization'] = team;
    const response = await this.fetcher(path + search(query), {
      method,
      headers,
      body:
        body === undefined
          ? undefined
          : body instanceof JsonText
            ? body.text
            : JSON.stringify(body),
      credentials: 'same-origin',
      cache: 'no-store',
    });
    if (!response.ok) {
      const error = await toError(response);
      this.onError?.(error);
      throw error;
    }
    if (response.status === 204) return undefined as T;
    const text = await response.text();
    return (text === '' ? undefined : JSON.parse(text)) as T;
  }
}

function search(query?: Query): string {
  if (!query) return '';
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== null && value !== undefined && value !== '') params.set(key, String(value));
  }
  const text = params.toString();
  return text ? `?${text}` : '';
}

async function toError(response: Response): Promise<ApiError> {
  const retry = Number(response.headers.get('Retry-After'));
  const retryAfter = Number.isFinite(retry) && retry > 0 ? retry : null;
  const sqlMessage = decode(response.headers.get('Memoire-Sql-Message'));
  try {
    const parsed = (await response.json()) as { error?: ErrorBody };
    if (parsed?.error?.code)
      return new ApiError(response.status, parsed.error, retryAfter, sqlMessage);
  } catch {
    /* not an error envelope: a proxy failure or an HTML page */
  }
  return new ApiError(response.status, { code: `http_${response.status}` }, retryAfter);
}

function decode(header: string | null): string | null {
  if (!header) return null;
  try {
    return decodeURIComponent(header);
  } catch {
    return header;
  }
}
