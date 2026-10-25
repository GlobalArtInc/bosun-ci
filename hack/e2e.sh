#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

scenarios="two-job-workflow failing-step cancel-sleep labels cross-replica-cancel server-restart"
scenarios="$scenarios step-environment step-outputs journal-replay project-settings pools tokens agents runs"
scenarios="$scenarios log-search secrets lint admission dispatch-inputs interface"

usage() {
  echo "usage: hack/e2e.sh [scenario...]" >&2
  echo "scenarios: $scenarios" >&2
  echo "BOSUN_TEST_DATABASE_URL names a Postgres server where the script creates its database" >&2
  exit 2
}

selected="${*:-$scenarios}"
for name in $selected; do
  case " $scenarios " in
    *" $name "*) ;;
    *) echo "there is no scenario $name" >&2; usage ;;
  esac
done

if [ -z "${BOSUN_TEST_DATABASE_URL:-}" ]; then
  usage
fi
postgres="$BOSUN_TEST_DATABASE_URL"
for name in $(compgen -e); do
  case "$name" in
    BOSUN_*) unset "$name" ;;
  esac
done
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="$NO_PROXY"

tools="cargo curl jq psql python3"
case " $selected " in
  *" interface "*) tools="$tools corepack node" ;;
esac
for tool in $tools; do
  command -v "$tool" >/dev/null || { echo "hack/e2e.sh needs $tool on the PATH" >&2; exit 1; }
done
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0

work="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/bosun-e2e.XXXXXX")" && pwd -P)"
export XDG_CONFIG_HOME="$work/config"
mkdir -p "$work/logs" "$work/agents" "$work/workflows" "$work/out" "$work/gates" "$work/store"
database="bosun_e2e_$(python3 -c 'import secrets; print(secrets.token_hex(8))')"
databases=""
current="setup"
started=""
serves=0

finish() {
  local status=$?
  set +e
  local entry newest_first=""
  for entry in $started; do
    newest_first="$entry $newest_first"
  done
  for entry in $newest_first; do
    stop_process "${entry%%=*}" "${entry#*=}" 30 >/dev/null 2>&1
  done
  if [ "$status" -ne 0 ]; then
    for log in "$work"/logs/*.log; do
      [ -s "$log" ] || continue
      echo "---- the last lines of ${log#"$work"/} ----" >&2
      tail -n 40 "$log" >&2
    done
  fi
  for name in $databases; do
    psql "$postgres" -qXc "DROP DATABASE IF EXISTS $name WITH (FORCE)" >/dev/null 2>&1 ||
      echo "the database $name could not be dropped" >&2
  done
  rm -rf "$work"
  exit "$status"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

fail() {
  echo "scenario $current FAILED: $*" >&2
  exit 1
}

now_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

spawn() {
  local name="$1" log="$2"
  shift 2
  "$@" >"$log" 2>&1 &
  spawned=$!
  started="$started $name=$spawned"
}

forget() {
  local kept="" entry
  for entry in $started; do
    [ "${entry%%=*}" = "$1" ] || kept="$kept $entry"
  done
  started="$kept"
}

pid_of() {
  local entry
  for entry in $started; do
    if [ "${entry%%=*}" = "$1" ]; then
      echo "${entry#*=}"
      return
    fi
  done
}

exited_within() {
  local pid="$1" seconds="$2" deadline
  deadline=$((SECONDS + seconds))
  while kill -0 "$pid" 2>/dev/null; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.1
  done
}

reap() {
  if wait "$1"; then
    reaped=0
  else
    reaped=$?
  fi
}

stop_process() {
  local name="$1" pid="$2" seconds="$3"
  forget "$name"
  kill -TERM "$pid" 2>/dev/null || return 0
  if ! exited_within "$pid" "$seconds"; then
    kill -KILL "$pid" 2>/dev/null
    echo "$name did not stop within ${seconds}s of SIGTERM and was killed" >&2
  fi
  wait "$pid" 2>/dev/null || true
}

await_line() {
  local file="$1" pattern="$2" seconds="$3" what="$4" deadline
  deadline=$((SECONDS + seconds))
  until grep -qE -- "$pattern" "$file" 2>/dev/null; do
    [ "$SECONDS" -lt "$deadline" ] ||
      fail "$what did not happen within ${seconds}s; $file holds: $(cat "$file" 2>/dev/null)"
    sleep 0.1
  done
}

server() {
  env "${settings[@]}" "$bin/bosun-server" --config "$work/server.toml" "$@"
}

create_database() {
  psql "$postgres" -qXv ON_ERROR_STOP=1 -c "CREATE DATABASE $1" >/dev/null ||
    fail "the database $1 could not be created"
  databases="$databases $1"
  created_url="$(python3 -c '
import sys, urllib.parse as u
p = u.urlsplit(sys.argv[1])
print(u.urlunsplit(p._replace(path="/" + sys.argv[2])))
' "$postgres" "$1")"
}

ready() {
  local what="$1" log="$2" deadline
  await_line "$log" '"message":"listening"' 60 "$what listening"
  address="$(jq -Rr 'fromjson? | select(.message == "listening") | .address' "$log" | head -n 1)"
  [ -n "$address" ] || fail "$what logged no address: $(cat "$log")"
  deadline=$((SECONDS + 30))
  until curl -fsS --noproxy '*' "http://$address/readyz" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$deadline" ] || fail "$what at $address did not become ready within 30s"
    sleep 0.2
  done
}

serve() {
  local replica="$1" listen="$2" log
  serves=$((serves + 1))
  log="$work/logs/server-$replica.$serves.log"
  spawn "server-$replica" "$log" env "${settings[@]}" BOSUN_SERVER_LISTEN="$listen" \
    "$bin/bosun-server" --config "$work/server.toml" serve
  ready "replica $replica" "$log"
}

first_start() {
  local theme="$1" data="$work/first-start-$1"
  create_database "${database}_$theme"
  mkdir -p "$data"
  spawn "first-start-$theme" "$work/logs/first-start-$theme.log" \
    env BOSUN_DATABASE_URL="$created_url" BOSUN_DATABASE_MAX_CONNECTIONS=4 BOSUN_KEYS_KEK="$kek" \
    BOSUN_SERVER_DATA_DIR="$data" "$bin/bosun-server" --config "$work/server.toml" serve
  ready "the $theme first start" "$work/logs/first-start-$theme.log"
  [ -s "$data/admin.password" ] || fail "the $theme first start wrote no admin.password"
  first_starts="$(jq -c --arg theme "$theme" --arg url "http://$address" \
    --rawfile password "$data/admin.password" \
    '. + {($theme): {url: $url, password: ($password | rtrimstr("\n"))}}' <<<"$first_starts")"
}

cli() {
  "$bin/bosun" "$@"
}

api() {
  local method="$1" path="$2"
  shift 2
  curl -sS --noproxy '*' --fail-with-body -X "$method" -H "Authorization: Bearer $owner_token" \
    -H 'Content-Type: application/json' "$@" "http://$address_a$path"
}

push() {
  local name="$1" file="$work/workflows/$1.yml"
  cat >"$file"
  cli --profile a workflow push e2e/m0 "$file" >"$work/out/push-$name.txt" 2>&1 ||
    fail "bosun workflow push of $name failed: $(cat "$work/out/push-$name.txt")"
  grep -q "^created the workflow $name " "$work/out/push-$name.txt" ||
    fail "bosun workflow push of $name did not create it: $(cat "$work/out/push-$name.txt")"
}

watch() {
  local profile="$1" workflow="$2"
  watched="$work/out/watch-$workflow.txt"
  spawn "watch-$workflow" "$watched" \
    "$bin/bosun" --profile "$profile" workflow dispatch "e2e/m0/$workflow" --watch
  watcher=$spawned
  await_line "$watched" '^run [0-9a-f-]+ of the workflow ' 30 "the dispatch of $workflow"
  run_id="$(sed -n 's/^run \([0-9a-f-]*\) of the workflow .*/\1/p' "$watched" | head -n 1)"
}

watch_ended() {
  local seconds="$1" workflow="$2"
  exited_within "$watcher" "$seconds" ||
    fail "bosun workflow dispatch --watch of $workflow did not end within ${seconds}s: $(cat "$watched")"
  forget "watch-$workflow"
  reap "$watcher"
}

job_id() {
  sed -n "s/^$1: [a-z_ ]*, job \([0-9a-f-]*\)\$/\1/p" "$watched" | head -n 1
}

