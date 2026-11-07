#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: hack/pack-fixture-action.sh [--check] <fixture dir> <output dir>" >&2
  echo "packs the files of <fixture dir> as <output dir>/tarball.tar.gz, byte for byte the same each time," >&2
  echo "and writes its sha256 into <output dir>/manifest.json, whose commit it keeps" >&2
  echo "--check builds nothing into <output dir>: it fails when the committed tarball is not the pack of" >&2
  echo "<fixture dir> or the manifest does not hold the tarball's sha256" >&2
  exit 2
}

check=""
if [ "${1:-}" = "--check" ]; then
  check=1
  shift
fi
[ $# -eq 2 ] || usage
command -v python3 >/dev/null || { echo "hack/pack-fixture-action.sh needs python3 on the PATH" >&2; exit 1; }

python3 -I - "$check" "$1" "$2" <<'PY'
import gzip
import hashlib
import io
import json
import os
import sys
import tarfile

check, fixture, output = sys.argv[1:]
fixture = os.path.normpath(fixture)
manifest_path = os.path.join(output, "manifest.json")
tarball_path = os.path.join(output, "tarball.tar.gz")

if not os.path.isdir(fixture):
    sys.exit("hack/pack-fixture-action.sh: %s is not a directory" % fixture)
with open(manifest_path) as file:
    manifest = json.load(file)
commit = manifest["commit"]


def pack():
    directory = "bosun-fixtures-actions-" + commit[:7] + "/" + os.path.basename(fixture)
    names = []
    for current, _, files in os.walk(fixture):
        for name in files:
            names.append(os.path.relpath(os.path.join(current, name), fixture))
    packed = io.BytesIO()
    with gzip.GzipFile(fileobj=packed, mode="wb", mtime=0, compresslevel=9) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
            for name in sorted(names):
                with open(os.path.join(fixture, name), "rb") as file:
                    content = file.read()
                entry = tarfile.TarInfo(directory + "/" + name)
                entry.size = len(content)
                entry.mode = 0o644
                entry.mtime = 0
                entry.uid = entry.gid = 0
                entry.uname = entry.gname = ""
                archive.addfile(entry, io.BytesIO(content))
    return packed.getvalue()


def digest(content):
    return hashlib.sha256(content).hexdigest()


built = pack()
if not check:
    with open(tarball_path, "wb") as file:
        file.write(built)
    manifest["sha256"] = digest(built)
    with open(manifest_path, "w") as file:
        json.dump(manifest, file, indent=2)
        file.write("\n")
    print("packed %s as %s, sha256 %s" % (fixture, tarball_path, manifest["sha256"]))
    sys.exit(0)

with open(tarball_path, "rb") as file:
    committed = file.read()
if manifest["sha256"] != digest(committed):
    sys.exit(
        "%s holds the sha256 %s, but %s has %s; run hack/pack-fixture-action.sh %s %s"
        % (manifest_path, manifest["sha256"], tarball_path, digest(committed), fixture, output)
    )
if gzip.decompress(committed) != gzip.decompress(built):
    sys.exit(
        "%s is not the pack of %s; run hack/pack-fixture-action.sh %s %s and commit the result"
        % (tarball_path, fixture, fixture, output)
    )
print("%s is the pack of %s" % (tarball_path, fixture))
PY
