import { inject } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { SessionStore } from './session';

// Inject before the first await: the injection context ends there.
export const signedIn: CanActivateFn = async (_, state) => {
  const session = inject(SessionStore);
  const router = inject(Router);
  try {
    await session.restore();
  } catch {
    // The sign-in page shows why (session.restoreError) and lets the person retry.
    return router.createUrlTree(['/sign-in'], { queryParams: { next: state.url } });
  }
  if (session.user()) return true;
  return router.createUrlTree(['/sign-in'], { queryParams: { next: state.url } });
};

export const signedOut: CanActivateFn = async (route) => {
  const session = inject(SessionStore);
  const router = inject(Router);
  try {
    await session.restore();
  } catch {
    return true;
  }
  return session.user() ? router.parseUrl(safeNext(route.queryParamMap.get('next'))) : true;
};

/** Only same-app paths, never another origin. */
export function safeNext(next: string | null | undefined): string {
  return next && next.startsWith('/') && !next.startsWith('//') ? next : '/databases';
}

export const hasTeam: CanActivateFn = () => {
  const session = inject(SessionStore);
  if (session.team()) return true;
  return inject(Router).createUrlTree(['/teams/new']);
};