expect_line() {
  grep -qxF -- "$2" "$1" || fail "$3: there is no line '$2' in: $(cat "$1")"
}

expect_ended() {
  grep -qxE -- "$2(, job [0-9a-f-]+)?" "$1" || fail "$3: there is no line '$2' in: $(cat "$1")"
}

expect_order() {
  local file="$1" previous=0 line number
  shift
  for line in "$@"; do
    number="$(grep -nxF -- "$line" "$file" | head -n 1 | cut -d: -f1)"
    [ -n "$number" ] || fail "there is no line '$line' in: $(cat "$file")"
    [ "$number" -gt "$previous" ] || fail "'$line' comes too early in: $(cat "$file")"
    previous="$number"
  done
}

counted_lines() {
  local from="$1" to="$2" prefix="$3" i
  for i in $(seq "$from" "$to"); do
    echo "${prefix}line $i"
  done
}

begin() {
  current="$1"
  echo "scenario $1: $2"
}

passed() {
  echo "scenario $current: passed"
}

scenario_two_job_workflow() {
  begin two-job-workflow \
    "a two-job workflow runs end to end through bosun workflow dispatch --watch with live logs"
  local gate="$work/gates/two-jobs"
  push two-jobs <<EOF
name: two-jobs
on: workflow_dispatch
jobs:
  build:
    runs-on: [self-hosted, alpha]
    steps:
      - name: Build
        run: |
          echo "build begins"
          while [ ! -e "$gate" ]; do sleep 0.1; done
          echo "build ends"
  test:
    runs-on: [self-hosted, alpha]
    needs: build
    steps:
      - name: Test
        run: echo "test runs after build"
EOF
  watch a two-jobs
  await_line "$watched" '^build \| build begins$' 60 "the first line of build reaching the terminal"
  if grep -qE '^build \| build ends$|^build: completed' "$watched"; then
    fail "the watch printed the end of build before the step could finish: $(cat "$watched")"
  fi
  touch "$gate"
  watch_ended 60 two-jobs
  [ "$reaped" -eq 0 ] ||
    fail "bosun workflow dispatch --watch exited with $reaped for a successful run: $(cat "$watched")"
  expect_line "$watched" "build: completed success" "the first job did not succeed"
  expect_line "$watched" "test: completed success" "the second job did not succeed"
  expect_order "$watched" "build | build begins" "build | build ends" "build: completed success"
  expect_order "$watched" "build | build ends" "test | test runs after build" "test: completed success"
  [ "$(tail -n 1 "$watched")" = "run $run_id: completed success" ] ||
    fail "the watch did not end with the run's conclusion: $(cat "$watched")"
  passed
}

scenario_failing_step() {
  begin failing-step "a failing step fails its job, skips the dependent job and fails the run"
  push failing <<'EOF'
name: failing
on: workflow_dispatch
jobs:
  build:
    runs-on: [self-hosted, alpha]
    steps:
      - run: |
          echo "about to fail"
          exit 3
      - run: echo "the step after the failure runs"
  test:
    runs-on: [self-hosted, alpha]
    needs: build
    steps:
      - run: echo "the dependent job runs"
EOF
  watch a failing
  watch_ended 60 failing
  [ "$reaped" -eq 3 ] ||
    fail "bosun workflow dispatch --watch exited with $reaped for a failed run, not 3: $(cat "$watched")"
  expect_line "$watched" "build | about to fail" "the failing step's output is missing"
  expect_line "$watched" "build: completed failure" "the job with the failing step did not fail"
  expect_line "$watched" "test: completed skipped" "the dependent job was not skipped"
  [ "$(tail -n 1 "$watched")" = "run $run_id: completed failure" ] ||
    fail "the run did not fail: $(cat "$watched")"
  if grep -qE 'the step after the failure runs|the dependent job runs' "$watched"; then
    fail "a step ran after the failure: $(cat "$watched")"
  fi
  passed
}

cancel_within_ten_seconds() {
  local workflow="$1" label="$2" watching="$3" cancelling="$4" pid started_at elapsed
  push "$workflow" <<EOF
name: $workflow
on: workflow_dispatch
jobs:
  sleep:
    runs-on: [self-hosted, $label]
    steps:
      - run: |
          echo "sleeping as \$\$"
          exec sleep 600
EOF
  watch "$watching" "$workflow"
  await_line "$watched" '^sleep \| sleeping as [0-9]+$' 60 "the start of sleep 600"
  pid="$(sed -n 's/^sleep | sleeping as \([0-9]*\)$/\1/p' "$watched" | head -n 1)"
  kill -0 "$pid" 2>/dev/null || fail "the sleep 600 of the step, process $pid, is not running"
  started_at="$(now_ms)"
  cli --profile "$cancelling" run cancel "$run_id" >"$work/out/cancel-$workflow.txt" 2>&1 ||
    fail "bosun run cancel failed: $(cat "$work/out/cancel-$workflow.txt")"
  watch_ended 10 "$workflow"
  elapsed=$(($(now_ms) - started_at))
  [ "$elapsed" -le 10000 ] || fail "the run ended ${elapsed}ms after bosun run cancel, more than 10 seconds"
  [ "$(tail -n 1 "$watched")" = "run $run_id: completed cancelled" ] ||
    fail "the run did not end cancelled: $(cat "$watched")"
  expect_line "$watched" "sleep: completed cancelled" "the job did not end cancelled"
  if kill -0 "$pid" 2>/dev/null; then
    fail "the sleep 600 of the step, process $pid, still runs after the run ended"
  fi
  echo "the run ended ${elapsed}ms after bosun run cancel"
}

scenario_cancel_sleep() {
  begin cancel-sleep "bosun run cancel stops a running sleep 600 within 10 seconds"
  cancel_within_ten_seconds sleep-alpha alpha a a
  passed
}

scenario_cross_replica_cancel() {
  begin cross-replica-cancel "a cancel through one replica reaches an agent connected to the other"
  cancel_within_ten_seconds sleep-beta beta b a
  passed
}

scenario_labels() {
  begin labels "agents receive only jobs their labels satisfy"
  local run jobs alpha beta both idle state deadline
  push labels <<'EOF'
name: labels
on: workflow_dispatch
jobs:
  on-alpha:
    runs-on: [self-hosted, alpha]
    steps:
      - run: echo "workspace $GITHUB_WORKSPACE"
  on-beta:
    runs-on: [self-hosted, beta]
    steps:
      - run: echo "workspace $GITHUB_WORKSPACE"
  on-both:
    runs-on: [self-hosted, alpha, beta]
    steps:
      - run: echo "an agent with both labels ran this"
EOF
  run="$(cli --profile a --json workflow dispatch e2e/m0/labels | jq -r .id)" ||
    fail "bosun workflow dispatch of labels failed"
  jobs="$(api GET "/api/v1/runs/$run/attempts/1/jobs")" ||
    fail "the jobs of run $run could not be read: $jobs"
  alpha="$(jq -r '.items[] | select(.key == "on-alpha") | .id' <<<"$jobs")"
  beta="$(jq -r '.items[] | select(.key == "on-beta") | .id' <<<"$jobs")"
  both="$(jq -r '.items[] | select(.key == "on-both") | .id' <<<"$jobs")"
  for job in "alpha=$alpha" "beta=$beta"; do
    local label="${job%%=*}" id="${job#*=}" out="$work/out/labels-${job%%=*}.txt"
    spawn "logs-$label" "$out" "$bin/bosun" --profile b job logs "$id" --follow
    exited_within "$spawned" 60 ||
      fail "bosun job logs --follow of on-$label did not end within 60s: $(cat "$out")"
    forget "logs-$label"
    reap "$spawned"
    [ "$reaped" -eq 0 ] ||
      fail "bosun job logs --follow of on-$label exited with $reaped: $(cat "$out")"
    grep -qxF '==> step 1: Run echo "workspace $GITHUB_WORKSPACE"' "$out" ||
      fail "the log of on-$label has no heading of its step: $(cat "$out")"
    grep -qE "^[^ ]+ workspace $work/agents/$label/" "$out" ||
      fail "on-$label did not run on the agent labelled $label: $(cat "$out")"
  done
  sleep 3
  cli --profile a --json agent list >"$work/out/labels-agents.json" || fail "bosun agent list failed"
  idle='[.[] | select(.online and .execution_id == null and (.name == "e2e-alpha" or .name == "e2e-beta"))] | length'
  [ "$(jq "$idle" "$work/out/labels-agents.json")" -eq 2 ] ||
    fail "an agent took the job no agent's labels satisfy: $(cat "$work/out/labels-agents.json")"
  state="$(api GET "/api/v1/jobs/$both" | jq -r '.status + " " + (.execution_id // "none")')"
  [ "$state" = "queued none" ] || fail "the job that needs both labels left the queue: it is $state"
  cli --profile a run cancel "$run" >/dev/null ||
    fail "bosun run cancel of the run with the unplaceable job failed"
  deadline=$((SECONDS + 30))
  until [ "$state" = "completed cancelled" ]; do
    [ "$SECONDS" -lt "$deadline" ] ||
      fail "the run with the unplaceable job did not end cancelled: it is $state"
    sleep 0.2
    state="$(api GET "/api/v1/runs/$run" | jq -r '.status + " " + (.conclusion // "")')"
  done
  passed
}

