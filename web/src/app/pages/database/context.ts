import { Injectable, inject, signal } from '@angular/core';
import { Api } from '../../api/api';
import { Database, Mode, Schema, StatementResult } from '../../api/memoire';
import { describe } from '../../messages';

export interface ConsoleRun {
  statements: string[];
  results: StatementResult[];
  mode: Mode;
  ms: number;
}

/** State shared by one database's tabs; provided by DatabasePage, so it lives while the page does. */
@Injectable()
export class DatabaseContext {
  private readonly api = inject(Api);
  readonly name = signal('');
  readonly database = signal<Database | null>(null);
  readonly schema = signal<Schema | null>(null);
  readonly error = signal('');
  readonly schemaError = signal('');

  // The console keeps its draft and last run across tab changes.
  sql = '';
  params = '';
  readonly mode = signal<Mode>('read');
  transaction = false;
  readonly lastRun = signal<ConsoleRun | null>(null);

  async open(name: string): Promise<void> {
    if (name === this.name()) return;
    this.name.set(name);
    this.database.set(null);
    this.schema.set(null);
    this.lastRun.set(null);
    this.sql = '';
    this.params = '';
    this.mode.set('read');
    await this.refresh();
  }

  async refresh(): Promise<void> {
    await this.refreshDatabase();
    if (this.database()?.state === 'active') await this.refreshSchema();
    else this.schema.set(null);
  }

  async refreshDatabase(): Promise<void> {
    try {
      this.database.set(await this.api.memoire.get(this.name()));
      this.error.set('');
    } catch (error) {
      this.error.set(describe(error));
    }
  }

  async refreshSchema(): Promise<void> {
    try {
      this.schema.set(await this.api.memoire.schema(this.name()));
      this.schemaError.set('');
    } catch (error) {
      this.schemaError.set(describe(error));
    }
  }
}
