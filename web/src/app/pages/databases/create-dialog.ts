import { Component, inject, output, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Api } from '../../api/api';
import { Database, Kind } from '../../api/memoire';
import { DATABASE_NAME } from '../../format';
import { describe } from '../../messages';
import { Modal } from '../../ui/modal';

export const TTL_CHOICES = [
  { seconds: 3600, label: '1 hour' },
  { seconds: 6 * 3600, label: '6 hours' },
  { seconds: 86400, label: '24 hours (default)' },
  { seconds: 3 * 86400, label: '3 days' },
  { seconds: 7 * 86400, label: '7 days' },
];

@Component({
  selector: 'app-create-database',
  imports: [FormsModule, Modal],
  template: `
    <dialog appModal (closed)="cancelled.emit()" aria-labelledby="create-title">
      <form (ngSubmit)="submit()">
        <header><h2 id="create-title">New database</h2></header>
        <label class="field">
          <span>Name</span>
          <input
            type="text"
            name="name"
            class="mono"
            required
            autocomplete="off"
            spellcheck="false"
            [attr.aria-invalid]="name !== '' && !valid()"
            [(ngModel)]="name"
            autofocus
          />
          <small
            >2 to 63 characters: a lowercase letter, then lowercase letters, digits and
            dashes.</small
          >
        </label>
        <fieldset class="kinds">
          <legend>Kind</legend>
          <label class="kind" [class.on]="kind() === 'burstable'">
            <input
              type="radio"
              name="kind"
              value="burstable"
              [checked]="kind() === 'burstable'"
              (change)="kind.set('burstable')"
            />
            <strong>Burstable</strong>
            <span
              >Scratch space. Expires when idle, at most 7 days after creation. No backups.</span
            >
          </label>
          <label class="kind" [class.on]="kind() === 'persistent'">
            <input
              type="radio"
              name="kind"
              value="persistent"
              [checked]="kind() === 'persistent'"
              (change)="kind.set('persistent')"
            />
            <strong>Persistent</strong>
            <span>Lives until deleted. Snapshots every 6 hours, kept 7 days.</span>
          </label>
        </fieldset>
        @if (kind() === 'burstable') {
          <label class="field">
            <span>Expires after idle for</span>
            <select name="ttl" [(ngModel)]="ttl">
              @for (choice of ttlChoices; track choice.seconds) {
                <option [ngValue]="choice.seconds">{{ choice.label }}</option>
              }
            </select>
          </label>
        }
        @if (error()) {
          <p class="alert" role="alert">{{ error() }}</p>
        }
        <footer>
          <button class="btn" type="button" (click)="cancelled.emit()">Cancel</button>
          <button class="btn primary" type="submit" [disabled]="busy() || !valid()">
            Create database
          </button>
        </footer>
      </form>
    </dialog>
  `,
  styles: `
    .kinds {
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 8px;
      margin: 0;
      padding: 0;
      border: 0;
    }
    legend {
      margin-bottom: 4px;
      font-size: 12px;
      font-weight: 600;
      color: var(--ink-2);
    }
    .kind {
      display: grid;
      grid-template-columns: auto 1fr;
      gap: 2px 8px;
      align-items: start;
      padding: 10px;
      border: 1px solid var(--rule-strong);
      border-radius: var(--radius);
      cursor: pointer;
    }
    .kind.on {
      border-color: var(--accent);
      background: var(--accent-soft);
    }
    .kind input {
      margin: 3px 0 0;
      accent-color: var(--accent);
    }
    .kind span {
      grid-column: 2;
      font-size: 12px;
      color: var(--ink-2);
    }
  `,
})
export class CreateDatabaseDialog {
  private readonly api = inject(Api);
  readonly created = output<Database>();
  readonly cancelled = output<void>();
  readonly ttlChoices = TTL_CHOICES;
  readonly kind = signal<Kind>('burstable');
  readonly busy = signal(false);
  readonly error = signal('');
  name = '';
  ttl = 86400;

  valid(): boolean {
    return DATABASE_NAME.test(this.name);
  }

  async submit(): Promise<void> {
    if (!this.valid()) return;
    this.busy.set(true);
    this.error.set('');
    try {
      const kind = this.kind();
      const database = await this.api.memoire.create(
        kind === 'burstable'
          ? { name: this.name, kind, idle_ttl_seconds: this.ttl }
          : { name: this.name, kind },
      );
      this.created.emit(database);
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.busy.set(false);
    }
  }
}
