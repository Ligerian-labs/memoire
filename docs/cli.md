# memoire CLI

`memoire` creates, queries and shares SQLite databases through a memoire
gateway, over the HTTP API in [api.md](api.md). It prints tables for people
and JSON with `--json` for scripts and agents.

There are no release binaries yet, so Cargo builds the CLI from source. That
needs Rust 1.98 or later. The binary is `memoire`.

```sh
cargo install --locked --git https://github.com/ligerian-labs/memoire memoire-cli
cargo install --locked --path crates/memoire-cli   # the same, from a checkout
```

The README's [Get started](../README.md#get-started) section goes from this
install to a signed-in CLI on the hosted service, whose URL is
`https://app.memoire.network`.

## Signing in

```sh
memoire login --server https://memoire.example     # prompts for the API key
memoire whoami
```

`login` checks the key with the gateway, then stores the URL and the key in
`~/.config/memoire/credentials.json`: the directory is mode 0700, the file
0600, and a file that others can read is refused. `MEMOIRE_CONFIG_DIR`, or
`$XDG_CONFIG_HOME/memoire`, moves it.

The key never goes in an argument, where other users and shell history could
see it. `login` reads it from `--key-file FILE`, else `MEMOIRE_API_KEY`,
else standard input: a prompt without echo on a terminal, or the piped text.

```sh
memoire login --server https://memoire.example < key.txt
```

An agent can skip `login`: `MEMOIRE_SERVER` and `MEMOIRE_API_KEY` take
precedence over the stored values. The stored key only goes to the stored
server, so a `MEMOIRE_SERVER` that names another one needs `MEMOIRE_API_KEY`
as well.

## Commands

| Command | Does |
| --- | --- |
| `login --server URL [--key-file FILE]` | Check and store an API key |
| `whoami` | Show the server, the caller and its team |
| `db create NAME [--burstable [--ttl 24h] \| --persistent]` | Create a database; burstable is the default |
| `db ls [--all]` | List the team's databases; `--all` includes expired ones |
| `db info NAME` | Show one database |
| `db rm NAME` | Delete a database, its file and its snapshots |
| `db extend NAME [--ttl 2h]` | Restart a burstable database's idle clock, optionally with a new TTL |
| `query DB "SQL" [-p VALUE]... [--write]` | Run one statement |
| `shell DB [--write]` | Open a SQL shell |
| `export DB -o FILE` | Download a consistent copy |
| `import DB FILE` | Load a SQLite file into an empty database |
| `snapshots ls DB` | List a persistent database's snapshots |
| `snapshots restore DB SNAPSHOT_ID` | Restore one, after a `pre_restore` snapshot of the current contents |
| `keys create --name MACHINE [--team org_…] [--expires 90d] [--read-only] [--memory]` | Create a machine and an API key for it |
| `memory init [--burstable [--ttl 24h] \| --persistent]` | Create a memory store's database and tables; persistent is the default |
| `memory remember --title TITLE [--body BODY \| --body-file FILE] [--kind KIND] [--scope SCOPE] [--tag TAG]... [--evidence TEXT]... [--force] [--supersedes OLD_ID --reason REASON]` | Record a memory; with `--supersedes`, it replaces `OLD_ID` in the same write |
| `memory recall [QUERY] [--scope SCOPE... \| --all-scopes] [--kind KIND]... [--tag TAG]... [--status STATUS]... [--limit 10]` | Search memories, best first; without QUERY, the newest |
| `memory show ID [--history]` | Show a memory, its confirmations and disputes |
| `memory revise ID --reason REASON [--title TITLE] [--body BODY] [--tag TAG \| --add-tag TAG]...` | Write a new revision; only the given fields change |
| `memory confirm ID --evidence TEXT... [--rev N]` | Confirm a memory's current text |
| `memory dispute ID --reason REASON [--evidence TEXT]... [--rev N]` | Dispute a memory |
| `memory resolve ID --reason REASON` | Close a memory's open disputes |
| `memory supersede ID --by NEW_ID --reason REASON` | Replace a memory by another |
| `memory forget ID --reason REASON [--purge]` | Leave a tombstone, or erase the memory and its history |
| `memory log [--since 24h \| --unseen [--reader NAME]] [--scope SCOPE]... [--limit 50]` | List the store's events, newest first; `--unseen`: those this session or reader has not seen |
| `memory export [--scope SCOPE]... -o FILE` | Write the memories as JSON Lines |
| `skill install --claude \| --codex \| --dir PATH [--force]` | Install the memoire skill for coding agents |

Durations are a whole number and a unit: `90s`, `30m`, `24h`, `7d`.

### Queries

`query` runs in read mode, on a read-only connection; `--write` runs it on
the writer. Each `-p` binds the next `?`. A value that parses as JSON binds
as that JSON in the API's value encoding (`42`, `2.5`, `null`, `true`,
`{"type":"blob","value":"AAE="}`); anything else binds as text. Quote text
that looks like JSON: `-p '"42"'`.

