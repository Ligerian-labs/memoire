import { Component, OnInit, inject } from '@angular/core';
import { DatabaseContext } from './context';

@Component({
  selector: 'app-schema',
  template: `
    <div class="toolbar">
      <span class="spacer"></span>
      <button class="btn small" type="button" (click)="ctx.refreshSchema()">Refresh</button>
    </div>
    @if (ctx.schemaError()) {
      <p class="alert" role="alert">{{ ctx.schemaError() }}</p>
    }
    @if (ctx.schema(); as schema) {
      @for (table of schema.tables; track table.name) {
        <section class="panel">
          <header>
            <h2 class="mono">{{ table.name }}</h2>
            <span class="badge">table</span>
            @if (table.without_rowid) {
              <span class="badge">without rowid</span>
            }
          </header>
          <div class="content">
            <table class="data">
              <thead>
                <tr>
                  <th scope="col">Column</th>
                  <th scope="col">Type</th>
                  <th scope="col">Null</th>
                  <th scope="col">Default</th>
                  <th scope="col">Primary key</th>
                </tr>
              </thead>
              <tbody>
                @for (column of table.columns; track column.name) {
                  <tr>
                    <td class="mono">
                      {{ column.name }}
                      @if (column.generated) {
                        <span class="faint"> generated</span>
                      }
                    </td>
                    <td class="mono">{{ column.type || '—' }}</td>
                    <td>{{ column.not_null ? 'not null' : '' }}</td>
                    <td class="mono">{{ column.default ?? '' }}</td>
                    <td>{{ column.primary_key ? column.primary_key : '' }}</td>
                  </tr>
                }
              </tbody>
            </table>
            @if (table.indexes.length) {
              <h3>Indexes</h3>
              <ul class="indexes">
                @for (index of table.indexes; track index.name) {
                  <li>
                    <code>{{ index.name }}</code>
                    @if (index.unique) {
                      <span class="badge">unique</span>
                    }
                    <span class="faint mono">({{ indexColumns(index.columns) }})</span>
                  </li>
                }
              </ul>
            }
            <pre class="sql">{{ table.sql }}</pre>
          </div>
        </section>
      }
      @for (view of schema.views; track view.name) {
        <section class="panel">
          <header>
            <h2 class="mono">{{ view.name }}</h2>
            <span class="badge">view</span>
          </header>
          <div class="content">
            <pre class="sql">{{ view.sql }}</pre>
          </div>
        </section>
      }
      @if (!schema.tables.length && !schema.views.length) {
        <div class="panel empty">
          <p>No tables or views yet. Create one from the console in write mode.</p>
        </div>
      }
    }
  `,
  styles: `
    :host {
      display: grid;
      grid-template-columns: minmax(0, 1fr);
      min-width: 0;
      gap: 12px;
    }
    .spacer {
      flex: 1;
    }
    .content {
      display: grid;
      gap: 10px;
    }
    h3 {
      color: var(--ink-2);
    }
    .indexes {
      display: grid;
      gap: 4px;
      margin: 0;
      padding: 0;
      list-style: none;
      font-size: 13px;
    }
    .sql {
      margin: 0;
      padding: 8px 10px;
      overflow: auto;
      border-radius: var(--radius);
      background: var(--surface-2);
      font-family: var(--mono);
      font-size: 12.5px;
      white-space: pre-wrap;
    }
  `,
})
export class SchemaView implements OnInit {
  readonly ctx = inject(DatabaseContext);

  ngOnInit(): void {
    void this.ctx.refreshSchema();
  }

  indexColumns(columns: (string | null)[]): string {
    return columns.map((c) => c ?? 'expression').join(', ');
  }
}
