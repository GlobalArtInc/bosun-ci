#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

scenarios="two-job-workflow failing-step cancel-sleep labels cross-replica-cancel server-restart"
scenarios="$scenarios step-environment journal-replay"

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

for tool in cargo curl jq psql python3; do
  command -v "$tool" >/dev/null || { echo "hack/e2e.sh needs $tool on the PATH" >&2; exit 1; }
done

work="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/bosun-e2e.XXXXXX")" && pwd -P)"
export XDG_CONFIG_HOME="$work/config"
mkdir -p "$work/logs" "$work/agents" "$work/workflows" "$work/out" "$work/gates" "$work/store"
database="bosun_e2e_$(python3 -c 'import secrets; print(secrets.token_hex(8))')"
database_created=0
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
  if [ "$database_created" -eq 1 ]; then
    psql "$postgres" -qXc "DROP DATABASE IF EXISTS $database WITH (FORCE)" >/dev/null 2>&1 ||
      echo "the database $database could not be dropped" >&2
  fi
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

serve() {
  local replica="$1" listen="$2" log deadline
  serves=$((serves + 1))
  log="$work/logs/server-$replica.$serves.log"
  spawn "server-$replica" "$log" env "${settings[@]}" BOSUN_SERVER_LISTEN="$listen" \
    "$bin/bosun-server" --config "$work/server.toml" serve
  await_line "$log" '"message":"listening"' 60 "replica $replica listening"
  address="$(jq -Rr 'fromjson? | select(.message == "listening") | .address' "$log" | head -n 1)"
  [ -n "$address" ] || fail "replica $replica logged no address: $(cat "$log")"
  deadline=$((SECONDS + 30))
  until curl -fsS --noproxy '*' "http://$address/readyz" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$deadline" ] || fail "replica $replica at $address did not become ready within 30s"
    sleep 0.2
  done
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
  [ "$reaped" -eq 1 ] ||
    fail "bosun workflow dispatch --watch exited with $reaped for a failed run, not 1: $(cat "$watched")"
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
  idle='[.[] | select(.online and .execution_id == null)] | length'
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

echo "building bosun-server, bosun-agent and bosun"
cargo build --manifest-path "$root/Cargo.toml" -p bosun-server-bin -p bosun-agent -p bosun-cli
target="$(cargo metadata --manifest-path "$root/Cargo.toml" --format-version 1 --no-deps |
  jq -r .target_directory)"
bin="$target/debug"

psql "$postgres" -qXv ON_ERROR_STOP=1 -c "CREATE DATABASE $database" >/dev/null
database_created=1
database_url="$(python3 -c '
import sys, urllib.parse as u
p = u.urlsplit(sys.argv[1])
print(u.urlunsplit(p._replace(path="/" + sys.argv[2])))
' "$postgres" "$database")"
kek="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
printf '[server]\nlisten = "127.0.0.1:0"\n' >"$work/server.toml"
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
