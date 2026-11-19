import { AfterViewInit, Directive, ElementRef, inject, input, output } from '@angular/core';

/**
 * A native modal dialog: opened when it renders, so `@if` controls it. The
 * browser traps focus and closes on Escape; `closed` reports both.
 */
@Directive({
  selector: 'dialog[appModal]',
  host: { class: 'modal', '(close)': 'closed.emit()', '(cancel)': 'cancel($event)' },
})
export class Modal implements AfterViewInit {
  private readonly element = inject<ElementRef<HTMLDialogElement>>(ElementRef);
  readonly closed = output<void>();
  /** While true, Escape does not close the dialog (a request is in flight). */
  readonly locked = input(false);

  cancel(event: Event): void {
    event.preventDefault();
    if (!this.locked()) this.closed.emit();
  }

  ngAfterViewInit(): void {
    const dialog = this.element.nativeElement;
    if (!dialog.open) dialog.showModal?.();
  }
}
