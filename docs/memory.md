# memoire memory

`memoire memory` keeps a shared memory for a fleet of agents in an ordinary
memoire database. Agents record what they verified, recall it before they
work, and confirm, dispute, revise or retire what others wrote. Every change
is a new revision, so the history stays readable.

The memory is versioned SQL in the database. The CLI calls the gateway's
memory API, under `…/{name}/memory` (see [api.md](api.md#memory-routes)),
and the gateway runs each verb. Against a gateway without that API, which
answers `404 not_found` for its routes, the CLI runs the same verb itself
and sends its read and write batches over the query route of
[api.md](api.md). Both paths run the same engine on the same tables, so a
store can be used through either. `MEMOIRE_MEMORY_SQL=1` makes the CLI take
the query route without asking. Output, error codes and exit codes are the
same on both paths, with two exceptions: the permission order of
[Read-only keys](#read-only-keys), and an `sql_error` of the store's own SQL,
which the API reports without SQLite's message. Any database can hold a store, and every table of a
store starts with `memoire_memory_`, so a store can share a database with an
application's own tables without touching them.

It has three uses:

- A company's memory for its projects and clients: decisions, procedures,
  pitfalls and preferences that outlive one session or one agent.
- Shared state for an agent workload: agents of one run pass work to each
  other as memories instead of files and handoff documents.
- An application keeps using the database that holds a store. The store
  touches only its own tables.

Agents learn the rules from the skill that `memoire skill install` writes
(see [The skill](#the-skill)).

## Quick start

```sh
export MEMOIRE_AGENT=claude-opus-5-5 MEMOIRE_SESSION=s-41
memoire memory init
memoire memory remember --kind pitfall --tag deploy \
  --title "Widgets deploys fail with a timeout unless the VPN is connected" \
  --evidence "make deploy, run at 14:02 UTC"
memoire memory recall "deploy vpn"
MEMOIRE_SESSION=s-42 memoire memory confirm mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8 --evidence "make deploy with the VPN up"
```

The CLI signs in as for every other command (see [cli.md](cli.md#signing-in)).
Every verb takes `--json`; agents should always pass it.

## Stores and topology

A store lives in one database. The default database is `memory`; `--db NAME`
or `MEMOIRE_MEMORY_DB` picks another. `--db`, `--agent`, `--session` and
`--json` go before or after the verb.

`memoire memory init` creates the database when it does not exist, persistent
unless `--burstable` is given (the opposite of `db create`), then creates or
migrates the store's tables. A persistent store lives until deleted and is
snapshotted every 6 hours, with snapshots kept 7 days. A burstable store
expires after its idle TTL (24 hours by default, `--ttl` to change it) and at
most 7 days after creation, and has no backups.

The CLI does not prescribe a layout. Some that work:

- One company store, `memory`, with scopes for each project and client.
- One store per client (`--db acme-memory`), so a key's access to a database
  decides who reads that client's memory.
- One burstable store per run (`--db run-42 init --burstable`) for a
  multi-agent workload, beside a persistent store for what should outlive it.

## Records

A memory has a stable id, `mem_` and a 26-character ULID in Crockford base32
(48 bits of milliseconds, 80 random bits). The CLI generates it, so ids sort
by creation time. Verbs accept the id in either case.

### Revisions

A memory's history is a list of revisions numbered from 1. A change writes
revision N + 1 and never edits an earlier one; only a purge deletes them. Every
revision holds:

| Field | Holds |
| --- | --- |
| `memory_id`, `rev` | The memory and the revision number |
| `kind`, `scope`, `title`, `body`, `tags`, `evidence` | The content after this revision |
| `status` | The status right after this revision |
| `supersedes`, `superseded_by` | Links to other memories, or `null` |
| `author_principal`, `author_agent`, `author_session` | Who wrote it |
| `reason` | Why: `remembered` for revision 1, or the `--reason` of a `remember --supersedes`; else the verb's `--reason`; or `purged` once a purge erased the memory this revision linked to |
| `created_at` | When, on the gateway host's clock, to the millisecond |

`remember` (and, with `--supersedes`, a revision of the memory it
replaces), `revise`, `supersede` (on both memories) and `forget` write a
revision. `confirm`, `dispute` and `resolve` do not.

A `revise` takes the old text out of search at once, but the old revision
stays in the history by design, and `show --history` prints it. Revising a
secret away does not remove it. Only `forget --purge` erases it (see
[Forgetting and purging](#forgetting-and-purging)).

### Scope and kind

A scope labels where a memory applies. It matches
`^[a-z0-9][a-z0-9._:/-]{0,127}$`. The documented conventions, which the CLI
does not enforce:

| Scope | For |
| --- | --- |
| `project:<host>/<path>` | One repository |
| `client:<name>` | One client |
| `org:<name>` | The whole organization |
| `global` | Anything else |
| `run:<id>` | One workload |

Without `--scope`, `remember` uses the project scope of the git checkout it
runs in, else `global`; with `--supersedes`, the scope of the memory it
replaces. The project scope is the host and the path of `git
remote get-url origin`, lowercased, without user, port or a trailing `.git`.
Every clone gets the same scope, over SSH or HTTPS:

| `origin` | Scope |
| --- | --- |
| `git@github.com:Acme/Widgets.git` | `project:github.com/acme/widgets` |
| `https://gitlab.example.com:8443/group/sub/repo.git` | `project:gitlab.example.com/group/sub/repo` |

A checkout without `origin`, or with a local path or `file://` remote, has no
project scope.

A kind matches `^[a-z][a-z0-9-]{1,31}$` and defaults to `fact` (with
`remember --supersedes`, to the replaced memory's kind). The documented
kinds are `fact`, `decision`, `procedure`, `preference`, `pitfall` and
`handoff`.

### Tags and evidence

Tags match `^[a-z0-9][a-z0-9._-]{0,47}$`. Evidence items are free text, such as
the command that was run, a file and line, a URL or a commit. Both keep their order
and drop repeats.

### Limits

| Field | Limit | Over the limit |
| --- | --- | --- |
| title | one line, trimmed, 1 to 120 characters | `memory_too_large` (7), exit 2 when empty or several lines |
| body | 8 KiB (8192 bytes of UTF-8), may be empty | `memory_too_large` (7) |
| tags | 10 per memory, each 1 to 48 characters | `memory_too_large` (7), exit 2 for bad syntax |
| evidence | 10 items, each 1 to 500 characters | `memory_too_large` (7), exit 2 for an empty item |
| reason | trimmed, 1 to 500 characters | `memory_too_large` (7), exit 2 when empty |
| scope | 1 to 128 characters | exit 2 |
| kind | 2 to 32 characters | exit 2 |
| `--agent` | 64 characters, no control character | exit 2 |
| `--session` | 128 characters, no control character | exit 2 |

`memory_too_large` names the `field` and its `limit`.

## Statuses and trust

| Status | Means | In recall by default |
| --- | --- | --- |
| `inferred` | Written, not independently confirmed | yes |
| `confirmed` | At least one confirmation that counts | yes |
| `disputed` | At least one open dispute | yes, flagged |
| `superseded` | Replaced by another memory | no |
| `forgotten` | A tombstone | no |

Confirmations and disputes are rows of their own, attached to the revision
whose text their writer verified: `--rev`, else the current one. The status is derived from those rows in SQL inside each
write's transaction, in this order: forgotten, superseded, an open dispute,
a confirmation that counts, else inferred. The CLI never computes it.

The content revision is the latest revision that changed the title, the body
or the kind. Its author's principal and session are the content author. A
confirmation counts when it was made on the content revision or later, and
its principal and session are not both the content author's. So:

- A new title, body or kind resets the status to `inferred`, or keeps it
  `disputed` while a dispute is open, because the old confirmations covered
  the old text. A change of scope, tags or evidence keeps them.
- `confirm` refuses the content author's own principal and session
  (`memory_self_confirm`). The check uses the content author, not the author
  of the latest revision, so retagging someone's text does not let them
  confirm it themselves.
- A confirmation does not close a dispute. `resolve` does.

`confirmers` is the number of distinct (principal, session) pairs among the
confirmations that count. The same pair confirming twice counts once.

### Who wrote it

The principal is the subject of the key the CLI uses (`key_…` for an API
key), never a flag. Through the memory API the gateway takes it from the
credential of the request. On the SQL path the CLI asks
`GET /v1/memoire/whoami` once per write and sends it in the batch. The agent
(`--agent`, `MEMOIRE_AGENT`) and the session (`--session`,
`MEMOIRE_SESSION`) are reported by the client and stored as given, empty
when unset, on both paths.

So authorship is as honest as the keys and the clients that hold them. Agents
that share one key are told apart only by the sessions they report, and
an agent that reports another session can confirm its own text. When it
matters who confirmed what, give each agent or machine its own key
(`memoire keys create`). Without `MEMOIRE_SESSION`, every run of one key is
the same writer and cannot confirm text an earlier run wrote.

Through the memory API, the principal and the guards are the gateway's: the
client cannot change them. The session is still the client's word, so a key
can still present another session than its own. On the SQL path the client
sends the batches, so everything is as honest as the client: a key that may
write SQL can write any row of the store's tables.

## Verbs

Global options: `--db NAME`, `--agent NAME`, `--session ID`, `--json`. With
`--json`, each verb prints one JSON value on standard output. Without it,
verbs print the memory's fields or a table. The recall table has a `REV` column for
`--rev`, and `show --history` prints each revision's reason, title and body.

Every verb first checks the store's schema version (see
[Schema and migrations](#schema-and-migrations)). Every verb but `init` can
also fail with `memory_not_initialized`, `memory_schema_older`,
`memory_schema_newer`, exit 5 `database_not_found`, exit 4 `unauthorized`
for a write with a read-only key, and exit 8 when the store is busy.

### The memory object

Every verb that prints a memory prints this object:

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

`author` is the latest revision's author, and `rev` and `author` change only
with a revision. `updated_at` is the last time anything was written about the
memory: a revision, a confirmation, a dispute or a resolution.

### init

```sh
memoire memory init [--burstable [--ttl DURATION] | --persistent]
```

Creates the database when it is missing or expired, persistent by default,
then applies the missing schema versions. A database that exists is kept;
when its kind differs from the flag, a note goes to standard error. Safe to
repeat. An expired database is created again, as a new store.

```json
{"database": {"name": "memory", "kind": "persistent", "state": "active", …},
 "created": true, "schema_version": 1, "migrated_from": 0,
 "store_id": "mst_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8"}
```

`database` is the API's database object. `migrated_from` is the version the
store had, `0` for a new store and `1` when it was already current.

### remember

```sh
memoire memory remember --title TITLE [--body BODY | --body-file FILE] [--kind KIND] \
  [--scope SCOPE] [--tag TAG]... [--evidence TEXT]... [--force] \
  [--supersedes OLD_ID --reason REASON]
```

Records revision 1 of a new memory, `inferred`. `--body-file -` reads the
body from standard input; nothing else reads it, so an open standard input
never blocks a verb. Unless `--force`, a memory with a similar title in the
same scope refuses it (see [Near-duplicates](#near-duplicates)).

With `--supersedes OLD_ID`, the new memory replaces `OLD_ID` in the same
write, as `supersede OLD_ID --by NEW_ID` would, so both are never current
at once. `--supersedes` and `--reason` each need the other (exit 2
otherwise). `OLD_ID` must be current; a disputed memory may be replaced.

- The new memory takes `OLD_ID`'s scope, kind and tags unless `--scope`,
  `--kind` or `--tag` gives them. Any `--tag` replaces the whole list.
- Its title, body and evidence are its own: no `--body` is an empty body, no
  `--evidence` is none. It starts `inferred`; confirmations of `OLD_ID` do
  not carry over.
- `OLD_ID` gets a revision, `superseded` with `superseded_by` set, and leaves
  recall. The reason is that revision's `reason` and the `reason` of the new
  memory's revision 1.
- The near-duplicate check skips `OLD_ID`, so a title close to its title
  needs no `--force`.
- The log gets a `remember` event on the new memory, then a `supersede` event
  on `OLD_ID`.

Prints the memory object, the new memory with `supersedes` set. Errors:
`memory_duplicate`, `memory_secret_detected`, `memory_too_large`; with
`--supersedes`, also `memory_not_found` (for a purged id, its message says
when it was purged), `memory_not_current` (`OLD_ID` superseded or
forgotten; `superseded_by` says by what) and `memory_conflict` (see
[Concurrency](#concurrency)).

### show

```sh
memoire memory show ID [--history]
```

```json
{"memory": {…},
 "confirmations": [{"rev": 1, "principal": "key_0192…", "agent": "claude-opus-5-5",
                    "session": "s-42", "evidence": ["make deploy with the VPN up"],
                    "created_at": "2026-11-19T11:00:00.000Z", "counts": true}],
 "disputes": [{"rev": 2, "principal": "key_0193…", "agent": "codex", "session": "r-7",
               "reason": "the deploy passed without the VPN", "evidence": ["make deploy"],
               "created_at": "2026-11-19T12:00:00.000Z",
               "resolved_at": null, "resolution": null}]}
```

`counts` says whether a confirmation counts for the current text.
`--history` adds `"revisions"`, every revision with the fields listed in
[Revisions](#revisions), oldest first. Shows every status, `forgotten`
included. Error: `memory_not_found`. For a purged id, its message says when
it was purged.

### revise

```sh
memoire memory revise ID --reason REASON [--title TITLE] [--body BODY | --body-file FILE] \
  [--kind KIND] [--scope SCOPE] [--tag TAG... | --add-tag TAG...] \
  [--evidence TEXT... | --add-evidence TEXT...]
```

Writes a new revision in which only the given fields change. `--tag` and
`--evidence` replace the list; `--add-tag` and `--add-evidence` append to it.
At least one field is required. A new title, body or kind makes this
revision the content revision (see [Statuses and trust](#statuses-and-trust)).
`revise` runs no near-duplicate check.

Prints the memory object. Errors: `memory_conflict`, `memory_not_current`
(superseded or forgotten), `memory_unchanged` (the result equals the current
revision), `memory_not_found`, `memory_secret_detected`, `memory_too_large`.

### recall

```sh
memoire memory recall [QUERY] [--scope SCOPE... | --all-scopes] [--kind KIND]... \
  [--tag TAG]... [--status STATUS]... [--limit N]
```

Searches memories, best first (see [Recall and ranking](#recall-and-ranking)),
or lists the newest without a query.

- Scopes: the given ones; by default the project scope of the current git
  checkout, when there is one, and `global`; `--all-scopes` searches all.
- `--kind` repeated: any of them. `--tag` repeated: all of them.
- `--status` replaces the default set `inferred`, `confirmed`, `disputed`
  and may name `superseded` and `forgotten`.
- `--limit` takes 1 to 100, default 10.

```json
{"db": "memory", "query": "deploy vpn",
 "scopes": ["project:github.com/acme/widgets", "global"],
 "memories": [{"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", …, "score": 2.84}]}
```

Each entry is the memory object with a `score`, present only with a query.
`scopes` is what was searched, `null` with `--all-scopes`. A query with no
word to search for exits 2 with `invalid_query`.

### confirm

```sh
memoire memory confirm ID --evidence TEXT... [--rev N]
```

Records a confirmation, with what checked it. At least one `--evidence` is
required. `--rev` is the revision the confirmer read and verified, the `rev`
of the recall or show output; without it, the revision the CLI reads. When
the title, body or kind changed after that revision, `confirm` exits 6 with
`memory_conflict` and records nothing, so a confirmation never lands on text
its author did not read. A later revision of only scope, tags or evidence
does not refuse it. A `--rev` above the current revision exits 2 with
`invalid_argument`. The confirmation is recorded on that revision, and its
event's summary is `confirmed rev N` for it. The write also refuses a memory
that was superseded or forgotten after the CLI read it, with
`memory_not_current`.

A confirmation that counts now stops counting when the text is revised
later (see [Statuses and trust](#statuses-and-trust)).

Prints the memory object. Errors: `memory_conflict`, `memory_self_confirm`,
`memory_not_current` (superseded or forgotten), `memory_not_found`,
`memory_secret_detected`.

### dispute

```sh
memoire memory dispute ID --reason REASON [--evidence TEXT]... [--rev N]
```

Opens a dispute. `--rev` works as for `confirm`: when the text changed after
the revision the disputer read, `dispute` exits 6 with `memory_conflict` and
records nothing. The dispute is recorded on the revision it names. The
memory becomes `disputed` and stays in recall, flagged, until `resolve`.
Works on a superseded memory, not on a forgotten one, and the write refuses a
memory forgotten after the CLI read it with `memory_not_current`.

Prints the memory object. Errors: `memory_conflict`, `memory_not_current`
(forgotten), `memory_not_found`, `memory_secret_detected`.

### resolve

```sh
memoire memory resolve ID --reason REASON
```

Closes every open dispute of the memory with the reason as their
resolution. The status then follows the rows again, `confirmed` when a
confirmation counts, else `inferred`. The write refuses a memory forgotten
after the CLI read it with `memory_not_current`, and one whose disputes were
closed meanwhile with `memory_no_open_dispute`.

Prints the memory object. Errors: `memory_no_open_dispute`,
`memory_not_current` (forgotten), `memory_not_found`.

### supersede

```sh
memoire memory supersede ID --by NEW_ID --reason REASON
```

Marks `ID` as replaced by `NEW_ID`, two memories that both exist already.
Both get a revision. `ID` becomes `superseded` with `superseded_by` set and
leaves recall; `NEW_ID` keeps its status and gets `supersedes` set, the
memory it replaced most recently. To write a replacement and retire the old
memory in one step, use `remember --supersedes ID --reason REASON` (see
[remember](#remember)).

```json
{"superseded": {"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "status": "superseded", …},
 "by": {"id": "mem_01JD0RB2C3D4E5F6G7H8J9K0M1", "supersedes": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", …}}
```

Errors: `memory_cycle` (a memory cannot supersede itself, and `NEW_ID` must
not already be replaced by `ID`, directly or through a chain of any length),
`memory_not_current` (`ID` superseded or forgotten, or `NEW_ID` forgotten),
`memory_conflict`, `memory_not_found`.

The cycle check runs again inside the write transaction, so two supersedes
running at once cannot close a cycle between them. When the write fails, the
CLI reads both memories again and reports what is true then: `memory_cycle`,
`memory_not_current`, `memory_not_found` or `memory_conflict`.

### forget

```sh
memoire memory forget ID --reason REASON [--purge]
```

Without `--purge`, writes a `forgotten` revision, a tombstone. Recall hides
it unless `--status forgotten`; `show` and `log` still have it. It prints the
memory object. Errors: `memory_not_current` (already forgotten),
`memory_conflict`, `memory_not_found`.

With `--purge`, erases the memory, whatever its status. `--reason` is still
required, checked and scanned, but not stored. It prints:

```json
{"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "purged": true}
```

See [Forgetting and purging](#forgetting-and-purging).

### log

```sh
memoire memory log [--since DURATION | --unseen [--reader NAME]] [--scope SCOPE]... [--limit N]
```

What happened in the store, newest first; every scope unless `--scope`.
`--limit` takes 1 to 1000, default 50. `--since 24h` counts back from the store's
clock.

```json
{"events": [{"seq": 41, "memory_id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8", "verb": "confirm",
             "rev": 2, "scope": "project:github.com/acme/widgets",
             "principal": "key_0192…", "agent": "claude-opus-5-5", "session": "s-42",
             "summary": "confirmed rev 2", "created_at": "2026-11-19T11:00:00.000Z"}]}
```

| `verb` | `summary` |
| --- | --- |
| `remember` | The title |
| `revise`, `dispute`, `resolve`, `forget` | The reason |
| `confirm` | `confirmed rev N` |
| `supersede` | `by NEW_ID: reason`, on the replaced memory, from `supersede` or `remember --supersedes`; `by NEW_ID` once `NEW_ID` is purged |
| `purge` | `null`; `rev` and `scope` are `null` too |

`--unseen` shows what this reader has not seen yet and adds an `unseen`
object; `--reader NAME` (or `MEMOIRE_READER`) names a long-lived reader and
matters only with `--unseen`. Both are described in
[The log and cursors](#the-log-and-cursors).

### export

```sh
memoire memory export [--scope SCOPE]... -o FILE
```

Writes JSON Lines, one memory object per line, every status but `forgotten`,
in creation order. It writes a private temporary file beside `FILE` and
renames it over `FILE` once complete. Revisions, confirmations and disputes
are not exported.

```json
{"path": "memories.jsonl", "count": 12, "scopes": null}
```

## Recall and ranking

The query is plain words. The CLI lowercases it, cuts it at every character
that is not a letter or a digit, drops repeats and keeps the first 32 words.
Each word is quoted, and the words are joined with `OR`. Any of them may
match, and a memory that matches more of them ranks higher. Quotes,
`AND`, `NOT`, `NEAR`, `*` and column filters have no meaning; they are words
or nothing.

The index is an FTS5 table over the current title, body, tags and scope,
with the tokenizer `porter unicode61 remove_diacritics 2`. It stems words
(English), folds case and removes diacritics, so `deploying` finds
`deploys` and `cafe` finds `café`.

The score is computed in one SQL statement on the store's clock:

```text
relevance = -bm25(title 4.0, body 1.0, tags 2.0, scope 0.5)
trust     = 1 + 0.15 * min(confirmers, 4)   when confirmed
            0.5                              when disputed
            1.0                              otherwise
recency   = 0.75 + 0.25 / (1 + age_days / 90)
score     = relevance * trust * recency
```

`age_days` is the time since `updated_at`, never below 0. A confirmation,
a dispute and a resolution move `updated_at` as a revision does, so a memory
that was just re-verified is fresh again. Ties break on `updated_at`, newest
first, then on `id`. Without a query, recall lists the newest by `updated_at`,
then `id`, so recently confirmed or disputed memories come first.

On a small store, scores can be tiny. bm25 gives a term almost no weight
when it appears in more than half of the memories, so in a store of a few
memories that share their words, the order then follows trust and recency
more than the words.

## Near-duplicates

`remember` without `--force` compares the new title with the titles in the
same scope:

1. Title tokens: the query words of the title without common English
   stopwords (`the`, `to`, `is`, …), with a plural `s` dropped from words of
   more than three letters that do not end in `ss`. A title of stopwords only
   skips the check.
2. Candidates: the 20 best memories of the same scope, not superseded or
   forgotten, whose title matches any token. With `--supersedes`, the memory
   being replaced is never a candidate.
3. Similarity: the Dice coefficient of the two token sets,
   `2 * |A ∩ B| / (|A| + |B|)`. At 0.65 or more, the candidate is a hit.

Any hit refuses the write with `memory_duplicate`, exit 6, and lists the hits,
most similar first:

```json
{"error": {"code": "memory_duplicate", "classification": "conflict",
           "message": "a memory in global has a similar title (mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8); revise it, or pass --force to record this one too",
           "candidates": [{"id": "mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8",
                           "title": "Deploys require the VPN to be connected",
                           "status": "inferred", "similarity": 0.75}]}}
```

The check compares words, not meaning. Titles built on one sentence with a
different subject collide: "Staging database password rotates every 90
days" and "Production database password rotates every 90 days" score 0.86.
Pass `--force` when the new memory is a different fact. The check also sees
one scope only, and two agents that remember the same title at the same
moment both pass it.

## Concurrency

Nothing takes a lock. Each verb reads what it needs, then runs one write
batch as one transaction on the database's single writer, so writes are
serialized and a batch applies whole or not at all. Through the memory API
the gateway runs those batches and guards; on the SQL path the CLI sends the
same ones over the query route.

A verb that writes a revision reads revision N and inserts N + 1 only if the
memory is still at N. When another writer got there first, the insert fails,
nothing of the batch applies, and the CLI exits 6 with `memory_conflict`,
naming the `id` and the `current_rev`. Read the memory again with `show`,
and apply the change again to the new revision if it still makes sense. The
CLI never retries by itself.

`confirm`, `dispute` and `resolve` write no revision. Inside the write
transaction they check that the memory is still as the CLI read it:

- `confirm` and `dispute`: the text (title, body or kind) has not changed
  since the revision they name with `--rev` or that the CLI read, else
  `memory_conflict`. Show the memory again and verify the new text before you
  confirm or dispute it.
- `confirm`: the memory is neither superseded nor forgotten. `dispute` and
  `resolve`: it is not forgotten. Else `memory_not_current`.
- `resolve`: an open dispute remains, else `memory_no_open_dispute`.

A write that finds the memory purged in between exits 5 with
`memory_not_found` and leaves no row behind.

A revision never writes back the links of a stale read. It carries the
`supersedes` and `superseded_by` that the memory's row has when the write
runs, so a link to a memory purged in between does not come back.

`remember --supersedes` writes the old memory's revision first, guarded on
the rev it read. When the old memory was revised, superseded, forgotten or
purged in between, nothing of the batch applies, the new memory included,
and the CLI reports what is true now: `memory_not_found`,
`memory_not_current`, or `memory_conflict` with the old memory's `id` and
`current_rev`. Read it again and decide whether the replacement still
applies.

Two `supersede` runs cannot close a cycle (see [supersede](#supersede)), and
two `forget --purge` runs of one memory leave one purge event: the second
exits 5 with `memory_not_found`, saying when the first purged it.

A writer kept busy past the gateway's busy timeout answers `sqlite_busy`
(503), exit 8. Try again later.

## The log and cursors

Every write appends one event to `memoire_memory_events`, numbered by an
increasing `seq`. `log --unseen` shows the events after the reader's
cursor, then moves the cursor once they are printed:

```json
{"events": [ … ], "unseen": {"reader": "session:s-41", "first": false,
                             "after": 12, "through": 40, "skipped": 0}}
```

- The reader is, in this order: `name:NAME` when `--reader NAME` is given or
  `MEMOIRE_READER` is set, for a role that runs as many sessions (a nightly
  reviewer, for example) and continues where its last run stopped;
  `session:ID` when the session (`--session`, `MEMOIRE_SESSION`) is set and
  not empty; else `machine`. `NAME` has the syntax of a tag; another value
  exits 2. An empty `MEMOIRE_READER` counts as unset. Without `--unseen`,
  the reader is ignored, whatever it holds.
- There is one cursor per server URL, database, scope filter (the sorted
  `--scope` list) and reader. Sessions on one machine each have their own
  cursor, so one does not consume another's news. A filtered read keeps its
  own cursor, so it never hides other scopes' events from a later unfiltered
  read.
- A reader with no cursor for the store has a first look (`first: true`;
  without `--json`, a note on standard error says so). It starts at the
  events of the last 24 hours on the store's clock: `after` is the highest
  `seq` among older events, 0 when there is none. This takes the events'
  times to follow their `seq`, as they do while the gateway host's clock does
  not step backwards. It shows at most `--limit` of them, like any read.
  `log --since 72h` reads older events.
- `after` is where the read started: the cursor, or the first look's start.
  The read returns the newest `--limit` events after it, and `skipped` counts
  the older ones beyond the limit, which will not be shown as unseen again.
- Once the events are printed, the cursor moves to the highest `seq` in the
  store at the time of the read, `through`, whatever the scope filter. A
  first look that shows nothing still leaves a cursor, and the next read
  shows only newer events. Within one store a cursor never moves backwards,
  so of two runs at once, the one that read less does not undo the other.
  Two runs of one reader at the same moment may both show the same events:
  the cursor is not a queue.
- Each cursor remembers the store's `store_id`. When the database is deleted
  and created again under the same name, the new store has a new id and the
  reader gets a first look again.
- The cursors file is `memory-cursors.json` in the CLI's configuration
  directory, beside the credentials (`~/.config/memoire/` by default,
  `MEMOIRE_CONFIG_DIR` or `$XDG_CONFIG_HOME/memoire` to move it). The
  directory is mode 0700 and the file 0600, replaced atomically. A file that
  others can read is refused (`insecure_cursors`), and one the CLI cannot read
  is refused too (`invalid_cursors`). It is version 2:

  ```json
  {"version": 2,
   "cursors": [{"server": "https://memoire.example", "db": "memory", "scopes": [],
                "reader": "session:s-41", "store_id": "mst_…", "seq": 40,
                "seen_at": 1795170000}]}
  ```

  `seen_at` is this machine's clock, in unix seconds, at the last
  `log --unseen` that used the cursor, whether or not it moved.
- A `memory-cursors.lock` file beside it serializes concurrent runs on one
  machine. Each run holds an exclusive file lock on it while it reads the
  cursors file, merges its cursor in and replaces the file, so agents that
  share the directory do not lose each other's cursors.
- Every write of the file drops the `session:` cursors not used for 7 days.
  A session that looks again after that gets a first look. Named and
  `machine` cursors are kept.
- A version 1 file, from an earlier CLI, is read as version 2: each of its
  cursors becomes a `machine` cursor, and an entry whose key the CLI cannot
  parse is dropped. The file is rewritten as version 2 at the next `log --unseen`. An
  earlier CLI refuses a version 2 file with `invalid_cursors`.
- A purge event has no scope, so a log filtered by `--scope` does not show
  it.

## Guards

Before any write, the engine scans every free-text field it is about to store
(title, body, kind, scope, tags, evidence, reason, agent and session) for
text that looks like a secret. The scope is scanned as stored, including one
taken from the git remote:

| Pattern | Catches |
| --- | --- |
| `private_key` | A `-----BEGIN … PRIVATE KEY-----` line |
| `memoire_api_key` | `mzk_` and at least 20 key characters |
| `memoire_session_token` | `mzs_` and at least 20 key characters |
| `anthropic_api_key` | `sk-ant-` and at least 20 key characters |
| `openai_api_key` | `sk-` and at least 20 key characters, one a digit |
| `github_token` | `ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_` or `github_pat_` and at least 30 characters |
| `aws_access_key_id` | `AKIA` or `ASIA` and exactly 16 capitals or digits |
| `slack_token` | `xoxa-`, `xoxb-`, `xoxp-`, `xoxr-` or `xoxs-` and at least 10 characters |
| `jwt` | `eyJ` and at least 10 more characters, a dot, a payload of at least 2, a dot and a signature of at least 10 |
| `env_assignment` | A line `NAME=value` (optionally `export`, optionally quoted, optionally followed by a `# comment`) whose name holds `SECRET`, `TOKEN`, `PASSWORD` or `API_KEY` and whose value has 8 characters or more and is not a placeholder, a number, a path or a URL |

The CLI runs the scan before it sends a request, so a value it catches
never leaves the machine; the gateway scans again, including the values a
replacement inherits. A hit exits 7 with `memory_secret_detected`. The error names the `pattern`
and the `field` and never echoes the text. There is no override. Write the
memory without the value, for example by naming where the secret is kept.

The scanner guards against accidents. It is not a guarantee, and a secret in a
format it does not know passes. The size limits are in [Limits](#limits).

## Forgetting and purging

`forget` leaves a tombstone, a `forgotten` revision. The memory leaves recall
and export, and its history stays for `show --history` and `log`.

`forget --purge` is for text that must not be stored, such as a leaked
secret. In one transaction it deletes:

- the memory's row and its search row, then merges the search index so the
  deleted words leave it;
- every revision, confirmation, dispute and event of the memory;
- the links to it in other memories' rows (`supersedes`, `superseded_by`).
  Their status is derived again in the same transaction, so a memory that
  the purged one had replaced is no longer superseded and returns to recall;
- what other memories say about linking to it. The `reason` of each of their
  revisions that set a link to it becomes `purged`, and their `supersede`
  events naming it are cut to `by <id>`.

It leaves one `purge` event with the memory's id and who purged it. Its
`summary`, `rev` and `scope` are `null`, so a purge stores no text from the
caller, the reason included. The id itself stays, in that event and in the
link columns of other memories' older revisions. `show` of the id then
reports `memory_not_found` with the time of the purge.

A purge finds only what is tied to the memory. Text that quotes it from an
unrelated place, such as another memory's body or the reason of an
unrelated revision, is not found and stays.

A purge does not reach copies:

- Snapshots of a persistent database taken before the purge still hold the
  text until they age out, after 7 days.
- A file written earlier by `memoire memory export` or `memoire export`
  still holds it.
- SQLite may keep the deleted bytes in its file until the pages are reused.

So a purged secret must still be rotated.

## Schema and migrations

| Table | Holds |
| --- | --- |
| `memoire_memory_meta` | `schema_version`, `store_id` and when each migration ran |
| `memoire_memory_memories` | One row per memory: its current state, content author and status |
| `memoire_memory_revisions` | Every revision |
| `memoire_memory_confirmations` | Confirmations, each on a revision |
| `memoire_memory_disputes` | Disputes, each on a revision, and their resolution |
| `memoire_memory_events` | The log |
| `memoire_memory_fts` | The FTS5 search index of the current title, body, tags and scope |

FTS5 keeps the index in five more tables: `memoire_memory_fts_config`,
`memoire_memory_fts_content`, `memoire_memory_fts_data`,
`memoire_memory_fts_docsize` and `memoire_memory_fts_idx`. `GET schema` and
the shell's `.tables` list them as ordinary tables. The web debugger groups the
store's tables under "Memory store" and folds these five away. They are the
index's own storage, and the gateway refuses direct writes to them.

The store records its version in `memoire_memory_meta` as `schema_version`;
this CLI's version is 1. `init` applies each missing version as one
transaction, and a second client applying the same version at the same time
fails harmlessly and reads the version again. A store at the current version
is left as it is.

Every other verb reads the version first, in the same batch as its first
read:

| Code | Exit | Means |
| --- | --- | --- |
| `memory_not_initialized` | 6 | The database holds no store. Run `memoire memory init`. |
| `memory_schema_older` | 6 | The store is older than this CLI. Run `memoire memory init`. |
| `memory_schema_newer` | 6 | The store is newer than this CLI. Upgrade memoire. |

The last two carry `store_version` and `supported_version`. A missing
database answers exit 5 `database_not_found`, with a hint to run `init`.

The schema is plain SQL in the database, and the gateway's memory routes
and this CLI's SQL path run the same engine on it. Through the API the
gateway checks the version with its own engine; the codes are the same.

## Read-only keys

A key created with `memoire keys create --read-only` can run `recall`,
`show`, `log` (with `--unseen`, whose cursor is local) and `export`. These
verbs only read and never call `whoami`. `init` works only on a store that
already exists at this version: it reads the store's version and writes
nothing. Every other verb exits 4 with `unauthorized`.

Through the memory API the gateway checks the permission before it reads
anything, so every write verb exits 4 at once. On the SQL path a verb can
refuse earlier for a reason it finds while reading: `resolve` on a memory
with no open dispute exits 6 with `memory_no_open_dispute` before it tries
to write. An input the CLI refuses itself (a malformed id, a secret, a field
too large) is refused the same way on both paths, before any request.

## Memory-only keys

`memoire keys create --memory` mints a key holding only
`memoire.memory.read` and `memoire.memory.write` (`--read-only` too: only
`memoire.memory.read`). Such a key runs every verb through the memory API.
It cannot run `query` or the shell, read the schema or the rows, export,
import, list or restore snapshots, or create, list or delete databases, so
it cannot use the SQL path either (`MEMOIRE_MEMORY_SQL=1` exits 4). `init`
creates the store in a database that exists, through the memory API, and
prints `"database": null` because the key may not read the catalog; kind
flags are ignored then. It cannot create the database: on a missing one it
exits 5 `database_not_found`, and a key that may create databases must run
`memoire memory init` once. A key with the write permission but neither read
permission gets `memory_duplicate` with an empty `candidates` list. A
gateway without the memory API does not know these permissions, and
`keys create --memory` fails with `unknown_permission`.

## Errors

The codes this feature adds, in the CLI's error envelope (see
[cli.md](cli.md#output-and-errors)). Exit 6 reports the classification
`conflict`, 8 `transient`, the others `permanent`. The engine makes the
memory's refusals: on the SQL path in the CLI, through the API in the
gateway, which sends their members in `data` and the CLI reports them as
its own. The codes, messages and members are the same either way.

| Code | Exit | Extra members |
| --- | --- | --- |
| `memory_not_initialized` | 6 | |
| `memory_schema_older` | 6 | `store_version`, `supported_version` |
| `memory_schema_newer` | 6 | `store_version`, `supported_version` |
| `memory_conflict` | 6 | `id`, `current_rev` |
| `memory_duplicate` | 6 | `candidates`: `[{"id", "title", "status", "similarity"}]`, empty for a key that may not read |
| `memory_not_current` | 6 | `id`, `status`, `superseded_by` |
| `memory_no_open_dispute` | 6 | |
| `memory_cycle` | 6 | |
| `skill_exists` | 6 | `path` |
| `memory_not_found` | 5 | `id` |
| `memory_self_confirm` | 7 | |
| `memory_secret_detected` | 7 | `pattern`, `field` |
| `memory_too_large` | 7 | `field`, `limit` |
| `memory_unchanged` | 7 | |
| `invalid_query` | 2 | |
| `insecure_cursors` | 1 | |
| `invalid_cursors` | 1 | |

## The skill

```sh
memoire skill install --claude | --codex | --dir PATH [--force]
```

Writes the memoire skill, which tells an agent how to use the memory, where
the agent loads skills from:

| Flag | Writes |
| --- | --- |
| `--claude` | `$CLAUDE_CONFIG_DIR/skills/memoire/SKILL.md`, else `~/.claude/skills/memoire/SKILL.md` |
| `--codex` | `$CODEX_HOME/skills/memoire/SKILL.md`, else `~/.codex/skills/memoire/SKILL.md` |
| `--dir PATH` | `PATH/memoire/SKILL.md` |

The flags combine. When two of them name the same destination, such as
`--claude --dir ~/.claude/skills` or a relative path to the same directory,
the skill is installed once and the second entry reports `changed: false`.
The text is embedded in the binary, the same as
[`skills/memoire/SKILL.md`](../skills/memoire/SKILL.md), so the skill matches
the CLI that installed it. An identical file is left alone. A different file
is refused with `skill_exists` (exit 6) unless `--force`, and every target is
checked before any is written. A file that appears at a target during the
install is refused the same way. It needs no login.

```json
{"installed": [{"path": "/home/agent/.claude/skills/memoire/SKILL.md", "changed": true}]}
```
