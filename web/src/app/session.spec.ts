import { TestBed } from '@angular/core/testing';
import { Router } from '@angular/router';
import { Api, FETCH } from './api/api';
import { SessionStore } from './session';

const json = (data: unknown, status = 200) =>
  new Response(JSON.stringify(data), { status, headers: { 'Content-Type': 'application/json' } });
const user = { id: 'usr_1', email: 'dev@example.test', email_verified: true, display_name: null };
const org = (id: string, name: string) => ({
  organization: { id, name, slug: name.toLowerCase(), created_at: '2026-11-19T10:00:00Z' },
  membership: {},
});

function store(...responses: Response[]) {
  const fetcher = vi.fn(
    async () => responses.shift() ?? json({ error: { code: 'unexpected' } }, 500),
  );
  TestBed.configureTestingModule({ providers: [{ provide: FETCH, useValue: fetcher }] });
  return { session: TestBed.inject(SessionStore), fetcher };
}

describe('SessionStore', () => {
  it('restores the session, its teams and its active team', async () => {
    const { session } = store(
      json({ session: { id: 's', active_organization_id: 'org_b' }, user, csrf_token: 'c' }),
      json({ items: [org('org_b', 'Beta'), org('org_a', 'Alpha')], next_cursor: null }),
    );
    await session.restore();
    expect(session.user()?.email).toBe('dev@example.test');
    expect(session.teams().map((t) => t.name)).toEqual(['Alpha', 'Beta']);
    expect(session.team()?.id).toBe('org_b');
  });

  it('treats 401 as signed out and shares one restore', async () => {
    const { session, fetcher } = store(json({ error: { code: 'unauthenticated' } }, 401));
    await Promise.all([session.restore(), session.restore()]);
    expect(session.user()).toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it('forgets an active team the member no longer belongs to', async () => {
    const { session } = store(
      json({ session: { id: 's', active_organization_id: 'org_gone' }, user, csrf_token: 'c' }),
      json({ items: [], next_cursor: null }),
    );
    await session.restore();
    expect(session.teamId()).toBeNull();
  });

  it('reopens the last team used in this browser when a new session has none', async () => {
    localStorage.setItem('memoire.last-team', 'org_b');
    const { session, fetcher } = store(
      json({ session: { id: 's', active_organization_id: null }, user, csrf_token: 'c' }, 201),
      json({ items: [org('org_a', 'Alpha'), org('org_b', 'Beta')], next_cursor: null }),
      json({ session: { id: 's', active_organization_id: 'org_b' } }),
    );
    await session.signIn('dev@example.test', 'pw');
    expect(session.team()?.name).toBe('Beta');
    expect(
      JSON.parse((fetcher.mock.calls[2] as unknown as [string, RequestInit])[1].body as string),
    ).toEqual({
      organization_id: 'org_b',
    });
    localStorage.clear();
  });

  it('falls back to the first team', async () => {
    localStorage.clear();
    const { session } = store(
      json({ session: { id: 's', active_organization_id: null }, user, csrf_token: 'c' }, 201),
      json({ items: [org('org_b', 'Beta'), org('org_a', 'Alpha')], next_cursor: null }),
      json({ session: { id: 's', active_organization_id: 'org_a' } }),
    );
    await session.signIn('dev@example.test', 'pw');
    expect(session.team()?.name).toBe('Alpha');
  });

  it('creates a team, then makes it active', async () => {
    const { session, fetcher } = store(
      json(org('org_n', 'New'), 201),
      json({ items: [org('org_n', 'New')], next_cursor: null }),
      json({ session: { id: 's', active_organization_id: 'org_n' } }),
    );
    await session.createTeam('New', 'new');
    expect(session.team()?.name).toBe('New');
    expect(fetcher.mock.calls.map((c) => (c as unknown[])[0])).toEqual([
      '/v1/identity/organizations',
      '/v1/identity/organizations?limit=100',
      '/v1/identity/session/organization',
    ]);
  });

  it('sends a signed-in person to sign in when the session ends', async () => {
    const { session } = store(
      json({ session: { id: 's', active_organization_id: 'org_a' }, user, csrf_token: 'c' }),
      json({ items: [org('org_a', 'Alpha')], next_cursor: null }),
      json({ error: { code: 'unauthenticated' } }, 401),
    );
    const router = TestBed.inject(Router);
    const navigate = vi.spyOn(router, 'navigate').mockResolvedValue(true);
    await session.restore();
    await expect(session.refreshTeams()).rejects.toThrow('unauthenticated');
    expect(session.user()).toBeNull();
    expect(navigate).toHaveBeenCalledWith(['/sign-in'], { queryParams: { next: '/' } });
  });

  it('keeps the session when sign-out fails for another reason than an ended session', async () => {
    const { session } = store(
      json({ session: { id: 's', active_organization_id: 'org_a' }, user, csrf_token: 'c' }),
      json({ items: [org('org_a', 'Alpha')], next_cursor: null }),
      json({ error: { code: 'overloaded' } }, 503),
    );
    await session.restore();
    await expect(session.signOut()).rejects.toThrow('overloaded');
    expect(session.user()?.id).toBe('usr_1');
    expect(TestBed.inject(Api).http.csrf).toBe('c');
  });

  it('names the active team in the memoire header', async () => {
    const { session } = store(
      json({ session: { id: 's', active_organization_id: 'org_a' }, user, csrf_token: 'c' }),
      json({ items: [org('org_a', 'Alpha')], next_cursor: null }),
    );
    await session.restore();
    expect(TestBed.inject(Api).http.team).toBe('org_a');
  });

  it('clears the restore error once the session is known, signed out included', async () => {
    const { session } = store(
      json({ error: { code: 'overloaded' } }, 503),
      json({ error: { code: 'unauthenticated' } }, 401),
    );
    await expect(session.restore()).rejects.toThrow('overloaded');
    expect(session.restoreError()).not.toBeNull();
    await session.restore();
    expect(session.restoreError()).toBeNull();
  });

  it('rethrows other failures so the page can show them, and retries next time', async () => {
    const { session } = store(
      json({ error: { code: 'overloaded' } }, 503),
      json({ session: { id: 's', active_organization_id: null }, user, csrf_token: 'c' }),
      json({ items: [], next_cursor: null }),
    );
    await expect(session.restore()).rejects.toThrow('overloaded');
    await session.restore();
    expect(session.user()?.id).toBe('usr_1');
  });
});
