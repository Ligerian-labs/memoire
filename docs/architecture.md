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
   The guest is one Merezha application (`config/dev/application.toml`,
   PostgreSQL storage): the host's `application` module migrates the store,
   runs the projection in the background and serves `/v1/application/…`
   under the same permissions. The memoire module opens a second
   application instance on the same store and runs the commands and
   queries in process, as the caller the edge authenticated, so its routes
   keep memoire's own JSON shapes. A command whose team stream moved under
   it (`concurrency_conflict`) is decided again, up to five times; its reads
   wait for the projection to reach the command (`min_position`). The
   gateway mints database ids; the guest checks their shape and that the
   team never used them. A team's history goes whole to the guest on every
   decision, so it is bounded by Merezha's `max_stream_events` (4096) and
   the runtime's 1 MiB input: roughly a thousand database lifecycles per
   team. Compacting the stream is a later change.
2. **Data plane** (core, the native `memoire` module in
   `crates/memoire-gateway`). Opens files, runs queries, enforces limits,
   tracks last use, makes snapshots, exports and imports. It acts on catalog
   decisions: after an accepted `DatabaseCreated` it creates the file
   idempotently, and after a delete or expiry it removes the file. A
   reconciler at startup repairs drift between the files and the catalog
   read model. It never reads another context's tables. The memory routes
   run the memory engine (`crates/memoire-memory`) on these same
   connections, one batch per transaction as the query route runs a
   client's, recording the caller's subject as the writer.
3. **Identity** (generic, Merezha's block). Accounts, teams, invitations,
   service accounts, API keys and RBAC. memoire registers the permissions
   `memoire.databases.create`, `.delete`, `.read`, `.write` and `.snapshots`,
   and `memoire.memory.read` and `.write` for the memory routes; the team
   roles admin and member hold them by default, and an API key can carry a
   subset, such as the two memory permissions alone. The grants are
   configuration, in every deployment:

   ```toml
   [modules.identity.rbac.builtin_grants]
   admin = ["memoire.databases.*", "memoire.memory.*"]
   member = ["memoire.databases.*", "memoire.memory.*"]
   ```

   A machine's key is capped by its service account's own grants, so the
   service account needs a team role (`member`) for its key to act.

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

Data plane specifics:

- Each statement is prepared with the authorizer on: `ATTACH`, `DETACH`,
  `load_extension`, `fts3_tokenizer` and transaction control are refused,
  and only an allow-list of pragmas that touch no file, journal or size
  limit passes. `VACUUM INTO` is refused by its text, since SQLite shows the
  authorizer no action for it. Connections run with `SQLITE_DBCONFIG_DEFENSIVE`,
  untrusted schemas and `sqlite3_limit` caps (100 KiB statements, 16 MiB
  values, 999 parameters).
- Without `transaction` each write statement runs in a savepoint, so a
  failing statement (a result limit included) leaves no change; `VACUUM`
  runs outside one, as SQLite requires.
- `size_bytes` is the file and its write-ahead log, whose pages belong to
  the database until a checkpoint moves them.
- Last use lives in `memoire.last_used` (team, database, Unix
  milliseconds), flushed every `last_used_flush_seconds` and on shutdown.

Lifecycle:

- The sweep and the snapshots are Merezha jobs (`memoire.sweep`,
  `memoire.snapshot`) on the `memoire` queue. Each run first enqueues the
  next slot of its interval, deduplicated by the slot, so retries and
  several instances still make one job per slot; the module's start seeds
  both.
- The sweep visits every team (`memoire.teams`, recorded at create, and the
  team directories). For each active burstable database past
  `min((last use or created) + idle TTL, created + 7 days)` it sends
  `ExpireDatabase` as the system principal `sys_memoire` with `now` and the
  last use; the guest decides, then the file goes. It also removes files the
  catalog no longer holds as active, once left alone for five minutes.
- Snapshots are taken at each slot of `snapshot_interval_seconds` for every
  active persistent database: one-pass online backups to
  `<data_dir>/snapshots/<team>/<db>/<snap>.sqlite`, recorded in
  `memoire.snapshots`, and pruned past `snapshot_retention_seconds`.
  Restore takes a `pre_restore` snapshot, then swaps a copy of the snapshot
  in by rename once the requests on the database finished; import swaps the
  uploaded file in the same way, after `PRAGMA integrity_check`, while the
  database is empty. Export streams a one-pass backup.
- At startup the reconciler also gives an active database whose file is
  missing its latest snapshot, or an empty file. Only the startup pass does
  this: under a lagging read, a missing file may be one being deleted.

Merezha's edge rewrites any error body whose `error` object carries a
member beyond `code`, `classification` and `details`, so `sql_error` sends
SQLite's message in the `Memoire-Sql-Message` header. Letting a module add
its own error member is a later upstream improvement.

## Limits

| Limit | Value |
| --- | --- |
| Database size | 1 GiB |
| Active databases per team | 50 |
| Burstable idle TTL | default 24 hours, 1 minute to 7 days |
| Burstable lifetime | 7 days from creation |
| Snapshots | every 6 hours, kept 7 days |

## Configuration

`[modules.memoire]`:

| Key | Default | Meaning |
| --- | --- | --- |
| `data_dir` | required | Root of the SQLite files; relative to the configuration file |
| `min_idle_ttl_seconds` | 60 | Shortest burstable idle TTL a client may ask for (1 to 604 800); lower it only to watch an expiry in tests |
| `default_idle_ttl_seconds` | 86 400 | Idle TTL of a burstable database created without one |
| `max_database_bytes` | 1 073 741 824 | Largest file (65 536 to 1 GiB), through `max_page_count` |
| `idle_close_seconds` | 300 | A burstable database's connections close after this long without a request |
| `last_used_flush_seconds` | 60 | How often last use reaches PostgreSQL |
| `sweep_interval_seconds` | 60 | How often the sweeper runs |
| `snapshot_interval_seconds` | 21 600 | How often persistent databases are snapshotted |
| `snapshot_retention_seconds` | 604 800 | How long snapshots are kept |

The gateway also needs `[application] path` naming the catalog
application, and `[runtime] manifest` naming `guests/runtime.toml` (component
paths stay inside the manifest's directory).

## Repository

| Path | What |
| --- | --- |
| `crates/memoire-api` | Wire types of the HTTP contract (`docs/api.md`), shared by the gateway and the CLI |
| `crates/memoire-memory` | The memory engine: its model, SQL, guards and verbs, run through a backend that executes a batch as one transaction; no network or file access of its own |
| `crates/memoire-gateway` | The memoire module and the `memoire-gateway` binary; its memory routes run the memory engine on the data plane |
| `crates/memoire-cli` | The `memoire` binary; `memoire memory` calls the gateway's memory routes, and runs the engine over the query route against a gateway without them |
| `guests/` | WASM guests, a separate Cargo workspace; `guests/runtime.toml` lists them and is the gateway's runtime manifest |
| `config/dev/` | The development gateway configuration: `memoire.toml` and the catalog application |
| `scripts/` | Development gateway, throwaway PostgreSQL, guest build and a development owner with a team and an API key |
| `web/` | The Angular debugger and its static server (`web/README.md`) |

## Hosting

The gateway binds loopback. The web debugger's production build is served by
a small static server, also on loopback, that proxies `/v1` and `/health` to
the gateway. Exposure beyond the host (a tailnet, a reverse proxy) is the
operator's choice; the public origin is configuration
(`modules.identity.app_origin`), never code.