scenario_server_restart() {
  begin server-restart "a server restart in the middle of a job loses and duplicates no log line"
  local gate="$work/gates/restart" job expected
  push restart <<EOF
name: restart
on: workflow_dispatch
jobs:
  count:
    runs-on: [self-hosted, beta]
    steps:
      - name: Count
        run: |
          for i in \$(seq 1 20); do echo "line \$i"; done
          while [ ! -e "$gate" ]; do sleep 0.1; done
          for i in \$(seq 21 40); do echo "line \$i"; sleep 0.05; done
EOF
  watch b restart
  await_line "$watched" '^count \| line 20$' 60 "line 20 reaching the terminal"
  job="$(job_id count)"
  [ -n "$job" ] || fail "the watch named no job: $(cat "$watched")"
  stop_process server-b "$(pid_of server-b)" 30
  touch "$gate"
  sleep 2
  serve b "$address_b"
  watch_ended 120 restart
  [ "$reaped" -eq 0 ] || fail "bosun workflow dispatch --watch exited with $reaped: $(cat "$watched")"
  grep -q 'the connection to the server was lost' "$work/logs/agent-beta.log" ||
    fail "the agent never lost its connection, so the restart did not interrupt the job"
  expected="$(counted_lines 1 40 "count | ")"
  [ "$(grep '^count | ' "$watched")" = "$expected" ] ||
    fail "the watch did not print lines 1 to 40 exactly once each: $(cat "$watched")"
  cli --profile b job logs "$job" --step 1 >"$work/out/restart-log.txt" 2>&1 ||
    fail "bosun job logs failed: $(cat "$work/out/restart-log.txt")"
  [ "$(cut -d' ' -f2- "$work/out/restart-log.txt")" = "$(counted_lines 1 40 "")" ] ||
    fail "the stored log does not hold lines 1 to 40 once each: $(cat "$work/out/restart-log.txt")"
  passed
}

scenario_step_environment() {
  begin step-environment \
    "a step sees the variables of the dispatch fixture with the values of the run, and the payload"
  local dump="$work/out/environment.json" event="$work/out/event.json" run job payload
  push backup <<EOF
name: Nightly backup
on: workflow_dispatch
jobs:
  backup:
    runs-on: [self-hosted, alpha]
    steps:
      - run: |
          python3 -c 'import json, os, sys; json.dump(dict(os.environ), open(sys.argv[1], "w"))' "$dump"
          cp "\$GITHUB_EVENT_PATH" "$event"
EOF
  watch a backup
  watch_ended 60 backup
  [ "$reaped" -eq 0 ] ||
    fail "bosun workflow dispatch --watch exited with $reaped: $(cat "$watched")"
  run="$(api GET "/api/v1/runs/$run_id")" || fail "run $run_id could not be read: $run"
  job="$(job_id backup)"
  [ -n "$job" ] || fail "the watch named no job: $(cat "$watched")"
  payload="$(psql "$database_url" -XAtc "SELECT trigger -> 'payload' FROM bosun.run WHERE id = '$run_id'")" ||
    fail "the payload of run $run_id could not be read"
  python3 - "$root/contracts/fixtures/github/workflow-dispatch-without-repository.json" "$dump" \
    "$event" "$run" "$job" "$payload" "$work/agents/alpha" >"$work/out/environment.txt" 2>&1 <<'PY' ||
import json
import platform
import re
import sys

fixture_path, dump_path, event_path, run_text, job, payload, agent = sys.argv[1:]
fixture = json.load(open(fixture_path))["environment"]
seen = json.load(open(dump_path))
run = json.loads(run_text)
temp = agent + "/work/_temp"
commands = temp + "/_runner_file_commands/"
step = re.fullmatch(re.escape(commands) + "set_env_([0-9a-f-]{36})", seen.get("GITHUB_ENV", ""))
if step is None:
    sys.exit("GITHUB_ENV is %r" % seen.get("GITHUB_ENV"))
step = step.group(1)
arch = {"x86_64": "X64", "amd64": "X64", "arm64": "ARM64", "aarch64": "ARM64"}
of_the_run = {
    "BOSUN_JOB_ID": job,
    "BOSUN_RUN_ID": run["id"],
    "BOSUN_RUN_URL": "",
    "BOSUN_URL": "",
    "GITHUB_ACTOR": "e2e",
    "GITHUB_EVENT_PATH": temp + "/_github_workflow/event.json",
    "GITHUB_RETENTION_DAYS": "90",
    "GITHUB_RUN_ATTEMPT": str(run["attempt"]),
    "GITHUB_RUN_ID": str(run["global_number"]),
    "GITHUB_RUN_NUMBER": str(run["number"]),
    "GITHUB_TRIGGERING_ACTOR": "e2e",
    "GITHUB_WORKSPACE": agent + "/work/e2e.m0/m0",
    "RUNNER_ARCH": arch[platform.machine().lower()],
    "RUNNER_NAME": "e2e-alpha",
    "RUNNER_OS": {"Linux": "Linux", "Darwin": "macOS"}[platform.system()],
    "RUNNER_TEMP": temp,
    "RUNNER_TOOL_CACHE": agent + "/tools",
    "RUNNER_WORKSPACE": agent + "/work/e2e.m0",
}
for variable, prefix in [
    ("GITHUB_ENV", "set_env_"),
    ("GITHUB_OUTPUT", "set_output_"),
    ("GITHUB_PATH", "add_path_"),
    ("GITHUB_STATE", "save_state_"),
    ("GITHUB_STEP_SUMMARY", "step_summary_"),
]:
    of_the_run[variable] = commands + prefix + step
stray = sorted(set(of_the_run) - set(fixture))
if stray:
    sys.exit("the fixture has no variables %s" % stray)
wrong = [
    "%s: expected %r, the step saw %r" % (name, of_the_run.get(name, value), seen.get(name))
    for name, value in sorted(fixture.items())
    if seen.get(name) != of_the_run.get(name, value)
]
if wrong:
    sys.exit("\n".join(wrong))
if json.load(open(event_path)) != json.loads(payload):
    sys.exit("GITHUB_EVENT_PATH holds %s, the payload of the run is %s" % (open(event_path).read(), payload))
print("the step saw the %d variables of the fixture and the payload of the run" % len(fixture))
PY
    fail "the environment of the step differs: $(cat "$work/out/environment.txt")"
  cat "$work/out/environment.txt"
  passed
}

scenario_step_outputs() {
  begin step-outputs \
    "an output a step writes, also as a heredoc, reaches a later step, the job outputs and the needs of a dependent job"
  local job outputs
  push outputs <<'YAML'
name: outputs
on: workflow_dispatch
jobs:
  build:
    runs-on: [self-hosted, alpha]
    outputs:
      version: ${{ steps.version.outputs.version }}
      notes: ${{ steps.version.outputs.notes }}
    steps:
      - id: version
        run: |
          echo "version=1.2.3" >> "$GITHUB_OUTPUT"
          {
            echo "notes<<END"
            echo "first line"
            echo "second line"
            echo "END"
          } >> "$GITHUB_OUTPUT"
      - run: echo "the next step saw ${{ steps.version.outputs.version }}"
  test:
    runs-on: [self-hosted, alpha]
    needs: build
    steps:
      - run: |
          echo "the dependent job saw ${{ needs.build.outputs.version }}"
          echo '${{ toJSON(needs.build.outputs.notes) }}'
YAML
  watch a outputs
  watch_ended 60 outputs
  [ "$reaped" -eq 0 ] ||
    fail "bosun workflow dispatch --watch exited with $reaped: $(cat "$watched")"
  expect_line "$watched" "build | the next step saw 1.2.3" "a later step did not see the output"
  expect_line "$watched" "test | the dependent job saw 1.2.3" "the dependent job did not see the output"
  expect_line "$watched" 'test | "first line\nsecond line"' "the dependent job did not see the heredoc"
  job="$(job_id build)"
  [ -n "$job" ] || fail "the watch named no job: $(cat "$watched")"
  outputs="$(api GET "/api/v1/jobs/$job" | jq -cS .outputs)" || fail "the job $job could not be read"
  [ "$outputs" = '{"notes":"first line\nsecond line","version":"1.2.3"}' ] ||
    fail "the job stored the outputs $outputs"
  passed
}

