import { Component, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { RouterLink } from '@angular/router';
import { Api } from '../../api/api';
import { describe } from '../../messages';

@Component({
  selector: 'app-register',
  imports: [FormsModule, RouterLink],
  styleUrl: './auth.css',
  template: `
    <main class="card">
      <div class="brand"><img src="favicon.svg" alt="" />memoire</div>
      @if (sentTo(); as address) {
        <h1>Check your email</h1>
        <p>
          We sent a confirmation link to <strong>{{ address }}</strong>. Open it in this browser to
          finish creating your account.
        </p>
        <p class="switch"><a routerLink="/sign-in">Back to sign in</a></p>
      } @else {
        <h1>Create an account</h1>
        <form (ngSubmit)="submit()">
          <label class="field">
            <span>Email</span>
            <input type="email" name="email" autocomplete="email" required [(ngModel)]="email" />
          </label>
          <label class="field">
            <span>Password</span>
            <input
              type="password"
              name="password"
              autocomplete="new-password"
              minlength="12"
              required
              [(ngModel)]="password"
            />
            <small>At least 12 characters.</small>
          </label>
          @if (error()) {
            <p class="alert" role="alert">{{ error() }}</p>
          }
          <button class="btn primary" type="submit" [disabled]="busy()">
            {{ busy() ? 'Creating…' : 'Create account' }}
          </button>
        </form>
        <p class="switch">Already registered? <a routerLink="/sign-in">Sign in</a></p>
      }
    </main>
  `,
})
export class Register {
  private readonly api = inject(Api);
  email = '';
  password = '';
  readonly busy = signal(false);
  readonly error = signal('');
  readonly sentTo = signal('');

  async submit(): Promise<void> {
    this.busy.set(true);
    this.error.set('');
    try {
      await this.api.identity.register(this.email.trim(), this.password);
      this.password = '';
      this.sentTo.set(this.email.trim());
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.busy.set(false);
    }
  }
}
