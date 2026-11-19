#!/usr/bin/env bash
# Builds the WASM guests listed in guests/runtime.toml into import-free
# Merezha components under guests/components/, with Merezha's guest build at
# the pinned revision (`memoire-gateway build`). Prints Merezha's JSON report.
#
#   scripts/build-guests.sh
#
# Requires the wasm32-unknown-unknown target (rust-toolchain.toml installs it).
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"
cargo build --locked --quiet -p memoire-gateway
"${CARGO_TARGET_DIR:-$root/target}/debug/memoire-gateway" build \
  --manifest "$root/guests/runtime.toml" \
  --workspace "$root/guests/Cargo.toml"
