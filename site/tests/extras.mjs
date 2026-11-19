// Extra views: a touch trail, reduced motion, and the page without WebGL.
// Usage: node tests/extras.mjs [url] [outDir]
import { mkdir } from "node:fs/promises";
import { chromium } from "playwright";

const url = process.argv[2] ?? "http://127.0.0.1:4173/";
const outDir = process.argv[3] ?? "tests/out";
await mkdir(outDir, { recursive: true });
const browser = await chromium.launch({
  args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist", "--enable-gpu-rasterization"],
});
const phone = { viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true };
let failed = false;

async function open(name, query, options = {}) {
  const context = await browser.newContext({ ...phone, ...options, ignoreHTTPSErrors: true });
  const page = await context.newPage();
  page.on("console", (m) => {
    if (m.type() === "error") {
      failed = true;
      console.log(`${name}: ${m.text()}`);
    }
  });
  page.on("pageerror", (e) => {
    failed = true;
    console.log(`${name}: ${e.message}`);
  });
  await page.goto(`${url}?${query}`, { waitUntil: "networkidle" });
  return { context, page, shot: (suffix) => page.screenshot({ path: `${outDir}/${name}-${suffix}.png`, scale: "css" }) };
}

// A finger drawing an S across the hero.
{
  const { context, page, shot } = await open("touch", "scale=0.75");
  await page.waitForTimeout(3500);
  const client = await context.newCDPSession(page);
  const point = (k) => ({ x: 195 + 130 * Math.sin(k * Math.PI * 2), y: 120 + 520 * k });
  await client.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [point(0)] });
  for (let i = 1; i <= 40; i++) {
    await client.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [point(i / 40)] });
    await page.waitForTimeout(16);
  }
  await client.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await shot("trail");
  await page.waitForTimeout(1800);
  await shot("fading");
  await context.close();
}

for (const [name, query, options] of [
  ["reduced", "scale=0.75", { reducedMotion: "reduce" }],
  ["nogl", "nogl", {}],
]) {
  const { context, page, shot } = await open(name, query, options);
  await page.waitForTimeout(1200);
  await shot("hero");
  for (const id of ["life", "kinds", "access", "status"]) {
    await page.evaluate((target) => document.getElementById(target)?.scrollIntoView(), id);
    await page.waitForTimeout(900);
    await shot(id);
  }
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  console.log(`${name}: horizontal overflow ${overflow}px`);
  if (overflow > 0) failed = true;
  await context.close();
}

await browser.close();
process.exit(failed ? 1 : 0);
