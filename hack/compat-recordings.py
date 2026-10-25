#!/usr/bin/env python3
import argparse
import base64
import binascii
import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CASES = ROOT / "contracts" / "fixtures" / "expr"
WORKFLOW = ROOT / "compat" / "recordings" / "expr.yml"
RECORDINGS = ROOT / "compat" / "recordings" / "expr.json"
AVAILABILITY = "availability.json"
SUPPLIED = ("env", "matrix")
STEPS_PER_JOB = 100
NAME = re.compile(r"^[A-Za-z0-9._-]+\[[0-9]+\]$")
MARKER = re.compile(r"bosun-(result|outcome) (\S+) ([0-9a-f]{12}) (\S*)")


class Refusal(Exception):
    pass


def shown(path):
    try:
        return str(path.resolve().relative_to(ROOT))
    except ValueError:
        return str(path)


def load(path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise Refusal(f"{shown(path)}: {error}")


def compact(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def cases(directory):
    found = []
    paths = sorted(directory.glob("*.json"))
    if not paths:
        raise Refusal(f"{shown(directory)} holds no cases")
    for path in paths:
        if path.name == AVAILABILITY:
            continue
        listed = load(path)
        if not isinstance(listed, list):
            raise Refusal(f"{shown(path)}: a list of cases is expected")
        for index, case in enumerate(listed):
            if not isinstance(case, dict) or "contexts" not in case:
                continue
            if case.get("hosted") is False:
                continue
            name = f"{path.name}[{index}]"
            if not NAME.match(name):
                raise Refusal(f"{name}: a case file is named with letters, digits, `.`, `_` and `-`")
            kind = "expression" if "expression" in case else "template"
            contexts = case["contexts"]
            for context in contexts:
                if context not in SUPPLIED:
                    raise Refusal(
                        f"{name} supplies `{context}`, which a hosted runner cannot be given; "
                        'mark it with "hosted": false'
                    )
            text = case[kind]
            digest = hashlib.sha256(compact([kind, text, contexts]).encode("utf-8")).hexdigest()
            found.append(
                {
                    "name": name,
                    "digest": digest[:12],
                    "kind": kind,
                    "text": text,
                    "contexts": contexts,
                }
            )
    return found


def quoted(text):
    escaped = []
    for character in text:
        code = ord(character)
        if character == '"':
            escaped.append('\\"')
        elif character == "\\":
            escaped.append("\\\\")
        elif character == "\n":
            escaped.append("\\n")
        elif character == "\t":
            escaped.append("\\t")
        elif character == "\r":
            escaped.append("\\r")
        elif (
            code < 0x20
            or 0x7F <= code <= 0x9F
            or 0xD800 <= code <= 0xDFFF
            or code in (0x2028, 0x2029, 0xFEFF)
        ):
            escaped.append(f"\\u{code:04x}")
        else:
            escaped.append(character)
    return '"' + "".join(escaped) + '"'


def flow(value):
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return json.dumps(value)
    if isinstance(value, str):
        return quoted(value)
    if isinstance(value, list):
        return "[" + ", ".join(flow(item) for item in value) + "]"
    return "{" + ", ".join(f"{quoted(key)}: {flow(item)}" for key, item in value.items()) + "}"


def evaluated(case):
    if case["kind"] == "expression":
        return "[${{ toJSON(" + case["text"] + ") }}]"
    return "[" + case["text"] + "]"


def jobs(found):
    groups = {}
    for case in found:
        groups.setdefault(compact(case["contexts"]), []).append(case)
    chunks = []
    for members in groups.values():
        for start in range(0, len(members), STEPS_PER_JOB):
            chunks.append(members[start : start + STEPS_PER_JOB])
    return chunks


def workflow(found):
    lines = [
        "name: Expression recordings",
        "",
        "on:",
        "  push:",
        "    branches: [main]",
        "  workflow_dispatch:",
        "",
        "permissions: {}",
        "",
        "jobs:",
    ]
    step = 0
    for number, members in enumerate(jobs(found), 1):
        contexts = members[0]["contexts"]
        lines += [
            f"  cases-{number}:",
            f"    name: Expression cases {number}",
            "    runs-on: ubuntu-latest",
            "    timeout-minutes: 30",
        ]
        if "matrix" in contexts:
            lines += ["    strategy:", "      matrix:"]
            lines += [f"        {quoted(name)}: [{flow(value)}]" for name, value in contexts["matrix"].items()]
        if "env" in contexts:
            lines += ["    env:"]
            lines += [f"      {quoted(name)}: {quoted(value)}" for name, value in contexts["env"].items()]
        lines += ["    steps:"]
        outcomes = []
        for case in members:
            step += 1
            identifier = f"case-{step}"
            lines += [
                f"      - name: {case['name']}",
                f"        id: {identifier}",
                "        continue-on-error: true",
                "        env:",
                f"          BOSUN_CASE: {quoted(evaluated(case))}",
                "        run: |",
                f"          printf 'bosun-%s %s %s %s\\n' result '{case['name']}' {case['digest']} "
                '"$(printf \'%s\' "$BOSUN_CASE" | base64 -w0)"',
            ]
            outcomes.append(
                f"          printf 'bosun-%s %s %s %s\\n' outcome '{case['name']}' {case['digest']} "
                f"'${{{{ steps.{identifier}.outcome }}}}'"
            )
        lines += ["      - name: Outcomes", "        if: always()", "        run: |"]
        lines += outcomes
        lines += [""]
    return "\n".join(lines[:-1]) + "\n"


def generate(arguments):
    text = workflow(cases(arguments.cases))
    if arguments.check:
        try:
            committed = arguments.workflow.read_text(encoding="utf-8")
        except OSError as error:
            raise Refusal(f"{shown(arguments.workflow)}: {error}")
        if committed != text:
            raise Refusal(
                f"{shown(arguments.workflow)} is not the workflow of the cases in "
                f"{shown(arguments.cases)}; run hack/compat-recordings.py generate and commit it"
            )
        print(f"{shown(arguments.workflow)} is up to date")
        return
    arguments.workflow.parent.mkdir(parents=True, exist_ok=True)
    arguments.workflow.write_text(text, encoding="utf-8")
    print(f"wrote {shown(arguments.workflow)}")


def markers(logs):
    found = {"result": {}, "outcome": {}}
    for path in logs:
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as error:
            raise Refusal(f"{shown(path)}: {error}")
        for line in text.splitlines():
            match = MARKER.search(line)
            if not match:
                continue
            kind, name, digest, value = match.groups()
            seen = found[kind].setdefault((name, digest), value)
            if seen != value:
                raise Refusal(f"{shown(path)}: the log holds two different {kind}s of {name}")
    return found


def output(case, encoded):
    try:
        text = base64.b64decode(encoded, validate=True).decode("utf-8")
    except (binascii.Error, UnicodeDecodeError) as error:
        raise Refusal(f"the result of {case['name']} is not base64 of UTF-8 text: {error}")
    if not (text.startswith("[") and text.endswith("]")):
        raise Refusal(f"the result of {case['name']} is not between its markers: {text!r}")
    return text[1:-1]


def recordings(found, logged):
    known = {(case["name"], case["digest"]) for case in found}
    for kind in ("result", "outcome"):
        for name, digest in logged[kind]:
            if (name, digest) not in known:
                raise Refusal(
                    f"the log records {name} as a case that this commit does not hold; import the "
                    "log of a run of the workflow that `generate` writes for this commit"
                )
    recorded = []
    for case in found:
        key = (case["name"], case["digest"])
        outcome = logged["outcome"].get(key)
        result = logged["result"].get(key)
        entry = {"case": case["name"], case["kind"]: case["text"], "contexts": case["contexts"]}
        if outcome == "success" and result is not None:
            entry["output"] = output(case, result)
        elif outcome == "failure" and result is None:
            entry["failed"] = True
        elif outcome is None:
            raise Refusal(
                f"the log holds no outcome of {case['name']}; import the log of a whole run of the "
                "workflow that `generate` writes for this commit"
            )
        else:
            raise Refusal(
                f"{case['name']} ended with the outcome {outcome} "
                + ("and printed a result" if result is not None else "without a result")
            )
        recorded.append(entry)
    return recorded


def record(arguments):
    found = cases(arguments.cases)
    recorded = recordings(found, markers(arguments.logs))
    text = json.dumps(recorded, ensure_ascii=False, indent=2) + "\n"
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(text, encoding="utf-8")
    failed = sum(1 for entry in recorded if entry.get("failed"))
    print(f"wrote {shown(arguments.output)}: {len(recorded)} recordings, {failed} of them failed")


def main():
    parser = argparse.ArgumentParser(
        prog="hack/compat-recordings.py",
        description="Record the expression cases on GitHub's hosted runners",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    generating = commands.add_parser(
        "generate", help="write the workflow that prints the result of every case"
    )
    generating.add_argument("--cases", type=Path, default=CASES)
    generating.add_argument("--workflow", type=Path, default=WORKFLOW)
    generating.add_argument(
        "--check", action="store_true", help="fail when the committed workflow is stale"
    )
    generating.set_defaults(run=generate)
    importing = commands.add_parser(
        "import", help="turn the log of a run of that workflow into recordings"
    )
    importing.add_argument("logs", type=Path, nargs="+", metavar="LOG")
    importing.add_argument("--cases", type=Path, default=CASES)
    importing.add_argument("--output", type=Path, default=RECORDINGS)
    importing.set_defaults(run=record)
    arguments = parser.parse_args()
    try:
        arguments.run(arguments)
    except Refusal as refusal:
        print(f"{parser.prog}: {refusal}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
