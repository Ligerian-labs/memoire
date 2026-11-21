// Smoke test of the built site: serves dist/ with serve.mjs and drives it in
// Chromium at a phone viewport and wider. Run `pnpm build` first.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const freePort = () =>
  new Promise((resolve, reject) => {
    const probe = createServer();
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });

const port = await freePort();
const origin = `http://127.0.0.1:${port}`;
const server = spawn(process.execPath, [fileURLToPath(new URL("../serve.mjs", import.meta.url))], {
  env: { ...process.env, PORT: String(port), HOST: "127.0.0.1" },
  stdio: ["ignore", "pipe", "inherit"],
});
await new Promise((resolve, reject) => {
  server.stdout.once("data", resolve);
  server.once("exit", (code) => reject(new Error(`server exited with ${code}`)));
});

const browser = await chromium.launch({
  args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"],
});
const phone = { viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true };
const results = [];

async function check(name, options, query, body) {
  const context = await browser.newContext(options);
  const page = await context.newPage();
  const problems = [];
  page.on("console", (m) => m.type() === "error" && problems.push(m.text()));
  page.on("pageerror", (e) => problems.push(e.message));
  page.on("requestfailed", (r) => problems.push(`request failed: ${r.url()}`));
  try {
    await page.goto(`${origin}/${query}`, { waitUntil: "networkidle" });
    await body(page);
    assert.deepEqual(problems, [], "console errors");
    results.push(`ok    ${name}`);
  } catch (error) {
    results.push(`FAIL  ${name}: ${error.message}`);
    process.exitCode = 1;
  } finally {
    await context.close();
  }
}

const overflow = (page) =>
  page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
const scrollInto = (page, selector, offset = 0) =>
  page.evaluate(
    ([sel, extra]) => {
      const el = document.querySelector(sel);
      window.scrollTo(0, el.getBoundingClientRect().top + window.scrollY + extra);
    },
    [selector, offset],
  );

