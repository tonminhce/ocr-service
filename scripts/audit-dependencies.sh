#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

tree=$(mktemp)
trap 'rm -f "$tree"' EXIT
cargo tree --workspace --locked --target all --all-features --prefix none > "$tree"
if grep -Eq '^rsa v[0-9]' "$tree"; then
  echo "RSA is active in the dependency graph; the inactive-SQLx advisory exception is invalid" >&2
  exit 1
fi
# SQLx weak features retain its inactive MySQL/RSA packages in Cargo.lock.
# Fail above if RSA becomes active on any target/feature before applying this
# single exception. See docs/release-operations.md for sources and review scope.
cargo audit --ignore RUSTSEC-2023-0071 "$@"
