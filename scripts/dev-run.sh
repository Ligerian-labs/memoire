#!/usr/bin/env bash
# Runs the development gateway on 127.0.0.1:28960 (config/dev/memoire.toml).
#
#   scripts/dev-run.sh [--listen ADDRESS] [--set SECTION.KEY=VALUE]...
#
# The database is MEMOIRE_DATABASE_URL when set (an existing, disposable
# PostgreSQL database); otherwise a throwaway cluster from
# scripts/dev-postgres.sh on MEMOIRE_DB_PORT (default 28962). Secrets are
# generated once under .memoire-dev/secrets/ (mode 0600). The catalog guest is
# built first when guests/components/ lacks it. Synthetic data only. Then
# scripts/dev-owner.py gives a signed-in owner, a team and an API key.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
dev="$root/.memoire-dev"
mkdir -p "$dev/secrets" "$dev/data"
chmod 700 "$dev" "$dev/secrets"
umask 077
[ -f "$dev/secrets/identity.key" ] || head -c 48 /dev/urandom | base64 | tr -d '\n' > "$dev/secrets/identity.key"
[ -f "$dev/secrets/notifications.key" ] || head -c 32 /dev/urandom | base64 | tr -d '\n' > "$dev/secrets/notifications.key"
if [ -z "${MEMOIRE_DATABASE_URL:-}" ]; then
  MEMOIRE_DATABASE_URL=$("$root/scripts/dev-postgres.sh" start "$dev/pg" "${MEMOIRE_DB_PORT:-28962}")
fi
export MEMOIRE_DATABASE_URL
cd "$root"
[ -s guests/components/memoire-catalog.wasm ] || scripts/build-guests.sh >/dev/null
exec cargo run --locked -p memoire-gateway -- run --config "$root/config/dev/memoire.toml" "$@"
