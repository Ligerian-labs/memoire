import { Component, inject, signal } from '@angular/core';
import { Router, RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { describe } from '../../messages';
import { SessionStore } from '../../session';

@Component({
  selector: 'app-shell',
  imports: [RouterOutlet, RouterLink, RouterLinkActive],
  styleUrl: './shell.css',
  template: `
    <nav class="rail" aria-label="Main">
      <a class="brand" routerLink="/databases"><img src="favicon.svg" alt="" />memoire</a>
      <label class="team">
        <span>Team</span>
        <select
          [value]="session.teamId() ?? ''"
          (change)="switchTeam($any($event.target).value)"
          [disabled]="switching()"
          data-testid="team-switch"
        >
          @if (!session.teamId()) {
            <option value="" disabled>No team selected</option>
          }
          @for (team of session.teams(); track team.id) {
            <option [value]="team.id">{{ team.name }}</option>
          }
        </select>
      </label>
      <a class="new-team" routerLink="/teams/new" routerLinkActive="on">New team</a>
      @if (error()) {
        <p class="rail-error" role="alert">{{ error() }}</p>
      }
      <ul class="links">
        <li><a routerLink="/databases" routerLinkActive="on">Databases</a></li>
        <li><a routerLink="/machines" routerLinkActive="on">Machines and API keys</a></li>
      </ul>
      <div class="me">
        <span class="email" [title]="session.user()?.email ?? ''">{{ session.user()?.email }}</span>
        <button class="btn small ghost" type="button" (click)="signOut()">Sign out</button>
      </div>
    </nav>
    <div class="main"><router-outlet /></div>
  `,
})
export class Shell {
  readonly session = inject(SessionStore);
  private readonly router = inject(Router);
  readonly switching = signal(false);
  readonly error = signal('');

  async switchTeam(id: string): Promise<void> {
    if (!id || id === this.session.teamId()) return;
    this.switching.set(true);
    this.error.set('');
    try {
      await this.session.selectTeam(id);
      await this.router.navigateByUrl('/databases');
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.switching.set(false);
    }
  }

  async signOut(): Promise<void> {
    await this.session.signOut().catch(() => undefined);
    await this.router.navigateByUrl('/sign-in');
  }
}
