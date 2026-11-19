# memoire CLI

`memoire` creates, queries and shares SQLite databases through a memoire
gateway, over the HTTP API in [api.md](api.md). It prints tables for people
and JSON with `--json` for scripts and agents.

```sh
cargo install --locked --path crates/memoire-cli   # installs `memoire`
```

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
precedence over the stored values.

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
| `keys create --name MACHINE [--team org_…] [--expires 90d] [--read-only]` | Create a machine and an API key for it |

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

`shell` reads statements that end with `;` and may span lines. Dot commands:
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
input. Then it creates the machine and its key with the five memoire
permissions, or only `memoire.databases.read` with `--read-only`. It prints
the key once and signs the temporary session out. The team is `--team`,
else the stored key's team. The `--server` URL must be the gateway's
application origin, because the sign-in carries it as `Origin`.

Without `--json` the key is the only thing on standard output, so it can go
straight to a file:

```sh
memoire keys create --name ci-runner > ci-runner.key
```

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

The CLI's own failures use the same envelope, with codes such as
`not_logged_in`, `connection_failed`, `timeout` and `invalid_param`.

The exit code gives the class of the failure and never changes:

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Anything else: an unexpected answer, a gateway fault, a local file problem |
| 2 | Wrong command line or input |
| 3 | No credential, or the gateway refused it (`401`) |
| 4 | Permission, team or statement denied (`403`) |
| 5 | No such database, table or snapshot (`404`) |
| 6 | The database's state forbids it: exists, not empty, wrong kind, expired (`409`, `410`) |
| 7 | Request refused as is: invalid input, an SQL error, a limit (`400`, `413`, `415`, `422`) |
| 8 | Try again later: busy, starting, rate limited or unreachable (`429`, `503`, connection failures) |
