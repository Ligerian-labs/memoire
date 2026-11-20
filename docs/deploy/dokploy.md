# Deploying on Dokploy

One Dokploy project holds four services in one environment. Dokploy builds the three applications from this repository with its GitHub App provider, from `main`, on every push.

| Service | Kind | Build | Listens |
| --- | --- | --- | --- |
| `memoire-db` | native PostgreSQL 17 | | 5432 on the private network |
| `memoire-gateway` | Application | `infra/docker/gateway.Dockerfile`, context `.` | 8080 |
| `memoire-web` | Application | `infra/docker/web.Dockerfile`, context `.` | 8080 |
| `memoire-site` | Application | `infra/docker/site.Dockerfile`, context `.` | 8080 |

## Routes

Dokploy's proxy terminates TLS and routes by host and path:

- `memoire.network` goes to `memoire-site`, the landing page.
- On `app.memoire.network`, `/v1` and `/health` go to `memoire-gateway`. Every other path goes to `memoire-web`, whose Caddy serves the build and falls back to `index.html` for client-side routes.

Browsers, the CLI and agents use one origin, `https://app.memoire.network`. The gateway's session cookie and its `Origin` check need that one origin, and so does `memoire keys create`, which signs in as a member. Point the CLI at it with `memoire login --server https://app.memoire.network`.

DNS: an A record for `memoire.network` pointing at the Dokploy server, and `app.memoire.network` as a CNAME of `memoire.network`.

## memoire-gateway

- The image carries the gateway, the catalog guest, `guests/runtime.toml`, the catalog application (`config/dev/application.toml`, the same document in every environment) and [`infra/prod/memoire.toml`](../../infra/prod/memoire.toml), all under `/etc/memoire/`. The configuration holds no secret.
- Mount a persistent volume at `/var/lib/memoire` (`data_dir`). It holds every team's SQLite files and the snapshots of persistent databases. Losing it loses the data.
- Environment:

  | Variable | Value |
  | --- | --- |
  | `MEMOIRE_DATABASE_URL` | `postgres://…@<memoire-db service name>:5432/memoire` |
  | `MEMOIRE_IDENTITY_KEY` | `openssl rand -hex 32` |
  | `MEMOIRE_NOTIFICATIONS_KEY` | `openssl rand -base64 32` |
  | `BREVO_API_KEY` | the Brevo account's API key; the sender's domain must be authenticated in Brevo |
  | `MEMOIRE_BREVO_WEBHOOK_TOKEN` | `openssl rand -base64 48` |

- `database.migrate = true` applies migrations at startup. `GET /health/ready` answers 200 once startup completes; the image's health check calls it.
- A top-level module key can be overridden from the environment as `MEREZHA_MODULES__<MODULE>__<KEY>`, for example `MEREZHA_MODULES__IDENTITY__REGISTRATION=open`.

## First owner

Registration is closed, so only invited people join.

1. Set `MEREZHA_MODULES__IDENTITY__REGISTRATION=open` and redeploy.
2. `POST /v1/identity/registrations` with the owner's email and a throwaway password, header `Origin: https://app.memoire.network`. Do not keep the `merezha_verify` cookie.
3. The owner opens the verification email in their own browser and confirms. Without the binding cookie, confirmation claims the account and clears the throwaway password. The web app then asks the owner to set their own password.
4. Remove the variable and redeploy, then check that a registration answers `403`.

The owner then creates a team in the web app, invites members and mints machine keys with `memoire keys create`.

## Backups

Persistent databases are snapshotted into the volume every 6 hours and kept for 7 days. These snapshots protect against mistakes, not against losing the host. Back up the `/var/lib/memoire` volume and `memoire-db` off the server as well.