scenario_journal_replay() {
  begin journal-replay "projected statuses equal the replay of the journal in the property test"
  local test="tests::journal::projected_statuses_always_equal_a_replay_of_the_journal"
  BOSUN_TEST_DATABASE_URL="$postgres" cargo test --manifest-path "$root/Cargo.toml" --workspace \
    --lib "$test" -- --exact >"$work/out/journal.txt" 2>&1 ||
    fail "the property test failed: $(tail -n 40 "$work/out/journal.txt")"
  grep -qF "test $test ... ok" "$work/out/journal.txt" ||
    fail "the property test did not run: $(tail -n 20 "$work/out/journal.txt")"
  passed
}

scenario_project_settings() {
  begin project-settings "bosun project settings prints the settings and changes them with --set"
  local shown="$work/out/settings.json" changed="$work/out/settings-changed.txt"
  local refused="$work/out/settings-refused.txt"
  api POST /api/v1/projects --data '{"organisation": "e2e", "slug": "m1-settings", "name": "Settings"}' \
    >/dev/null || fail "the project of the scenario could not be created"
  cli --profile a --json project settings e2e/m1-settings >"$shown" 2>&1 ||
    fail "bosun project settings failed: $(cat "$shown")"
  [ "$(jq -S -c . "$shown")" = "$(jq -S -c . "$root/contracts/fixtures/api/project-settings.json")" ] ||
    fail "the settings of a new project are not the defaults: $(cat "$shown")"
  cli --profile b project settings e2e/m1-settings --set fork_policy=approval >"$changed" 2>&1 ||
    fail "bosun project settings --set fork_policy=approval failed: $(cat "$changed")"
  expect_line "$changed" "SETTING            VALUE" "the printed settings have no heading"
  expect_line "$changed" "fork_policy        approval" "the printed settings do not hold the change"
  expect_line "$changed" "report             checks" "the printed settings lost a setting"
  [ "$(api GET /api/v1/projects/e2e/m1-settings/settings | jq -r .fork_policy)" = approval ] ||
    fail "the change did not reach the server"
  if cli --profile a project settings e2e/m1-settings --set fork_policy=sometimes >"$refused" 2>&1; then
    fail "bosun project settings accepted fork_policy=sometimes: $(cat "$refused")"
  fi
  expect_line "$refused" "error: the settings are not valid" "the refusal did not say why"
  grep -q '^  fork_policy: ' "$refused" || fail "the refusal does not name fork_policy: $(cat "$refused")"
  passed
}

scenario_pools() {
  begin pools \
    "bosun pool create, bosun pool list and bosun alias set send ubuntu-latest to the agent of a new pool"
  local token deadline
  cli --profile a pool create e2e-gamma --project e2e/m0 >"$work/out/pool-create.txt" 2>&1 ||
    fail "bosun pool create failed: $(cat "$work/out/pool-create.txt")"
  grep -qE '^created the trusted pool e2e-gamma [0-9a-f-]+, open to e2e/m0$' "$work/out/pool-create.txt" ||
    fail "bosun pool create printed: $(cat "$work/out/pool-create.txt")"
  cli --profile b pool list >"$work/out/pool-list.txt" 2>&1 ||
    fail "bosun pool list failed: $(cat "$work/out/pool-list.txt")"
  grep -qE '^default +trusted +all projects +[0-9a-f-]+$' "$work/out/pool-list.txt" ||
    fail "bosun pool list does not show the pool default open to all projects: $(cat "$work/out/pool-list.txt")"
  grep -qE '^e2e-gamma +trusted +e2e/m0 +[0-9a-f-]+$' "$work/out/pool-list.txt" ||
    fail "bosun pool list does not show the new pool: $(cat "$work/out/pool-list.txt")"
  token="$(cli --profile a --json pool token create e2e-gamma | jq -r .token)" ||
    fail "bosun pool token create e2e-gamma failed"
  "$bin/bosun-agent" register --config "$work/agents/gamma.toml" --url "http://$address_a" \
    --token "$token" --name e2e-gamma --dir "$work/agents/gamma" >"$work/logs/register-gamma.log" 2>&1 ||
    fail "bosun-agent register of gamma failed: $(cat "$work/logs/register-gamma.log")"
  spawn agent-gamma "$work/logs/agent-gamma.log" "$bin/bosun-agent" run --config "$work/agents/gamma.toml"
  deadline=$((SECONDS + 60))
  until [ "$(cli --profile a --json agent list | jq '[.[] | select(.name == "e2e-gamma" and .online)] | length')" -eq 1 ]; do
    [ "$SECONDS" -lt "$deadline" ] || fail "the agent of e2e-gamma did not come online"
    sleep 0.2
  done
  cli --profile a alias set ubuntu-latest --pool e2e-gamma >"$work/out/alias.txt" 2>&1 ||
    fail "bosun alias set failed: $(cat "$work/out/alias.txt")"
  expect_line "$work/out/alias.txt" \
    "the label ubuntu-latest of the installation goes to the pool e2e-gamma with either capability" \
    "bosun alias set did not print the alias"
  push hosted <<'EOF'
name: hosted
on: workflow_dispatch
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "workspace $GITHUB_WORKSPACE"
EOF
  watch a hosted
  watch_ended 60 hosted
  [ "$reaped" -eq 0 ] ||
    fail "bosun workflow dispatch --watch exited with $reaped: $(cat "$watched")"
  grep -qE "^build \| workspace $work/agents/gamma/" "$watched" ||
    fail "the job of ubuntu-latest did not run on the agent of the pool e2e-gamma: $(cat "$watched")"
  passed
}

scenario_tokens() {
  begin tokens "bosun token create makes a token that views one project and cannot dispatch there"
  local token created="$work/out/token-create.txt" refused="$work/out/token-refused.txt"
  api POST /api/v1/projects --data '{"organisation": "e2e", "slug": "m1-tokens", "name": "Tokens"}' \
    >/dev/null || fail "the project of the scenario could not be created"
  push token-probe <<'EOF'
name: token-probe
on: workflow_dispatch
jobs:
  probe:
    runs-on: [self-hosted]
    steps:
      - run: echo probe
EOF
  token="$(cli --profile a token create --name e2e-viewer --scope project.view --project e2e/m0 \
    --expires 1d 2>"$created")" || fail "bosun token create failed: $(cat "$created")"
  case "$token" in
    bst_*) ;;
    *) fail "bosun token create printed '$token' rather than a personal token" ;;
  esac
  grep -qE '^the token e2e-viewer carries project\.view on e2e/m0 until [^ ]+; it is shown only once$' \
    "$created" || fail "bosun token create described the token as: $(cat "$created")"
  printf '%s\n' "$token" | cli --profile viewer login --server "http://$address_a" >/dev/null ||
    fail "bosun login with the new token failed"
  cli --profile viewer project settings e2e/m0 >"$refused" 2>&1 ||
    fail "the token cannot read the project it views: $(cat "$refused")"
  if cli --profile viewer project settings e2e/m1-tokens >"$refused" 2>&1; then
    fail "the token reads a project it is not restricted to: $(cat "$refused")"
  fi
  expect_line "$refused" 'error: there is no project `e2e/m1-tokens`' \
    "the token learnt of a project outside its restriction"
  if cli --profile viewer workflow dispatch e2e/m0/token-probe >"$refused" 2>&1; then
    fail "the token that views dispatched a run: $(cat "$refused")"
  fi
  expect_line "$refused" 'error: `e2e` does not hold `run.dispatch` on project `m0`' \
    "the refusal of the dispatch did not say why"
  if cli --profile a token create --name e2e-long --scope project.view --expires 400d >"$refused" 2>&1; then
    fail "bosun token create made a token that lives 400 days: $(cat "$refused")"
  fi
  expect_line "$refused" "error: the personal token is not valid" \
    "the refusal of the expiry did not say why"
  grep -q '^  expires_at: lies more than a year ahead' "$refused" ||
    fail "the refusal does not name expires_at: $(cat "$refused")"
  passed
}

