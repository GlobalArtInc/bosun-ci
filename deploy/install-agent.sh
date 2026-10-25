#!/bin/sh
set -eu

server='__BOSUN_SERVER__'
version='__BOSUN_VERSION__'

binary=/usr/local/bin/bosun-agent
unit=/etc/systemd/system/bosun-agent@.service
configs=/etc/bosun
agents=/var/lib/bosun/agents
cache=/var/lib/bosun/cache
cache_group=bosun-cache
subordinate_first=100000
subordinate_count=65536

say() {
  printf 'install-agent: %s\n' "$*"
}

fail() {
  printf 'install-agent: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat >&2 <<EOF
usage: curl -fsSL $server/install-agent.sh | sudo sh -s -- --token brt_... [--instances <n>] [--label <label>]...

  --token <token>    a registration token of the pool the agents join, from bosun pool token create
  --instances <n>    how many agents run on this host, one job at a time each (default 1); more
                     than one needs a token created with --reusable
  --label <label>    a label the agents get beyond the automatic ones; repeat it for more
EOF
  exit 2
}

parse() {
  token=
  instances=1
  labels=
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --token | --instances | --label)
        [ "$#" -ge 2 ] || usage
        option="$1"
        value="$2"
        shift 2
        ;;
      --token=* | --instances=* | --label=*)
        option="${1%%=*}"
        value="${1#*=}"
        shift
        ;;
      -h | --help)
        usage
        ;;
      *)
        printf 'install-agent: there is no option %s\n' "$1" >&2
        usage
        ;;
    esac
    case "$option" in
      --token) token="$value" ;;
      --instances) instances="$value" ;;
      --label)
        case "$value" in
          '' | *[[:space:],]*) fail "the label '$value' is empty or holds a space or a comma" ;;
        esac
        labels="$labels $value"
        ;;
    esac
  done
  if [ -z "$token" ]; then
    printf 'install-agent: --token is required\n' >&2
    usage
  fi
  case "$instances" in
    '' | *[!0-9]* | 0*) fail "--instances is '$instances', which is not a whole number from 1 up" ;;
  esac
}

check_host() {
  case "$server" in
    http://* | https://*) ;;
    *) fail "this script runs as a Bosun server serves it: curl -fsSL https://<server>/install-agent.sh | sudo sh -s -- --token brt_..." ;;
  esac
  [ "$(id -u)" -eq 0 ] || fail "the script runs as root, such as through sudo"
  [ "$(uname -s)" = Linux ] || fail "agents run on Linux, and this host runs $(uname -s)"
  [ -d /run/systemd/system ] || fail "the host does not run systemd, which starts the agents"
  for tool in curl getent groupadd useradd usermod install systemctl awk; do
    command -v "$tool" >/dev/null 2>&1 || fail "the script needs $tool, which the host does not have"
  done
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "there is no agent for the architecture $(uname -m)" ;;
  esac
}

install_binary() {
  install -d -m 0755 "$(dirname "$binary")"
  download="$(mktemp "$binary.XXXXXX")"
  trap 'rm -f "$download"' EXIT
  curl -fsSL -o "$download" "$server/_runtime/agent/linux-$arch" ||
    fail "the agent for linux-$arch could not be downloaded from $server"
  chmod 0755 "$download"
  reported="$("$download" --version 2>&1)" || fail "the downloaded agent does not run: $reported"
  [ "$reported" = "bosun-agent $version" ] ||
    fail "the server handed out $reported, and the server runs version $version"
  mv -f "$download" "$binary"
  say "installed bosun-agent $version for linux-$arch in $binary"
}

install_unit() {
  cat >"$unit" <<'EOF'
[Unit]
Description=Bosun agent %i
Wants=network-online.target
After=network-online.target

[Service]
Type=exec
User=bosun-agent-%i
ExecStart=/usr/local/bin/bosun-agent run --config /etc/bosun/agent-%i.toml
Delegate=yes
KillMode=mixed
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
}

prepare_directories() {
  getent group "$cache_group" >/dev/null || groupadd --system "$cache_group"
  install -d -m 0755 "$configs" /var/lib/bosun "$agents"
  install -d -m 2775 -o root -g "$cache_group" "$cache"
}

subordinate() {
  if grep -q "^$1:" "$2" 2>/dev/null; then
    return
  fi
  first="$(awk -F: -v first="$subordinate_first" '
    NF >= 3 && $2 + $3 > first { first = $2 + $3 }
    END { print first }
  ' "$2" 2>/dev/null || echo "$subordinate_first")"
  usermod "$3" "$first-$((first + subordinate_count - 1))" "$1"
}

linger() {
  if command -v loginctl >/dev/null 2>&1 && loginctl enable-linger "$1" >/dev/null 2>&1; then
    return
  fi
  say "lingering, which the podman capability needs, could not be enabled for $1: run loginctl enable-linger $1"
}

prepare_user() {
  user="bosun-agent-$1"
  if ! id -u "$user" >/dev/null 2>&1; then
    useradd --create-home --user-group --shell /usr/sbin/nologin --comment "Bosun agent $1" "$user"
    say "created the user $user"
  fi
  usermod --append --groups "$cache_group" "$user"
  subordinate "$user" /etc/subuid --add-subuids
  subordinate "$user" /etc/subgid --add-subgids
  linger "$user"
  install -d -m 0700 -o "$user" -g "$user" "$agents/$1"
}

register() {
  config="$configs/agent-$1.toml"
  if [ -e "$config" ]; then
    say "agent $1 is registered already, in $config"
    return
  fi
  name="$(uname -n)-$1"
  set -f
  set -- --config "$config" --url "$server" --token "$token" --name "$name" --dir "$agents/$1"
  for label in $labels; do
    set -- "$@" --label "$label"
  done
  set +f
  "$binary" register "$@"
  chown "$user:$user" "$config"
}

start() {
  systemctl enable --quiet "bosun-agent@$1.service"
  systemctl restart "bosun-agent@$1.service"
  say "bosun-agent@$1.service runs; its log is journalctl -u bosun-agent@$1"
}

podman_lacks() {
  if ! command -v podman >/dev/null 2>&1; then
    echo "podman 4.4 or newer"
  else
    found="$(podman --version 2>/dev/null | awk '{ print $NF }')"
    major="${found%%.*}"
    minor="${found#*.}"
    minor="${minor%%.*}"
    case "$major.$minor" in
      *[!0-9.]* | .* | *.) echo "podman 4.4 or newer, where it finds podman '$found'" ;;
      *)
        if [ "$major" -lt 4 ] || { [ "$major" -eq 4 ] && [ "$minor" -lt 4 ]; }; then
          echo "podman 4.4 or newer, where it finds podman $found"
        fi
        ;;
    esac
  fi
  [ -f /sys/fs/cgroup/cgroup.controllers ] || echo "cgroups v2"
  if ! command -v netavark >/dev/null 2>&1 &&
    [ ! -x /usr/libexec/podman/netavark ] && [ ! -x /usr/lib/podman/netavark ]; then
    echo "netavark"
  fi
}

check_podman() {
  podman_lacks | while IFS= read -r lack; do
    say "the agents run without the podman capability, which needs $lack"
  done
}

main() {
  parse "$@"
  check_host
  install_binary
  prepare_directories
  install_unit
  n=1
  while [ "$n" -le "$instances" ]; do
    prepare_user "$n"
    register "$n"
    start "$n"
    n=$((n + 1))
  done
  check_podman
}

main "$@"
