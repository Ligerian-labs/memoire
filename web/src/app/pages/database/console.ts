import { Component, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ApiError } from '../../api/http';
import { Mode, Statement } from '../../api/memoire';
import { cell, parseParams, ParamsError } from '../../api/values';
import { describe } from '../../messages';
import { splitStatements } from '../../sql/split';
import { Grid } from '../../ui/grid';
import { DatabaseContext } from './context';

@Component({
  selector: 'app-console',
  imports: [FormsModule, Grid],
  templateUrl: './console.html',
  styleUrl: './console.css',
})
export class Console {
  readonly ctx = inject(DatabaseContext);
  readonly running = signal(false);
  readonly error = signal('');
  /** The text of the statement SQLite refused, shown under the error. */
  readonly failedSql = signal<string | null>(null);
  readonly showParams = signal(this.ctx.params !== '');
  readonly cell = cell;

  setMode(mode: Mode): void {
    this.ctx.mode.set(mode);
  }

  onKey(event: KeyboardEvent): void {
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void this.run();
    }
  }

  async run(): Promise<void> {
    const statements = splitStatements(this.ctx.sql);
    if (!statements.length || this.running()) return;
    this.error.set('');
    this.failedSql.set(null);
    let params;
    try {
      params = parseParams(this.ctx.params);
    } catch (error) {
      this.error.set(error instanceof ParamsError ? error.message : String(error));
      return;
    }
    if (params !== undefined && statements.length > 1) {
      this.error.set('Parameters apply to a single statement. Run the statements one at a time.');
      return;
    }
    const body: Statement[] = statements.map((sql) => ({ sql }));
    const mode = this.ctx.mode();
    this.running.set(true);
    const started = performance.now();
    try {
      const answer = await this.ctx.memoire.query(
        this.ctx.name(),
        { mode, transaction: this.ctx.transaction, statements: body },
        params === undefined ? undefined : this.ctx.params,
      );
      this.ctx.lastRun.set({
        statements,
        results: answer.results,
        mode,
        ms: Math.round(performance.now() - started),
      });
      if (mode === 'write') await this.ctx.refresh();
    } catch (error) {
      this.ctx.lastRun.set(null);
      this.error.set(describe(error));
      const failed = error instanceof ApiError ? error.sql : null;
      if (failed) this.failedSql.set(statements[failed.statement] ?? null);
      if (mode === 'write') await this.ctx.refresh();
    } finally {
      this.running.set(false);
    }
  }
}
