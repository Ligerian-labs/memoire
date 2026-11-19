import { Component, OnDestroy, effect, inject, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router } from '@angular/router';
import { Api } from '../../api/api';
import { Database } from '../../api/memoire';
import { bytes, duration, relative, timestamp } from '../../format';
import { describe } from '../../messages';
import { SessionStore } from '../../session';
import { CreateDatabaseDialog } from './create-dialog';

@Component({
  selector: 'app-databases',
  imports: [FormsModule, CreateDatabaseDialog],
  templateUrl: './databases.html',
})
export class Databases implements OnDestroy {
  private readonly api = inject(Api);
  private readonly router = inject(Router);
  readonly session = inject(SessionStore);
  readonly items = signal<Database[]>([]);
  readonly next = signal<string | null>(null);
  readonly loading = signal(true);
  readonly error = signal('');
  readonly creating = signal(false);
  readonly now = signal(Date.now());
  showExpired = false;
  private readonly tick = setInterval(() => this.now.set(Date.now()), 15_000);
  readonly bytes = bytes;
  readonly relative = relative;
  readonly timestamp = timestamp;
  readonly duration = duration;

  constructor() {
    effect(() => {
      this.session.teamId();
      untracked(() => void this.load());
    });
  }

  ngOnDestroy(): void {
    clearInterval(this.tick);
  }

  async load(more = false): Promise<void> {
    this.loading.set(true);
    this.error.set('');
    try {
      const page = await this.api.memoire.list(
        this.showExpired ? 'all' : 'active',
        more ? this.next() : null,
      );
      this.items.update((items) => (more ? [...items, ...page.items] : page.items));
      this.next.set(page.next_cursor);
      this.now.set(Date.now());
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.loading.set(false);
    }
  }

  open(database: Database): void {
    void this.router.navigate(['/databases', database.name]);
  }

  async created(database: Database): Promise<void> {
    this.creating.set(false);
    await this.router.navigate(['/databases', database.name]);
  }
}
