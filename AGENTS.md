# memoire

SQLite databases on demand: Rust gateway on Merezha (`crates/memoire-gateway`), wire types (`crates/memoire-api`), memory engine (`crates/memoire-memory`), CLI (`crates/memoire-cli`), WASM guests (`guests/`, own workspace), Angular web debugger (`web/`, pnpm through Corepack).

- Checks (build the guests first; the process tests load them): `scripts/build-guests.sh`, `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace`, `(cd guests && cargo test --locked)`; web: `(cd web && pnpm lint && pnpm test && pnpm typecheck && pnpm build && pnpm e2e)` (e2e runs a real gateway, see `web/README.md`).
- PostgreSQL tests need `MEREZHA_TEST_DATABASE_URL` (`scripts/dev-postgres.sh start` prints one) and `MEREZHA_TEST_REQUIRE_DATABASE=1` so they cannot skip.
- Dev gateway: `scripts/dev-run.sh` (127.0.0.1:28960; `MEMOIRE_DATABASE_URL` overrides the throwaway database); `scripts/dev-owner.py` gives a signed-in owner, a team and an API key in `.memoire-dev/owner.json`.
- `docs/api.md` is the HTTP contract; changing it needs the gateway, CLI and web owners to agree, and `crates/memoire-api` must match it.
- Merezha is pinned by one git revision in both `Cargo.toml` and `guests/Cargo.toml`; rusqlite stays exactly Merezha's pin. Upstream changes go to Merezha, never a local patch.
- Guests import nothing: no file, network or database handle.
- Public repository: synthetic data only; secrets in files or the environment, never in code, logs or command arguments.
