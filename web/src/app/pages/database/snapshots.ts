import { Component, effect, inject, signal, untracked } from '@angular/core';
import { Api } from '../../api/api';
import { Snapshot } from '../../api/memoire';
import { bytes, timestamp } from '../../format';
import { describe } from '../../messages';
import { Modal } from '../../ui/modal';
import { DatabaseContext } from './context';

@Component({
  selector: 'app-snapshots',
  imports: [Modal],
  template: `
    @if (ctx.database()?.kind === 'burstable') {
      <div class="panel empty">
        <p>Burstable databases have no snapshots.</p>
        <p class="faint">
          Create a persistent database to get a snapshot every 6 hours, kept 7 days.
        </p>
      </div>
    } @else {
      <div class="toolbar">
        <span class="faint"
          >Taken every 6 hours and kept 7 days. Restoring first saves the current contents.</span
        >
        <span class="spacer"></span>
        <button class="btn small" type="button" (click)="load()" [disabled]="loading()">
          Refresh
        </button>
      </div>
      @if (error()) {
        <p class="alert" role="alert">{{ error() }}</p>
      }
      @if (notice()) {
        <p class="notice" role="status">{{ notice() }}</p>
      }
      <div class="table-wrap">
        <table class="data">
          <thead>
            <tr>
              <th scope="col">Taken</th>
              <th scope="col">Reason</th>
              <th scope="col" class="num">Size</th>
              <th scope="col">Id</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            @for (snapshot of items(); track snapshot.id) {
              <tr>
                <td>{{ timestamp(snapshot.created_at) }}</td>
                <td>{{ snapshot.reason === 'pre_restore' ? 'before a restore' : 'scheduled' }}</td>
                <td class="num">{{ bytes(snapshot.size_bytes) }}</td>
                <td class="mono faint">{{ snapshot.id }}</td>
                <td class="num">
                  <button class="btn small" type="button" (click)="confirming.set(snapshot)">
                    Restore
                  </button>
                </td>
              </tr>
            } @empty {
              <tr>
                <td colspan="5">
                  <div class="empty">
                    {{
                      loading()
                        ? 'Loading…'
                        : 'No snapshots yet. The first is taken within 6 hours.'
                    }}
                  </div>
                </td>
              </tr>
            }
          </tbody>
        </table>
      </div>
    }

    @if (confirming(); as snapshot) {
      <dialog appModal (closed)="confirming.set(null)" aria-labelledby="restore-title">
        <div class="body">
          <header>
            <h2 id="restore-title">Restore {{ ctx.name() }}</h2>
          </header>
          <p>
            Replace the current contents with the snapshot from
            <strong>{{ timestamp(snapshot.created_at) }}</strong>? The current contents are saved as
            a new snapshot first, so this can be undone.
          </p>
          @if (restoreError()) {
            <p class="alert" role="alert">{{ restoreError() }}</p>
          }
          <footer>
            <button class="btn" type="button" (click)="confirming.set(null)">Cancel</button>
            <button
              class="btn primary"
              type="button"
              (click)="restore(snapshot)"
              [disabled]="busy()"
            >
              Restore snapshot
            </button>
          </footer>
        </div>
      </dialog>
    }
  `,
  styles: `
    :host {
      display: grid;
      grid-template-columns: minmax(0, 1fr);
      min-width: 0;
      gap: 10px;
    }
    .spacer {
      flex: 1;
    }
    .toolbar .faint {
      font-size: 12.5px;
    }
  `,
})
export class Snapshots {
  private readonly api = inject(Api);
  readonly ctx = inject(DatabaseContext);
  readonly items = signal<Snapshot[]>([]);
  readonly loading = signal(false);
  readonly error = signal('');
  readonly notice = signal('');
  readonly confirming = signal<Snapshot | null>(null);
  readonly busy = signal(false);
  readonly restoreError = signal('');
  readonly bytes = bytes;
  readonly timestamp = timestamp;

  constructor() {
    effect(() => {
      const db = this.ctx.database();
      if (db?.kind === 'persistent' && db.state === 'active') untracked(() => void this.load());
    });
  }

  async load(): Promise<void> {
    this.loading.set(true);
    this.error.set('');
    try {
      this.items.set((await this.api.memoire.snapshots(this.ctx.name())).items);
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.loading.set(false);
    }
  }

  async restore(snapshot: Snapshot): Promise<void> {
    this.busy.set(true);
    this.restoreError.set('');
    try {
      this.ctx.database.set(await this.api.memoire.restore(this.ctx.name(), snapshot.id));
      this.confirming.set(null);
      this.notice.set(`Restored the snapshot from ${timestamp(snapshot.created_at)}.`);
      // Setting the database reloads the list (the effect above).
      await this.ctx.refreshSchema();
    } catch (error) {
      this.restoreError.set(describe(error));
    } finally {
      this.busy.set(false);
    }
  }
}
