import { createAccess } from "./access";
import { createEmber } from "./ember";
import { createHeadings } from "./headings";
import { HeatField, type Painter } from "./heat/field";
import { createHero } from "./hero";
import { createKinds } from "./kinds";
import { createLife, createStillLife } from "./life";
import { Pointer } from "./pointer";
import { must, type Env, type Scene } from "./scene";

const root = document.documentElement;
const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const params = new URLSearchParams(location.search);

const field = params.has("nogl") ? null : HeatField.create(must<HTMLCanvasElement>(document, "#field"));
root.classList.add(field ? "gl" : "no-gl");

const pointer = new Pointer();
const scenes: Scene[] = [
  createHero(must<HTMLElement>(document, "#hero"), reduced),
  // With reduced motion the story stays a plain list: no sticky stage, no typing.
  (reduced ? createStillLife : createLife)(must<HTMLElement>(document, "#life")),
  createKinds(must<HTMLElement>(document, "#kinds"), reduced, field !== null),
  createAccess(must<HTMLElement>(document, "#access")),
  createEmber(must<HTMLElement>(document, "#status")),
  ...(reduced ? [] : [createHeadings(must<HTMLElement>(document, "#main"))]),
];

const visible = new Set<Scene>();
const observer = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      const scene = scenes.find((s) => s.el === entry.target);
      if (!scene) continue;
      if (entry.isIntersecting) visible.add(scene);
      else visible.delete(scene);
    }
    wake();
  },
  { rootMargin: "15% 0px" },
);
for (const scene of scenes) observer.observe(scene.el);

// Heat rises against gravity: tilting the phone leans the plumes.
let up: [number, number] = [0, 1];
if (!reduced && typeof DeviceOrientationEvent !== "undefined" && !("requestPermission" in DeviceOrientationEvent)) {
  window.addEventListener(
    "deviceorientation",
    (e) => {
      if (e.gamma === null) return;
      const lean = (Math.max(-40, Math.min(40, e.gamma)) * Math.PI) / 180;
      up = [-Math.sin(lean), Math.cos(lean)];
    },
    { passive: true },
  );
}

// Drawing buffer scale: start near the device's, step down if frames run long.
const SCALES = [2, 1.5, 1.25, 1, 0.75].filter((s) => s <= Math.min(2, window.devicePixelRatio || 1) + 0.01);
if (SCALES.length === 0) SCALES.push(1);
// ?scale=1 pins the buffer scale: screenshots and slow test machines use it.
const pinned = Number(params.get("scale")) || 0;
if (pinned > 0) SCALES.splice(0, SCALES.length, Math.min(3, pinned));
const locked = pinned > 0;
let scaleIndex = 0;
let slowFrames = 0;
let fastFrames = 0;
field?.setPixelScale(SCALES[0] ?? 1);
field?.resize();

let running = false;
let last = 0;
let time = 0;
let frameCount = 0;
let lastScroll = window.scrollY;
/** How far the field has travelled with the page: scroll minus pinned stretches. */
let carried = 0;
/** Frames keep coming until this time, so the field can settle after input. */
let awakeUntil = 0;

function wake(): void {
  awakeUntil = performance.now() + 4500;
  if (!running && !document.hidden) {
    running = true;
    last = 0;
    requestAnimationFrame(frame);
  }
}

function frame(now: number): void {
  const elapsed = last === 0 ? 16.7 : now - last;
  last = now;
  const dt = Math.min(elapsed, 50) / 1000;
  time += dt;

  if (!locked && field) adaptQuality(elapsed);

  const scrollY = window.scrollY;
  frameCount += 1;
  const env: Env = { dt, time, frame: frameCount, vw: window.innerWidth, vh: window.innerHeight, pointer, reduced };
  let animating = pointer.pending || scrollY !== lastScroll;
  for (const scene of visible) animating ||= scene.animating;
  if (animating) awakeUntil = now + 4500;

  const draw = (painter: Painter): void => {
    for (const scene of visible) scene.paint(painter, env);
    pointer.paint(painter);
  };
  if (field) {
    field.paint(draw);
    // Content normally moves with the scroll; a pinned stage says otherwise.
    let scrollDelta = scrollY - lastScroll;
    for (const scene of visible) scrollDelta = scene.carried ?? scrollDelta;
    carried += scrollDelta;
    field.render({ dt, time: reduced ? 0 : time, scrollDelta, scrollY: carried, up, motion: reduced ? 0 : 1 });
  } else {
    draw(NO_PAINT);
  }
  lastScroll = scrollY;
  for (const scene of visible) scene.update(env);

  if (now < awakeUntil && !document.hidden) requestAnimationFrame(frame);
  else running = false;
}

/** A painter that draws nothing, for pages without WebGL: scenes still measure. */
const NO_PAINT = new Proxy({}, { get: () => () => undefined }) as Painter;

function adaptQuality(elapsed: number): void {
  if (elapsed > 26) {
    slowFrames += 1;
    fastFrames = 0;
  } else {
    fastFrames += 1;
    if (fastFrames > 90) slowFrames = 0;
  }
  if (slowFrames > 24 && scaleIndex < SCALES.length - 1) {
    scaleIndex += 1;
    slowFrames = 0;
    field?.setPixelScale(SCALES[scaleIndex] ?? 1);
  }
}

for (const type of ["scroll", "pointermove", "touchstart", "touchmove", "pointerdown", "keydown"] as const) {
  window.addEventListener(type, wake, { passive: true });
}
window.addEventListener("resize", () => {
  field?.resize();
  wake();
});
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) wake();
});
wake();
