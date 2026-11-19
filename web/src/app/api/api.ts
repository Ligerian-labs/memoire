import { Injectable, InjectionToken, inject } from '@angular/core';
import { Http } from './http';
import { IdentityApi } from './identity';
import { MemoireApi } from './memoire';

export const FETCH = new InjectionToken<typeof fetch>('fetch', {
  providedIn: 'root',
  factory: () => fetch.bind(globalThis),
});

/** The gateway, reached through the same origin (the dev proxy or the static server). */
@Injectable({ providedIn: 'root' })
export class Api {
  readonly http = new Http(inject(FETCH));
  readonly identity = new IdentityApi(this.http);
  readonly memoire = new MemoireApi(this.http);
}
