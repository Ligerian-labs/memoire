import type { Painter } from "./heat/field";

interface Segment {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  touch: boolean;
}

/**
 * Collects mouse and touch movement between frames. Touch listeners are
 * passive, so the trail keeps recording while the page scrolls natively.
 */
export class Pointer {
  /** Last known position in viewport pixels; far away until first input. */
  x = -1e5;
  y = -1e5;
  /** `performance.now()` of the last movement. */
  lastMove = -1e9;

  private segments: Segment[] = [];
  private readonly last = new Map<number, { x: number; y: number }>();

  constructor() {
    const opts: AddEventListenerOptions = { passive: true };
    window.addEventListener(
      "pointermove",
      (e) => {
        if (e.pointerType === "mouse") this.move(-1, e.clientX, e.clientY, false);
      },
      opts,
    );
    document.addEventListener("pointerleave", () => this.last.delete(-1), opts);
    const onTouch = (e: TouchEvent): void => {
      for (const t of Array.from(e.changedTouches)) this.move(t.identifier, t.clientX, t.clientY, true);
    };
    const onEnd = (e: TouchEvent): void => {
      for (const t of Array.from(e.changedTouches)) this.last.delete(t.identifier);
    };
    window.addEventListener("touchstart", onTouch, opts);
    window.addEventListener("touchmove", onTouch, opts);
    window.addEventListener("touchend", onEnd, opts);
    window.addEventListener("touchcancel", onEnd, opts);
  }

  /** True when there is movement the field has not absorbed yet. */
  get pending(): boolean {
    return this.segments.length > 0;
  }

  paint(painter: Painter): void {
    for (const s of this.segments) {
      if (s.touch) painter.streak(s.x1, s.y1, s.x2, s.y2, 58, 0.34);
      else painter.streak(s.x1, s.y1, s.x2, s.y2, 44, 0.15);
    }
    this.segments.length = 0;
  }

  private move(id: number, x: number, y: number, touch: boolean): void {
    const prev = this.last.get(id);
    this.segments.push({ x1: prev?.x ?? x, y1: prev?.y ?? y, x2: x, y2: y, touch });
    if (this.segments.length > 64) this.segments.shift();
    this.last.set(id, { x, y });
    this.x = x;
    this.y = y;
    this.lastMove = performance.now();
  }
}
