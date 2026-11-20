# memoire HTTP API

This is the contract between the gateway and its clients: the `memoire` CLI,
the web debugger and agents. Version 1. The Rust types in
`crates/memoire-api` are its machine form; when they and this document
disagree, fix the code. Changing the contract needs agreement from the
gateway, CLI and web owners.

## Conventions

- memoire routes mount under `/v1/memoire/`. The gateway also serves
  Merezha's identity routes under `/v1/identity/` and the health routes.
- Request and response bodies are JSON (`Content-Type: application/json`,
  otherwise `415 unsupported_media_type`), except export and import, which
  carry a raw SQLite file (`application/vnd.sqlite3`).
- Request bodies are capped at 1 MiB (`413 body_limit`), except import
  (1 GiB).
- Timestamps are RFC 3339 UTC strings (`2026-11-19T10:00:00Z`). Durations are
  whole seconds in fields named `*_seconds`.
- Ids are opaque prefixed strings: databases `db_…`, snapshots `snap_…`,
  teams `org_…`, users `usr_…`, API keys `key_…`, memory stores `mst_…`.
  Do not parse them. Memory ids (`mem_…`) are the exception, see
  [Memory routes](#memory-routes).
- `{name}` in a path is a database name. `{name}` of another team answers
  `404 database_not_found`, the same as a name that does not exist, so
  names cannot be probed across teams.
- Unknown fields in a request body fail `422 validation_failed`. An
  optional field sent as `null` is the same as omitting it.
- Every response carries `Cache-Control: no-store`.

## Authentication and the team

Every memoire route needs a credential.

| Client | Credential | Team |
| --- | --- | --- |
| CLI, agents, machines | `Authorization: Bearer mzk_…`, a team API key (a member's own or a machine's) | The key's own team |
| Web | The session cookie from identity's sign-in: `__Host-merezha_session`, or `merezha_session` when the gateway runs on loopback with an `http://localhost` application origin (development) | The session's active team |
| Scripts signed in as a member | `Authorization: Bearer mzs_…`, a session from a sign-in with `"transport": "bearer"` | The session's active team |

- A session's active team is set with `PUT /v1/identity/session/organization`
  `{"organization_id": "org_…"}`, or per request with the
  `Merezha-Organization: org_…` header, which wins. Either must name a team
  the member belongs to, otherwise `404 not_found`. A team key is active in
  its own team only: a header naming another team answers `404 not_found`.
- A missing, malformed, unknown, revoked or expired credential answers
  `401 unauthenticated`. A cookie and an `Authorization` header together
  answer `400 ambiguous_credentials`.
- A caller with no active team (a session without one, or an
  application-scoped API key) answers `403 tenant_required` on every route
  except `whoami`.
- Cookie requests that change state (`POST`, `PUT`, `PATCH`, `DELETE`) pass
  identity's two checks: `Origin` equals the application origin (or is
  absent with `Sec-Fetch-Site: same-origin`), else `403 origin_rejected`;
  and `X-CSRF-Token` carries the `csrf_token` returned at sign-in (also by
  `GET /v1/identity/session`), else `403 csrf_failed`. The web, served from
  the application origin and proxying `/v1` to the gateway, meets both.
  Bearer requests skip both checks.
- A route answers `403 unauthorized` when the caller lacks its permission at
  the team. memoire adds the seven permissions below to identity's catalog;
  the team roles `admin` and `member` hold all seven by default, and an API
  key may carry a subset (a read-only agent key holds only
  `memoire.databases.read`; an agent that only uses the memory holds
  `memoire.memory.read` and `memoire.memory.write`). The permissions are
  registered together with the database routes; until they are, identity
  refuses them in a key request (`422 unknown_permission`).

| Permission | Allows |
| --- | --- |
| `memoire.databases.read` | list, detail, read queries, schema, rows, export, snapshot list; the memory's read routes |
| `memoire.databases.write` | write queries, extend, import; the memory's write routes |
| `memoire.databases.create` | create |
| `memoire.databases.delete` | delete |
| `memoire.databases.snapshots` | snapshot restore |
| `memoire.memory.read` | the memory's read routes: store, recall, show, events, export |
| `memoire.memory.write` | the memory's write routes: init and every verb that writes |

Teams, invitations, machines and keys are Merezha identity's routes; see
"Identity routes the clients use" below.

## Errors

Errors use Merezha's shape. Clients branch on `code`; each code always has
the same status.

```json
{"error": {"code": "database_exists", "classification": "conflict"}}
```

- `classification` is `permanent` (do not retry as is), `transient` (retry
  later; `429` and `503` carry `Retry-After` in seconds) or `conflict`.
- `validation_failed` adds `details`, one entry per field: a JSON Pointer
  into the request body and a code. Input is never echoed.

  ```json
  {"error": {"code": "validation_failed", "classification": "permanent",
             "details": [{"path": "/name", "code": "invalid"}]}}
  ```

