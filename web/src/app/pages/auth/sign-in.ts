import { Component, inject, input, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router, RouterLink } from '@angular/router';
import { describe } from '../../messages';
import { SessionStore } from '../../session';

@Component({
  selector: 'app-sign-in',
  imports: [FormsModule, RouterLink],
  styleUrl: './auth.css',
  template: `
    <main class="card">
      <div class="brand"><img src="favicon.svg" alt="" />memoire</div>
      <h1>Sign in</h1>
      @if (session.restoreError(); as failure) {
        <p class="alert" role="alert">
          Could not reach your session: {{ explain(failure) }}
          <button class="btn small" type="button" (click)="retry()">Retry</button>
        </p>
      }
      <form (ngSubmit)="submit()">
        <label class="field">
          <span>Email</span>
          <input type="email" name="email" autocomplete="username" required [(ngModel)]="email" />
        </label>
        <label class="field">
          <span>Password</span>
          <input
            type="password"
            name="password"
            autocomplete="current-password"
            required
            [(ngModel)]="password"
          />
        </label>
        @if (error()) {
          <p class="alert" role="alert">{{ error() }}</p>
        }
        <button class="btn primary" type="submit" [disabled]="busy()">
          {{ busy() ? 'Signing in…' : 'Sign in' }}
        </button>
      </form>
      <p class="switch">No account yet? <a routerLink="/register">Create one</a></p>
    </main>
  `,
})
export class SignIn {
  readonly session = inject(SessionStore);
  readonly explain = describe;
  private readonly router = inject(Router);
  readonly next = input<string>();
  email = '';
  password = '';
  readonly busy = signal(false);
  readonly error = signal('');

  async retry(): Promise<void> {
    try {
      await this.session.restore();
      if (this.session.user()) await this.router.navigateByUrl(safeNext(this.next()));
    } catch {
      /* restoreError shows it */
    }
  }

  async submit(): Promise<void> {
    this.busy.set(true);
    this.error.set('');
    try {
      await this.session.signIn(this.email.trim(), this.password);
      this.password = '';
      await this.router.navigateByUrl(safeNext(this.next()));
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.busy.set(false);
    }
  }
}

/** Only same-app paths, never another origin. */
export function safeNext(next: string | undefined): string {
  return next && next.startsWith('/') && !next.startsWith('//') ? next : '/databases';
}
