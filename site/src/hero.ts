import type { Painter } from "./heat/field";
import { approach, clamp, must, type Env, type Scene } from "./scene";

interface Letter {
  el: HTMLElement;
  /** Temperature the letter settles at when left alone. */
  rest: number;
  temp: number;
  /** Seconds before the opening heat starts to leave this letter. */
  hold: number;
  x: number;
  y: number;
  shown: number;
}

interface Spark {
  /** Position inside the free band of the hero, 0 to 1. */
  u: number;
  v: number;
  /** Radius as a share of the hero's shorter side. */
  radius: number;
  /** Seconds one bloom-and-fade lasts. */
  life: number;
  age: number;
  round: number;
}

/** Deterministic scatter, so the hero looks the same on every load. */
function scatter(n: number): number {
  const x = Math.sin(n * 127.1 + 311.7) * 43758.5453;
  return x - Math.floor(x);
}

/**
 * The hero: one database kept warm, scratch databases that bloom and fade
 * around it, and a headline that holds heat. "Agents" stays warm; "forget."
 * cools down.
 */
export function createHero(root: HTMLElement, reduced: boolean): Scene {
  const title = must<HTMLElement>(root, "[data-kinetic]");
  const copy = must<HTMLElement>(root, ".hero__copy");
  const core = must<HTMLElement>(root, "[data-core]");
  const letters = splitLetters(title, reduced);
  const sparks: Spark[] = Array.from({ length: 5 }, (_, i) => ({
    u: scatter(i),
    v: scatter(i + 40),
    radius: 0.07 + scatter(i + 80) * 0.07,
    life: 5 + scatter(i + 120) * 4.5,
    age: -i * 1.1,
    round: 0,
  }));
  let fontSize = 64;

  // Size the headline so its hottest, widest state still fits the column.
  const fit = (): void => {
    title.style.fontSize = "";
    for (const l of letters) styleLetter(l.el, 1);
    const base = Number.parseFloat(getComputedStyle(title).fontSize);
    const room = copy.clientWidth;
    const widest = title.scrollWidth;
    if (widest > room) title.style.fontSize = `${((base * room) / widest) * 0.985}px`;
    fontSize = Number.parseFloat(getComputedStyle(title).fontSize) || fontSize;
    for (const l of letters) styleLetter(l.el, l.temp);
  };
  fit();
  void document.fonts.ready.then(fit);
  window.addEventListener("resize", fit);

  return {
    el: root,
    animating: !reduced,

    paint(painter: Painter, env: Env): void {
      const hero = root.getBoundingClientRect();
      const text = copy.getBoundingClientRect();
      const anchor = core.getBoundingClientRect();
      for (const l of letters) {
        const r = l.el.getBoundingClientRect();
        l.x = r.left + r.width / 2;
        l.y = r.top + r.height / 2;
      }
      const unit = Math.min(hero.width, hero.height);

      // The database that stays: steady, with a slow breath.
      const breathe = reduced ? 1 : 1 + 0.03 * Math.sin(env.time * 1.3);
      const coreRadius = Math.min(unit * 0.31, 240);
      painter.spot(anchor.left, anchor.top, coreRadius * 2.1, 0.22);
      painter.spot(anchor.left, anchor.top, coreRadius * 1.25 * breathe, 0.8);

      // Scratch databases: a quick bloom, then a long fade to nothing.
      const top = hero.top + Math.min(88, hero.height * 0.12);
      const bandHeight = Math.max(40, text.top - top - unit * 0.06);
      for (const s of sparks) {
        if (s.age < 0) continue;
        const k = s.age / s.life;
        const level = reduced ? 0.6 : Math.min(1, s.age / 0.45) * Math.exp(-k * 2.6) * (1 - k);
        painter.spot(
          hero.left + (0.06 + s.u * 0.8) * hero.width,
          top + s.v * bandHeight,
          s.radius * unit + 14,
          level,
        );
      }

      for (const l of letters) painter.spot(l.x, l.y, fontSize * 0.66, l.temp * 0.7);
    },

    update(env: Env): void {
      const touching = performance.now() - env.pointer.lastMove < 900;
      for (const l of letters) {
        l.hold -= env.dt;
        let target = l.hold > 0 ? 1 : l.rest;
        if (touching) {
          const d = Math.hypot(env.pointer.x - l.x, env.pointer.y - l.y);
          const near = Math.exp(-(d * d) / (2 * (fontSize * 0.85) ** 2));
          target = Math.max(target, near);
        }
        l.temp = approach(l.temp, target, target > l.temp ? 9 : 0.85, env.dt);
        if (Math.abs(l.temp - l.shown) > 0.004) {
          l.shown = l.temp;
          styleLetter(l.el, l.temp);
        }
      }
      if (reduced) return;
      for (const s of sparks) {
        s.age += env.dt;
        if (s.age > s.life) {
          s.round += 1;
          s.age = -0.3 - scatter(s.round * 7 + s.radius) * 1.8;
          s.u = scatter(s.round * 13.7 + s.radius);
          s.v = scatter(s.round * 29.3 + s.life);
        }
      }
    },
  };
}

function splitLetters(title: HTMLElement, reduced: boolean): Letter[] {
  const lines = Array.from(title.querySelectorAll<HTMLElement>(".line"));
  title.setAttribute("aria-label", lines.map((l) => l.textContent ?? "").join(" "));
  const letters: Letter[] = [];
  lines.forEach((line, lineIndex) => {
    const text = line.textContent ?? "";
    line.setAttribute("aria-hidden", "true");
    line.textContent = "";
    for (const char of text) {
      const el = document.createElement("span");
      el.className = "ch";
      el.textContent = char;
      line.append(el);
      const index = letters.length;
      const rest = lineIndex === 0 ? 0.66 : 0.1;
      const letter: Letter = {
        el,
        rest,
        temp: reduced ? rest : 1,
        hold: reduced ? 0 : 0.7 + index * 0.09,
        x: 0,
        y: 0,
        shown: -1,
      };
      styleLetter(el, letter.temp);
      letters.push(letter);
    }
  });
  return letters;
}

/** Heat widens and thickens a letter, and moves it from cold lilac to white-hot. */
function styleLetter(el: HTMLElement, temp: number): void {
  const t = clamp(temp);
  const width = 70 + 58 * t ** 0.9;
  const weight = 250 + 610 * t ** 0.8;
  el.style.fontVariationSettings = `"wght" ${weight.toFixed(0)}, "wdth" ${width.toFixed(1)}`;
  el.style.color = `color-mix(in oklab, #a79cec, #fffaf0 ${(clamp(t * 1.6) * 100).toFixed(0)}%)`;
}
