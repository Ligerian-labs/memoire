# Architecture

memoire gives agents SQLite databases on demand and lets teams share them
across machines. One gateway process is always up and handles every request;
the SQLite files live on the gateway's host. The `memoire` CLI creates and
queries databases, and a web debugger lets a person look at the data.

## Language

- **Team**: a Merezha identity organization, the tenant.
- **Member**: a user in a team. **Machine**: a service account of a team.
  Members and machines hold API keys.
- **Database**: one SQLite file owned by one team. It has a name (unique per
  team, `^[a-z][a-z0-9-]{1,62}$`), an id `db_…`, a kind, a state (`active`,
  `expired`, `deleted`), `created_at`, `last_used_at`, and for burstable
  databases an idle TTL and an absolute expiry.
- **Burstable**: a scratch database. It expires after an idle TTL (default
  24 hours) or 7 days after creation, whichever comes first, and has no
  backups.
- **Persistent**: lives until deleted. Snapshotted every 6 hours; snapshots
  are kept 7 days.
- **Query**: a batch of SQL statements with parameters, run in `read` or
  `write` mode, optionally as one transaction.
- **Snapshot**: a consistent copy of a persistent database made with SQLite's
  online backup API.

## Shape

```text
 memoire CLI ─┐                    ┌──────────── gateway (one process) ─────────────┐
 agents ──────┼── HTTP JSON ──────▶│ Merezha host: edge, identity, jobs, PostgreSQL  │
 web debugger ┘  /v1/memoire/…     │   ├─ memoire module (native): data plane        │
                 /v1/identity/…    │   └─ catalog guest (WASM): lifecycle decisions  │
                                   └────────────┬─────────────────────┬──────────────┘
                                                │                     │
                                   <data_dir>/teams/<team>/<db>.sqlite  PostgreSQL
                                   (WAL, one writer + reader pool)      (events, identity,
                                                                        last use, jobs)
```

The gateway is a [Merezha](https://github.com/MartianGreed/merezha) host,
pinned by git revision. `memoire-gateway run --config FILE` starts it with
Merezha's blocks (identity, notifications, content) and the memoire module.
Merezha provides the HTTP edge, authentication, the error registry,
PostgreSQL, migrations, jobs, health and telemetry.

## Bounded contexts

1. **Catalog** (core, WASM guest `guests/catalog`). Decides which databases
   exist. Its aggregate is `Catalog/{team}`: one stream per team holding at
   most 50 active and 50 expired entries, so name uniqueness and the quota are
   one transaction.
   Commands `CreateDatabase`, `ExtendDatabase`, `DeleteDatabase` and
   `ExpireDatabase` (system only: the host supplies `now` and `last_used_at`
   in the payload because guests are deterministic, and the guest checks the
   rule) emit `DatabaseCreated`, `DatabaseExtended`, `DatabaseDeleted` and
   `DatabaseExpired`. The `databases` projection, partitioned by team, feeds
   the list and detail queries. The guest speaks application protocol 2, so
   it sees the principal and tenant the host established. Guests import
   nothing: no file, network or database handle.
2. **Data plane** (core, the native `memoire` module in
   `crates/memoire-gateway`). Opens files, runs queries, enforces limits,
   tracks last use, makes snapshots, exports and imports. It acts on catalog
   decisions: after an accepted `DatabaseCreated` it creates the file
   idempotently, and after a delete or expiry it removes the file. A
   reconciler at startup repairs drift between the files and the catalog
   read model. It never reads another context's tables.
3. **Identity** (generic, Merezha's block). Accounts, teams, invitations,
   service accounts, API keys and RBAC. memoire registers the permissions
   `memoire.databases.create`, `.delete`, `.read`, `.write` and `.snapshots`;
   the team roles admin and member hold them by default, and an API key can
   carry a subset.

## Data plane rules

- rusqlite is exactly the version Merezha pins (`=0.40.2`, bundled), so only
  one `libsqlite3-sys` is linked. CI fails when a second one appears.
- Files live at `<data_dir>/teams/<team id>/<db id>.sqlite` in WAL mode. Each
  open database has one writer connection and a small reader pool. A
  burstable database's connections close after 5 minutes idle and reopen on
  the next request.
- Limits: `max_page_count` for 1 GiB, a per-statement deadline through the
  progress handler (default 5 s, max 30 s), maximum result rows and bytes, a
  request body limit and `sqlite3_limit` settings. An authorizer denies
  `ATTACH`, `DETACH`, `load_extension`, `VACUUM INTO` and file-touching
  pragmas. `read` mode uses a read-only connection.
- `last_used_at` is updated in memory and flushed periodically to the
  module's own PostgreSQL schema; it is not an event per query.
- The sweeper (a recurring Merezha job) issues `ExpireDatabase` for burstable
  databases past their idle TTL or lifetime. Persistent databases are
  snapshotted every 6 hours and snapshots older than 7 days are pruned.

## Limits

| Limit | Value |
| --- | --- |
| Database size | 1 GiB |
| Active databases per team | 50 |
| Burstable idle TTL | default 24 hours, 1 minute to 7 days |
| Burstable lifetime | 7 days from creation |
| Snapshots | every 6 hours, kept 7 days |

## Repository

| Path | What |
| --- | --- |
| `crates/memoire-api` | Wire types of the HTTP contract (`docs/api.md`), shared by the gateway and the CLI |
| `crates/memoire-gateway` | The memoire module and the `memoire-gateway` binary |
| `crates/memoire-cli` | The `memoire` binary |
| `guests/` | WASM guests, a separate Cargo workspace; `guests/runtime.toml` lists them |
| `config/dev/` | The development gateway configuration |
| `scripts/` | Development gateway, throwaway PostgreSQL and guest build |
| `web/` | The Angular debugger (to come) |

## Hosting

The gateway binds loopback. The web debugger's production build is served by
a small static server, also on loopback, that proxies `/v1` and `/health` to
the gateway. Exposure beyond the host (a tailnet, a reverse proxy) is the
operator's choice; the public origin is configuration
(`modules.identity.app_origin`), never code.
