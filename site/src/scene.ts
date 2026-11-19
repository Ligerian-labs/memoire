import type { Painter } from "./heat/field";
import type { Pointer } from "./pointer";

/** What every scene is told about the current frame. */
export interface Env {
  /** Seconds since the previous frame, capped. */
  dt: number;
  /** Seconds since the page started. */
  time: number;
  /** Counts frames, so a scene can tell whether it was painted on the previous one. */
  frame: number;
  /** Viewport size in CSS pixels. */
  vw: number;
  vh: number;
  pointer: Pointer;
  /** The visitor asked for reduced motion. */
  reduced: boolean;
}

/**
 * A part of the page that feeds the heat field. Each frame the loop calls
 * `paint` on every visible scene first (layout reads only), then `update`
 * (DOM writes), so a frame never forces layout twice.
 */
export interface Scene {
  readonly el: Element;
  /** True while the scene needs frames even when nobody scrolls or touches. */
  readonly animating: boolean;
  /**
   * Set by a scene that pins part of the page (a sticky stage): how far its
   * content really moved up on screen since the last frame. The field then
   * follows that instead of the scroll position. Undefined otherwise.
   */
  readonly carried?: number | undefined;
  paint(painter: Painter, env: Env): void;
  update(env: Env): void;
}

export function clamp(v: number, lo = 0, hi = 1): number {
  return v < lo ? lo : v > hi ? hi : v;
}

export function smooth(v: number): number {
  const t = clamp(v);
  return t * t * (3 - 2 * t);
}

/** Moves `current` toward `target` at `rate` per second, frame-rate independent. */
export function approach(current: number, target: number, rate: number, dt: number): number {
  return target + (current - target) * Math.exp(-rate * dt);
}

export function must<T extends Element>(root: ParentNode, selector: string): T {
  const el = root.querySelector<T>(selector);
  if (!el) throw new Error(`missing element: ${selector}`);
  return el;
}
