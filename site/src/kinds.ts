import type { Painter } from "./heat/field";
import { approach, clamp, must, type Env, type Scene } from "./scene";

const SVG_NS = "http://www.w3.org/2000/svg";

// The product's own numbers, in hours.
const IDLE_TTL = 24;
const LIFETIME = 7 * 24;
const SNAPSHOT_EVERY = 6;
const SNAPSHOT_KEPT = 7 * 24;

/** Simulated hours per real second: a day in about five seconds. */
const HOURS_PER_SECOND = 4.6;
/** Hours after creation at which the simulated agent queries scratch. */
const SCRIPTED_QUERIES = [7, 16, 27];
/** How long an expired database stays on screen before a new one is created. */
const EXPIRED_FOR = 13;

interface Scratch {
  created: number;
  lastUsed: number;
  /** Hour it expired, or null while active. */
  expiredAt: number | null;
  scripted: number;
}

function setText(el: Element, text: string): void {
  if (el.textContent !== text) el.textContent = text;
}

function hours(h: number): string {
  if (h >= 48) return `${Math.floor(h / 24)}d ${Math.floor(h % 24)}h`;
  if (h >= 1) return `${Math.floor(h)}h`;
  return `${Math.max(1, Math.round(h * 60))}m`;
}

/**
 * A week in fast forward. The burstable database cools toward its idle TTL
 * and expires; the persistent one gains a ring per snapshot and drops rings
 * older than seven days.
 */
