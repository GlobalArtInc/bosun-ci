#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
script="$root/hack/compat-recordings.py"
fixtures="$root/compat/fixtures/recordings"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

failed=0

expect_success() {
  local name="$1" output
  shift
  if ! output="$("$@" 2>&1)"; then
    echo "$name: compat-recordings.py failed" >&2
    echo "$output" >&2
    failed=1
  else
    echo "$name: passed as expected"
  fi
}

expect_failure() {
  local name="$1" expected="$2" output
  shift 2
  if output="$("$@" 2>&1)"; then
    echo "$name: compat-recordings.py passed, but it must fail with: $expected" >&2
    failed=1
  elif ! grep -qF -- "$expected" <<<"$output"; then
    echo "$name: compat-recordings.py failed without saying: $expected" >&2
    echo "$output" >&2
    failed=1
  else
    echo "$name: refused as expected"
  fi
}

expect_same() {
  local name="$1" produced="$2" committed="$3"
  if cmp -s "$produced" "$committed"; then
    echo "$name: equals ${committed#"$root/"}"
  else
    echo "$name: differs from ${committed#"$root/"}" >&2
    diff -u "$committed" "$produced" >&2 || true
    failed=1
  fi
}

expect_success generate "$script" generate --cases "$fixtures/cases" --workflow "$scratch/expr.yml"
expect_same "the workflow of the fixture cases" "$scratch/expr.yml" "$fixtures/expr.yml"

expect_success check "$script" generate --check --cases "$fixtures/cases" --workflow "$fixtures/expr.yml"
cp "$fixtures/expr.yml" "$scratch/stale.yml"
printf '\n' >> "$scratch/stale.yml"
expect_failure stale "is not the workflow of the cases" \
  "$script" generate --check --cases "$fixtures/cases" --workflow "$scratch/stale.yml"

expect_success import "$script" import --cases "$fixtures/cases" --output "$scratch/expr.json" "$fixtures/run.log"
expect_same "the recordings of the fixture log" "$scratch/expr.json" "$fixtures/expr.json"

grep -v 'bosun-outcome values.json\[1\]' "$fixtures/run.log" > "$scratch/partial.log"
expect_failure partial "the log holds no outcome of values.json[1]" \
  "$script" import --cases "$fixtures/cases" --output "$scratch/partial.json" "$scratch/partial.log"

sed 's/bosun-result values.json\[1\] [0-9a-f]*/bosun-result values.json[1] 000000000000/' \
  "$fixtures/run.log" > "$scratch/other.log"
expect_failure other "the log records values.json[1] as a case that this commit does not hold" \
  "$script" import --cases "$fixtures/cases" --output "$scratch/other.json" "$scratch/other.log"

sed 's/\(bosun-outcome values.json\[2\] [0-9a-f]*\) failure/\1 success/' \
  "$fixtures/run.log" > "$scratch/inconsistent.log"
expect_failure inconsistent "values.json[2] ended with the outcome success without a result" \
  "$script" import --cases "$fixtures/cases" --output "$scratch/inconsistent.json" "$scratch/inconsistent.log"

mkdir -p "$scratch/cases"
cp "$fixtures/cases/"*.json "$scratch/cases/"
printf '[\n  {\n    "expression": "github.sha",\n    "contexts": {\n      "github": {"sha": "abc"}\n    },\n    "result": "abc"\n  }\n]\n' \
  > "$scratch/cases/supplied.json"
expect_failure supplied "supplied.json[0] supplies \`github\`, which a hosted runner cannot be given" \
  "$script" generate --cases "$scratch/cases" --workflow "$scratch/supplied.yml"

if [ "$failed" -ne 0 ]; then
  echo "test-compat-recordings.sh: some cases failed" >&2
  exit 1
fi
echo "test-compat-recordings.sh: every case passed"
