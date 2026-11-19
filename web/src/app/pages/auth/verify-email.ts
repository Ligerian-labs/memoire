import { Component, OnInit, inject, signal } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import { describe } from '../../messages';
import { SessionStore } from '../../session';

/** The landing of identity's verification link: `/auth/verify-email#token=…`. */
@Component({
  selector: 'app-verify-email',
  imports: [RouterLink],
  styleUrl: './auth.css',
  template: `
    <main class="card">
      <div class="brand"><img src="favicon.svg" alt="" />memoire</div>
      <h1>Confirm your email</h1>
      @if (error()) {
        <p class="alert" role="alert">{{ error() }}</p>
        <p class="switch"><a routerLink="/sign-in">Go to sign in</a></p>
      } @else {
        <p class="muted">Confirming…</p>
      }
    </main>
  `,
})
export class VerifyEmail implements OnInit {
  private readonly session = inject(SessionStore);
  private readonly router = inject(Router);
  readonly error = signal('');

  async ngOnInit(): Promise<void> {
    const token = new URLSearchParams(location.hash.slice(1)).get('token');
    // The token is single-use and secret: drop it from the address bar and history.
    history.replaceState(null, '', location.pathname);
    if (!token) {
      this.error.set('This link has no token. Open the link from the email again.');
      return;
    }
    try {
      await this.session.confirmEmail(token);
      await this.router.navigateByUrl('/teams/new');
    } catch (error) {
      this.error.set(describe(error));
    }
  }
}
