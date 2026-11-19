import type { Painter } from "./heat/field";
import { approach, clamp, must, smooth, type Env, type Scene } from "./scene";

const SVG_NS = "http://www.w3.org/2000/svg";

interface Word {
  typed: HTMLElement;
  rest: HTMLElement;
  text: string;
  /** Index of the word's first character in the command. */
  start: number;
}

interface Typed {
  words: Word[];
  length: number;
  /** Share of the beat during which this command types, 0 to 1. */
  from: number;
  to: number;
  shown: number;
  live: boolean;
}

interface Beat {
  el: HTMLElement;
  commands: Typed[];
  out: HTMLElement | null;
}

interface Mark {
  el: HTMLElement;
  meta: HTMLElement | null;
  x: number;
  y: number;
  /** Eased 0 to 1 presence. */
  level: number;
}

const TYPE_FROM = 0.06;
const TYPE_TO = 0.5;
const OUT_AT = 0.56;

/**
 * The scroll story. Scroll position is the only clock: it types the
 * commands, switches the four beats and drives what the stage shows, so
 * scrolling back rewinds it.
 */
export function createLife(root: HTMLElement): Scene {
  document.documentElement.classList.add("js-life");
  const track = must<HTMLElement>(root, ".life__track");
  const view = must<HTMLElement>(root, ".life__view");
  const host = must<HTMLElement>(root, "[data-host-dot]");
  const rings = must<SVGSVGElement>(root, ".life__rings");
  const beats = Array.from(root.querySelectorAll<HTMLElement>(".beat")).map(prepareBeat);
  const marks = new Map<string, Mark>();
  for (const el of Array.from(root.querySelectorAll<HTMLElement>("[data-mark]"))) {
    marks.set(el.dataset.mark ?? "", { el, meta: el.querySelector("[data-meta]"), x: 0, y: 0, level: 0 });
  }
  const mark = (name: string): Mark => {
    const m = marks.get(name);
    if (!m) throw new Error(`missing mark: ${name}`);
    return m;
  };
  const scratch = mark("scratch");
  const notes = mark("notes");
  const agents = [mark("laptop"), mark("build-bot"), mark("reviewer")];
  const circles = Array.from({ length: 4 }, () => {
    const c = document.createElementNS(SVG_NS, "circle");
    c.setAttribute("opacity", "0");
    rings.append(c);
    return { el: c, level: 0 };
  });

  // Where the story stands, 0 to 4, and what the stage should show for it.
  let progress = 0;
  let size = 300;
  let viewLeft = 0;
  let viewTop = 0;
  let scratchHeat = 0;
  let notesHeat = 0;
  let queryGlow = 0;
  let activeBeat = -1;
  let expired = false;

  const firstCommand = beats[0]?.commands[0];
  const secondCommand = beats[0]?.commands[1];
  const scratchAt = firstCommand ? firstCommand.to : 0.3;
  const notesAt = secondCommand ? secondCommand.to : 0.5;

  const stage = must<HTMLElement>(root, ".life__stage");
  // Matches the media query in style.css that turns the sticky stage on.
  const staged = window.matchMedia("(min-height: 37.5rem), (min-width: 64rem)");
  let carried: number | undefined;
  let stageTop = 0;
  let paintedAt = -2;

  return {
    el: root,
    animating: true,
    get carried(): number | undefined {
      return carried;
    },

    paint(painter: Painter, env: Env): void {
      const tr = track.getBoundingClientRect();
      // The stage is sticky: follow where it really is, not the scroll position.
      const top = stage.getBoundingClientRect().top;
      carried = env.frame - paintedAt === 1 ? stageTop - top : undefined;
      stageTop = top;
      paintedAt = env.frame;
      const vr = view.getBoundingClientRect();
      // Too short for the stage: show the story at its busiest, fully typed.
      progress = staged.matches ? clamp(-tr.top / Math.max(1, tr.height - env.vh)) * beats.length : 2.6;
      size = Math.min(vr.width, vr.height * 1.25);
      viewLeft = vr.left;
      viewTop = vr.top;
      for (const m of marks.values()) {
        const r = m.el.getBoundingClientRect();
        m.x = r.left;
        m.y = r.top;
      }

      // The gateway never sleeps: its light is always on.
      const hr = host.getBoundingClientRect();
      painter.spot(hr.left + hr.width / 2, hr.top + hr.height / 2, 22, 0.62);

      const flicker = env.reduced ? 1 : 1 + 0.035 * Math.sin(env.time * 2.3);
      painter.spot(scratch.x, scratch.y, size * 0.21, scratchHeat * 0.66 * flicker);
      painter.spot(notes.x, notes.y, size * 0.26, notesHeat * (0.84 + queryGlow * 0.2));
      if (queryGlow > 0.02) painter.spot(notes.x, notes.y, size * 0.42, queryGlow * 0.42);

      agents.forEach((agent, i) => {
        if (agent.level < 0.02) return;
        painter.spot(agent.x, agent.y, 20, agent.level * 0.4);
        const targets = i === 0 && scratchHeat > 0.2 ? [notes, scratch] : [notes];
        for (const target of targets) {
          painter.line(agent.x, agent.y, target.x, target.y, 3, agent.level * 0.3);
          if (env.reduced) continue;
          // A request travelling to the database.
          const k = (env.time * 0.45 + i * 0.37 + (target === scratch ? 0.5 : 0)) % 1;
          painter.spot(
            agent.x + (target.x - agent.x) * k,
            agent.y + (target.y - agent.y) * k,
            Math.max(13, size * 0.04),
            agent.level * 0.6 * Math.sin(k * Math.PI),
          );
        }
      });
    },

    update(env: Env): void {
      const beat = Math.min(beats.length - 1, Math.floor(progress));
      const t = progress - beat;

      if (beat !== activeBeat) {
        activeBeat = beat;
        beats.forEach((b, i) => b.el.classList.toggle("is-on", i === beat));
      }
      beats.forEach((b, i) => {
        const local = !staged.matches || i < beat ? 1 : i > beat ? 0 : t;
        // The caret sits on the last command that has started typing.
        const live = staged.matches && i === beat ? b.commands.filter((c) => local >= c.from).pop() : undefined;
        for (const c of b.commands) type(c, local, c === live);
        b.out?.classList.toggle("is-on", local >= OUT_AT);
      });

      const cooling = beat === 3 ? smooth((t - 0.1) / 0.5) : 0;
      const nowExpired = beat === 3 && t > 0.62;
      const scratchOn = progress >= scratchAt;
      const notesOn = progress >= notesAt;
      scratch.level = scratchOn ? 1 : 0;
      notes.level = notesOn ? 1 : 0;
      scratchHeat = approach(scratchHeat, scratchOn ? 1 - cooling : 0, 5, env.dt);
      notesHeat = approach(notesHeat, notesOn ? 1 : 0, 5, env.dt);

      // The query lands: the database it touches warms up.
      const querying = beat === 1 && t >= TYPE_TO && t < 0.98;
      const wave = env.reduced ? 0.6 : 0.5 + 0.5 * Math.sin(env.time * 3.1);
      queryGlow = approach(queryGlow, querying ? wave : 0, 6, env.dt);

      agents.forEach((agent, i) => {
        const on = progress >= 2.04 + i * 0.1 && progress < 3.9;
        agent.level = approach(agent.level, on ? 1 : 0, 6, env.dt);
        agent.el.classList.toggle("is-on", on);
      });
      scratch.el.classList.toggle("is-on", scratchOn);
      notes.el.classList.toggle("is-on", notesOn);
      if (nowExpired !== expired) {
        expired = nowExpired;
        scratch.el.classList.toggle("is-gone", expired);
        if (scratch.meta) scratch.meta.textContent = expired ? "expired" : "burstable";
      }

      // One ring per snapshot, drawn around the persistent database.
      const taken = beat === 3 ? clamp((t - 0.12) / 0.7) * circles.length : 0;
      circles.forEach((c, i) => {
        c.level = approach(c.level, taken > i ? 1 : 0, 7, env.dt);
        const radius = size * (0.13 + i * 0.045) * (0.86 + 0.14 * c.level);
        c.el.setAttribute("cx", (notes.x - viewLeft).toFixed(1));
        c.el.setAttribute("cy", (notes.y - viewTop).toFixed(1));
        c.el.setAttribute("r", radius.toFixed(1));
        c.el.setAttribute("opacity", (c.level * (0.85 - i * 0.14)).toFixed(2));
      });
    },
  };
}

