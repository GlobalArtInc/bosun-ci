#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="${1:-$root/Cargo.toml}"

cargo metadata --format-version 1 --all-features --manifest-path "$manifest" | python3 -c "$(cat <<'PY'
import json
import sys

SERVER_SIDE = {"bosun-db", "bosun-server", "bosun-server-bin"}
EXECUTION_SIDE = {"bosun-runner", "bosun-agent"}

ALLOWED = {
    "bosun-types": set(),
    "bosun-expr": {"bosun-types"},
    "bosun-workflow": {"bosun-types", "bosun-expr"},
    "bosun-proto": {"bosun-types"},
    "bosun-db": {"bosun-types"},
    "bosun-blob": set(),
    "bosun-forge": {"bosun-types"},
    "bosun-services": {"bosun-types", "bosun-blob"},
    "bosun-plugin-host": {"bosun-types"},
    "bosun-server": {
        "bosun-types",
        "bosun-expr",
        "bosun-workflow",
        "bosun-proto",
        "bosun-db",
        "bosun-blob",
        "bosun-forge",
        "bosun-services",
        "bosun-plugin-host",
    },
    "bosun-server-bin": {"bosun-types", "bosun-server"},
    "bosun-runner": {"bosun-types", "bosun-expr", "bosun-workflow", "bosun-proto"},
    "bosun-agent": {"bosun-types", "bosun-runner", "bosun-proto"},
    "bosun-cli": {"bosun-types", "bosun-workflow", "bosun-runner", "bosun-services", "bosun-blob"},
    "bosun-wasm": {"bosun-expr", "bosun-workflow"},
    "bosun-testkit": None,
}

UNREACHABLE = {
    "bosun-runner": SERVER_SIDE,
    "bosun-agent": SERVER_SIDE,
    "bosun-cli": SERVER_SIDE,
    "bosun-wasm": SERVER_SIDE,
    "bosun-server": EXECUTION_SIDE,
    "bosun-server-bin": EXECUTION_SIDE,
}

PURE = {"bosun-types", "bosun-expr", "bosun-workflow"}

IO_CRATES = {
    "async-io",
    "async-std",
    "axum",
    "h2",
    "hyper",
    "mio",
    "postgres",
    "reqwest",
    "sea-orm",
    "smol",
    "sqlx",
    "tokio",
    "tokio-postgres",
    "tonic",
    "ureq",
}

meta = json.load(sys.stdin)
names = {package["id"]: package["name"] for package in meta["packages"]}
members = {names[member]: member for member in meta["workspace_members"]}
nodes = {node["id"]: node for node in meta["resolve"]["nodes"]}


def runtime_edges(package_id):
    for dependency in nodes[package_id]["deps"]:
        if any(kind["kind"] in (None, "build") for kind in dependency["dep_kinds"]):
            yield dependency["pkg"]


def reach(package_id):
    seen = set()
    stack = [package_id]
    while stack:
        for next_id in runtime_edges(stack.pop()):
            if next_id not in seen:
                seen.add(next_id)
                stack.append(next_id)
    return {names[package] for package in seen}


problems = []
for name, package_id in sorted(members.items()):
    if name not in ALLOWED:
        problems.append(
            f"{name} has no rule in hack/check-deps.sh; add one together with its row in "
            "docs/design/01-overview.md"
        )
        continue
    internal = {names[dep] for dep in runtime_edges(package_id)} & members.keys()
    if name != "bosun-testkit" and "bosun-testkit" in internal:
        problems.append(f"{name} depends on bosun-testkit outside dev-dependencies")
    allowed = ALLOWED[name]
    if allowed is not None:
        for dep in sorted(internal - allowed - {"bosun-testkit"}):
            problems.append(f"{name} depends directly on {dep}, which its rule does not allow")
    reached = reach(package_id)
    for dep in sorted(reached & UNREACHABLE.get(name, set())):
        problems.append(f"{name} reaches {dep} through its dependencies")
    if name in PURE:
        for dep in sorted(reached & IO_CRATES):
            problems.append(f"{name} reaches {dep}, an async runtime or I/O crate")

for problem in problems:
    print(problem, file=sys.stderr)
if problems:
    sys.exit(1)
print(f"checked {len(members)} workspace crates: dependency rules hold")
PY
)"
