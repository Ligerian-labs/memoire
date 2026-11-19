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

> Status: the catalog is live: databases are created, listed, extended
> and deleted through the gateway, with the 50-database quota and the
> memoire permissions. The data plane (files, queries), the sweeper,
> snapshots, the CLI commands and the web client land next.

## Documentation

- [docs/api.md](docs/api.md): the HTTP API contract.
- [docs/architecture.md](docs/architecture.md): contexts, data plane rules
  and repository layout.

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
memoire's five permissions; the secrets go to `.memoire-dev/owner.json`
(mode 0600), never to the terminal:

```sh
export MEMOIRE_API_KEY=$(jq -r .api_key .memoire-dev/owner.json)
curl -s -H @<(printf 'Authorization: Bearer %s\n' "$MEMOIRE_API_KEY") localhost:28960/v1/memoire/databases
```

## License

Apache-2.0. See [LICENSE](LICENSE).
