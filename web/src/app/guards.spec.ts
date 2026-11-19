import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, Router, RouterStateSnapshot, UrlTree } from '@angular/router';
import { FETCH } from './api/api';
import { signedIn, signedOut } from './guards';

const json = (data: unknown, status = 200) =>
  new Response(JSON.stringify(data), { status, headers: { 'Content-Type': 'application/json' } });
const user = { id: 'usr_1', email: 'dev@example.test', email_verified: true, display_name: null };

function setup(...responses: Response[]) {
  const fetcher = vi.fn(
    async () => responses.shift() ?? json({ error: { code: 'unexpected' } }, 500),
  );
  TestBed.configureTestingModule({ providers: [{ provide: FETCH, useValue: fetcher }] });
}
const route = {} as ActivatedRouteSnapshot;
const state = { url: '/databases/scratch' } as RouterStateSnapshot;
const run = (guard: typeof signedIn) =>
  TestBed.runInInjectionContext(() => guard(route, state)) as Promise<boolean | UrlTree>;
const url = (result: boolean | UrlTree) => TestBed.inject(Router).serializeUrl(result as UrlTree);

describe('guards', () => {
  it('sends a signed-in person away from sign-in', async () => {
    setup(
      json({ session: { id: 's', active_organization_id: 'org_a' }, user, csrf_token: 'c' }),
      json({
        items: [{ organization: { id: 'org_a', name: 'A', slug: 'a', created_at: '' } }],
        next_cursor: null,
      }),
    );
    expect(url(await run(signedOut))).toBe('/databases');
  });

  it('sends a signed-out person to sign in, keeping where they were going', async () => {
    setup(json({ error: { code: 'unauthenticated' } }, 401));
    expect(url(await run(signedIn))).toBe('/sign-in?next=%2Fdatabases%2Fscratch');
  });

  it('shows sign-in instead of a blank page when the session cannot be loaded', async () => {
    setup(json({ error: { code: 'overloaded' } }, 503));
    expect(url(await run(signedIn))).toBe('/sign-in?next=%2Fdatabases%2Fscratch');
    expect(await run(signedOut)).toBe(true);
  });
});
