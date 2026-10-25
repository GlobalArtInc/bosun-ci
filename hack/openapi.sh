#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="$root/contracts/openapi.json"
generated="$(mktemp)"
trap 'rm -f "$generated"' EXIT

cargo run --quiet --manifest-path "$root/Cargo.toml" -p bosun-server-bin --bin bosun-server -- openapi > "$generated"
cat "$generated" > "$target"
echo "wrote ${target#"$root/"}"
