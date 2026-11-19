import { AfterViewInit, Directive, ElementRef, inject, output } from '@angular/core';

/**
 * A native modal dialog: opened when it renders, so `@if` controls it. The
 * browser traps focus and closes on Escape; `closed` reports both.
 */
@Directive({
  selector: 'dialog[appModal]',
  host: { class: 'modal', '(close)': 'closed.emit()', '(cancel)': 'closed.emit()' },
})
export class Modal implements AfterViewInit {
  private readonly element = inject<ElementRef<HTMLDialogElement>>(ElementRef);
  readonly closed = output<void>();

  ngAfterViewInit(): void {
    const dialog = this.element.nativeElement;
    if (!dialog.open) dialog.showModal?.();
  }
}
