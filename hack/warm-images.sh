#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
lock="$root/compat/images.lock"
upstream="$root/target/node-upstream"

usage() {
  echo "usage: hack/warm-images.sh [--lock FILE] [--node-upstream DIRECTORY]" >&2
  exit 2
}

fail() {
  echo "hack/warm-images.sh: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --lock)
      [ $# -ge 2 ] || usage
      lock="$2"
      shift 2
      ;;
    --node-upstream)
      [ $# -ge 2 ] || usage
      upstream="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done

[ -f "$lock" ] || fail "there is no image lock at $lock"

pinned='^[a-z0-9][a-z0-9._/-]*(:[a-zA-Z0-9._-]+)?@sha256:[0-9a-f]{64}$'
refused=0
count=0
references=""
number=0
while IFS= read -r line || [ -n "$line" ]; do
  number=$((number + 1))
  [ -n "$line" ] || continue
  if [[ "$line" =~ $pinned ]]; then
    references="$references $line"
    count=$((count + 1))
  else
    echo "$lock:$number: '$line' is not pinned by digest; write name:tag@sha256:<64 hex digits>" >&2
    refused=1
  fi
done <"$lock"
[ "$refused" -eq 0 ] || fail "$lock has an image that is not pinned by digest; nothing was pulled"
[ "$count" -gt 0 ] || fail "$lock lists no image"

command -v podman >/dev/null 2>&1 || fail "podman is needed to pull the images"
command -v jq >/dev/null 2>&1 || fail "jq is needed to read the image configurations"

extract_node() {
  local reference="$1" configuration architecture version name directory container checksums digest
  configuration="$(podman image inspect "$reference")" || fail "podman cannot inspect $reference"
  architecture="$(jq -r '.[0].Architecture' <<<"$configuration")"
  [ "$architecture" = amd64 ] || fail "$reference is $architecture; its Node is packed for amd64 only"
  version="$(jq -r '.[0].Config.Env[]? | select(startswith("NODE_VERSION=")) | sub("^NODE_VERSION="; "")' \
    <<<"$configuration")"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "$reference has no NODE_VERSION of the form x.y.z"
  name="node-v$version-linux-x64-musl"
  directory="$(mktemp -d "${TMPDIR:-/tmp}/bosun-node.XXXXXX")"
  mkdir -p "$directory/$name/bin" "$upstream/v$version"
  container="$(podman create "$reference")" || fail "podman cannot create a container of $reference"
  if ! podman cp "$container:/usr/local/bin/node" "$directory/$name/bin/node"; then
    podman rm "$container" >/dev/null 2>&1 || true
    fail "$reference has no /usr/local/bin/node"
  fi
  podman rm "$container" >/dev/null
  chmod 755 "$directory/$name/bin/node"
  tar -C "$directory" -cJf "$upstream/v$version/$name.tar.xz" "$name"
  digest="$(python3 -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' \
    "$upstream/v$version/$name.tar.xz")"
  checksums="$upstream/v$version/SHASUMS256.txt"
  touch "$checksums"
  grep -v "  $name.tar.xz\$" "$checksums" >"$checksums.next" || true
  printf '%s  %s.tar.xz\n' "$digest" "$name" >>"$checksums.next"
  mv "$checksums.next" "$checksums"
  rm -rf "$directory"
  echo "packed Node $version of $reference as $name in $upstream"
}

for reference in $references; do
  echo "pulling $reference"
  podman pull --quiet "$reference" >/dev/null || fail "podman could not pull $reference"
  case "$reference" in
    node:24-alpine@*) extract_node "$reference" ;;
  esac
done
echo "warmed $count images from $lock"