scenario_lint() {
  begin lint \
    "bosun workflow lint and POST /api/v1/lint give the same diagnostics for every workflow fixture"
  local tree="$work/lint" file path place status expected
  local linted="$work/out/lint-cli.json" said="$work/out/lint-cli.txt" answered="$work/out/lint-api.json"
  for place in .bosun/workflows .github/workflows; do
    mkdir -p "$tree/$place"
    cp "$root"/contracts/fixtures/workflows/*.yml "$tree/$place/"
  done
  for file in "$root"/contracts/fixtures/workflows/*.yml; do
    for place in .bosun/workflows .github/workflows; do
      path="$place/$(basename "$file")"
      status=0
      (cd "$tree" && cli --json workflow lint "$path") >"$linted" 2>"$said" || status=$?
      jq -n --rawfile source "$file" --arg path "$path" '{source: $source, path: $path}' |
        api POST /api/v1/lint --data-binary @- >"$answered" ||
        fail "POST /api/v1/lint of $path failed: $(cat "$answered")"
      [ "$(jq -S -c . "$linted")" = "$(jq -S -c . "$answered")" ] ||
        fail "bosun workflow lint printed $(cat "$linted") $(cat "$said") for $path, and the API answered $(cat "$answered")"
      expected=0
      if jq -e 'any(.diagnostics[]; .severity == "error")' "$answered" >/dev/null; then
        expected=1
      fi
      [ "$status" -eq "$expected" ] ||
        fail "bosun workflow lint exited with $status for $path, whose diagnostics are $(cat "$answered")"
    done
  done
  passed
}

job_of() {
  api GET "/api/v1/runs/$1/attempts/1/jobs" | jq -r '.items[0] | .id + " " + .status'
}

until_job() {
  local run="$1" status="$2" what="$3" deadline found
  deadline=$((SECONDS + 60))
  found="$(job_of "$run")"
  until [ "${found#* }" = "$status" ]; do
    [ "$SECONDS" -lt "$deadline" ] || fail "$what: the job of run $run is ${found#* }, not $status"
    sleep 0.2
    found="$(job_of "$run")"
  done
  job="${found%% *}"
}

until_run() {
  local run="$1" expected="$2" deadline state
  deadline=$((SECONDS + 60))
  state="$(api GET "/api/v1/runs/$run" | jq -r '.status + " " + (.conclusion // "")')"
  until [ "$state" = "$expected" ]; do
    [ "$SECONDS" -lt "$deadline" ] || fail "run $run is $state, not $expected"
    sleep 0.2
    state="$(api GET "/api/v1/runs/$run" | jq -r '.status + " " + (.conclusion // "")')"
  done
}

scenario_agents() {
  begin agents \
    "a drained agent finishes its job and takes no other until the drain is lifted, also after a restart, and a deleted one is cancelled and refused"
  local token gate="$work/gates/drained" first second third pid deadline execution
  token="$(cli --profile a --json pool token create default | jq -r .token)" ||
    fail "bosun pool token create default failed"
  "$bin/bosun-agent" register --config "$work/agents/delta.toml" --url "http://$address_b" \
    --token "$token" --name e2e-delta --dir "$work/agents/delta" --label delta \
    >"$work/logs/register-delta.log" 2>&1 ||
    fail "bosun-agent register of delta failed: $(cat "$work/logs/register-delta.log")"
  spawn agent-delta "$work/logs/agent-delta.log" "$bin/bosun-agent" run --config "$work/agents/delta.toml"
  deadline=$((SECONDS + 60))
  until [ "$(cli --profile a --json agent list | jq '[.[] | select(.name == "e2e-delta" and .online)] | length')" -eq 1 ]; do
    [ "$SECONDS" -lt "$deadline" ] || fail "the agent e2e-delta did not come online"
    sleep 0.2
  done
  push drained <<EOF
name: drained
on: workflow_dispatch
jobs:
  hold:
    runs-on: [self-hosted, delta]
    steps:
      - run: |
          echo "holding as \$\$"
          while [ ! -e "$gate" ]; do sleep 0.1; done
EOF
  first="$(cli --profile a --json workflow dispatch e2e/m0/drained | jq -r .id)" ||
    fail "bosun workflow dispatch of drained failed"
  until_job "$first" in_progress "the first job"
  cli --profile a agent drain e2e-delta >"$work/out/drain.txt" 2>&1 ||
    fail "bosun agent drain failed: $(cat "$work/out/drain.txt")"
  grep -qE '^drained the agent e2e-delta [0-9a-f-]+; it finishes the job it runs and is offered no other$' \
    "$work/out/drain.txt" || fail "bosun agent drain printed: $(cat "$work/out/drain.txt")"
  await_line "$work/logs/agent-delta.log" 'the server drained this agent: it finishes the job it runs' 30 \
    "the Drain reaching the agent on replica b"
  second="$(cli --profile a --json workflow dispatch e2e/m0/drained | jq -r .id)" ||
    fail "the second dispatch of drained failed"
  touch "$gate"
  until_run "$first" "completed success"
  sleep 3
  until_job "$second" queued "the drained agent"
  cli --profile b agent list >"$work/out/drained-agents.txt" || fail "bosun agent list failed"
  grep -qE '^e2e-delta +online, drained ' "$work/out/drained-agents.txt" ||
    fail "bosun agent list does not show the agent drained: $(cat "$work/out/drained-agents.txt")"

  stop_process agent-delta "$(pid_of agent-delta)" 30
  spawn agent-delta "$work/logs/agent-delta-again.log" "$bin/bosun-agent" run --config "$work/agents/delta.toml"
  await_line "$work/logs/agent-delta-again.log" 'the server drained this agent: it is offered no job' 60 \
    "the Drain after the agent restarted"
  sleep 3
  until_job "$second" queued "the drained agent after its restart"
  cli --profile b agent drain e2e-delta --lift >"$work/out/lift.txt" 2>&1 ||
    fail "bosun agent drain --lift failed: $(cat "$work/out/lift.txt")"
  grep -qE '^lifted the drain of the agent e2e-delta [0-9a-f-]+; it is offered jobs again$' \
    "$work/out/lift.txt" || fail "bosun agent drain --lift printed: $(cat "$work/out/lift.txt")"
  until_run "$second" "completed success"

  rm -f "$gate"
  third="$(cli --profile a --json workflow dispatch e2e/m0/drained | jq -r .id)" ||
    fail "the third dispatch of drained failed"
  until_job "$third" in_progress "the job of the agent to delete"
  deadline=$((SECONDS + 30))
  pid=""
  until [ -n "$pid" ]; do
    [ "$SECONDS" -lt "$deadline" ] || fail "the job of the agent to delete logged no process"
    sleep 0.2
    pid="$(cli --profile a job logs "$job" --step 1 2>/dev/null |
      sed -n 's/^[^ ]* holding as \([0-9]*\)$/\1/p' | head -n 1)"
  done
  kill -0 "$pid" 2>/dev/null || fail "the step of the agent to delete, process $pid, is not running"
  cli --profile a agent delete e2e-delta >"$work/out/delete.txt" 2>&1 ||
    fail "bosun agent delete failed: $(cat "$work/out/delete.txt")"
  grep -qE '^deleted the agent e2e-delta [0-9a-f-]+ and revoked its credential$' "$work/out/delete.txt" ||
    fail "bosun agent delete printed: $(cat "$work/out/delete.txt")"
  until_run "$third" "completed failure"
  execution="$(api GET "/api/v1/jobs/$job" | jq -r .execution_id)"
  [ "$(api GET "/api/v1/executions/$execution" | jq -r '.conclusion + " " + .reason')" = "failure agent_removed" ] ||
    fail "the execution of the deleted agent did not end as failure with agent_removed"
  deadline=$((SECONDS + 15))
  while kill -0 "$pid" 2>/dev/null; do
    [ "$SECONDS" -lt "$deadline" ] || fail "the step of the deleted agent, process $pid, still runs"
    sleep 0.2
  done
  await_line "$work/logs/agent-delta-again.log" 'the agent was deleted, which revoked its credential' 60 \
    "the refusal of the deleted agent"
  [ "$(cli --profile b --json agent list | jq '[.[] | select(.name == "e2e-delta")] | length')" -eq 0 ] ||
    fail "bosun agent list still lists the deleted agent"
  stop_process agent-delta "$(pid_of agent-delta)" 30
}

scenario_secrets() {
  begin secrets \
    "a secret set with bosun secret set and printed raw, as JSON and as base64 is masked in the stored log"
  local value='s3cr3t "quoted" \ back' job log="$work/out/secrets-log.txt" listed="$work/out/secrets-list.txt"
  printf '%s\n' "$value" | cli --profile a secret set e2e_token --project e2e/m0 \
    >"$work/out/secret-set.txt" 2>&1 || fail "bosun secret set failed: $(cat "$work/out/secret-set.txt")"
  expect_line "$work/out/secret-set.txt" "set the secret E2E_TOKEN of project e2e/m0" \
    "bosun secret set did not say what it set"
  cli --profile b var set greeting hello --organisation e2e >"$work/out/var-set.txt" 2>&1 ||
    fail "bosun var set failed: $(cat "$work/out/var-set.txt")"
  expect_line "$work/out/var-set.txt" "set the variable GREETING of organisation e2e" \
    "bosun var set did not say what it set"
  cli --profile a secret list --project e2e/m0 >"$listed" 2>&1 ||
    fail "bosun secret list failed: $(cat "$listed")"
  grep -qE '^E2E_TOKEN +[0-9]{4}-' "$listed" ||
    fail "bosun secret list does not show the secret: $(cat "$listed")"
  if grep -qF 's3cr3t' "$listed"; then
    fail "bosun secret list shows the value of the secret: $(cat "$listed")"
  fi
  push secrets <<'EOF'
name: secrets
on: workflow_dispatch
jobs:
  print:
    runs-on: [self-hosted, alpha]
    if: vars.GREETING == 'hello'
    env:
      TOKEN: ${{ secrets.E2E_TOKEN }}
    steps:
      - run: |
          printf 'raw %s\n' "$TOKEN"
          python3 -c 'import json, os; print("json " + json.dumps(os.environ["TOKEN"]))'
          printf 'base64 %s\n' "$(printf '%s' "$TOKEN" | base64)"
          echo "greeting ${{ vars.GREETING }}"
EOF
  watch a secrets
  watch_ended 60 secrets
  [ "$reaped" -eq 0 ] || fail "bosun workflow dispatch --watch exited with $reaped: $(cat "$watched")"
  job="$(job_id print)"
  [ -n "$job" ] || fail "the watch named no job: $(cat "$watched")"
  cli --profile b job logs "$job" --step 1 >"$work/out/secrets-raw-log.txt" 2>&1 ||
    fail "bosun job logs failed: $(cat "$work/out/secrets-raw-log.txt")"
  cut -d' ' -f2- "$work/out/secrets-raw-log.txt" >"$log"
  [ "$(cat "$log")" = "$(printf '%s\n' 'raw ***' 'json "***"' 'base64 ***' 'greeting hello')" ] ||
    fail "the stored log does not show *** in place of the three forms of the secret: $(cat "$log")"
  python3 - "$value" "$log" "$watched" "$work"/logs/server-*.log >"$work/out/secrets-forms.txt" 2>&1 <<'PY' ||
import base64
import json
import sys

value, *paths = sys.argv[1:]
forms = {
    "raw": value,
    "JSON": json.dumps(value)[1:-1],
    "base64": base64.b64encode(value.encode()).decode(),
}
leaked = [
    "%s holds the %s form" % (path, name)
    for path in paths
    for name, form in forms.items()
    if form in open(path, encoding="utf-8", errors="replace").read()
]
if leaked:
    sys.exit("\n".join(leaked))
print("no form of the secret is in the log of the step, the watch or the servers")
PY
    fail "a form of the secret leaked: $(cat "$work/out/secrets-forms.txt")"
  cat "$work/out/secrets-forms.txt"
  passed
}

scenario_runs() {
  begin runs \
    "bosun lists projects, workflows and runs, disables and enables a workflow, cancels one job, and run watch exits with the status of the conclusion"
  local failing run job shown="$work/out/runs-shown.txt" refused="$work/out/runs-refused.txt"
  cli --profile a project list >"$shown" 2>&1 || fail "bosun project list failed: $(cat "$shown")"
  grep -qE '^e2e/m0 +M0 +- +[0-9a-f-]+$' "$shown" ||
    fail "bosun project list does not show the project e2e/m0: $(cat "$shown")"
  [ "$(cli --profile b --json project show e2e/m0 | jq -r .path)" = e2e/m0 ] ||
    fail "bosun project show --json did not print the project e2e/m0"
  push watched-failure <<'EOF'
name: watched-failure
on: workflow_dispatch
jobs:
  build:
    runs-on: [self-hosted, alpha]
    steps:
      - run: |
          echo "about to fail"
          exit 1
EOF
  cli --profile a workflow disable e2e/m0/watched-failure >"$shown" 2>&1 ||
    fail "bosun workflow disable failed: $(cat "$shown")"
  grep -qE '^disabled the workflow watched-failure [0-9a-f-]+; a dispatch of it is refused and nothing else runs it$' \
    "$shown" || fail "bosun workflow disable printed: $(cat "$shown")"
  cli --profile b workflow list e2e/m0 >"$shown" 2>&1 || fail "bosun workflow list failed: $(cat "$shown")"
  grep -qE '^watched-failure +server +disabled +- +[0-9a-f-]+$' "$shown" ||
    fail "bosun workflow list does not show the workflow disabled: $(cat "$shown")"
  if cli --profile a workflow dispatch e2e/m0/watched-failure >"$refused" 2>&1; then
    fail "a disabled workflow was dispatched: $(cat "$refused")"
  fi
  expect_line "$refused" 'error: workflow `watched-failure` is disabled, so it cannot be dispatched' \
    "the refused dispatch did not say why"
  cli --profile b workflow enable e2e/m0/watched-failure >"$shown" 2>&1 ||
    fail "bosun workflow enable failed: $(cat "$shown")"
  grep -qE '^enabled the workflow watched-failure [0-9a-f-]+; it is dispatched and its triggers run it$' \
    "$shown" || fail "bosun workflow enable printed: $(cat "$shown")"

  failing="$(cli --profile a --json workflow dispatch e2e/m0/watched-failure | jq -r .id)" ||
    fail "the dispatch of the enabled workflow failed"
  watched="$work/out/run-watch-failure.txt"
  spawn run-watch-failure "$watched" "$bin/bosun" --profile b run watch "$failing"
  watcher=$spawned
  exited_within "$watcher" 60 ||
    fail "bosun run watch of the failed run did not end within 60s: $(cat "$watched")"
  forget run-watch-failure
  reap "$watcher"
  [ "$reaped" -eq 3 ] || fail "bosun run watch exited with $reaped for a failed run, not 3: $(cat "$watched")"
  expect_line "$watched" "build | about to fail" "bosun run watch printed no log line"
  expect_ended "$watched" "build: completed failure" "bosun run watch did not show the job fail"
  [ "$(tail -n 1 "$watched")" = "run $failing: completed failure" ] ||
    fail "bosun run watch did not end with the conclusion: $(cat "$watched")"
  [ "$(cli --profile a --json workflow show e2e/m0/watched-failure | jq -r .latest_run.id)" = "$failing" ] ||
    fail "the workflow does not carry the run as its latest"

  cli --profile a --json run list --status failure >"$work/out/run-list.json" ||
    fail "bosun run list --status failure --json failed"
  api GET "/api/v1/runs?status=failure" | jq -S .items >"$work/out/run-list-api.json" ||
    fail "the API did not list the failed runs"
  [ "$(jq -S . "$work/out/run-list.json")" = "$(cat "$work/out/run-list-api.json")" ] ||
    fail "bosun run list --json printed $(cat "$work/out/run-list.json") while the API returns $(cat "$work/out/run-list-api.json")"
  jq -e --arg id "$failing" 'any(.id == $id) and all(.conclusion == "failure")' \
    "$work/out/run-list.json" >/dev/null || fail "the failed runs do not hold the run, or hold another one"
  cli --profile b run list --workflow e2e/m0/watched-failure --event workflow_dispatch >"$shown" 2>&1 ||
    fail "bosun run list --workflow failed: $(cat "$shown")"
  grep -qE "^completed failure +watched-failure #1 +workflow_dispatch +- +e2e +[^ ]+ +$failing\$" "$shown" ||
    fail "bosun run list does not show the failed run: $(cat "$shown")"

  push cancel-one <<'EOF'
name: cancel-one
on: workflow_dispatch
jobs:
  hold:
    runs-on: [self-hosted, alpha]
    steps:
      - run: |
          echo "holding"
          exec sleep 600
  after:
    runs-on: [self-hosted, alpha]
    needs: hold
    if: always()
    steps:
      - run: echo "hold ended ${{ needs.hold.result }}"
  apart:
    runs-on: [self-hosted, beta]
    steps:
      - run: echo "apart runs"
EOF
  run="$(cli --profile a --json workflow dispatch e2e/m0/cancel-one | jq -r .id)" ||
    fail "the dispatch of cancel-one failed"
  watched="$work/out/run-watch-cancel.txt"
  spawn run-watch-cancel "$watched" "$bin/bosun" --profile b run watch "$run"
  watcher=$spawned
  await_line "$watched" '^hold \| holding$' 60 "the start of the job to cancel"
  job="$(job_id hold)"
  [ -n "$job" ] || fail "the watch named no job: $(cat "$watched")"
  cli --profile a job cancel "$job" >"$shown" 2>&1 || fail "bosun job cancel failed: $(cat "$shown")"
  expect_line "$shown" "the cancellation of job hold $job is accepted" "bosun job cancel printed no acceptance"
  exited_within "$watcher" 60 ||
    fail "bosun run watch of the run with a cancelled job did not end within 60s: $(cat "$watched")"
  forget run-watch-cancel
  reap "$watcher"
  [ "$reaped" -eq 4 ] ||
    fail "bosun run watch exited with $reaped for a cancelled run, not 4: $(cat "$watched")"
  expect_line "$watched" "hold: completed cancelled" "the cancelled job did not end cancelled"
  expect_line "$watched" "after | hold ended cancelled" "the dependent job did not see cancelled"
  expect_line "$watched" "after: completed success" "the dependent job did not run after the cancel"
  expect_ended "$watched" "apart: completed success" "the job apart was not left to finish"
  [ "$(tail -n 1 "$watched")" = "run $run: completed cancelled" ] ||
    fail "the run with a cancelled job did not end cancelled: $(cat "$watched")"
  cli --profile a run show "$run" >"$shown" 2>&1 || fail "bosun run show failed: $(cat "$shown")"
  grep -qE '^status +completed cancelled$' "$shown" || fail "bosun run show printed: $(cat "$shown")"
  grep -qE "^hold +hold +completed cancelled +$job\$" "$shown" ||
    fail "bosun run show does not show the cancelled job: $(cat "$shown")"
  passed
}

scenario_dispatch_inputs() {
  begin dispatch-inputs \
    "bosun workflow dispatch --input passes inputs that a step sees typed, and an undeclared one is refused"
  local refused="$work/out/inputs-refused.txt" twice="$work/out/inputs-twice.txt"
  push inputs <<'EOF'
name: inputs
on:
  workflow_dispatch:
    inputs:
      flag:
        type: boolean
      count:
        type: number
      level:
        type: choice
        options: [debug, info]
        default: info
jobs:
  build:
    runs-on: [self-hosted, alpha]
    steps:
      - run: echo '${{ toJSON(inputs.flag) }} ${{ toJSON(github.event.inputs.flag) }} ${{ toJSON(inputs.count) }} ${{ inputs.level }}'
EOF
  watched="$work/out/watch-inputs.txt"
  spawn watch-inputs "$watched" "$bin/bosun" --profile a workflow dispatch e2e/m0/inputs \
    --input flag=true --input count=3 --watch
  watcher=$spawned
  watch_ended 60 inputs
  [ "$reaped" -eq 0 ] ||
    fail "bosun workflow dispatch --watch with inputs exited with $reaped: $(cat "$watched")"
  expect_line "$watched" 'build | true "true" 3 info' "the step did not see the inputs typed"
  if cli --profile a workflow dispatch e2e/m0/inputs --input colour=blue >"$refused" 2>&1; then
    fail "a dispatch with an undeclared input was accepted: $(cat "$refused")"
  fi
  expect_line "$refused" "error: the dispatch is not valid" "the refused dispatch did not say why"
  expect_line "$refused" '  inputs.colour: the workflow declares no input `colour`' \
    "the refused dispatch did not name the input"
  if cli --profile a workflow dispatch e2e/m0/inputs --input flag=true --input flag=false \
    >"$twice" 2>&1; then
    fail "a dispatch with an input given twice was sent: $(cat "$twice")"
  fi
  expect_line "$twice" 'error: --input gives `flag` twice' "an input given twice was not refused"
  passed
}

numbered() {
  sed -E 's/^([0-9]+): [^ ]+ /\1: /' "$1"
}

scenario_log_search() {
  begin log-search \
    "bosun job logs --grep prints the matching lines with their numbers, the same before and after compaction"
  local job execution before after compacted deadline
  push search <<'YAML'
name: search
on: workflow_dispatch
jobs:
  print:
    runs-on: [self-hosted, alpha]
    steps:
      - name: Print
        run: |
          for i in $(seq 1 1200); do echo "row $i"; done
          echo "the NEEDLE is here"
      - name: Again
        run: echo "another needle"
YAML
  watch a search
  watch_ended 60 search
  [ "$reaped" -eq 0 ] || fail "bosun workflow dispatch --watch exited with $reaped: $(cat "$watched")"
  job="$(job_id print)"
  [ -n "$job" ] || fail "the watch named no job: $(cat "$watched")"
  before="$work/out/grep-before.txt"
  after="$work/out/grep-after.txt"
  cli --profile a job logs "$job" --grep needle >"$before" 2>&1 ||
    fail "bosun job logs --grep failed: $(cat "$before")"
  [ "$(numbered "$before")" = "$(printf '%s\n' '==> step 1: Print' '1201: the NEEDLE is here' \
    '==> step 2: Again' '1: another needle')" ] ||
    fail "bosun job logs --grep printed: $(cat "$before")"
  cli --profile b job logs "$job" --step 1 --grep "ROW 115" >"$work/out/grep-step.txt" 2>&1 ||
    fail "bosun job logs --step 1 --grep failed: $(cat "$work/out/grep-step.txt")"
  [ "$(numbered "$work/out/grep-step.txt")" = "$(printf '%s\n' '115: row 115' \
    '1150: row 1150' '1151: row 1151' '1152: row 1152' '1153: row 1153' '1154: row 1154' \
    '1155: row 1155' '1156: row 1156' '1157: row 1157' '1158: row 1158' '1159: row 1159')" ] ||
    fail "bosun job logs --step 1 --grep printed: $(cat "$work/out/grep-step.txt")"
  execution="$(api GET "/api/v1/jobs/$job" | jq -r .execution_id)" || fail "the job $job could not be read"
  deadline=$((SECONDS + 60))
  until compacted="$(psql "$database_url" -XAtc \
    "SELECT log_compacted_at IS NOT NULL FROM bosun.execution WHERE id = '$execution'")" &&
    [ "$compacted" = t ]; do
    [ "$SECONDS" -lt "$deadline" ] || fail "the log of $execution was not compacted within 60s"
    sleep 0.5
  done
  cli --profile b job logs "$job" --grep needle >"$after" 2>&1 ||
    fail "bosun job logs --grep failed after the compaction: $(cat "$after")"
  cmp -s "$before" "$after" ||
    fail "the search changed with the compaction: before $(cat "$before"), after $(cat "$after")"
  passed
}

member() {
  local login="$1" role="$2" keys="$3" headers="$work/out/sign-in-$1.txt" cookie csrf expires token
  api POST /api/v1/users --data "$(jq -cn --arg login "$login" \
    '{login: $login, name: $login, email: ($login + "@example.org"), password: "correct horse battery staple"}')" \
    >/dev/null || fail "the user $login could not be created"
  api POST /api/v1/role-bindings --data "$(jq -cn --arg login "$login" --arg role "$role" \
    '{role: $role, user: $login, team: null, organisation: "e2e", project: null}')" \
    >/dev/null || fail "the role $role could not be bound to $login"
  csrf="$(curl -sS --noproxy '*' --fail-with-body -D "$headers" -H 'Content-Type: application/json' \
    --data "$(jq -cn --arg login "$login" '{login: $login, password: "correct horse battery staple"}')" \
    "http://$address_a/api/v1/auth/login" | jq -r .csrf_token)" || fail "$login could not sign in"
  cookie="$(sed -n 's/^[Ss]et-[Cc]ookie: \([^;]*\).*/\1/p' "$headers" | head -n 1)"
  [ -n "$cookie" ] || fail "the sign-in of $login set no cookie: $(cat "$headers")"
  curl -sS --noproxy '*' --fail-with-body -H "Cookie: $cookie" -H "X-Bosun-CSRF: $csrf" \
    -H 'Content-Type: application/json' \
    --data '{"current_password": "correct horse battery staple", "new_password": "correct horse battery stapler"}' \
    "http://$address_a/api/v1/auth/password" >/dev/null || fail "$login could not set a password"
  expires="$(python3 -c 'import datetime as d; print((d.datetime.now(d.timezone.utc) + d.timedelta(days=1)).strftime("%Y-%m-%dT%H:%M:%SZ"))')"
  token="$(curl -sS --noproxy '*' --fail-with-body -H "Cookie: $cookie" -H "X-Bosun-CSRF: $csrf" \
    -H 'Content-Type: application/json' \
    --data "$(jq -cn --arg expires "$expires" --argjson keys "$keys" \
      '{name: "e2e", permissions: $keys, organisations: [], projects: [], expires_at: $expires}')" \
    "http://$address_a/api/v1/tokens" | jq -r .token)" || fail "$login could not create a token"
  printf '%s\n' "$token" | cli --profile "$login" login --server "http://$address_a" >/dev/null ||
    fail "bosun login of $login failed"
}

