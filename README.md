# memoire

SQLite databases on demand for agents, shared across teams and machines.

A gateway that is always up owns the SQLite files on its host and answers
every request. Agents and people create databases with the `memoire` CLI,
query them over an HTTP JSON API, and debug the data in a web client.

- **Burstable** databases are scratch space: they expire after an idle TTL
  (24 hours by default, at most 7 days after creation) and have no backups.
- **Persistent** databases live until deleted and are snapshotted every 6
  hours, with snapshots kept 7 days.
- Access is per team. Members sign in; machines and agents use API keys
  (`mzk_…`) that can be narrowed, for example to read-only.

memoire is built on [Merezha](https://github.com/MartianGreed/merezha): the
gateway is a Merezha host with its identity block, a native module for the
data plane, and a WASM guest for the database lifecycle rules.

> Status: v1 is in progress. The gateway, the CLI and the web client work
> today. A hosted service runs at <https://app.memoire.network>, by invitation
> only for now. The CLI has no release binaries yet.

## Get started

From nothing to a signed-in CLI. The hosted service at
<https://app.memoire.network> is invite-only for now, so you need an
invitation to get an account. You can also run your own gateway:
[docs/deploy/dokploy.md](docs/deploy/dokploy.md).

1. Install the CLI. There are no release binaries yet, so Cargo builds the
   CLI from source. That needs Rust 1.98 or later. The binary is `memoire`.

   ```sh
   cargo install --locked --git https://github.com/ligerian-labs/memoire memoire-cli
   ```

2. Open <https://app.memoire.network>, sign in and create a team. Databases,
   machines and keys belong to it.

3. Create a key for this machine. In the web app, open **Machines and API
   keys** and add a machine. The app then offers a key for it. Pick read-only
   access if the machine only needs to read. The app shows the key (`mzk_…`)
   once, so copy it now.

4. Sign the CLI in. `login` asks for the key and does not echo it. Paste it
   at the prompt. The CLI checks the key with the service, then stores it in
   `~/.config/memoire/credentials.json` with mode 0600.

   ```sh
   memoire login --server https://app.memoire.network
   ```

   Then check who the CLI is signed in as.

   ```sh
   memoire whoami
   ```

5. Teach your agents (optional). This installs the memoire skill, which
   teaches Claude Code and Codex the commands. It needs no login.

   ```sh
   memoire skill install --claude --codex
   ```

6. Agents and CI. An agent or a CI job skips `login` and reads two
   variables: `MEMOIRE_SERVER=https://app.memoire.network` and
   `MEMOIRE_API_KEY`, the machine's key from your secret store. Give each
   machine its own key.

   Once signed in, a team owner or admin can mint those keys from the CLI. It
   asks for your email and password without echo and prints only the key, so
   the key goes straight to a file and never into an argument. The `umask`
   keeps that file readable by you alone.

   ```sh
   (umask 077 && memoire keys create --name ci-runner > ci-runner.key)
   ```

   Move the file to the new machine, store the key and remove the file.

   ```sh
   memoire login --server https://app.memoire.network --key-file ci-runner.key && rm ci-runner.key
   ```

[docs/cli.md](docs/cli.md) covers every command.

## Agent memory

`memoire memory` turns a database into a shared memory for a fleet of agents.
Agents record what they verified with its evidence, recall it before they
work, and confirm, dispute or revise what others wrote. Every change is a new
revision, kept in `memoire_memory_` tables beside the database's other tables.

```sh
memoire memory init
memoire memory remember --title "Widgets deploys need the VPN" --evidence "make deploy, run at 14:02 UTC"
memoire memory recall "deploy vpn" --json
MEMOIRE_SESSION=s-42 memoire memory confirm mem_01JD0Q7ZQ5Y8W1N3K4M5P6R7S8 --evidence "make deploy with the VPN up"
memoire skill install --claude
```

The gateway runs the memory's rules and records who wrote what, and
`memoire keys create --memory` gives an agent a key that can use the memory
and nothing else. [docs/memory.md](docs/memory.md) is the reference.

## Documentation

- [docs/api.md](docs/api.md): the HTTP API contract.
- [docs/cli.md](docs/cli.md): the `memoire` command line.
- [docs/memory.md](docs/memory.md): the shared agent memory.
- [docs/architecture.md](docs/architecture.md): contexts, data plane rules
  and repository layout.
- [docs/deploy/dokploy.md](docs/deploy/dokploy.md): the production images and
  their deployment on Dokploy.
- [docs/benchmarks.md](docs/benchmarks.md): latency and throughput of the
  hosted service measured from one client, with the method, the limits and
  the script to run it again ([scripts/bench.py](scripts/bench.py)).

## Development

Requirements: Rust (the version in `rust-toolchain.toml` is installed by
rustup automatically, with the `wasm32-unknown-unknown` target) and
PostgreSQL 17 binaries for the throwaway development database.

```sh
scripts/build-guests.sh             # WASM guests into guests/components/ (the gateway loads them)
scripts/dev-run.sh                  # gateway on 127.0.0.1:28960, throwaway PostgreSQL
curl -s localhost:28960/health/ready
scripts/dev-owner.py                # a signed-in owner, a team and an API key: .memoire-dev/owner.json

cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
export MEREZHA_TEST_DATABASE_URL=$(scripts/dev-postgres.sh start)
cargo test --locked --workspace
(cd guests && cargo test --locked)
```

`scripts/dev-run.sh` uses `MEMOIRE_DATABASE_URL` instead of the throwaway
cluster when it is set. Development secrets and data live in `.memoire-dev/`
(ignored by Git). `scripts/dev-owner.py` registers a synthetic account through
identity and the capture mail driver, creates a team and mints a key with
memoire's five database permissions; the secrets go to `.memoire-dev/owner.json`
(mode 0600), never to the terminal:

```sh
export MEMOIRE_API_KEY=$(jq -r .api_key .memoire-dev/owner.json)
curl -s -H @<(printf 'Authorization: Bearer %s\n' "$MEMOIRE_API_KEY") localhost:28960/v1/memoire/databases
```

## License

Apache-2.0. See [LICENSE](LICENSE).
