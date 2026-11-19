// Takes screenshots through the whole scroll, at a phone and a desktop viewport.
// Usage: node tests/shots.mjs [url] [outDir] [--reduced] [--full] [--phone|--desktop]
import { mkdir } from "node:fs/promises";
import { chromium } from "playwright";

const args = process.argv.slice(2).filter((a) => !a.startsWith("--"));
const flags = new Set(process.argv.slice(2).filter((a) => a.startsWith("--")));
const url = args[0] ?? "http://127.0.0.1:4173/";
const outDir = args[1] ?? "tests/out";
const reduced = flags.has("--reduced");
const full = flags.has("--full");
// Software WebGL is slow: a small drawing buffer keeps the field near real time.
const scale = full ? 2 : 0.75;

const PROFILES = [
  {
    name: "phone",
    context: {
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 3,
      isMobile: true,
      hasTouch: true,
      userAgent:
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
    },
  },
  { name: "desktop", context: { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 } },
];

await mkdir(outDir, { recursive: true });
const browser = await chromium.launch({
  args: [
    "--use-gl=angle",
    "--use-angle=swiftshader",
    "--enable-unsafe-swiftshader",
    "--ignore-gpu-blocklist",
    "--enable-gpu-rasterization",
  ],
});
let failed = false;

const only = flags.has("--phone") ? "phone" : flags.has("--desktop") ? "desktop" : null;

for (const profile of PROFILES.filter((p) => !only || p.name === only)) {
  const context = await browser.newContext({
    ...profile.context,
    reducedMotion: reduced ? "reduce" : "no-preference",
    ignoreHTTPSErrors: true,
  });
  const page = await context.newPage();
  const problems = [];
  page.on("console", (m) => {
    if (m.type() === "error" || m.type() === "warning") problems.push(`${m.type()}: ${m.text()}`);
  });
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  page.on("requestfailed", (r) => problems.push(`requestfailed: ${r.url()}`));

  await page.goto(`${url}${url.includes("?") ? "&" : "?"}scale=${scale}`, { waitUntil: "networkidle" });
  const tag = `${profile.name}${reduced ? "-reduced" : ""}`;
  const shot = (name) =>
    page.screenshot({ path: `${outDir}/${tag}-${name}.png`, scale: full ? "device" : "css" });

  await page.waitForTimeout(350);
  await shot("00-load");
  await page.waitForTimeout(3200);
  await shot("01-hero");

  const { height, vh } = await page.evaluate(() => ({
    height: document.documentElement.scrollHeight,
    vh: window.innerHeight,
  }));
  const steps = Math.ceil((height - vh) / (vh * 0.8));
  for (let i = 1; i <= steps; i++) {
    const y = Math.min(height - vh, Math.round(i * vh * 0.8));
    // Scroll in small moves, like a thumb, so the field has frames to follow.
    await page.evaluate(async (target) => {
      const start = window.scrollY;
      for (let k = 1; k <= 12; k++) {
        window.scrollTo(0, start + ((target - start) * k) / 12);
        await new Promise((r) => requestAnimationFrame(r));
      }
    }, y);
    await page.waitForTimeout(900);
    await shot(String(i + 1).padStart(2, "0"));
  }

  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  console.log(`${tag}: ${steps + 2} screenshots, page ${height}px, horizontal overflow ${overflow}px`);
  if (overflow > 0) failed = true;
  for (const p of problems) console.log(`  ${p}`);
  if (problems.some((p) => !p.startsWith("warning"))) failed = true;
  await context.close();
}

await browser.close();
process.exit(failed ? 1 : 0);
