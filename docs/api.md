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
  teams `org_…`, users `usr_…`. Do not parse them.
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
  the team. memoire adds the five permissions below to identity's catalog;
  the team roles `admin` and `member` hold all five by default, and an API
  key may carry a subset (a read-only agent key holds only
  `memoire.databases.read`). The permissions are registered together with
  the database routes; until they are, identity refuses them in a key
  request (`422 unknown_permission`).

| Permission | Allows |
| --- | --- |
| `memoire.databases.read` | list, detail, read queries, schema, rows, export, snapshot list |
| `memoire.databases.write` | write queries, extend, import |
| `memoire.databases.create` | create |
| `memoire.databases.delete` | delete |
| `memoire.databases.snapshots` | snapshot restore |

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

- `sql_error` adds `sql`: which statement SQLite refused and SQLite's
  message about it (at most 512 bytes, never a file path). Merezha's shared
  error builder has no such member, so the gateway writes this body itself,
  with the same envelope, status and headers as every other error.

  ```json
  {"error": {"code": "sql_error", "classification": "permanent",
             "sql": {"statement": 1, "sqlite_code": "SQLITE_CONSTRAINT",
                     "message": "UNIQUE constraint failed: users.email"}}}
  ```

memoire's codes:

| Code | Status | When |
| --- | --- | --- |
| `statement_denied` | 403 | The authorizer refused the statement: `ATTACH`, `DETACH`, `load_extension`, `VACUUM INTO` or a file-touching `PRAGMA` |
| `database_not_found` | 404 | No database of that name in the team |
| `table_not_found` | 404 | No table or view of that name in the database |
| `snapshot_not_found` | 404 | No snapshot of that id for the database |
| `database_exists` | 409 | Create with a name an active database of the team holds |
| `database_not_empty` | 409 | Import into a database that has a user table, view, index or trigger |
| `not_burstable` | 409 | Extend on a persistent database |
| `not_persistent` | 409 | A snapshot route on a burstable database |
| `database_expired` | 410 | Query, schema, rows, export, import, extend or snapshots on an expired database |
| `database_too_large` | 413 | A write or import would pass the 1 GiB file limit |
| `database_limit` | 422 | Create when the team already has 50 active databases |
| `read_only` | 422 | A statement in `read` mode tried to write |
| `sql_error` | 422 | SQLite refused a statement (syntax, constraint, missing table…); see `sql` |
| `statement_timeout` | 422 | A statement ran past its deadline and was interrupted |
| `result_too_large` | 422 | A statement returned more than 10 000 rows, or the response passed 16 MiB |
| `invalid_import` | 422 | The import body is not a SQLite database or fails `PRAGMA integrity_check` |
| `sqlite_busy` | 503 | The writer stayed locked past the busy timeout; transient |

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
| `last_used_at` | Last query, import or extend; `null` until the first. Updated in memory and flushed about every minute, so it may lag by that much. |
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
expired.

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
- `last_insert_rowid` is set only for an INSERT or REPLACE into a rowid
  table with `rows_affected` > 0: `sqlite3_last_insert_rowid` read right
  after the statement, the rowid of its last inserted row, in the value
  encoding. It is `null` otherwise, including inserts into `WITHOUT ROWID`
  tables and inserts that `OR IGNORE` skipped.

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
| Machines | `GET`, `POST /v1/identity/organizations/{org}/service-accounts` `{name, description?}` |
| API keys | `GET`, `POST /v1/identity/organizations/{org}/api-keys` `{name, permissions, expires_at, service_account_id?}` → `201 {api_key, key}`; `DELETE …/api-keys/{key_id}` |

`transport: "bearer"` returns the session token once, in `session.token`;
send it as `Authorization: Bearer mzs_…`. The default `cookie` transport
sets `__Host-merezha_session` instead.

Requests without a credential to identity's routes (register, sign in,
verify, accept an invitation) must carry an `Origin` header equal to the
gateway's application origin (`modules.identity.app_origin`), otherwise
they fail `403 origin_rejected`. Browsers send it on their own. Bearer
requests are not Origin-checked.

Creating a machine and its key needs `identity.service_accounts.manage` and a
recent sign-in, which an API key is not. `memoire keys create --name MACHINE
[--team org_…] [--expires 90d] [--read-only]` therefore:

1. Picks the team: `--team`, else the `team_id` of the stored login key
   (`whoami`), else fails asking for `--team`.
2. Signs in with the member's email and password, read from the terminal,
   never argv: `POST /v1/identity/sessions` with `transport: "bearer"` and
   `Origin` set to the origin of the stored `--server` URL. That URL is
   therefore the public origin the web is served from, which is the
   application origin in a deployment.
3. Sends `Merezha-Organization` for that team, creates the service account,
   then the key: the five memoire permissions (only
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
