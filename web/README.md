# memoire web

The debugger: an Angular 22 client for people to look at a team's databases.
Sign in or register, create and switch teams, list databases (kind, state,
expiry, size), create one, browse its tables page by page, run SQL in read or
write mode, read its schema, restore a snapshot, and manage the team's
machines and API keys. It speaks only the HTTP contract in
[`docs/api.md`](../docs/api.md).

Node 24 and pnpm through Corepack (`corepack enable`; the version is pinned in
`package.json`).

```sh
cd web
pnpm install --frozen-lockfile
pnpm start            # http://localhost:4200, proxies /v1 and /health to 127.0.0.1:28960
```

Run the development gateway beside it with `scripts/dev-run.sh`; its
configuration already names `http://localhost:4200` as the application origin.
Register an account: in development the gateway captures mail instead of
sending it, and `GET /v1/notifications/dev/captures` lists the confirmation
link.

## Checks

| Command | What |
| --- | --- |
| `pnpm lint` | Prettier |
| `pnpm test` | vitest units (Angular's unit-test builder) and the server's `node --test` suite |
| `pnpm typecheck` | development build and the end-to-end sources |
| `pnpm build` | production build in `dist/memoire-web/browser` |
| `pnpm e2e` | Playwright against a real gateway (below) |

`pnpm e2e` starts everything it needs on loopback and removes it afterwards:
a throwaway PostgreSQL 17 (`scripts/dev-postgres.sh`, port 28982), the
gateway with `config/dev/memoire.toml` and test overrides (port 28983), and
the production build behind `server/serve.mjs` (port 28981). It builds the
gateway with Cargo unless `MEMOIRE_GATEWAY_BIN` names one, and uses
`MEMOIRE_E2E_DATABASE_URL` instead of its own cluster when set (CI). It runs
`pnpm build` first unless `MEMOIRE_E2E_REUSE_BUILD=1`. Each step
of the journey saves a screenshot in `test-results/screenshots/`. On failure
the logs stay in `test-results/e2e-runtime/`.

## Serving the production build

```sh
pnpm build
node server/serve.mjs --listen 127.0.0.1:28951 --gateway http://127.0.0.1:28950
```

`server/serve.mjs` serves the build and proxies `/v1` and `/health` to the
gateway, so the browser sees one origin and the session cookie, `Origin` and
CSRF checks work unchanged. It has no dependencies beyond Node.

| Setting | Flag | Environment | Default |
| --- | --- | --- | --- |
| Listen address (IP:PORT) | `--listen` | `MEMOIRE_WEB_LISTEN` | `127.0.0.1:28951` |
| Gateway origin | `--gateway` | `MEMOIRE_WEB_GATEWAY` | `http://127.0.0.1:28950` |
| Build directory | `--root` | `MEMOIRE_WEB_ROOT` | `dist/memoire-web/browser` |

Flags win over the environment. The listen address must be loopback
(`127.0.0.0/8` or `::1`): exposing the debugger is the job of whatever fronts
it. The listen address must be loopback
(`127.0.0.0/8` or `::1`): exposing the debugger is the job of whatever fronts
it. Invalid settings stop the server before it
listens, with every problem listed. It logs one JSON line per request
(method, path without the query, status, duration) and drains for up to ten
seconds on `SIGTERM`.

The public origin is not a web setting: the build uses relative URLs only.
Set the gateway's `modules.identity.app_origin` to the origin people open in
their browser (whatever proxy or tailnet name fronts the server); identity
rejects state changes from any other origin. App routes fall back to
`index.html`; a missing file with an extension answers 404. Hashed bundles
are cached for a year, everything else is revalidated.