- `sql_error` names the statement SQLite refused in one `details` entry,
  path `/statements/{i}/sql` (0-based) and code `sqlite_` plus SQLite's
  primary result code in lower case (`sqlite_constraint`, `sqlite_error`…).
  SQLite's message about it (at most 512 bytes before encoding, never a
  file path) is in the `Memoire-Sql-Message` response header,
  percent-encoded UTF-8: every byte but `A-Z a-z 0-9 - . _ ~` is `%XX`.
  Merezha's error shape has no room for a member of memoire's own, hence
  the header.

  ```http
  HTTP/1.1 422 Unprocessable Entity
  Memoire-Sql-Message: UNIQUE%20constraint%20failed%3A%20users.email

  {"error": {"code": "sql_error", "classification": "permanent",
             "details": [{"path": "/statements/1/sql", "code": "sqlite_constraint"}]}}
  ```

- A refusal of the memory routes carries what a client acts on in a member
  `data` beside `error`, present only when the code has such members (see
  [Memory routes](#memory-routes) for the members of each code). `error`
  keeps Merezha's shape; Merezha's edge checks that member only.

  ```json
  {"error": {"code": "memory_conflict", "classification": "conflict"},
   "data": {"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "current_rev": 3}}
  ```

memoire's codes:

| Code | Status | When |
| --- | --- | --- |
| `statement_denied` | 403 | The authorizer refused the statement: `ATTACH`, `DETACH`, `load_extension`, `VACUUM INTO` or a file-touching `PRAGMA` |
| `database_not_found` | 404 | No database of that name in the team |
| `table_not_found` | 404 | No table or view of that name in the database |
| `snapshot_not_found` | 404 | No snapshot of that id for the database |
| `memory_not_found` | 404 | No memory of that id in the store |
| `database_exists` | 409 | Create with a name an active database of the team holds |
| `database_not_empty` | 409 | Import into a database that has a user table, view, index or trigger |
| `not_burstable` | 409 | Extend on a persistent database |
| `not_persistent` | 409 | A snapshot route on a burstable database |
| `memory_not_initialized` | 409 | A memory route but `init` on a database that holds no store |
| `memory_schema_older` | 409 | The store is older than the gateway's engine; `init` migrates it |
| `memory_schema_newer` | 409 | The store is newer than the gateway's engine |
| `memory_conflict` | 409 | The memory moved after it was read, or its text changed after the `rev` a verdict names |
| `memory_duplicate` | 409 | `remember` without `force` and a memory of the scope has a similar title |
| `memory_not_current` | 409 | The memory is superseded or forgotten |
| `memory_no_open_dispute` | 409 | `resolve` on a memory without an open dispute |
| `memory_cycle` | 409 | A `supersede` that would close a cycle |
| `database_expired` | 410 | Query, schema, rows, export, import, extend, snapshots or a memory route on an expired database |
| `database_too_large` | 413 | A write or import would pass the 1 GiB file limit |
| `database_limit` | 422 | Create when the team already has 50 active databases |
| `read_only` | 422 | A statement in `read` mode tried to write |
| `sql_error` | 422 | SQLite refused a statement (syntax, constraint, missing table…); `details` names it, the `Memoire-Sql-Message` header carries SQLite's message |
| `statement_timeout` | 422 | A statement ran past its deadline and was interrupted |
| `result_too_large` | 422 | A statement returned more than 10 000 rows, or the response passed 16 MiB |
| `invalid_import` | 422 | The import body is not a SQLite database or fails `PRAGMA integrity_check` |
| `memory_self_confirm` | 422 | A confirmation by the text's author, same principal and session |
| `memory_secret_detected` | 422 | A field looks like it holds a secret |
| `memory_too_large` | 422 | A field passes its size limit |
| `memory_unchanged` | 422 | A `revise` that would change nothing |
| `sqlite_busy` | 503 | The writer stayed locked past the busy timeout; transient |

The `memory_*` codes of status 422 have the classification `permanent`,
those of 409 `conflict`, and `memory_not_found` `permanent`.

Merezha codes a client also meets: `400 ambiguous_credentials`,
`401 unauthenticated`, `403 unauthorized`, `403 tenant_required`,
`404 not_found` (no such route), `413 body_limit`,
`415 unsupported_media_type`, `422 validation_failed`, `422 invalid_page`
(a bad `cursor` or `after`), `429 rate_limited`, `503 overloaded`,
`503 starting`.

## Value encoding

SQLite values travel as JSON as follows, in query parameters, query results
and row pages.

| SQLite | JSON |
| --- | --- |
| NULL | `null` |
| INTEGER with \|n\| ≤ 2^53 − 1 | number with no fraction or exponent: `42` |
| INTEGER beyond that | `{"type": "integer", "value": "9223372036854775807"}` |
| REAL, finite | number that always has a fraction or exponent: `1.0`, `2.5e-8` |
| REAL, ±infinity | `{"type": "real", "value": "Infinity"}` or `"-Infinity"` |
| TEXT | string |
| BLOB | `{"type": "blob", "value": "<standard base64 with padding>"}` |

An integer that comes back as a number is always exact, even in
JavaScript. `JSON.parse` turns `1.0` and `1` into the same `Number`, so a
JavaScript client that must tell REAL from INTEGER reads the response with
a parser that keeps number tokens; a client that only displays values, or
writes them back into a column with REAL affinity (where SQLite stores 1 as
1.0), needs nothing more.

Parameters accept every form above and also:

- `true` and `false`, bound as INTEGER 1 and 0;
- any JSON integer within the 64-bit signed range, bound exactly;
- `{"type": "integer", "value": 12}` or `"12"`, `{"type": "real", "value":
  2.5}` or `"2.5"`, and `{"type": "text", "value": "…"}`.

A bare JSON number's kind comes from its token: with a fraction or exponent
it binds as REAL, without one as INTEGER. Arrays, other objects, NaN,
integers beyond the 64-bit signed range (either sign) and reals beyond the
double range fail `422 validation_failed` with the parameter's path.

## The database object

Every database route returns databases in this shape.

```json
{
  "id": "db_0192f5c4a1b27c3d8e9f00112233aabb",
  "name": "scratch",
  "kind": "burstable",
  "state": "active",
  "created_at": "2026-11-19T10:00:00Z",
  "last_used_at": "2026-11-19T10:05:12Z",
  "size_bytes": 8192,
  "idle_ttl_seconds": 86400,
  "expires_at": "2026-11-20T10:05:12Z",
  "expires_at_max": "2026-11-26T10:00:00Z"
}
```

| Field | Meaning |
| --- | --- |
| `name` | `^[a-z][a-z0-9-]{1,62}$`: 2 to 63 characters, a lowercase letter, then lowercase letters, digits and `-`. Unique among the team's active databases. |
| `kind` | `burstable` (scratch: expires when idle, no backups) or `persistent` (lives until deleted, snapshots every 6 hours kept 7 days) |
| `state` | `active` or `expired`. Deleted databases are never returned. |
| `last_used_at` | Last query, import, extend or memory request; `null` until the first. Updated in memory and flushed about every minute, so it may lag by that much. |
| `size_bytes` | File size; `null` when the file is not on disk (expired) |
| `idle_ttl_seconds` | Burstable: idle time before expiry, 60 to 604 800, default 86 400. `null` for persistent. |
| `expires_at` | Burstable: `min((last_used_at or created_at) + idle_ttl_seconds, expires_at_max)`. `null` for persistent. |
| `expires_at_max` | Burstable: `created_at` + 7 days. Never moves. `null` for persistent. |

A burstable database expires at `expires_at`; the sweeper runs every minute,
so it can still answer for up to a minute past that. Expiry removes the file
and leaves the entry listed with `state: "expired"` until it is deleted, a
new database takes its name, or it is among more than 50 expired entries of
the team, when the oldest expired entries are dropped. The 50-database quota
counts active databases only. A database created again under a used name is
a new database with a new id.

## Routes

`{name}` paths are relative to `/v1/memoire/databases/{name}`.

### `GET /v1/memoire/whoami`

Any credential, with or without a team. Answers `200`:

```json
{"subject": "usr_0192…", "kind": "user", "team_id": "org_0192…"}
```

`kind` is `user` for a session and `api_key` for an API key (a member's or a
machine's). `team_id` is `null` for a session with no active team and for an
application-scoped API key, which no memoire database route accepts. The CLI's
`whoami` and `login` use it to check a key.

### `GET /v1/memoire/databases`

`memoire.databases.read`. Query: `state` (`active`, the default, or `all` to
include expired), `cursor`, `limit` (1 to 100, default 50). Sorted by name.
Answers `200`:

```json
{"items": [{"id": "db_…", "name": "scratch", "...": "..."}], "next_cursor": null}
```

Pass `next_cursor` back as `cursor` for the next page; `null` means the last
page. A cursor is opaque and valid for the same `state` filter only
(`422 invalid_page` otherwise).

### `POST /v1/memoire/databases`

`memoire.databases.create`.

```json
{"name": "scratch", "kind": "burstable", "idle_ttl_seconds": 3600}
```

`idle_ttl_seconds` is optional for burstable (default 86 400) and refused
for persistent (`validation_failed`, path `/idle_ttl_seconds`). Answers
`201` with the database, its file created and empty. Errors:
`validation_failed` (`/name` `invalid`, `/idle_ttl_seconds` `out_of_range`),
`database_exists`, `database_limit`.

Creating is not idempotent: a retry after a lost response answers
`409 database_exists`; `GET` the name to see whether the first attempt
succeeded.

### `GET {name}`

`memoire.databases.read`. Answers `200` with the database, active or
expired. A key holding only `memoire.memory.*` cannot list databases or
read their detail: the memory routes name the database themselves.

### `DELETE {name}`

`memoire.databases.delete`. Deletes the database, its file and its
snapshots. Works on active and expired databases. Answers `204`. Errors:
`database_not_found`.

### `POST {name}/extend`

`memoire.databases.write`. Burstable only.

```json
{"idle_ttl_seconds": 7200}
```

The body may be `{}`: the idle clock restarts now and the TTL stays. With
`idle_ttl_seconds` (60 to 604 800) the TTL changes too. `expires_at` never
passes `expires_at_max`; asking for more is not an error, the result is
capped. Answers `200` with the database. Errors: `not_burstable`,
`database_expired`, `validation_failed`.

### `POST {name}/query`

`memoire.databases.read` for `read` mode, `memoire.databases.write` for
`write`.

```json
{
  "mode": "write",
  "transaction": true,
  "timeout_ms": 5000,
  "statements": [
    {"sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT, data BLOB)"},
    {"sql": "INSERT INTO notes (body, data) VALUES (?, ?)", "params": ["hello", {"type": "blob", "value": "AAE="}]},
    {"sql": "SELECT id, body FROM notes WHERE body = :body", "params": {"body": "hello"}}
  ]
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `mode` | `read` | `read` runs on a read-only connection; a write fails `read_only`. `write` runs on the database's single writer. |
| `transaction` | `false` | `true` runs the batch in one transaction: all statements apply or none. |
| `timeout_ms` | 5 000 | Per-statement deadline, 1 to 30 000. |
| `statements` | | 1 to 100, run in order. Each `sql` is exactly one statement (a trailing `;` is fine), at most 100 KiB. |
| `params` | `[]` | An array or an object, at most 999 values; see binding below. |

Binding follows SQLite's parameter numbering:

- An array binds by index: element *i* (0-based) is parameter *i + 1*, so
  it is `?` in order, `?NNN` when NNN = *i + 1*, or the named parameter
  SQLite numbered *i + 1*. Its length must equal the statement's parameter
  count (`sqlite3_bind_parameter_count`, the highest index used).
- An object binds by name and suits statements whose parameters are all
  named (`:a`, `@a`, `$a`). A key without a sigil matches `:key`, `@key` and
  `$key`; a key with a sigil matches only that parameter. Every named
  parameter needs exactly one value.
- Anything else fails `422 validation_failed` with the path of the
  statement's `params`: a count mismatch, a missing or unknown name, two
  keys for one parameter (`a` and `:a`), or an object for a statement with
  `?` or `?NNN` parameters.

Answers `200` with one result per statement:

```json
{
  "results": [
    {"columns": [], "rows": [], "rows_affected": 0, "last_insert_rowid": null},
    {"columns": [], "rows": [], "rows_affected": 1, "last_insert_rowid": 1},
    {"columns": [{"name": "id", "decltype": "INTEGER"}, {"name": "body", "decltype": "TEXT"}],
     "rows": [[1, "hello"]], "rows_affected": 0, "last_insert_rowid": null}
  ]
}
```

- `columns[].decltype` is the declared type as written in the schema, `null`
  for an expression.
- `rows_affected` is SQLite's change count (`sqlite3_changes`) for INSERT,
  UPDATE and DELETE, 0 otherwise.
- `last_insert_rowid` is the rowid of the last row the statement itself
  inserted into a rowid table, in the value encoding. It is `null` when the
  statement inserted no such row: not an INSERT or REPLACE, a `WITHOUT
  ROWID` table, a row `OR IGNORE` skipped, or an UPSERT that only updated.
  The gateway compares SQLite's value before and after the statement and
  reports it only when it moved, so a value left by an earlier statement
  never shows, while SQL's own `last_insert_rowid()` keeps its meaning
  across the batch (`INSERT INTO child VALUES (last_insert_rowid())` sees
  the row the previous statement inserted). A statement whose last insert
  lands on the rowid the value already held therefore reports `null`.

On failure the batch stops at the failing statement and the response is the
error alone (`sql_error`, `read_only`, `statement_denied`,
`statement_timeout`, `result_too_large`, `database_too_large`,
`sqlite_busy`). A failing statement never leaves its own changes, whatever
failed, a result limit included: with `transaction: false` each statement
runs in its own savepoint, so the statements before the failing one stay
committed and the failing one is rolled back; with `transaction: true`
nothing applies. Transaction control in statements (`BEGIN`, `COMMIT`,
`END`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`) fails `statement_denied` in
every mode: use `transaction` instead.

### `GET {name}/schema`

`memoire.databases.read`. Answers `200` with every user table and view,
sorted by name, without SQLite's own `sqlite_*` objects:

```json
{
  "tables": [{
    "name": "notes",
    "sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL DEFAULT '')",
    "without_rowid": false,
    "columns": [
      {"name": "id", "type": "INTEGER", "not_null": false, "default": null, "primary_key": 1, "generated": false},
      {"name": "body", "type": "TEXT", "not_null": true, "default": "''", "primary_key": 0, "generated": false}
    ],
    "indexes": [{"name": "notes_body", "unique": false, "columns": ["body"], "sql": "CREATE INDEX notes_body ON notes (body)"}]
  }],
  "views": [{"name": "recent", "sql": "CREATE VIEW recent AS SELECT * FROM notes", "columns": []}]
}
```

`columns` come from `PRAGMA table_xinfo` (hidden columns left out).
`type` is `""` when none was declared, `default` is the default expression as
SQL text, `primary_key` is the 1-based position in the primary key (0 when
not part of it). An index column is `null` for an expression; an index `sql`
is `null` for an automatic index (a `UNIQUE` or `PRIMARY KEY` constraint).
View `columns` carry the same fields; `not_null`, `default` and
`primary_key` are SQLite's report for views (usually false, null, 0).

### `GET {name}/tables/{table}/rows`

`memoire.databases.read`. The row browser: a table or view's rows, page by
page. `{table}` is the object's exact name, percent-encoded as one path
segment (a name with spaces, `/` or `%` included). Query: `limit` (1 to
1 000, default 100) and `after` (the `next_after` of the previous page).
Answers `200`:

```json
{"columns": [{"name": "id", "decltype": "INTEGER"}, {"name": "body", "decltype": "TEXT"}],
 "rows": [[1, "hello"], [2, "world"]],
 "next_after": "cm93aWQ6Mg"}
```

`next_after` is opaque, `null` on the last page, and valid only for the same
database and object (`422 invalid_page` otherwise). Rows come in a
deterministic order: tables by rowid, `WITHOUT ROWID` tables by primary key.
Views, and tables whose columns shadow all three rowid aliases (`rowid`,
`oid`, `_rowid_`), page by offset in the order SQLite returns, which is not
guaranteed stable: such a page can repeat or skip rows when the data or the
plan changes between pages. Errors: `table_not_found`, `invalid_page`.

### `GET {name}/export`

`memoire.databases.read`. Answers `200` with a consistent copy of the
database made with SQLite's online backup, `Content-Type:
application/vnd.sqlite3` and `Content-Disposition: attachment;
filename="<name>.sqlite"`. Writes may continue while it streams; the copy is
the state at one instant.

### `PUT {name}/import`

`memoire.databases.write`. The body is a SQLite database file
(`Content-Type: application/vnd.sqlite3`), at most 1 GiB. The target must
be active and empty (no user table, view, index or trigger). The gateway
checks the file with `PRAGMA integrity_check`, then replaces the empty
database with it in one step. Answers `200` with the database. Errors:
`database_not_empty`, `invalid_import`, `database_too_large`, `body_limit`.

### `GET {name}/snapshots`

`memoire.databases.read`. Persistent only. Newest first; snapshots are kept
7 days, so the list is short and fits one page. Answers `200`:

```json
{"items": [{"id": "snap_0192…", "created_at": "2026-11-19T06:00:00Z", "size_bytes": 8192, "reason": "scheduled"}],
 "next_cursor": null}
```

`reason` is `scheduled` (every 6 hours) or `pre_restore`. Errors:
`not_persistent`.

### `POST {name}/snapshots/{id}/restore`

`memoire.databases.snapshots`. Persistent only. No body. Takes a
`pre_restore` snapshot of the current contents, then replaces the database
with the snapshot; requests in flight finish on the old contents first.
Answers `200` with the database. Errors: `snapshot_not_found`,
`not_persistent`.

## Memory routes

These routes serve the shared memory of [memory.md](memory.md) under
`{name}/memory`. They run the same engine, on the same `memoire_memory_*`
tables, as the CLI's SQL convention over `POST {name}/query`, so both can
be used on one store. The model, the statuses, the ranking, the guards and
the limits are in memory.md; this section gives the HTTP form.

| Route | Permission | Answers |
| --- | --- | --- |
| `GET {name}/memory` | read | `200` the store |
| `POST {name}/memory/init` | write | `200` the store's migration |
| `POST {name}/memory/memories` | write | `201` the memory object (remember) |
| `GET {name}/memory/memories` | read | `200 {"memories"}` (recall) |
| `GET {name}/memory/memories/{id}` | read | `200 {"memory", "confirmations", "disputes"}` (show) |
| `POST {name}/memory/memories/{id}/revise` | write | `200` the memory object |
| `POST {name}/memory/memories/{id}/confirm` | write | `200` the memory object |
| `POST {name}/memory/memories/{id}/dispute` | write | `200` the memory object |
| `POST {name}/memory/memories/{id}/resolve` | write | `200` the memory object |
| `POST {name}/memory/memories/{id}/supersede` | write | `200 {"superseded", "by"}` |
| `POST {name}/memory/memories/{id}/forget` | write | `200` the memory object, or `{"id", "purged": true}` |
| `GET {name}/memory/events` | read | `200 {"events", "store_id", "after", "top", "matching"}` |
| `GET {name}/memory/export` | read | `200 {"memories", "next_after"}` |

- Admission: a read route admits a caller holding `memoire.memory.read` or
  `memoire.databases.read`, a write route one holding `memoire.memory.write`
  or `memoire.databases.write`. A key that may run SQL on a database can
  already do all of this, so keys minted before the memory permissions keep
  working. A key holding only `memoire.memory.*` gets `403 unauthorized` on
  every other database route. The refusals come in the usual order:
  `401 unauthenticated`, `403 tenant_required`, `403 unauthorized`, before
  the body is read.
- The database must exist and be active: `404 database_not_found`,
  `410 database_expired`, as for `query`. `init` creates the store's tables
  in an existing database; it never creates the database
  (`POST /v1/memoire/databases` does, with its own permission). Every
  memory request counts as a use of the database (`last_used_at`).
- Every route but `init` first checks the store's version, as the CLI does:
  `409 memory_not_initialized`, `memory_schema_older`, `memory_schema_newer`.
- `{id}` is a memory id: `mem_` and 26 Crockford base32 characters, in
  either case; answers carry it in upper case. Anything else answers
  `404 memory_not_found`, without `data`.
- Who writes: every write records the authenticated caller's subject as its
  principal (`key_…` for an API key, `usr_…` for a member signed in), never a
  value from the body. Every write body may carry `agent` (at most 64
  characters) and `session` (at most 128), without control characters,
  reported by the client and stored as given; both default to `""`. The
  author rules of memory.md (a confirmation counts from another principal or
  session) apply to the subject and the reported session.
- Request bodies are JSON objects; unknown fields, a wrong type or a bad
  syntax fail `422 validation_failed` with one detail per field
  (`/title` `required`, `/tags/2` `invalid`, `/add_tags` `not_allowed`…).
  An empty list is the same as an absent one. A query parameter that is not
  understood fails the same way, its path naming the parameter (`/limit`
  `out_of_range`, `/scope/1` `invalid` for the second `scope`).
- A failure of the store's own SQL that is not one of the memory's guards
  (its tables altered by hand, the database full, the writer busy) answers
  what `query` would: `sql_error` with its header, `database_too_large`,
  `sqlite_busy`, `statement_timeout`.

### The memory object

The memory routes return memories in this shape, the CLI's `--json` shape
(memory.md describes each field):

```json
{"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "rev": 2, "kind": "pitfall",
 "scope": "project:github.com/acme/widgets",
 "title": "Widgets deploys fail with a timeout unless the VPN is connected",
 "body": "", "tags": ["deploy"], "evidence": ["make deploy, run at 14:02 UTC"],
 "status": "confirmed", "supersedes": null, "superseded_by": null,
 "author": {"principal": "key_0192…", "agent": "claude-opus-5-5", "session": "s-41"},
 "confirmers": 1, "open_disputes": 0,
 "created_at": "2026-11-19T10:00:00.000Z", "updated_at": "2026-11-19T11:00:00.000Z"}
```

### Refusals and their `data`

| Code | `data` members |
| --- | --- |
| `memory_not_initialized` | none |
| `memory_schema_older`, `memory_schema_newer` | `store_version`, `supported_version` |
| `memory_conflict` | `id`, `current_rev`; `since_rev`, the rev a verdict was about, when its text changed after it |
| `memory_duplicate` | `candidates` (`[{"id", "title", "status", "similarity"}]`, closest first), `scope` |
| `memory_not_current` | `id`, `status`, `superseded_by` |
| `memory_no_open_dispute` | `id` |
| `memory_cycle` | `id`, `by` (the same id when a memory would supersede itself) |
| `memory_not_found` | `id`; `purged_at` when it was purged |
| `memory_self_confirm` | `id`, `without_session` (the author's session is `""`) |
| `memory_secret_detected` | `field`, `pattern`; the text is never echoed |
| `memory_too_large` | `field`, `limit`, `size` |
| `memory_unchanged` | `id` |

A field the engine refuses for its form answers `422 validation_failed`
with the field's path, like any other field problem: an empty title or
reason `required`, a multi-line title, an empty evidence item or a query
without a word `invalid`, a `rev` past the current one `out_of_range`. Only
the last carries `data`, with `id`, `rev` and `current_rev`:

```json
{"error": {"code": "validation_failed", "classification": "permanent",
           "details": [{"path": "/rev", "code": "out_of_range"}]},
 "data": {"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "rev": 4, "current_rev": 3}}
```

### `GET {name}/memory`

Read. Whether the database holds a store at this gateway's version. Answers
`200`:

```json
{"schema_version": 1, "supported_version": 1, "store_id": "mst_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8"}
```

Errors: `memory_not_initialized`, `memory_schema_older`,
`memory_schema_newer`.

### `POST {name}/memory/init`

Write. No body, or `{}`. Creates the store's tables, or applies the
versions it lacks; safe to repeat. Answers `200`:

```json
{"schema_version": 1, "migrated_from": 0, "store_id": "mst_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8"}
```

`migrated_from` is the version the store had: `0` for a new store, the
current version when there was nothing to do. Errors:
`memory_schema_newer`.

### `POST {name}/memory/memories`

Write. Remember: revision 1 of a new memory.

```json
{"title": "Widgets deploys fail with a timeout unless the VPN is connected",
 "body": "", "kind": "pitfall", "scope": "project:github.com/acme/widgets",
 "tags": ["deploy"], "evidence": ["make deploy, run at 14:02 UTC"],
 "force": false, "supersedes": null, "reason": null,
 "agent": "claude-opus-5-5", "session": "s-41"}
```

`title` is required. `scope` defaults to `global` and `kind` to `fact`;
with `supersedes` (a memory id) both default, with the tags, to the
replaced memory's. `supersedes` and `reason` each need the other. The
client chooses the scope: the CLI's default from a git checkout is its own.
Answers `201` with the memory object. Errors: `memory_duplicate`,
`memory_secret_detected`, `memory_too_large`; with `supersedes`, also
`memory_not_found`, `memory_not_current` and `memory_conflict`.

### `GET {name}/memory/memories`

Read. Recall. Query: `query` (plain words, optional), `scope`, `kind`,
`tag` and `status` (each repeatable), `limit` (1 to 100, default 10).
No `scope` searches every scope; no `status` means `inferred`, `confirmed`
and `disputed`. `kind` matches any of the given, `tag` all of them.
Answers `200`, best first with a query, else newest first:

```json
{"memories": [{"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "...": "...", "score": 2.84}]}
```

Each entry is the memory object; `score` is present only with a query.
A query without a word to search for fails `validation_failed`, path
`/query`.

### `GET {name}/memory/memories/{id}`

Read. Show. Query: `history` (`true` or `false`, default `false`).
Answers `200`:

```json
{"memory": {"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "...": "..."},
 "confirmations": [{"rev": 1, "principal": "key_0192…", "agent": "claude-opus-5-5",
                    "session": "s-42", "evidence": ["make deploy with the VPN up"],
                    "created_at": "2026-11-19T11:00:00.000Z", "counts": true}],
 "disputes": [{"rev": 2, "principal": "key_0193…", "agent": "codex", "session": "r-7",
               "reason": "the deploy passed without the VPN", "evidence": ["make deploy"],
               "created_at": "2026-11-19T12:00:00.000Z",
               "resolved_at": null, "resolution": null}]}
```

With `history=true` it adds `"revisions"`, every revision oldest first,
with the fields of memory.md's Revisions table. Every status is shown,
`forgotten` included. Errors: `memory_not_found`.

### `POST {name}/memory/memories/{id}/revise`

Write. `reason` (required) and at least one of `title`, `body`, `kind`,
`scope`, `tags`, `add_tags`, `evidence`, `add_evidence` (otherwise path `""`
`required`). `tags` replaces the list and `add_tags` appends to it; the two
exclude each other, and so do `evidence` and `add_evidence`. Answers `200`
with the memory object. Errors: `memory_conflict`, `memory_not_current`,
`memory_unchanged`, `memory_not_found`, `memory_secret_detected`,
`memory_too_large`.

### `POST {name}/memory/memories/{id}/confirm`

Write. `evidence` (at least one item) and `rev` (optional, a positive
integer: the revision the confirmer verified; default the current one).
Answers `200` with the memory object. Errors: `memory_conflict`,
`memory_self_confirm`, `memory_not_current`, `memory_not_found`,
`memory_secret_detected`, `memory_too_large`.

### `POST {name}/memory/memories/{id}/dispute`

Write. `reason` (required), `evidence` (optional), `rev` (as for
confirm). Answers `200` with the memory object. Errors: `memory_conflict`,
`memory_not_current`, `memory_not_found`, `memory_secret_detected`,
`memory_too_large`.

### `POST {name}/memory/memories/{id}/resolve`

Write. `reason` (required). Closes every open dispute. Answers `200` with
the memory object. Errors: `memory_no_open_dispute`, `memory_not_current`,
`memory_not_found`, `memory_secret_detected`, `memory_too_large`.

### `POST {name}/memory/memories/{id}/supersede`

Write. `by` (required, a memory id) and `reason` (required). Answers `200`:

```json
{"superseded": {"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "status": "superseded", "...": "..."},
 "by": {"id": "mem_01JD0RB2C3D4E5F6G7H8J9K0M1", "supersedes": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "...": "..."}}
```

Errors: `memory_cycle`, `memory_not_current`, `memory_conflict`,
`memory_not_found`, `memory_secret_detected`, `memory_too_large`.

### `POST {name}/memory/memories/{id}/forget`

Write. `reason` (required) and `purge` (default `false`). Without `purge`,
a tombstone: answers `200` with the memory object. With `purge: true`,
erases the memory (the reason is checked, not stored) and answers `200
{"id": "mem_…", "purged": true}`. Errors: `memory_not_current` (already
forgotten, without `purge`), `memory_conflict`, `memory_not_found`,
`memory_secret_detected`, `memory_too_large`.

### `GET {name}/memory/events`

Read. The log, newest first. Query: `scope` (repeatable; every scope
without it), `limit` (1 to 1 000, default 50), and where the read starts:

- nothing: at the first event;
- `since_seconds`: the events of that many seconds before the store's now;
- `after` and `store_id` together: a reader's cursor, the events after
  `after` when `store_id` is this store's, otherwise a first look;
- `first_look=true`: a reader with no cursor yet; the events of the last
  24 hours.

`since_seconds` combines with none of the other three, and `first_look`
with neither `after` nor `store_id`. Answers `200`:

```json
{"events": [{"seq": 41, "memory_id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "verb": "confirm",
             "rev": 2, "scope": "project:github.com/acme/widgets",
             "principal": "key_0192…", "agent": "claude-opus-5-5", "session": "s-42",
             "summary": "confirmed rev 2", "created_at": "2026-11-19T11:00:00.000Z"}],
 "store_id": "mst_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "after": 12, "top": 41, "matching": 1}
```

`after` is the `seq` the read started after, `top` the store's highest
`seq` whatever the filter, `matching` how many events pass the filter after
`after`, of which `events` holds the newest `limit`. A reader keeps
`store_id` and `top` as its next cursor; the gateway keeps none.

### `GET {name}/memory/export`

Read. The memories of every status but `forgotten`, in creation order, a
page at a time. Query: `scope` (repeatable), `after` (default 0) and
`limit` (1 to 500, default 500). Answers `200`:

```json
{"memories": [{"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "...": "..."}], "next_after": 12}
```

Pass `next_after` back as `after` for the next page; it is `null` when the
page is not full.

## Identity routes the clients use

These are Merezha identity's own routes, served by the gateway under
`/v1/identity/`; their full contract is Merezha's. The ones memoire's
clients need:

| Need | Route |
| --- | --- |
| Register | `POST /v1/identity/registrations` `{email, password}` → `{"status": "pending_verification"}`; the mailed token goes to `POST /v1/identity/email-verifications/confirm` `{token, transport?}` → `201 {session, user, csrf_token}` |
| Sign in, sign out | `POST /v1/identity/sessions` `{email, password, transport?}` → `201 {session, user, csrf_token}`, `DELETE /v1/identity/session` |
| Current user | `GET /v1/identity/me` |
| Teams | `POST /v1/identity/organizations` `{name, slug}`, `GET /v1/identity/organizations` |
| Switch team | `PUT /v1/identity/session/organization` `{organization_id}` |
| Invite and join | `POST /v1/identity/organizations/{org}/invitations` `{email, role_ids}`; the invitee opens the mailed token and the client calls `POST /v1/identity/invitations/inspect` `{token}`, then `POST /v1/identity/invitations/accept` `{token, transport?}`. Signed in with the invited address, accept adds the membership. Anonymous, it creates a verified account with a session when the address has none (or only an unverified one), and answers `401 sign_in_required` when a verified account exists: sign in, then retry with the same token. Signed in with another address it answers `403 email_mismatch`. |
| Machines | `GET /v1/identity/organizations/{org}/service-accounts`; `POST` there `{name, description?}` → `201 {service_account: {id, …}}` |
| Machine roles | `GET /v1/identity/organizations/{org}/roles` → `{items: [{id, key, …}]}`; `POST /v1/identity/organizations/{org}/role-assignments` `{service_account_id, role_id}` → `201`. A machine holds no permission until it has a role; its keys carry a subset of the role's. |
| API keys | `GET`, `POST /v1/identity/organizations/{org}/api-keys` `{name, permissions, expires_at, service_account_id?}` → `201 {api_key, key}`; `DELETE …/api-keys/{key_id}` |

`transport: "bearer"` returns the session token once, in `session.token`;
send it as `Authorization: Bearer mzs_…`. The default `cookie` transport
sets the session cookie named in the authentication table instead.

Requests without a credential to identity's routes (register, sign in,
verify, accept an invitation) must carry an `Origin` header equal to the
gateway's application origin (`modules.identity.app_origin`), otherwise
they fail `403 origin_rejected`. Browsers send it on their own. Bearer
requests are not Origin-checked.

Creating a machine, giving it a role and creating its key need
`identity.service_accounts.manage`, `identity.roles.assign` and a recent
sign-in, which an API key is not. The team roles `admin` and `owner` hold
both permissions and `member` does not. `memoire keys create --name MACHINE
[--team org_…] [--expires 90d] [--read-only]` therefore:

1. Picks the team: `--team`, else the `team_id` of the stored login key
   (`whoami`), else fails asking for `--team`.
2. Signs in with the member's email and password, read from the terminal,
   never argv: `POST /v1/identity/sessions` with `transport: "bearer"` and
   `Origin` set to the origin of the stored `--server` URL. That URL is
   therefore the public origin the web is served from, which is the
   application origin in a deployment.
3. Sends `Merezha-Organization` for that team, creates the service account,
   assigns it the team role `member` (found by `key` among the team's
   roles), then the key: the five `memoire.databases.*` permissions (only
   `memoire.databases.read` with `--read-only`) and `expires_at` = now plus
   `--expires` (default 90 days; identity refuses more than 365 days with
   `422 validation_failed`, path `/expires_at`).
4. Prints the key once (it is returned only in that `201` response) and
   signs the temporary session out (`DELETE /v1/identity/session`).

In development the gateway captures mail instead of sending it:
`GET /v1/notifications/dev/captures` lists verification and invitation
messages.

## Health

Unauthenticated, cheap, no body worth parsing:

- `GET /health/live`: `200` while the process runs.
- `GET /health/startup`: `200` once startup finished.
- `GET /health/ready`: `200` when the gateway can serve (PostgreSQL reachable,
  data directory present); `503` otherwise.
