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

> Status: foundation. The workspace, the gateway host, the CLI skeleton and
> the API contract are in place; the catalog, data plane, CLI commands and
> web client land next.

## Documentation

- [docs/api.md](docs/api.md): the HTTP API contract.
- [docs/architecture.md](docs/architecture.md): contexts, data plane rules
  and repository layout.

## Development

Requirements: Rust (the version in `rust-toolchain.toml` is installed by
rustup automatically, with the `wasm32-unknown-unknown` target) and
PostgreSQL 17 binaries for the throwaway development database.

```sh
scripts/dev-run.sh                  # gateway on 127.0.0.1:28960, throwaway PostgreSQL
curl -s localhost:28960/health/ready

scripts/build-guests.sh             # WASM guests into guests/components/

cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
export MEREZHA_TEST_DATABASE_URL=$(scripts/dev-postgres.sh start)
cargo test --locked --workspace
(cd guests && cargo test --locked)
```

`scripts/dev-run.sh` uses `MEMOIRE_DATABASE_URL` instead of the throwaway
cluster when it is set. Development secrets and data live in `.memoire-dev/`
(ignored by Git).

## License

Apache-2.0. See [LICENSE](LICENSE).
