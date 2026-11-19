import { inject } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { SessionStore } from './session';

export const signedIn: CanActivateFn = async (_, state) => {
  const session = inject(SessionStore);
  const router = inject(Router);
  await session.restore();
  if (session.user()) return true;
  return router.createUrlTree(['/sign-in'], { queryParams: { next: state.url } });
};

export const signedOut: CanActivateFn = async () => {
  const session = inject(SessionStore);
  await session.restore();
  return session.user() ? inject(Router).createUrlTree(['/databases']) : true;
};

export const hasTeam: CanActivateFn = () => {
  const session = inject(SessionStore);
  if (session.team()) return true;
  return inject(Router).createUrlTree(['/teams/new']);
};