function prepareBeat(el: HTMLElement): Beat {
  const codes = Array.from(el.querySelectorAll<HTMLElement>("[data-type]"));
  const total = codes.reduce((n, c) => n + (c.textContent ?? "").length, 0) || 1;
  let cursor = TYPE_FROM;
  const commands = codes.map((code) => {
    const text = code.textContent ?? "";
    // One unbreakable box per word, so a flag such as --read-only never splits.
    const words: Word[] = [];
    const nodes: Node[] = [];
    let start = 0;
    for (const part of text.split(" ")) {
      const box = document.createElement("span");
      const typed = document.createElement("span");
      const rest = document.createElement("span");
      box.className = "w";
      typed.className = "t";
      rest.className = "u";
      rest.textContent = part;
      box.append(typed, rest);
      if (nodes.length > 0) nodes.push(document.createTextNode(" "));
      nodes.push(box);
      words.push({ typed, rest, text: part, start });
      start += part.length + 1;
    }
    code.replaceChildren(...nodes);
    const span = ((TYPE_TO - TYPE_FROM) * text.length) / total;
    const command: Typed = { words, length: text.length, from: cursor, to: cursor + span, shown: 0, live: false };
    cursor += span;
    return command;
  });
  return { el, commands, out: el.querySelector<HTMLElement>("[data-out]") };
}

