# memoire landing page

The public landing page of memoire: one static page, built with Vite and
TypeScript, with no runtime dependencies.

The page is a thermal image. A small WebGL heat field runs under real HTML
text. A burstable database is a warm spot that cools toward its idle TTL. A
persistent one stays warm and gains a ring per snapshot. A touch leaves
warmth that fades. Without WebGL, or with reduced motion, the same content
shows as a still page.

## Commands

Node 22 or later and pnpm through Corepack.

```sh
corepack pnpm install
corepack pnpm dev          # development server
corepack pnpm build        # typecheck, then build into dist/
corepack pnpm start        # serve dist/ on 127.0.0.1:28952
corepack pnpm test         # smoke test of the build in Chromium
corepack pnpm shots        # screenshots through the scroll, phone and desktop
node tests/extras.mjs      # a touch trail, reduced motion and no-WebGL views
```

`pnpm test` and `pnpm shots` need Playwright's Chromium once:
`corepack pnpm exec playwright install chromium`. `pnpm shots` takes the URL
of a running site as its first argument (default `http://127.0.0.1:4173/`,
which is `pnpm preview`) and writes to `tests/out/`.

## Serving

`serve.mjs` is a static server with no dependencies. It reads and compresses
`dist/` once at startup, sends long-lived cache headers for hashed assets and
a strict Content-Security-Policy, and answers `GET /healthz`. Restart it
after a new build.

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT` | `28952` | Port to listen on |
| `HOST` | `127.0.0.1` | Address to bind |
| `SITE_ROOT` | `dist` next to `serve.mjs` | Directory holding the build |

It binds loopback. Exposure beyond the host is the operator's choice.

## Layout

| Path | What |
| --- | --- |
| `index.html` | All content, as plain semantic HTML |
| `src/style.css` | Styles, mobile first |
| `src/main.ts` | Frame loop: wakes on input, sleeps when the field has settled |
| `src/heat/` | The heat field (WebGL2), its source painter and the color ramp |
| `src/hero.ts`, `life.ts`, `kinds.ts`, `access.ts`, `start.ts`, `ember.ts`, `headings.ts` | One scene per part of the page |
| `src/fonts/` | Anybody and Martian Mono, self-hosted (SIL Open Font License) |
| `public/` | `robots.txt`, `llms.txt` and the font licenses, copied as they are |
| `tests/` | Smoke test and screenshot scripts |

Two query parameters help when testing: `?scale=1` pins the drawing-buffer
scale (the page otherwise lowers it when frames run long), and `?nogl` turns
the heat field off.

Facts on the page come from `README.md`, `docs/architecture.md`,
`docs/api.md` and `docs/cli.md` at the repository root. When those change,
change the page. The "Get started" section, `public/llms.txt` and the
README's "Get started" section give the same steps: change them together.