scenario_admission() {
  begin admission \
    "bosun run admit is refused to a developer, and a maintainer's reaches the state of the run"
  local run refused="$work/out/admission-refused.txt"
  member e2e-developer developer '["project.view", "run.dispatch", "run.cancel"]'
  member e2e-maintainer maintainer '["project.view", "run.dispatch", "run.cancel", "run.override"]'
  push admission <<'EOF'
name: admission
on: workflow_dispatch
jobs:
  build:
    runs-on: [self-hosted, nowhere]
    steps:
      - run: echo never
EOF
  run="$(cli --profile a --json workflow dispatch e2e/m0/admission | jq -r .id)" ||
    fail "the dispatch of admission failed"
  if cli --profile e2e-developer run admit "$run" >"$refused" 2>&1; then
    fail "a developer admitted a run: $(cat "$refused")"
  fi
  expect_line "$refused" 'error: `e2e-developer` does not hold `run.override` on project `m0`' \
    "the refusal of the developer's admission did not say why"
  if cli --profile e2e-maintainer run admit "$run" >"$refused" 2>&1; then
    fail "a run that is not from a fork was admitted: $(cat "$refused")"
  fi
  expect_line "$refused" \
    'error: the run does not wait for admission; `run.admit` needs a run from a fork that waits for it' \
    "the refusal of the maintainer's admission did not give the state of the run"
  cli --profile a run cancel "$run" >/dev/null || fail "the run of the scenario could not be cancelled"
  passed
}

