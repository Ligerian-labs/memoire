import { Component, computed, effect, inject, input, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Api } from '../../api/api';
import { RowPage } from '../../api/memoire';
import { describe } from '../../messages';
import { Grid } from '../../ui/grid';
import { DatabaseContext } from './context';

/** The row browser: pages forward with `next_after`, back through the cursors already seen. */
@Component({
  selector: 'app-rows',
  imports: [FormsModule, Grid],
  template: `
    <div class="toolbar">
      <h2 class="mono">{{ table() }}</h2>
      @if (kind(); as k) {
        <span class="badge">{{ k }}</span>
      }
      <span class="spacer"></span>
      <label class="check">
        Rows per page
        <select name="limit" [(ngModel)]="limit" (ngModelChange)="first()">
          @for (n of sizes; track n) {
            <option [ngValue]="n">{{ n }}</option>
          }
        </select>
      </label>
      <button class="btn small" type="button" (click)="reload()" [disabled]="loading()">
        Refresh
      </button>
    </div>
    @if (error()) {
      <p class="alert" role="alert">{{ error() }}</p>
    }
    @if (page(); as p) {
      @if (p.rows.length) {
        <app-grid
          [columns]="p.columns"
          [rows]="p.rows"
          [offset]="offset()"
          maxHeight="calc(100vh - 330px)"
        />
      } @else {
        <div class="panel empty">
          <p>{{ cursors().length > 1 ? 'No more rows.' : 'This table is empty.' }}</p>
        </div>
      }
      <div class="toolbar pager">
        <button
          class="btn small"
          type="button"
          (click)="previous()"
          [disabled]="loading() || cursors().length < 2"
        >
          Previous
        </button>
        <span class="faint">
          @if (p.rows.length) {
            Rows {{ offset() + 1 }}–{{ offset() + p.rows.length }}
          } @else {
            Page {{ cursors().length }}
          }
        </span>
        <button
          class="btn small"
          type="button"
          (click)="nextPage()"
          [disabled]="loading() || !p.next_after"
        >
          Next
        </button>
      </div>
    } @else if (loading()) {
      <p class="faint">Loading…</p>
    }
  `,
  styles: `
    :host {
      display: grid;
      grid-template-columns: minmax(0, 1fr);
      gap: 10px;
      min-width: 0;
    }
    .spacer {
      flex: 1;
    }
    .pager .faint {
      font-size: 12.5px;
      font-variant-numeric: tabular-nums;
    }
    select {
      height: 26px;
      font-size: 13px;
    }
  `,
})
export class Rows {
  private readonly api = inject(Api);
  private readonly ctx = inject(DatabaseContext);
  readonly table = input.required<string>();
  readonly sizes = [50, 100, 500, 1000];
  limit = 100;
  /** The `after` cursor of each page seen so far; the last is the current page's. */
  readonly cursors = signal<(string | null)[]>([null]);
  readonly page = signal<RowPage | null>(null);
  readonly loading = signal(false);
  readonly error = signal('');
  /** Rows before the shown page, fixed when that page arrives. */
  readonly offset = signal(0);
  private generation = 0;
  readonly kind = computed(() => {
    const schema = this.ctx.schema();
    if (!schema) return null;
    if (schema.views.some((v) => v.name === this.table())) return 'view';
    const table = schema.tables.find((t) => t.name === this.table());
    return table?.without_rowid ? 'without rowid' : null;
  });

  constructor() {
    effect(() => {
      this.table();
      this.ctx.name();
      untracked(() => void this.first());
    });
  }

  first(): Promise<void> {
    this.page.set(null);
    this.cursors.set([null]);
    this.offset.set(0);
    return this.load([null]);
  }

  reload(): Promise<void> {
    return this.load(this.cursors());
  }

  nextPage(): Promise<void> {
    const after = this.page()?.next_after;
    if (!after) return Promise.resolve();
    return this.load([...this.cursors(), after]);
  }

  previous(): Promise<void> {
    if (this.cursors().length < 2) return Promise.resolve();
    return this.load(this.cursors().slice(0, -1));
  }

  /** Loads the page at the end of `cursors`; the page, cursors and offset change together. */
  private async load(cursors: (string | null)[]): Promise<void> {
    const generation = ++this.generation;
    const limit = this.limit;
    this.loading.set(true);
    this.error.set('');
    try {
      const page = await this.api.memoire.rows(
        this.ctx.name(),
        this.table(),
        limit,
        cursors.at(-1),
      );
      if (generation !== this.generation) return;
      this.cursors.set(cursors);
      this.offset.set((cursors.length - 1) * limit);
      this.page.set(page);
    } catch (error) {
      if (generation !== this.generation) return;
      this.page.set(null);
      this.error.set(describe(error));
    } finally {
      if (generation === this.generation) this.loading.set(false);
    }
  }
}
