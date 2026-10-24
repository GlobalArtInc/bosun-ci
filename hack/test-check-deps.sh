#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

mkdir -p "$scratch/vendor/tokio/src"
: > "$scratch/vendor/tokio/src/lib.rs"
printf '[package]\nname = "tokio"\nversion = "0.0.0"\nedition = "2024"\n' > "$scratch/vendor/tokio/Cargo.toml"

workspace() {
  local dir="$scratch/$1"
  mkdir -p "$dir/crates"
  cp "$root/rust-toolchain.toml" "$dir/"
  printf '[workspace]\nresolver = "3"\nmembers = ["crates/*"]\n' > "$dir/Cargo.toml"
}

crate() {
  local dir="$scratch/$1/crates/$2"
  local name="$2"
  shift 2
  mkdir -p "$dir/src"
  : > "$dir/src/lib.rs"
  {
    printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n' "$name"
    printf '\n[dependencies]\n'
    for dep in "$@"; do
      case "$dep" in
        dev:*) ;;
        tokio) printf 'tokio = { path = "../../../vendor/tokio" }\n' ;;
        *) printf '%s = { path = "../%s" }\n' "$dep" "$dep" ;;
      esac
    done
    printf '\n[dev-dependencies]\n'
    for dep in "$@"; do
      case "$dep" in
        dev:*) printf '%s = { path = "../%s" }\n' "${dep#dev:}" "${dep#dev:}" ;;
      esac
    done
  } > "$dir/Cargo.toml"
}

failed=0

expect_failure() {
  local case="$1" expected="$2" output
  if output="$("$root/hack/check-deps.sh" "$scratch/$case/Cargo.toml" 2>&1)"; then
    echo "$case: check-deps.sh passed, but it must fail with: $expected" >&2
    failed=1
  elif ! grep -qF "$expected" <<<"$output"; then
    echo "$case: check-deps.sh failed without saying: $expected" >&2
    echo "$output" >&2
    failed=1
  else
    echo "$case: refused as expected"
  fi
}

expect_success() {
  local case="$1" output
  if ! output="$("$root/hack/check-deps.sh" "$scratch/$case/Cargo.toml" 2>&1)"; then
    echo "$case: check-deps.sh failed on an allowed graph" >&2
    echo "$output" >&2
    failed=1
  else
    echo "$case: accepted as expected"
  fi
}

workspace runner-reaches-db
crate runner-reaches-db bosun-types
crate runner-reaches-db bosun-db bosun-types
crate runner-reaches-db bosun-runner bosun-types bosun-db
expect_failure runner-reaches-db "bosun-runner reaches bosun-db through its dependencies"

workspace server-reaches-runner
crate server-reaches-runner bosun-types
crate server-reaches-runner bosun-runner bosun-types
crate server-reaches-runner bosun-server bosun-types bosun-runner
expect_failure server-reaches-runner "bosun-server reaches bosun-runner through its dependencies"

workspace types-uses-a-runtime
crate types-uses-a-runtime bosun-types tokio
expect_failure types-uses-a-runtime "bosun-types reaches tokio, an async runtime or I/O crate"

workspace unknown-crate
crate unknown-crate bosun-types
crate unknown-crate bosun-extra bosun-types
expect_failure unknown-crate "bosun-extra has no rule in hack/check-deps.sh"

workspace testkit-at-runtime
crate testkit-at-runtime bosun-types
crate testkit-at-runtime bosun-testkit bosun-types
crate testkit-at-runtime bosun-expr bosun-types bosun-testkit
expect_failure testkit-at-runtime "bosun-expr depends on bosun-testkit outside dev-dependencies"

workspace allowed-graph
crate allowed-graph bosun-types
crate allowed-graph bosun-testkit bosun-types tokio
crate allowed-graph bosun-expr bosun-types dev:bosun-testkit
crate allowed-graph bosun-workflow bosun-types bosun-expr
crate allowed-graph bosun-runner bosun-types bosun-expr bosun-workflow tokio
expect_success allowed-graph

exit "$failed"