header() {
  tr -d '\r' <"$1" | awk -v name="$2" 'tolower($0) ~ "^" name ":" { sub(/^[^:]*: */, ""); print; exit }'
}

expect_served() {
  local path="$1" status="$2" cache="$3" name="${1//[^a-z0-9]/_}" answer
  answer="$(curl -sS --noproxy '*' -D "$work/out/$name.headers" -o "$work/out/$name.body" \
    -w '%{http_code}' "http://$address_a$path")" || fail "GET $path did not answer"
  [ "$answer" = "$status" ] || fail "GET $path answered $answer, not $status"
  [ "$(header "$work/out/$name.headers" cache-control)" = "$cache" ] ||
    fail "GET $path answered with cache-control '$(header "$work/out/$name.headers" cache-control)', not '$cache'"
  served="$work/out/$name.body"
}

playwright() {
  local version
  if [ "$(uname -s)" = Linux ] && command -v docker >/dev/null; then
    version="$(jq -r .version "$root/web/node_modules/@playwright/test/package.json")"
    docker run --rm --network host --ipc host --user "$(id -u):$(id -g)" -e HOME=/tmp -e CI \
      -e BOSUN_E2E_URL -e BOSUN_E2E_FIRST_START -e BOSUN_E2E_TOKEN \
      -v "$root/web:$root/web" -w "$root/web" \
      "mcr.microsoft.com/playwright:v$version-noble" node node_modules/@playwright/test/cli.js test
  else
    (cd "$root/web" && corepack pnpm exec playwright install --only-shell chromium &&
      corepack pnpm exec playwright test)
  fi
}

