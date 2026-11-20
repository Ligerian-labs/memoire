---
name: memoire
description: Read and write the team's shared agent memory with `memoire memory`. Use it when you start a task, before you make a decision or ask the user something, after a failure, when you have verified something worth keeping, and when you hand work to another agent.
---

# memoire memory

Agents of the team share one memory of short records with evidence, which other agents confirm, dispute and correct. Recall before you work. Write back what you verified.

## Setup

The CLI needs a server and an API key. Either `MEMOIRE_SERVER` and `MEMOIRE_API_KEY` are already in the environment, or a login is stored once per machine. The key comes from a file, the environment or standard input, never from an argument.

```sh
memoire login --server https://memoire.example --key-file key.txt
```

Choose a session id once, at the start of the run, and write it as a literal. It must be unique to this run and never shared with another agent, because two agents on one key with the same session count as one writer and share one `log --unseen` cursor. Do not invent the unique part: agents pick the same letters. Run `od -An -N8 -tx8 /dev/urandom` once and copy the sixteen characters it prints into the id. Use the same literal for every command of the run. Never compute it in the command with `$(date)` or `$$`, because a shell that starts a new process per command would give each command a new session. If your shell does not keep the environment between commands, set both variables on each command or pass `--agent` and `--session`.

```sh
export MEMOIRE_AGENT=claude-opus-5-5           # your model id
export MEMOIRE_SESSION=widgets-5f3a9c1e07b2d4a6   # an example: a name and the characters `od` printed for you, kept for the whole run
MEMOIRE_AGENT=claude-opus-5-5 MEMOIRE_SESSION=widgets-5f3a9c1e07b2d4a6 memoire memory recall "deploy" --json  # when the shell forgets the environment
```

- Every write records the key's principal (the server says who it is; `memoire whoami --json` prints it as `subject`), your agent and your session. A confirmation counts only from another key or another session than the one that wrote the text. Without `MEMOIRE_SESSION`, every run of a key is the same writer and can never confirm its earlier text.
- Pass `--json` to every command. The result is one JSON value on standard output.
- The store is the database `memory`. `--db NAME` or `MEMOIRE_MEMORY_DB` picks another (lowercase letters, digits and `-`, starting with a letter).

## Recall first

At the start of a session:

```sh
memoire memory log --unseen --json                 # the store's recent events, then only what is new
memoire memory recall "deploy staging vpn" --json  # words from your task
```

If a command exits 5 `database_not_found` or 6 `memory_not_initialized`, nobody has created the store yet. Run `memoire memory init --json` once and carry on. It creates a persistent database and the store's tables, is safe to repeat, and any agent with a write key may run it.

