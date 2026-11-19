import type { Painter } from "./heat/field";
import { must, type Env, type Scene } from "./scene";

/** The closing image: one warm body left on the page, above the footer. */
export function createEmber(root: HTMLElement): Scene {
  const anchor = must<HTMLElement>(root, "[data-ember]");
  return {
    el: root,
    animating: false,
    paint(painter: Painter, env: Env): void {
      const r = anchor.getBoundingClientRect();
      const radius = Math.min(env.vw * 0.36, 210);
      const breathe = env.reduced ? 1 : 1 + 0.03 * Math.sin(env.time * 1.3);
      painter.spot(r.left, r.top, radius * 2, 0.22);
      painter.spot(r.left, r.top, radius * 1.2 * breathe, 0.8);
    },
    update(): void {},
  };
}