/** Shows as much of a command as the scroll has typed; the rest keeps its space. */
function type(c: Typed, local: number, live: boolean): void {
  const count = Math.round(clamp((local - c.from) / (c.to - c.from)) * c.length);
  if (count === c.shown && live === c.live) return;
  c.shown = count;
  c.live = live;
  let caret: Word | undefined;
  for (const w of c.words) {
    const n = clamp(count - w.start, 0, w.text.length);
    if (count >= w.start) caret = w;
    w.typed.textContent = w.text.slice(0, n);
    w.rest.textContent = w.text.slice(n);
    w.typed.classList.remove("is-live");
  }
  if (live) caret?.typed.classList.add("is-live");
}

/**
 * The story without motion: the stage is one still picture of a team at
 * work, and the four beats read as a plain list below it.
 */
export function createStillLife(root: HTMLElement): Scene {
  const host = must<HTMLElement>(root, "[data-host-dot]");
  const view = must<HTMLElement>(root, ".life__view");
  const at = (name: string): HTMLElement => must<HTMLElement>(root, `[data-mark="${name}"]`);
  const scratch = at("scratch");
  const notes = at("notes");
  const agents = [at("laptop"), at("build-bot"), at("reviewer")];

  return {
    el: root,
    animating: false,
    paint(painter: Painter): void {
      const vr = view.getBoundingClientRect();
      const size = Math.min(vr.width, vr.height * 1.25);
      const hr = host.getBoundingClientRect();
      const s = scratch.getBoundingClientRect();
      const n = notes.getBoundingClientRect();
      painter.spot(hr.left + hr.width / 2, hr.top + hr.height / 2, 22, 0.62);
      painter.spot(s.left, s.top, size * 0.21, 0.6);
      painter.spot(n.left, n.top, size * 0.26, 0.86);
      for (const agent of agents) {
        const a = agent.getBoundingClientRect();
        painter.spot(a.left, a.top, 20, 0.4);
        painter.line(a.left, a.top, n.left, n.top, 3, 0.3);
      }
    },
    update(): void {},
  };
}
