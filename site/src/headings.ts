import type { Painter } from "./heat/field";
import type { Env, Scene } from "./scene";

interface Heading {
  el: HTMLElement;
  /** Remaining warmth, 1 when the heading has just come into view. */
  level: number;
  seen: boolean;
}

/**
 * Section headings arrive warm: when one scrolls into view it heats the
 * field under its lines for a moment, and the field does the cooling.
 */
export function createHeadings(root: HTMLElement): Scene {
  const headings: Heading[] = Array.from(root.querySelectorAll<HTMLElement>("h2")).map((el) => ({
    el,
    level: 0,
    seen: false,
  }));
  const range = document.createRange();
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const heading = headings.find((h) => h.el === entry.target);
        if (!heading) continue;
        if (entry.isIntersecting && !heading.seen) heading.level = 1;
        heading.seen = entry.isIntersecting;
      }
    },
    { rootMargin: "0px 0px -12% 0px", threshold: 0.6 },
  );
  for (const h of headings) observer.observe(h.el);

  return {
    el: root,
    get animating(): boolean {
      return headings.some((h) => h.level > 0.02);
    },

    paint(painter: Painter): void {
      for (const h of headings) {
        if (h.level <= 0.02) continue;
        range.selectNodeContents(h.el);
        // One row of warm spots per line of text.
        for (const line of Array.from(range.getClientRects())) {
          const radius = line.height * 0.75;
          const count = Math.max(1, Math.round(line.width / (radius * 0.8)));
          for (let i = 0; i < count; i++) {
            painter.spot(
              line.left + ((i + 0.5) / count) * line.width,
              line.top + line.height / 2,
              radius,
              h.level * 0.56,
            );
          }
        }
      }
    },

    update(env: Env): void {
      for (const h of headings) h.level = Math.max(0, h.level - env.dt / 0.7);
    },
  };
}
