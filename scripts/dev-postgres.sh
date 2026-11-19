#!/usr/bin/env bash
# A throwaway PostgreSQL 17 for memoire development and tests: loopback TCP
# only, no Unix socket, trust authentication. Never use it for real data.
#
#   scripts/dev-postgres.sh start [DIR] [PORT]   # defaults: .memoire-dev/pg 28962
#   scripts/dev-postgres.sh stop  [DIR]
#   scripts/dev-postgres.sh url   [DIR] [PORT]
#
# `start` prints the URL; tests read it from MEREZHA_TEST_DATABASE_URL:
#   export MEREZHA_TEST_DATABASE_URL=$(scripts/dev-postgres.sh start)
# PG_BIN selects the PostgreSQL binaries; otherwise the first pg_ctl found.
set -euo pipefail
command_name=${1:-start}
dir=${2:-.memoire-dev/pg}
port=${3:-28962}
pg=${PG_BIN:-}
if [ -z "$pg" ]; then
  for candidate in /usr/lib/postgresql/17/bin "$(brew --prefix postgresql@17 2>/dev/null || true)/bin"; do
    if [ -x "$candidate/pg_ctl" ]; then pg=$candidate; break; fi
  done
fi
[ -n "$pg" ] || pg=$(dirname "$(command -v pg_ctl)")
case "$command_name" in
  start)
    mkdir -p "$dir"
    if [ ! -f "$dir/data/PG_VERSION" ]; then
      LC_ALL=C "$pg/initdb" -D "$dir/data" -U postgres -A trust --no-locale -E UTF8 >/dev/null
    fi
    if ! LC_ALL=C "$pg/pg_ctl" -D "$dir/data" status >/dev/null 2>&1; then
      LC_ALL=C "$pg/pg_ctl" -D "$dir/data" -l "$dir/log" \
        -o "-p $port -c unix_socket_directories= -c listen_addresses=127.0.0.1 -c max_connections=200" -w start >/dev/null
    fi
    echo "postgres://postgres@127.0.0.1:$port/postgres"
    ;;
  stop)
    LC_ALL=C "$pg/pg_ctl" -D "$dir/data" -m fast stop >/dev/null
    ;;
  url)
    echo "postgres://postgres@127.0.0.1:$port/postgres"
    ;;
  *)
    echo "usage: $0 start|stop|url [DIR] [PORT]" >&2
    exit 2
    ;;
esac
