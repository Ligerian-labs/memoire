import { ApiError, Http } from './http';
import { IdentityApi } from './identity';
import { MemoireApi } from './memoire';

const json = (data: unknown, status = 200, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(data), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  });

async function failure(promise: Promise<unknown>): Promise<ApiError> {
  try {
    await promise;
  } catch (error) {
    return error as ApiError;
  }
  throw new Error('expected a failure');
}

function make(...responses: Response[]) {
  const fetcher = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => {
    const next = responses.shift();
    if (!next) throw new Error('unexpected request');
    return next;
  });
  const http = new Http(fetcher as unknown as typeof fetch);
  return { http, fetcher, identity: new IdentityApi(http), memoire: new MemoireApi(http) };
}

const signedIn = {
  session: { id: 'ses_1', active_organization_id: null },
  user: { id: 'usr_1', email: 'dev@example.test', email_verified: true, display_name: null },
  csrf_token: 'csrf-1',
};

describe('Http', () => {
  it('sends the CSRF token from sign-in on state changes only', async () => {
    const { fetcher, identity, memoire } = make(
      json(signedIn, 201),
      json({ items: [], next_cursor: null }),
      json({}, 201),
    );
    await identity.signIn('dev@example.test', 'correct horse battery');
    await memoire.list('active');
    await memoire.create({ name: 'scratch', kind: 'burstable' });
    const [, list, create] = fetcher.mock.calls;
    expect((list[1]!.headers as Record<string, string>)['X-CSRF-Token']).toBeUndefined();
    expect((create[1]!.headers as Record<string, string>)['X-CSRF-Token']).toBe('csrf-1');
    expect(create[1]!.credentials).toBe('same-origin');
    expect(JSON.parse(create[1]!.body as string)).toEqual({ name: 'scratch', kind: 'burstable' });
  });

  it('drops the token on sign-out even when the gateway fails', async () => {
    const { http, fetcher, identity } = make(
      json(signedIn, 201),
      json({ error: { code: 'unauthenticated' } }, 401),
    );
    await identity.signIn('dev@example.test', 'pw');
    await expect(identity.signOut()).rejects.toBeInstanceOf(ApiError);
    expect(http.csrf).toBeNull();
    expect(fetcher.mock.calls[1][1]!.method).toBe('DELETE');
  });

  it('builds query strings and percent-encodes path segments', async () => {
    const { fetcher, memoire } = make(
      json({ columns: [], rows: [], next_after: null }),
      json({ items: [], next_cursor: null }),
    );
    await memoire.rows('scratch', 'my table/1%', 50, 'cm93aWQ6Mg');
    await memoire.list('all', null);
    expect(fetcher.mock.calls[0][0]).toBe(
      '/v1/memoire/databases/scratch/tables/my%20table%2F1%25/rows?limit=50&after=cm93aWQ6Mg',
    );
    expect(fetcher.mock.calls[1][0]).toBe('/v1/memoire/databases?state=all&limit=100');
  });

  it('parses the error envelope, including sql details and Retry-After', async () => {
    const sql = {
      statement: 1,
      sqlite_code: 'SQLITE_CONSTRAINT',
      message: 'UNIQUE constraint failed: t.a',
    };
    const { memoire } = make(
      json({ error: { code: 'sql_error', classification: 'permanent', sql } }, 422),
      json({ error: { code: 'sqlite_busy', classification: 'transient' } }, 503, {
        'Retry-After': '2',
      }),
    );
    const request = { mode: 'write' as const, transaction: false, statements: [{ sql: 'x' }] };
    const first = await failure(memoire.query('scratch', request));
    expect(first.status).toBe(422);
    expect(first.code).toBe('sql_error');
    expect(first.body.sql).toEqual(sql);
    const second = await failure(memoire.query('scratch', request));
    expect(second.retryAfter).toBe(2);
  });

  it('turns a non-JSON failure into an http_<status> code', async () => {
    const { memoire } = make(new Response('<html>bad gateway</html>', { status: 502 }));
    const error = await failure(memoire.whoami());
    expect(error.code).toBe('http_502');
  });

  it('answers undefined for 204', async () => {
    const { memoire } = make(new Response(null, { status: 204 }));
    await expect(memoire.delete('scratch')).resolves.toBeUndefined();
  });

  it('sends an empty object to extend without a new TTL', async () => {
    const { fetcher, memoire } = make(json({}), json({}));
    await memoire.extend('scratch');
    await memoire.extend('scratch', 7200);
    expect(fetcher.mock.calls[0][1]!.body).toBe('{}');
    expect(fetcher.mock.calls[1][1]!.body).toBe('{"idle_ttl_seconds":7200}');
  });

  it('selects a team with PUT /v1/identity/session/organization', async () => {
    const { fetcher, identity } = make(
      json({ session: { id: 'ses_1', active_organization_id: 'org_1' } }),
    );
    await identity.selectOrganization('org_1');
    expect(fetcher.mock.calls[0][0]).toBe('/v1/identity/session/organization');
    expect(fetcher.mock.calls[0][1]!.method).toBe('PUT');
    expect(fetcher.mock.calls[0][1]!.body).toBe('{"organization_id":"org_1"}');
  });
});
