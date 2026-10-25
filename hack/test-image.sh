#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

if [ "$#" -ne 0 ]; then
  echo "usage: hack/test-image.sh" >&2
  echo "builds the image, starts it against Postgres with nothing but its address and a volume," >&2
  echo "installs an agent with the script the server serves on a second host, runs the two-job" >&2
  echo "workflow through it, restarts the server on the same volume, and starts deploy/compose.yaml" >&2
  exit 2
fi
command -v docker >/dev/null || { echo "hack/test-image.sh needs docker on the PATH" >&2; exit 1; }

id="bosun-image-$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
work="$(mktemp -d "${TMPDIR:-/tmp}/bosun-image.XXXXXX")"
image="$id:server"
host_image="$id:host"
network="$id"
postgres="$id-postgres"
server="$id-server"
agent="$id-agent"
volume="$id-data"
compose_project="$id-compose"
current="setup"
socket="/var/run/docker.sock"
case "${DOCKER_HOST:-}" in
  unix://*) socket="${DOCKER_HOST#unix://}" ;;
esac

compose() {
  docker run --rm -v "$socket:/var/run/docker.sock" -v "$root/deploy:/deploy:ro" \
    -e DOCKER_HOST=unix:///var/run/docker.sock \
    -e BOSUN_IMAGE="$image" -e BOSUN_DATABASE_PASSWORD=compose -e BOSUN_PUBLISH=127.0.0.1:0 \
    docker.io/library/docker:29.7.2-cli \
    compose --project-name "$compose_project" --file /deploy/compose.yaml "$@"
}

