import type { Painter } from "./heat/field";
import { approach, must, type Env, type Scene } from "./scene";

/**
 * The key card: a switch narrows a machine's key to read-only, and the
 * four permissions it loses go cold.
 */
export function createAccess(root: HTMLElement): Scene {
  const input = must<HTMLInputElement>(root, "[data-readonly]");
  const flag = must<HTMLElement>(root, "[data-flag]");
  const status = must<HTMLElement>(root, "[data-perm-status]");
  const perms = Array.from(root.querySelectorAll<HTMLElement>("[data-perm]")).map((el) => ({
    el,
    dot: must<HTMLElement>(el, ".perms__dot"),
    readOnly: el.dataset.perm === "read",
    heat: 1,
    x: 0,
    y: 0,
  }));
  let settling = 0;

  const apply = (): void => {
    const readOnly = input.checked;
    flag.classList.toggle("is-on", readOnly);
    for (const p of perms) p.el.classList.toggle("is-off", readOnly && !p.readOnly);
    status.textContent = readOnly
      ? "This key can read. It cannot write, create, delete or restore."
      : "This key holds all five permissions.";
    settling = 1.5;
  };
  input.addEventListener("change", apply);
  apply();

  return {
    el: root,
    get animating(): boolean {
      return settling > 0;
    },

    paint(painter: Painter): void {
      for (const p of perms) {
        const r = p.dot.getBoundingClientRect();
        p.x = r.left + r.width / 2;
        p.y = r.top + r.height / 2;
        painter.spot(p.x, p.y, 34, p.heat * 0.62);
      }
    },

    update(env: Env): void {
      settling = Math.max(0, settling - env.dt);
      for (const p of perms) {
        const on = !input.checked || p.readOnly;
        p.heat = approach(p.heat, on ? 1 : 0, 3, env.dt);
      }
    },
  };
}
