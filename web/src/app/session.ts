import { Injectable, computed, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import { Api } from './api/api';
import { ApiError } from './api/http';
import { Organization, SignedIn, User } from './api/identity';

/**
 * Who is signed in and which team is active. The active team lives in the
 * gateway session (PUT /v1/identity/session/organization), so a reload
 * restores it from GET /v1/identity/session.
 */
@Injectable({ providedIn: 'root' })
export class SessionStore {
  private readonly api = inject(Api);
  private readonly router = inject(Router);
  readonly user = signal<User | null>(null);
  readonly teams = signal<Organization[]>([]);
  readonly teamId = signal<string | null>(null);
  /** Why the session could not be loaded (the gateway is down…), for the sign-in page. */
  readonly restoreError = signal<unknown>(null);
  readonly team = computed(() => this.teams().find((t) => t.id === this.teamId()) ?? null);
  private restoring: Promise<void> | null = null;

  constructor() {
    // A session that ends while in use (expired, revoked) sends the person back to sign in.
    this.api.http.onError = (error) => {
      if (error.code !== 'unauthenticated' || !this.user()) return;
      const next = this.router.url;
      this.clear();
      this.restoring = Promise.resolve();
      void this.router.navigate(['/sign-in'], { queryParams: { next } });
    };
  }

  /** Loads the current session once; later calls share the first. */
  restore(): Promise<void> {
    this.restoring ??= this.load();
    return this.restoring;
  }

  private async load(): Promise<void> {
    try {
      await this.adopt(await this.api.identity.session());
      this.restoreError.set(null);
    } catch (error) {
      if (!(error instanceof ApiError && error.status === 401)) {
        this.restoring = null;
        this.restoreError.set(error);
        throw error;
      }
      this.restoreError.set(null);
      this.clear();
    }
  }

  async signIn(email: string, password: string): Promise<void> {
    await this.adopt(await this.api.identity.signIn(email, password));
    this.restoring = Promise.resolve();
  }

  async confirmEmail(token: string): Promise<void> {
    await this.adopt(await this.api.identity.confirmEmail(token));
    this.restoring = Promise.resolve();
  }

  /** Signs out; on any failure but an already-ended session it keeps the session and throws. */
  async signOut(): Promise<void> {
    const csrf = this.api.http.csrf;
    try {
      await this.api.identity.signOut();
    } catch (error) {
      if (!(error instanceof ApiError && error.status === 401)) {
        this.api.http.csrf = csrf;
        throw error;
      }
    }
    this.clear();
    this.restoring = Promise.resolve();
  }

  async refreshTeams(): Promise<void> {
    const page = await this.api.identity.organizations();
    this.teams.set(
      page.items.map((m) => m.organization).sort((a, b) => a.name.localeCompare(b.name)),
    );
  }

  async selectTeam(id: string): Promise<void> {
    const answer = await this.api.identity.selectOrganization(id);
    this.setTeam(answer.session.active_organization_id);
    remember(this.teamId());
  }

  async createTeam(name: string, slug: string): Promise<Organization> {
    const created = await this.api.identity.createOrganization(name, slug);
    await this.refreshTeams();
    await this.selectTeam(created.organization.id);
    return created.organization;
  }

  private setTeam(id: string | null): void {
    this.teamId.set(id);
  }

  /** Forgets the session locally, for a 401 met anywhere. */
  clear(): void {
    this.user.set(null);
    this.teams.set([]);
    this.setTeam(null);
    this.api.http.csrf = null;
  }

  private async adopt(signedIn: SignedIn): Promise<void> {
    this.restoreError.set(null);
    this.user.set(signedIn.user);
    this.setTeam(signedIn.session.active_organization_id);
    await this.refreshTeams();
    // A session can point at a team the member has since left.
    if (this.teamId() && !this.team()) this.setTeam(null);
    // A new session has no team yet: reopen the last one used here, else the first.
    if (!this.teamId() && this.teams().length) {
      const last = recalled();
      const pick = this.teams().find((t) => t.id === last) ?? this.teams()[0];
      await this.selectTeam(pick.id);
    }
  }
}

// Only the team id, a non-secret preference, is kept in the browser.
const LAST_TEAM = 'memoire.last-team';
function remember(id: string | null): void {
  try {
    if (id) localStorage.setItem(LAST_TEAM, id);
  } catch {
    /* storage unavailable */
  }
}
function recalled(): string | null {
  try {
    return localStorage.getItem(LAST_TEAM);
  } catch {
    return null;
  }
}