Recall again before a decision, before you ask the user something the team may already know, and after a failure (search with the error's words).

Each entry of `memories` has a `status`:

| `status` | Treat it as |
| --- | --- |
| `inferred` | A lead. Verify it before you rely on it. |
| `confirmed` | Evidence. `confirmers` is how many distinct key and session pairs re-checked it. |
| `disputed` | Contested. Read the dispute with `memoire memory show ID --json` before you use it. |

Before you act on a remembered operational fact (a port, a path, a version, who owns what), check the live state.

- Every memory has an `id` such as `mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8`. The verbs below take it.
- By default recall searches the current git checkout's project scope and `global`. The output's `scopes` lists what it searched.
- `--scope S` (repeatable) searches those scopes instead. `--all-scopes` searches every scope.
- The query is plain words. Any of them may match, and a memory that matches more ranks higher. Quotes and operators mean nothing.
- `--kind K` (repeatable, any of them), `--tag T` (repeatable, all of them), `--limit N` (1 to 100, default 10). Without a query, newest first.
- `score` only orders the results of one query. Its size means nothing, and it can be tiny on a small store.
- No result means nothing matched those words in the scopes and statuses searched. Try other words or `--all-scopes` before you conclude the team has not recorded it. Then do the work and write what you verified.
- `log --unseen` follows your `MEMOIRE_SESSION`. A session's first look shows the events of the last 24 hours, the newest `--limit` of them, 50 by default; later looks show only what is new. `log --since 72h --json` shows a fixed window. A role that runs as many sessions, such as a nightly reviewer, passes `--reader NAME` or sets `MEMOIRE_READER` to continue where its last run stopped.

## What to write

Write verified findings, decisions with their rationale, procedures that worked, pitfalls after a real failure, and the owner's preferences.

- One idea per memory, under a title that stands alone. A stranger understands it without the body. At most 120 characters.
- Evidence with each memory, `--evidence` repeated: the command you ran, the file and line, the URL, the commit.
- Kinds: `fact` (the default), `decision`, `procedure`, `preference`, `pitfall`, `handoff`.
- Limits: body 8 KiB, 10 tags, 10 evidence items of 500 characters.
- Never store secrets, credentials or personal data. The CLI refuses text that looks like a secret (`memory_secret_detected`) and has no override. Describe where the secret lives instead of its value.
- Never store what the repository already records, such as code structure or git history.

```sh
memoire memory remember --json --kind pitfall \
  --title "Widgets deploys fail with a timeout unless the VPN is connected" \
  --tag deploy --evidence "make deploy, run at 14:02 UTC" --evidence "deploy/README.md:12"
```

A longer body comes from standard input:

```sh
memoire memory remember --json --kind procedure --title "Restore the widgets staging database from a snapshot" \
  --evidence "docs/ops.md:40" --body-file - <<'EOF'
1. memoire snapshots ls widgets-staging
2. memoire snapshots restore widgets-staging SNAPSHOT_ID
EOF
```

## Improve what is there

Prefer improving an existing memory over adding a near-duplicate.

- `memoire memory confirm ID --rev N --evidence "what you ran" --json` when you re-verified it yourself. `N` is the `rev` from the recall or show output you verified. If the text changed since that revision, it exits 6 `memory_conflict` instead of confirming text you did not read. You cannot confirm text you wrote in this session. Your evidence is stored on the confirmation, listed under `confirmations` by `memoire memory show ID --json`; the memory's own `evidence` does not change. When you verified a memory and found nothing new, this confirmation is your write-back. Do not write a second memory. It prints the memory, whose `status` and `confirmers` show the effect, so you need no `show` to check, and it marks the memory as fresh for ranking.
- `memoire memory dispute ID --rev N --reason "what reality shows" --evidence "how you saw it" --json` when reality disagrees, with `N` the `rev` you read. The memory stays in recall, flagged. Then fix it with one of the next verbs.
- `memoire memory revise ID --reason R --json` with `--title`, `--body`, `--body-file`, `--kind`, `--scope`, `--add-tag`, `--add-evidence` (or `--tag`, `--evidence` to replace the list) when it is right but unclear or incomplete. Only the given fields change. A new title, body or kind resets the status to `inferred`, because the old confirmations covered the old text.
- `memoire memory remember --supersedes OLD --reason R --title "…" --evidence "…" --json` when the meaning changed. It retires OLD and writes the new memory in one step, and OLD leaves recall. The new memory keeps OLD's scope, kind and tags unless you pass `--scope`, `--kind` or `--tag`. OLD's body and evidence are not copied, so pass the new memory's own. The duplicate check skips only OLD, so a title close to OLD's needs no `--force`. You need not dispute OLD first; if it has an open dispute that this settles, `resolve` it afterwards. Use `memoire memory supersede OLD --by NEW --reason R --json` only to link two memories that both exist already.
- `memoire memory resolve ID --reason R --json` once the dispute is settled. It closes every open dispute of the memory.
- `memoire memory forget ID --reason R --json` when it is wrong and harmful. Recall hides it; its history stays.
- `memoire memory forget ID --reason R --purge --json` only for text that must not be stored, such as a leaked secret (a `revise` keeps the old text in the history). It erases the memory and its history. Rotate the secret too, because backups and earlier exports may still hold it.

## Share

Choose the scope on purpose with `--scope`:

| Scope | For |
| --- | --- |
| `project:<host>/<path>` | One repository. The default inside a git checkout, such as `project:github.com/acme/widgets`. |
| `client:<name>`, `org:<name>` | Knowledge wider than one repository. |
| `global` | The default outside a checkout. Use it sparingly. |
| `run:<id>` | One workload. |

For a multi-agent workload, use one burstable store per run (it expires after 24 hours without use, at most 7 days after creation) and pass state as `handoff` memories instead of files:

```sh
memoire memory init --db run-42 --burstable --json
memoire memory remember --db run-42 --scope run:42 --kind handoff --json \
  --title "Handoff after the schema step of run 42" --evidence "branch feat/widgets-schema" --body-file - <<'EOF'
Done: ... Next: ... Blocked: ... Artifacts: ...
EOF
memoire memory recall --db run-42 --kind handoff --all-scopes --json   # the next agent starts here
```

- A handoff says what is done, what is next, what is blocked and where the artifacts are (paths, branches, commits, URLs).
- Give each handoff a title that names its step, so the duplicate check does not stop it. When a new handoff replaces an older one, write it with `remember --supersedes OLD --reason R`.

## Errors

Errors go to standard error as `{"error": {"code": "...", "message": "...", ...}}`. Act on the code:

| Exit | Code | Do |
| --- | --- | --- |
| 6 | `memory_conflict` | Someone revised it first (`current_rev`), or the text changed since the revision you read. Run `memoire memory show ID --json` again and re-verify the new text before you confirm or dispute it. Retry other changes on the new revision if they still apply. |
| 6 | `memory_duplicate` | Read `candidates` (`id`, `title`, `status`, `similarity`) and `show` the closest. `confirm` it if yours agrees, `revise` it if yours adds to it, `dispute` it if yours contradicts it, add `--supersedes ID --reason R` if yours replaces it. `--force` only when yours is a different fact. |
| 6 | `memory_not_current` | It was superseded or forgotten. Follow `superseded_by`. |
| 6 | `memory_not_initialized`, `memory_schema_older` | Run `memoire memory init --json`. Exit 5 `database_not_found` means the same. |
| 6 | `memory_schema_newer` | This CLI is too old for the store. Tell the user. |
| 7 | `memory_self_confirm` | You wrote this text in this session. Leave the confirmation to another agent. |
| 7 | `memory_secret_detected` | `field` and `pattern` say where. Rephrase without the value. |
| 7 | `memory_too_large` | `field` and `limit` say which. Split it into several memories. |
| 7 | `memory_unchanged` | Your revision changes nothing. Leave it. |
| 5 | `memory_not_found` | Wrong id, or the memory was purged. |
| 4 | `unauthorized` | The key is read-only. Recall, show, log and export work, writes do not. Tell the user what you would have recorded. |
| 8 | any | The store is busy or unreachable. Try again later. |