scenario_interface() {
  begin interface \
    "the server serves the interface, and the Playwright scenarios of web/e2e pass in both themes"
  local asset
  expect_served "/runs/$(python3 -c 'import uuid; print(uuid.uuid4())')" 200 no-cache
  grep -q '<div id="root"></div>' "$served" || fail "a client route did not answer with the page"
  asset="$(grep -oE '/assets/[^"]+\.js' "$served" | head -n 1)"
  [ -n "$asset" ] || fail "the page names no script under /assets/: $(cat "$served")"
  expect_served "$asset" 200 "public, max-age=31536000, immutable"
  expect_served /index.html 200 no-cache
  expect_served /api/v1/nothing 404 ""
  [ "$(jq -r .error.code "$served")" = not_found ] ||
    fail "an unknown API path did not answer with the JSON not_found: $(cat "$served")"
  first_starts="{}"
  first_start light
  first_start dark
  BOSUN_E2E_URL="http://$address_a" BOSUN_E2E_FIRST_START="$first_starts" BOSUN_E2E_TOKEN="$owner_token" \
    playwright ||
    fail "the Playwright scenarios of web/e2e failed"
  passed
}

case " $selected " in
  *" interface "*)
    echo "building the interface"
    (cd "$root/web" && corepack pnpm install --frozen-lockfile && corepack pnpm build)
    ;;
esac
echo "building bosun-server, bosun-agent and bosun"
cargo build --manifest-path "$root/Cargo.toml" -p bosun-server-bin -p bosun-agent -p bosun-cli
target="$(cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 --no-deps |
  jq -r .target_directory)"
bin="$target/debug"

create_database "$database"
database_url="$created_url"
kek="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
printf '[server]\nlisten = "127.0.0.1:0"\nmetrics_listen = "127.0.0.1:0"\n' >"$work/server.toml"
settings=(
  "BOSUN_DATABASE_URL=$database_url"
  "BOSUN_DATABASE_MAX_CONNECTIONS=8"
  "BOSUN_KEYS_KEK=$kek"
  "BOSUN_STORE_PATH=$work/store"
)

owner_token="$(server admin create e2e 2>"$work/logs/admin.log")" || fail "bosun-server admin create failed"
serve a 127.0.0.1:0
address_a="$address"
serve b 127.0.0.1:0
address_b="$address"
echo "replica a listens on $address_a and replica b on $address_b, both on database $database"

for replica in a b; do
  eval "replica_address=\$address_$replica"
  printf '%s\n' "$owner_token" |
    cli --profile "$replica" login --server "http://$replica_address" >/dev/null ||
    fail "bosun login to replica $replica failed"
done
api POST /api/v1/organisations --data '{"slug": "e2e", "name": "End to end"}' >/dev/null ||
  fail "the organisation could not be created"
api POST /api/v1/projects --data '{"organisation": "e2e", "slug": "m0", "name": "M0"}' >/dev/null ||
  fail "the project could not be created"

registration="$(cli --profile a --json pool token create default --reusable --expires 1h |
  jq -r .token)" ||
  fail "bosun pool token create failed"
for agent in alpha:a beta:b; do
  name="${agent%%:*}"
  eval "agent_address=\$address_${agent#*:}"
  "$bin/bosun-agent" register --config "$work/agents/$name.toml" --url "http://$agent_address" \
    --token "$registration" --name "e2e-$name" --dir "$work/agents/$name" --label "$name" \
    >"$work/logs/register-$name.log" 2>&1 ||
    fail "bosun-agent register of $name failed: $(cat "$work/logs/register-$name.log")"
  spawn "agent-$name" "$work/logs/agent-$name.log" \
    "$bin/bosun-agent" run --config "$work/agents/$name.toml"
done
deadline=$((SECONDS + 60))
until [ "$(cli --profile b --json agent list | jq '[.[] | select(.online)] | length')" -eq 2 ]; do
  [ "$SECONDS" -lt "$deadline" ] || fail "the agents did not come online: $(cli --profile b agent list)"
  sleep 0.2
done
cli --profile a agent list

for name in $selected; do
  "scenario_$(echo "$name" | tr - _)"
done
current="all"
echo "all scenarios passed: $selected"