finish() {
  local status=$?
  set +e
  if [ "$status" -ne 0 ]; then
    for container in "$server" "$postgres"; do
      echo "---- the last lines of $container ----" >&2
      docker logs --tail 60 "$container" >&2 2>&1
    done
    echo "---- the last lines of bosun-agent@1 ----" >&2
    docker exec "$agent" journalctl --no-pager -n 60 -u bosun-agent@1 >&2 2>&1
    for output in "$work"/*.txt; do
      [ -s "$output" ] || continue
      echo "---- ${output#"$work"/} ----" >&2
      tail -n 40 "$output" >&2
    done
    compose logs --tail 40 >&2 2>&1
  fi
  compose down --volumes --timeout 10 >/dev/null 2>&1
  docker rm -f "$agent" "$server" "$postgres" >/dev/null 2>&1
  docker network rm "$network" >/dev/null 2>&1
  docker volume rm "$volume" >/dev/null 2>&1
  docker image rm "$image" "$host_image" >/dev/null 2>&1
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

begin() {
  current="$1"
  echo "scenario $1: $2"
}

passed() {
  echo "scenario $current: passed"
}

within() {
  local seconds="$1" what="$2" deadline
  shift 2
  deadline=$((SECONDS + seconds))
  until "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$deadline" ] || fail "$what did not happen within ${seconds}s"
    sleep 1
  done
}

holds() {
  local pattern="$1" text
  shift
  text="$("$@" 2>&1)" || true
  grep -qE -- "$pattern" <<<"$text"
}

on_server() {
  docker exec "$server" "$@"
}

on_agent() {
  docker exec "$agent" "$@"
}

api() {
  on_agent curl -fsS -X POST -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
    --data "$2" "http://bosun:8080$1"
}

start_server() {
  docker run -d --name "$server" --network "$network" --network-alias bosun \
    -v "$volume:/bosun-data" \
    -e BOSUN_DATABASE_URL=postgres://bosun:bosun@postgres:5432/bosun \
    "$image" >/dev/null
  within 120 "bosun-server check passing in the container" on_server bosun-server check
}

state() {
  on_server sh -c 'cd /bosun-data && sha256sum keys/kek admin.token && find store -type f | sort'
}

online() {
  [ "$(on_agent bosun --json agent list | grep -cE '"online": *true')" -eq 1 ]
}

compacted() {
  [ "$(docker logs "$server" 2>&1 | grep -c '"message":"compacted the log of an execution"')" -ge 2 ]
}

scenario_quick_start() {
  begin quick-start "the image starts with only BOSUN_DATABASE_URL and a volume and writes admin.token"
  start_server
  [ "$(on_server id -u)" = 210 ] || fail "the server does not run as uid 210"
  [ "$(on_server stat -c '%a %u' /bosun-data/admin.token)" = "600 210" ] ||
    fail "admin.token is not readable only by the server's user"
  [ "$(on_server stat -c '%a %u' /bosun-data/keys/kek)" = "600 210" ] ||
    fail "the generated KEK is not readable only by the server's user"
  holds '"message":"generated the KEK' docker logs "$server" ||
    fail "the server did not say that it generated the KEK"
  token="$(on_server cat /bosun-data/admin.token)"
  case "$token" in
    bst_*) ;;
    *) fail "admin.token holds no personal token" ;;
  esac
  passed
}

scenario_install_agent() {
  begin install-agent "the command of bosun pool token create installs and registers an agent on another host"
  docker run -d --name "$agent" --hostname image-agent --network "$network" \
    --privileged --cgroupns=private --tmpfs /run --tmpfs /run/lock \
    -v "$work/cli/bosun:/usr/local/bin/bosun:ro" "$host_image" >/dev/null
  within 60 "systemd starting on the agent host" \
    holds '^(running|degraded)$' on_agent systemctl is-system-running --wait
  printf '%s\n' "$token" | docker exec -i "$agent" bosun login --server http://bosun:8080 \
    >"$work/login.txt" 2>&1 || fail "admin.token does not sign the CLI in: $(cat "$work/login.txt")"
  on_agent bosun pool token create default >"$work/token.txt" 2>"$work/token-stderr.txt" ||
    fail "bosun pool token create failed: $(cat "$work/token-stderr.txt")"
  local registration command
  registration="$(cat "$work/token.txt")"
  command="$(grep '^curl ' "$work/token-stderr.txt")" ||
    fail "bosun pool token create printed no command: $(cat "$work/token-stderr.txt")"
  [ "$command" = "curl -fsSL http://bosun:8080/install-agent.sh | sudo sh -s -- --token $registration" ] ||
    fail "the command is not the one-line install: $command"
  on_agent sh -c "$command" >"$work/install.txt" 2>&1 || fail "the install script failed"
  on_agent systemctl is-active --quiet bosun-agent@1.service || fail "bosun-agent@1 does not run"
  local installed served
  installed="$(on_agent bosun-agent --version)"
  served="$(on_server bosun-server --version)"
  [ "${installed#bosun-agent }" = "${served#bosun-server }" ] ||
    fail "the installed agent is $installed and the server is $served"
  holds '(^| )bosun-cache( |$)' on_agent id -nG bosun-agent-1 || fail "bosun-agent-1 is not in bosun-cache"
  on_agent grep -q '^bosun-agent-1:' /etc/subuid || fail "bosun-agent-1 has no subordinate uids"
  on_agent grep -q '^bosun-agent-1:' /etc/subgid || fail "bosun-agent-1 has no subordinate gids"
  [ "$(on_agent stat -c '%a %U' /etc/bosun/agent-1.toml)" = "600 bosun-agent-1" ] ||
    fail "the configuration of agent 1 is not readable only by its user"
  [ "$(on_agent stat -c '%a %G' /var/lib/bosun/cache)" = "2775 bosun-cache" ] ||
    fail "the cache directory does not belong to bosun-cache with the setgid bit"
  on_agent grep -qx 'Delegate=yes' /etc/systemd/system/bosun-agent@.service ||
    fail "the unit does not delegate its cgroup"
  within 60 "the agent coming online" online
  holds 'a cgroup per job under' on_agent journalctl --no-pager -u bosun-agent@1 ||
    fail "the agent does not track its jobs with cgroups under its delegated unit"
  on_agent sh -c "$command" >"$work/reinstall.txt" 2>&1 || fail "running the command again failed"
  grep -qxF 'install-agent: agent 1 is registered already, in /etc/bosun/agent-1.toml' \
    "$work/reinstall.txt" || fail "running the command again registered the agent again"
  on_agent systemctl is-active --quiet bosun-agent@1.service || fail "bosun-agent@1 does not run again"
  within 60 "the agent coming online after it was installed again" online
  echo "the host runs $installed for $(on_agent uname -m)"
  passed
}

scenario_two_job_workflow() {
  begin two-job-workflow "the agent the script installed runs the two-job workflow"
  api /api/v1/organisations '{"slug": "image", "name": "Image"}' >/dev/null ||
    fail "the organisation could not be created"
  api /api/v1/projects '{"organisation": "image", "slug": "install", "name": "Install"}' >/dev/null ||
    fail "the project could not be created"
  docker exec -i "$agent" sh -c 'cat >/root/two-jobs.yml' <<'EOF'
name: two-jobs
on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - name: Build
        run: |
          echo "build runs on $(uname -m) as $(id -un)"
          touch /tmp/build-done
  test:
    runs-on: self-hosted
    needs: build
    steps:
      - name: Test
        run: |
          test -e /tmp/build-done
          echo "test runs after build"
EOF
  on_agent bosun workflow push image/install /root/two-jobs.yml >"$work/push.txt" 2>&1 ||
    fail "bosun workflow push failed: $(cat "$work/push.txt")"
  on_agent bosun workflow dispatch image/install/two-jobs --watch >"$work/watch.txt" 2>&1 ||
    fail "the run did not succeed: $(cat "$work/watch.txt")"
  local watched="$work/watch.txt"
  grep -qxE 'build \| build runs on (x86_64|aarch64) as bosun-agent-1' "$watched" ||
    fail "the first job did not run on the installed agent: $(cat "$watched")"
  grep -qxF 'test | test runs after build' "$watched" ||
    fail "the second job did not run after the first: $(cat "$watched")"
  grep -qxF 'build: completed success' "$watched" || fail "the first job did not succeed: $(cat "$watched")"
  grep -qxF 'test: completed success' "$watched" || fail "the second job did not succeed: $(cat "$watched")"
  holds '^run [0-9a-f-]+: completed success$' tail -n 1 "$watched" ||
    fail "the run did not end in success: $(cat "$watched")"
  build_job="$(sed -n 's/^build: [a-z_ ]*, job \([0-9a-f-]*\)$/\1/p' "$watched" | head -n 1)"
  [ -n "$build_job" ] || fail "the watch named no job of build: $(cat "$watched")"
  passed
}

scenario_restart() {
  begin restart "a restart on the same volume keeps the KEK, the stored objects and the owner"
  within 90 "the logs of both jobs reaching the object store" compacted
  local before after
  before="$(state)"
  grep -q '^store/' <<<"$before" || fail "the object store holds no object: $before"
  docker stop --time 30 "$server" >/dev/null
  [ "$(docker inspect -f '{{.State.ExitCode}}' "$server")" = 0 ] || fail "the server did not stop cleanly"
  docker rm "$server" >/dev/null
  start_server
  after="$(state)"
  [ "$before" = "$after" ] || fail "the data directory changed: before $before, after $after"
  if holds '"message":"(generated the KEK|created the owner admin)' docker logs "$server"; then
    fail "the restarted server generated a KEK or an owner"
  fi
  on_agent bosun job logs "$build_job" >"$work/logs.txt" 2>&1 ||
    fail "the log of build could not be read after the restart: $(cat "$work/logs.txt")"
  grep -qE 'build runs on (x86_64|aarch64) as bosun-agent-1$' "$work/logs.txt" ||
    fail "the stored log of build lost its line: $(cat "$work/logs.txt")"
  passed
}

scenario_compose() {
  begin compose "docker compose -f deploy/compose.yaml up starts Bosun and Postgres and becomes ready"
  compose up --detach --wait --wait-timeout 180 >"$work/compose.txt" 2>&1 ||
    fail "deploy/compose.yaml did not become ready: $(cat "$work/compose.txt")"
  docker exec "$compose_project-bosun-1" bosun-server check >/dev/null ||
    fail "bosun-server check fails in the compose project"
  docker exec "$compose_project-bosun-1" test -s /bosun-data/admin.token ||
    fail "the compose project wrote no admin.token"
  passed
}

build_args=()
if [ -n "${CARGO_BUILD_JOBS:-}" ]; then
  build_args+=(--build-arg "CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS")
fi
echo "building the image, the bosun CLI and the agent host"
docker build ${build_args[@]+"${build_args[@]}"} \
  --build-arg "REVISION=$(git -C "$root" rev-parse HEAD 2>/dev/null || echo unknown)" \
  --tag "$image" "$root"
docker build ${build_args[@]+"${build_args[@]}"} --target cli --output "type=local,dest=$work/cli" "$root"
docker build --tag "$host_image" - <<'EOF'
FROM docker.io/library/debian:13
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl dbus sudo systemd systemd-sysv \
 && rm -rf /var/lib/apt/lists/*
ENV container=docker
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
EOF

docker network create "$network" >/dev/null
docker volume create "$volume" >/dev/null
docker run -d --name "$postgres" --network "$network" --network-alias postgres \
  -e POSTGRES_USER=bosun -e POSTGRES_PASSWORD=bosun -e POSTGRES_DB=bosun \
  docker.io/library/postgres:15 >/dev/null
within 60 "Postgres accepting connections" docker exec "$postgres" pg_isready -h 127.0.0.1 -U bosun -d bosun

scenario_quick_start
scenario_install_agent
scenario_two_job_workflow
scenario_restart
scenario_compose
current="all"
echo "all scenarios passed"
