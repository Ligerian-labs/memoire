/** Merezha's error envelope (docs/api.md, "Errors"). */
export interface ErrorBody {
  code: string;
  classification?: 'permanent' | 'transient' | 'conflict';
  details?: { path: string; code: string }[];
  sql?: { statement: number; sqlite_code: string; message: string };
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly body: ErrorBody,
    readonly retryAfter: number | null = null,
  ) {
    super(body.code);
    this.name = 'ApiError';
  }
  get code(): string {
    return this.body.code;
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
  /**
   * The team the page shows, sent as `Merezha-Organization` on memoire routes.
   * It wins over the session's active team, which another tab may have switched.
   */
  team: string | null = null;
  /** Called with every failure before it is thrown, for app-wide reactions such as an ended session. */
  onError: ((error: ApiError) => void) | null = null;

  constructor(private readonly fetcher: typeof fetch) {}

  async request<T>(method: string, path: string, body?: unknown, query?: Query): Promise<T> {
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    if (UNSAFE.has(method) && this.csrf) headers['X-CSRF-Token'] = this.csrf;
    if (this.team && path.startsWith('/v1/memoire/')) headers['Merezha-Organization'] = this.team;
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
  try {
    const parsed = (await response.json()) as { error?: ErrorBody };
    if (parsed?.error?.code) return new ApiError(response.status, parsed.error, retryAfter);
  } catch {
    /* not an error envelope: a proxy failure or an HTML page */
  }
  return new ApiError(response.status, { code: `http_${response.status}` }, retryAfter);
}