try {
  await check("server: headers, health and errors", phone, "", async (page) => {
    // The page's own CSP forbids fetch, so ask through the test client.
    const get = async (path, method = "GET") => {
      const response = await page.request.fetch(`${origin}${path}`, { method });
      return [response.status(), response.headers()];
    };
    const [status, headers] = await get("/");
    assert.equal(status, 200);
    assert.match(headers["content-security-policy"], /default-src 'none'/);
    assert.equal(headers["cache-control"], "no-cache");
    const asset = await page.evaluate(() => new URL(document.querySelector("script[src]").src).pathname);
    const [, assetHeaders] = await get(asset);
    assert.match(assetHeaders["cache-control"], /immutable/);
    assert.equal((await get("/healthz"))[0], 200);
    assert.equal((await get("/nope"))[0], 404);
    assert.equal((await get("/", "POST"))[0], 405);
  });

  await check("phone: content, heat field and story", phone, "?scale=0.5", async (page) => {
    assert.equal(await page.getByRole("heading", { level: 1 }).getAttribute("aria-label"), "Agents forget.");
    assert.equal(await page.locator("html.gl").count(), 1, "WebGL heat field is on");
    assert.ok(await page.getByText("v1 in progress").first().isVisible());
    const github = page.getByRole("link", { name: "View on GitHub" });
    assert.equal(await github.getAttribute("href"), "https://github.com/ligerian-labs/memoire");
    assert.equal(await overflow(page), 0, "no horizontal overflow");

    // Scrolling types the first commands and brings the scratch database up.
    assert.equal(await page.locator("html.js-life").count(), 1);
    const vh = page.viewportSize().height;
    await scrollInto(page, ".life__track", vh * 0.9);
    await page.waitForFunction(() => document.querySelector('[data-mark="scratch"]').classList.contains("is-on"));
    const typed = await page.evaluate(() =>
      Array.from(document.querySelectorAll('[data-beat="create"] .cmd:first-of-type .t'), (t) => t.textContent).join(" "),
    );
    assert.equal(typed, "memoire db create scratch --burstable --ttl 24h");
    assert.equal(await overflow(page), 0);
  });

  await check("phone: time-lapse and key controls", phone, "?scale=0.5", async (page) => {
    await scrollInto(page, "#kinds");
    const toggle = page.locator('[data-action="toggle"]');
    await toggle.tap();
    assert.equal(await toggle.getAttribute("aria-pressed"), "true");
    assert.equal(await toggle.textContent(), "Play");
    await page.locator('[data-action="query"]').tap();
    assert.match(await page.locator("[data-state]").textContent(), /active|410 database_expired/);

    await scrollInto(page, "#access");
    await page.locator(".switch").tap();
    assert.equal(await page.locator(".perms li.is-off").count(), 4);
    assert.match(await page.locator("[data-perm-status]").textContent(), /can read/);
    assert.equal(await overflow(page), 0);
  });

  await check("phone, reduced motion: plain story, manual time", { ...phone, reducedMotion: "reduce" }, "?scale=0.5", async (page) => {
    assert.equal(await page.locator("html.js-life").count(), 0);
    for (const beat of await page.locator(".beat").all()) assert.ok(await beat.isVisible());
    const toggle = page.locator('[data-action="toggle"]');
    assert.equal(await toggle.textContent(), "Advance 6 hours");
    await scrollInto(page, "#kinds");
    const before = await page.locator(".lapse__clock").textContent();
    await toggle.tap();
    await page.waitForFunction((was) => document.querySelector(".lapse__clock").textContent !== was, before);
    assert.equal(await overflow(page), 0);
  });

  const install = "cargo install --locked --git https://github.com/ligerian-labs/memoire memoire-cli";
  const clipboard = { permissions: ["clipboard-read", "clipboard-write"] };
  await check("phone: get started, copy a command", { ...phone, ...clipboard }, "?scale=0.5", async (page) => {
    assert.equal(await page.getByRole("link", { name: "Get started" }).getAttribute("href"), "#start");
    await scrollInto(page, "#start");
    const copy = page.locator("#start .copy").first();
    assert.equal(await copy.getAttribute("aria-label"), "Copy the install command");
    await copy.tap();
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), install);
    assert.equal(await copy.textContent(), "Copied");
    assert.equal(await copy.getAttribute("aria-label"), "Copied the install command");
    assert.equal(await page.locator("[data-copy-status]").textContent(), "Copied the install command.");
    await page.getByRole("button", { name: "Copy the sign-in command" }).tap();
    assert.equal(
      await page.evaluate(() => navigator.clipboard.readText()),
      "memoire login --server https://app.memoire.network",
    );
    await page.getByRole("button", { name: "Copy the install command" }).waitFor();
    assert.equal(await overflow(page), 0);
  });

  await check("phone, no scripts: steps readable, no dead buttons", { ...phone, javaScriptEnabled: false }, "", async (page) => {
    assert.ok(await page.getByText(install).isVisible());
    assert.equal(await page.locator("#start .copy").count(), 6);
    for (const button of await page.locator("#start .copy").all()) assert.ok(await button.isHidden());
    assert.equal(await overflow(page), 0);
  });

  await check("phone, no WebGL: still readable", phone, "?nogl", async (page) => {
    assert.equal(await page.locator("html.no-gl").count(), 1);
    assert.ok(await page.getByRole("heading", { name: "The life of a database" }).isVisible());
  });

  for (const width of [320, 768, 1440]) {
    await check(`${width}px wide: no horizontal overflow`, { viewport: { width, height: 800 } }, "?scale=0.5", async (page) => {
      await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
      await page.waitForTimeout(300);
      assert.equal(await overflow(page), 0);
    });
  }
} finally {
  await browser.close();
  server.kill("SIGTERM");
}

console.log(results.join("\n"));