export function createKinds(root: HTMLElement, reduced: boolean, gl: boolean): Scene {
  const burstable = must<HTMLElement>(root, '[data-kind="burstable"]');
  const persistent = must<HTMLElement>(root, '[data-kind="persistent"]');
  const scratchCore = must<HTMLElement>(burstable, "[data-core]");
  const notesCore = must<HTMLElement>(persistent, "[data-core]");
  const scratchView = must<HTMLElement>(burstable, ".specimen__view");
  const state = must<HTMLElement>(burstable, "[data-state]");
  const scratchReadout = must<HTMLElement>(burstable, "[data-readout]");
  const notesReadout = must<HTMLElement>(persistent, "[data-readout]");
  const idleMeter = must<HTMLElement>(burstable, "[data-meter-idle]");
  const lifeMeter = must<HTMLElement>(burstable, "[data-meter-life]");
  const nextMeter = must<HTMLElement>(persistent, "[data-meter-next]");
  const clockDay = must<HTMLElement>(root, "[data-clock-day]");
  const clockTime = must<HTMLElement>(root, "[data-clock-time]");
  const queryButton = must<HTMLButtonElement>(root, '[data-action="query"]');
  const toggleButton = must<HTMLButtonElement>(root, '[data-action="toggle"]');
  const svg = must<SVGSVGElement>(root, ".specimen__rings");
  const rings = Array.from({ length: SNAPSHOT_KEPT / SNAPSHOT_EVERY + 1 }, () => {
    const c = document.createElementNS(SVG_NS, "circle");
    c.setAttribute("opacity", "0");
    svg.append(c);
    return c;
  });

  let now = 54;
  let playing = !reduced;
  let scratch: Scratch = { created: 38, lastUsed: 46, expiredAt: null, scripted: 0 };
  const snapshots: number[] = [];
  for (let h = SNAPSHOT_EVERY; h <= now; h += SNAPSHOT_EVERY) snapshots.push(h);
  let refused = 0;
  let queryFlash = 0;
  let snapshotFlash = 0;
  let scratchHeat = 0;
  let cores = { sx: 0, sy: 0, nx: 0, ny: 0, size: 160 };

  const expiresAt = (): number => Math.min(scratch.lastUsed + IDLE_TTL, scratch.created + LIFETIME);

  const query = (): void => {
    if (scratch.expiredAt !== null) {
      refused = 1.6;
      return;
    }
    scratch.lastUsed = now;
    queryFlash = 1;
  };

  const advance = (dh: number): void => {
    const target = now + dh;
    while (now < target) {
      now = Math.min(target, now + 0.25);
      if (scratch.expiredAt === null) {
        const next = SCRIPTED_QUERIES[scratch.scripted];
        if (next !== undefined && now - scratch.created >= next) {
          scratch.scripted += 1;
          scratch.lastUsed = now;
          queryFlash = 1;
        }
        if (now >= expiresAt()) scratch.expiredAt = now;
      } else if (now - scratch.expiredAt >= EXPIRED_FOR) {
        // The name is free again: a new database, with a new id.
        scratch = { created: now, lastUsed: now, expiredAt: null, scripted: 0 };
        queryFlash = 1;
      }
      const due = (snapshots[snapshots.length - 1] ?? 0) + SNAPSHOT_EVERY;
      if (now >= due) {
        snapshots.push(due);
        snapshotFlash = 1;
      }
      while (snapshots.length > 0 && now - (snapshots[0] ?? now) > SNAPSHOT_KEPT) snapshots.shift();
    }
  };

  queryButton.addEventListener("click", query);
  if (reduced) toggleButton.textContent = "Advance 6 hours";
  toggleButton.addEventListener("click", () => {
    if (reduced) {
      advance(SNAPSHOT_EVERY);
      return;
    }
    playing = !playing;
    toggleButton.textContent = playing ? "Pause" : "Play";
    toggleButton.setAttribute("aria-pressed", String(!playing));
  });

  const render = (): void => {
    const active = scratch.expiredAt === null;
    const left = Math.max(0, expiresAt() - now);
    setText(clockDay, `day ${Math.floor(now / 24) + 1}`);
    setText(clockTime, `${String(Math.floor(now % 24)).padStart(2, "0")}:00`);
    setText(state, refused > 0 ? "410 database_expired" : active ? "active" : "expired");
    setText(scratchReadout, active ? `expires in ${hours(left)}` : "expired, file removed");
    idleMeter.style.transform = `scaleX(${active ? clamp(left / IDLE_TTL).toFixed(3) : 0})`;
    lifeMeter.style.transform = `scaleX(${
      active ? clamp((scratch.created + LIFETIME - now) / LIFETIME).toFixed(3) : 0
    })`;
    burstable.classList.toggle("is-expired", !active);

    const last = snapshots[snapshots.length - 1] ?? 0;
    setText(notesReadout, `${snapshots.length} snapshots, last ${hours(Math.max(0.02, now - last))} ago`);
    nextMeter.style.transform = `scaleX(${clamp((now - last) / SNAPSHOT_EVERY).toFixed(3)})`;

    rings.forEach((ring, i) => {
      const taken = snapshots[snapshots.length - 1 - i];
      if (taken === undefined) {
        ring.setAttribute("opacity", "0");
        return;
      }
      const age = (now - taken) / SNAPSHOT_KEPT;
      const fresh = clamp((now - taken) / 2.5);
      ring.setAttribute("r", (17 + 79 * age).toFixed(2));
      ring.setAttribute("stroke-width", (1.9 - 1.1 * clamp(age * 4)).toFixed(2));
      ring.setAttribute("opacity", (fresh * (0.95 - 0.6 * age) * clamp((1 - age) * 12)).toFixed(2));
    });
  };
  render();

  return {
    el: root,
    get animating(): boolean {
      return playing || queryFlash > 0.02 || refused > 0;
    },

    paint(painter: Painter, env: Env): void {
      const s = scratchCore.getBoundingClientRect();
      const n = notesCore.getBoundingClientRect();
      cores = { sx: s.left, sy: s.top, nx: n.left, ny: n.top, size: scratchView.getBoundingClientRect().width };
      const breathe = env.reduced ? 1 : 1 + 0.03 * Math.sin(env.time * 1.7);
      painter.spot(cores.sx, cores.sy, cores.size * 0.4, scratchHeat * breathe);
      if (queryFlash > 0.02) painter.spot(cores.sx, cores.sy, cores.size * 0.52, queryFlash * 0.5);
      painter.spot(cores.nx, cores.ny, cores.size * 0.3, 0.9 * breathe);
      if (snapshotFlash > 0.02) painter.spot(cores.nx, cores.ny, cores.size * 0.5, snapshotFlash * 0.4);
    },

    update(env: Env): void {
      if (playing) advance(env.dt * HOURS_PER_SECOND);
      const left = scratch.expiredAt === null ? clamp((expiresAt() - now) / IDLE_TTL) : 0;
      const target = scratch.expiredAt === null ? 0.16 + 0.74 * left ** 0.8 : 0;
      scratchHeat = approach(scratchHeat, target, 4, env.dt);
      queryFlash = approach(queryFlash, 0, 3.5, env.dt);
      snapshotFlash = approach(snapshotFlash, 0, 3, env.dt);
      refused = Math.max(0, refused - env.dt);
      // Without WebGL a CSS glow stands in for the field; it follows the same heat.
      if (!gl) scratchCore.style.setProperty("--t", scratchHeat.toFixed(2));
      render();
    },
  };
}
