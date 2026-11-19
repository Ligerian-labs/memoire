import { Component, OnDestroy, effect, inject, input, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router, RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { Api } from '../../api/api';
import { bytes, duration, relative, timestamp } from '../../format';
import { describe } from '../../messages';
import { Modal } from '../../ui/modal';
import { TTL_CHOICES } from '../databases/create-dialog';
import { DatabaseContext } from './context';

@Component({
  selector: 'app-database',
  imports: [FormsModule, RouterOutlet, RouterLink, RouterLinkActive, Modal],
  providers: [DatabaseContext],
  templateUrl: './database.html',
  styleUrl: './database.css',
})
export class DatabasePage implements OnDestroy {
  private readonly api = inject(Api);
  private readonly router = inject(Router);
  readonly ctx = inject(DatabaseContext);
  readonly name = input.required<string>();
  readonly now = signal(Date.now());
  readonly dialog = signal<'extend' | 'delete' | null>(null);
  readonly busy = signal(false);
  readonly actionError = signal('');
  readonly ttlChoices = TTL_CHOICES;
  extendTtl: number | null = null;
  confirmName = '';
  private readonly tick = setInterval(() => this.now.set(Date.now()), 15_000);
  readonly bytes = bytes;
  readonly relative = relative;
  readonly timestamp = timestamp;
  readonly duration = duration;

  constructor() {
    effect(() => {
      const name = this.name();
      untracked(() => void this.ctx.open(name));
    });
  }

  ngOnDestroy(): void {
    clearInterval(this.tick);
  }

  exportUrl(): string {
    return this.api.memoire.exportUrl(this.ctx.name());
  }

  openDialog(which: 'extend' | 'delete'): void {
    this.actionError.set('');
    this.extendTtl = null;
    this.confirmName = '';
    this.dialog.set(which);
  }

  async extend(): Promise<void> {
    await this.act(async () => {
      this.ctx.database.set(
        await this.api.memoire.extend(this.ctx.name(), this.extendTtl ?? undefined),
      );
      this.now.set(Date.now());
    });
  }

  async remove(): Promise<void> {
    await this.act(async () => {
      await this.api.memoire.delete(this.ctx.name());
      await this.router.navigateByUrl('/databases');
    });
  }

  private async act(work: () => Promise<void>): Promise<void> {
    this.busy.set(true);
    this.actionError.set('');
    try {
      await work();
      this.dialog.set(null);
    } catch (error) {
      this.actionError.set(describe(error));
    } finally {
      this.busy.set(false);
    }
  }
}
