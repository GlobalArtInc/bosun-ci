#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
corpus="$root/compat/workflows"

if [ "$#" -eq 0 ]; then
  echo "usage: hack/compat-sync.sh <organisation>..." >&2
  echo "fetches the workflow files of every repository of the organisations into compat/workflows/" >&2
  exit 2
fi

command -v gh >/dev/null || { echo "hack/compat-sync.sh needs the GitHub CLI, gh, signed in" >&2; exit 1; }

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

listing() {
  local organisation="$1" repository="$2" errors="$scratch/errors"
  if gh api "repos/$organisation/$repository/contents/.github/workflows" \
    --jq '.[] | select(.type == "file") | select(.name | test("\\.ya?ml$")) | [.name, (.name | @uri)] | @tsv' \
    2>"$errors"; then
    return 0
  fi
  if grep -qE 'HTTP (404|409)' "$errors"; then
    return 0
  fi
  cat "$errors" >&2
  return 1
}

for organisation in "$@"; do
  fetched="$scratch/$organisation"
  mkdir -p "$fetched"
  repositories="$(gh api --paginate "orgs/$organisation/repos?per_page=100&type=all" \
    --jq '.[] | select(.archived | not) | .name')"
  total=0
  for repository in $repositories; do
    files="$(listing "$organisation" "$repository")"
    count=0
    while IFS=$'\t' read -r name encoded; do
      [ -n "$name" ] || continue
      target="$fetched/$repository/.github/workflows/$name"
      mkdir -p "$(dirname "$target")"
      gh api -H 'Accept: application/vnd.github.raw+json' \
        "repos/$organisation/$repository/contents/.github/workflows/$encoded" > "$target"
      count=$((count + 1))
    done <<<"$files"
    total=$((total + count))
    echo "$organisation/$repository: $count workflow files"
  done
  rm -rf "${corpus:?}/$organisation"
  mkdir -p "$corpus"
  mv "$fetched" "$corpus/$organisation"
  echo "$organisation: $total workflow files in compat/workflows/$organisation"
done

echo "next: cargo run -q -p bosun-cli -- workflow inventory compat/workflows, then commit compat/workflows and compat/inventory"
