import { Component, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';
import { slugify } from '../../format';
import { describe } from '../../messages';
import { SessionStore } from '../../session';

@Component({
  selector: 'app-new-team',
  imports: [FormsModule],
  template: `
    <section class="page">
      <div class="page-head"><h1>New team</h1></div>
      @if (!session.teams().length) {
        <p class="muted">
          Databases, machines and keys belong to a team. Create one to start; you can invite others
          later.
        </p>
      }
      <form class="panel content-form" (ngSubmit)="submit()">
        <label class="field">
          <span>Name</span>
          <input
            type="text"
            name="name"
            required
            maxlength="100"
            [(ngModel)]="name"
            (ngModelChange)="nameChanged()"
          />
        </label>
        <label class="field">
          <span>Slug</span>
          <input
            type="text"
            name="slug"
            class="mono"
            required
            maxlength="63"
            [(ngModel)]="slug"
            (ngModelChange)="slugEdited = true"
          />
          <small>Lowercase letters, digits and dashes. Unique across the gateway.</small>
        </label>
        @if (error()) {
          <p class="alert" role="alert">{{ error() }}</p>
        }
        <div>
          <button class="btn primary" type="submit" [disabled]="busy() || !name.trim() || !slug">
            Create team
          </button>
        </div>
      </form>
    </section>
  `,
  styles: `
    .content-form {
      display: grid;
      gap: 14px;
      max-width: 440px;
      padding: 16px;
    }
  `,
})
export class NewTeam {
  readonly session = inject(SessionStore);
  private readonly router = inject(Router);
  name = '';
  slug = '';
  slugEdited = false;
  readonly busy = signal(false);
  readonly error = signal('');

  nameChanged(): void {
    if (!this.slugEdited) this.slug = slugify(this.name);
  }

  async submit(): Promise<void> {
    this.busy.set(true);
    this.error.set('');
    try {
      await this.session.createTeam(this.name.trim(), this.slug.trim());
      await this.router.navigateByUrl('/databases');
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.busy.set(false);
    }
  }
}
