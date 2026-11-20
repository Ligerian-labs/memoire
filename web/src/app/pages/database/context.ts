import { Injectable, computed, inject, signal } from '@angular/core';
import { Api } from '../../api/api';
import { Database, Mode, Schema, StatementResult } from '../../api/memoire';
import { describe } from '../../messages';
import { SessionStore } from '../../session';
import { groupSchema } from './groups';

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
  private readonly session = inject(SessionStore);
  /** memoire's API bound to the team active when this database opened. */
  memoire = this.api.memoire.forTeam(this.session.teamId());
  readonly name = signal('');
  readonly database = signal<Database | null>(null);
  readonly schema = signal<Schema | null>(null);
  /** The schema's tables split into the application's, the memory store's and FTS5's own. */
  readonly groups = computed(() => {
    const schema = this.schema();
    return schema ? groupSchema(schema) : null;
  });
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
    this.memoire = this.api.memoire.forTeam(this.session.teamId());
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
      this.database.set(await this.memoire.get(this.name()));
      this.error.set('');
    } catch (error) {
      this.error.set(describe(error));
    }
  }

  async refreshSchema(): Promise<void> {
    try {
      this.schema.set(await this.memoire.schema(this.name()));
      this.schemaError.set('');
    } catch (error) {
      this.schemaError.set(describe(error));
    }
  }
}
