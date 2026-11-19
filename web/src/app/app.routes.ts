import { Routes } from '@angular/router';
import { hasTeam, signedIn, signedOut } from './guards';

export const routes: Routes = [
  {
    path: 'sign-in',
    canActivate: [signedOut],
    title: 'Sign in · memoire',
    loadComponent: () => import('./pages/auth/sign-in').then((m) => m.SignIn),
  },
  {
    path: 'register',
    canActivate: [signedOut],
    title: 'Create an account · memoire',
    loadComponent: () => import('./pages/auth/register').then((m) => m.Register),
  },
  {
    path: 'auth/verify-email',
    title: 'Confirm your email · memoire',
    loadComponent: () => import('./pages/auth/verify-email').then((m) => m.VerifyEmail),
  },
  {
    path: '',
    canActivate: [signedIn],
    loadComponent: () => import('./pages/shell/shell').then((m) => m.Shell),
    children: [
      {
        path: 'teams/new',
        title: 'New team · memoire',
        loadComponent: () => import('./pages/teams/new-team').then((m) => m.NewTeam),
      },
      {
        path: 'databases',
        canActivate: [hasTeam],
        title: 'Databases · memoire',
        loadComponent: () => import('./pages/databases/databases').then((m) => m.Databases),
      },
      {
        path: 'databases/:name',
        canActivate: [hasTeam],
        loadComponent: () => import('./pages/database/database').then((m) => m.DatabasePage),
        children: [
          { path: '', pathMatch: 'full', redirectTo: 'console' },
          {
            path: 'console',
            loadComponent: () => import('./pages/database/console').then((m) => m.Console),
          },
          {
            path: 'tables/:table',
            loadComponent: () => import('./pages/database/rows').then((m) => m.Rows),
          },
          {
            path: 'schema',
            loadComponent: () => import('./pages/database/schema').then((m) => m.SchemaView),
          },
          {
            path: 'snapshots',
            loadComponent: () => import('./pages/database/snapshots').then((m) => m.Snapshots),
          },
        ],
      },
      {
        path: 'machines',
        canActivate: [hasTeam],
        title: 'Machines and API keys · memoire',
        loadComponent: () => import('./pages/machines/machines').then((m) => m.Machines),
      },
      { path: '', pathMatch: 'full', redirectTo: 'databases' },
    ],
  },
  { path: '**', redirectTo: '' },
];
