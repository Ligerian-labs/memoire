import { Component, computed, input } from '@angular/core';
import { Column } from '../api/memoire';
import { Cell, Value, cell } from '../api/values';

/** A read-only result grid: typed headers, one mono cell per value, a row number gutter. */
@Component({
  selector: 'app-grid',
  host: { style: 'display: block; min-width: 0' },
  template: `
    <div class="table-wrap" [style.max-height]="maxHeight()">
      <table class="data grid">
        <thead>
          <tr>
            <th class="rownum" scope="col"><span class="sr-only">Row</span>#</th>
            @for (column of columns(); track $index) {
              <th scope="col">
                {{ column.name }}
                @if (column.decltype) {
                  <span class="type">{{ column.decltype }}</span>
                }
              </th>
            }
          </tr>
        </thead>
        <tbody>
          @for (row of cells(); track $index; let i = $index) {
            <tr>
              <td class="rownum">{{ offset() + i + 1 }}</td>
              @for (value of row; track $index) {
                <td [class]="'v-' + value.kind" [attr.title]="value.title ?? null">
                  {{ value.text }}
                </td>
              }
            </tr>
          }
        </tbody>
      </table>
    </div>
  `,
})
export class Grid {
  readonly columns = input.required<Column[]>();
  readonly rows = input.required<Value[][]>();
  readonly offset = input(0);
  readonly maxHeight = input('none');
  readonly cells = computed<Cell[][]>(() => this.rows().map((row) => row.map(cell)));
}