```sh
memoire query scratch "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)" --write
memoire query scratch "INSERT INTO notes (body) VALUES (?)" -p hello --write
memoire query scratch "SELECT * FROM notes WHERE id = ?" -p 1 --json
```

### The shell

`shell` reads statements that end with `;`. A statement may span lines and
a line may hold several. A `;` inside quotes, a comment or a `CREATE
TRIGGER … END` body does not end one. Dot commands:
`.tables`, `.schema [NAME]`, `.mode read|write`, `.help`, `.quit`. On a
terminal it prompts and keeps going after an error. With a script on
standard input it prints no prompts and stops at the first error, exiting
with that error's code:

```sh
memoire shell scratch --write < migrate.sql
```

### Machine keys

`keys create` signs in as a member, because creating a machine needs a
recent sign-in, which an API key is not. It reads the email and password
from the terminal (the password without echo), or as two lines of standard
input. Then it creates the machine, gives it the team role `member` (a
machine holds no permission without a role), and creates its key with the
five `memoire.databases.*` permissions, or only `memoire.databases.read` with
`--read-only`. With `--memory`, the key holds only `memoire.memory.read` and
`memoire.memory.write` (only `memoire.memory.read` with `--read-only`): an
agent on it can use the memory and nothing else, see
[memory.md](memory.md#memory-only-keys). A gateway without the memory API
refuses those permissions with `unknown_permission`. Creating machines and assigning roles takes a team admin or
owner; a plain member cannot run it. It prints
the key once and signs the temporary session out. The team is `--team`,
else the stored key's team. `--expires` is at most `365d`, identity's limit.

`keys create` has no `--server` flag. It uses `MEMOIRE_SERVER`, else the
server that `login` stored. That URL must be the gateway's application
origin, because the sign-in carries it as `Origin`.

Without `--json` the key is the only thing on standard output, so it can go
straight to a file:

```sh
memoire keys create --name ci-runner > ci-runner.key
```

Before the first `login` there is no stored server or team, so name both.
`memoire whoami` on a signed-in machine prints the team's id.

```sh
MEMOIRE_SERVER=https://memoire.example memoire keys create --team org_… --name ci-runner > ci-runner.key
```

### Memory

`memory` keeps a shared memory for agents in a database, `memory` unless
`--db NAME` or `MEMOIRE_MEMORY_DB` names another. Writes record the key's
principal with `--agent` (`MEMOIRE_AGENT`) and `--session`
(`MEMOIRE_SESSION`). The verbs call the gateway's memory API, and fall back
to SQL over the query route against a gateway without it
(`MEMOIRE_MEMORY_SQL=1` forces that, `0` forbids it). [memory.md](memory.md) is the
reference: records, statuses, ranking, concurrency and the error codes.

```sh
memoire memory init
memoire memory remember --title "Widgets deploys need the VPN" --evidence "make deploy"
memoire memory recall "deploy vpn" --json
```

`skill install` writes the skill that teaches an agent these commands. It
needs no login.

## Output and errors

Results go to standard output. Notes and errors go to standard error. With
`--json`, each result is one JSON value: the API's database, statement
result or snapshot objects, or an array of them for `db ls` and
`snapshots ls`. Each error is the API's envelope with a `status` and a
`message` added:

```json
{"error": {"code": "database_exists", "classification": "conflict", "status": 409,
           "message": "the team already has an active database of that name"}}
```

An `sql_error` also gets an `sql` member, `{"statement", "sqlite_code",
"message"}`, built from the error's `details` and SQLite's message, which
the gateway sends in the `Memoire-Sql-Message` header.

The CLI's own failures use the same envelope, with codes such as
`not_logged_in`, `connection_failed`, `timeout` and `invalid_param`.
The memory commands add codes such as `memory_conflict`,
`memory_duplicate`, `memory_not_initialized`, `memory_self_confirm`,
`memory_secret_detected` and `memory_too_large`, some with extra members;
[memory.md](memory.md#errors) lists them all.

The exit code gives the class of the failure and never changes:

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Anything else: an unexpected answer, a gateway fault, a local file problem |
| 2 | Wrong command line or input |
| 3 | No credential, or the gateway refused it (`401`) |
| 4 | Permission, team or statement denied (`403`) |
| 5 | No such database, table or snapshot (`404`) |
| 6 | The database's state forbids it: exists, not empty, wrong kind, expired (`409`, `410`). Or a memory's: changed meanwhile, a near-duplicate, superseded |
| 7 | Request refused as is: invalid input, an SQL error, a limit (`400`, `413`, `415`, `422`), or a memory that looks like it holds a secret |
| 8 | Try again later: busy, starting, rate limited or unreachable (`429`, `503`, connection failures) |
