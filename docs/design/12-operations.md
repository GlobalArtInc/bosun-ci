# Operations

This document specifies how Bosun is deployed and run: the server in a cluster or on a host, the
agents, configuration, scaling, observability, backups, upgrades, and the path by which Bosun takes
over the CI of the team's own repositories.

## Delivery

Bosun installs on any Linux server the same way: one container image and a Postgres. Nothing in it
assumes the team's own cluster.

**The image.** `globalartltd/bosun` on Docker Hub, for `linux/amd64` and `linux/arm64`. It carries
`bosun-server` with the embedded interface, the `bosun-agent` binaries for `x86_64` and `aarch64`,
and `install-agent.sh`, so the server itself hands agents everything they need. It runs as uid and
gid 210 (`bosun`), listens on 8080, keeps every state of its own under `/bosun-data`, and checks
itself with `bosun-server check`, which is also the image's health check.

| Tag | Meaning |
|-----|---------|
| `<YYYYMMDDHHMMSS>-<sha7>` | one build of `main`; production pins one of these |
| `edge` | the newest build of `main` |
| `<version>` | a release, built from a tag `v<version>` |
| `latest` | the newest release |

A release also attaches the binaries to its GitHub release, for hosts without a container runtime.

The `Dockerfile` at the root of the repository builds the image. Its build stage runs on the
builder's own platform and cross-compiles static musl binaries of `bosun-server` and `bosun-agent`
for `x86_64` and `aarch64` with `cargo-zigbuild`, whose Zig toolchain also compiles the C code of
the TLS crates for either target. The image of each architecture is Alpine with the user `bosun`
and only copies files in: its own `bosun-server` to `/usr/local/bin/`, both agents to
`/usr/lib/bosun/agent/linux-x86_64` and `linux-aarch64`, and the empty `/bosun-data`; no step runs
on the target architecture, so building the `arm64` image needs no emulation. `install-agent.sh` is
built into the server binary. The Dockerfile's target `cli` leaves a static `bosun` for the
builder's architecture (`docker build --target cli --output type=local,dest=. .`).

CI builds the image on every commit and tests it with `hack/test-image.sh`: started against a
Postgres container with nothing but the database address and a volume, an agent installed by the
served script inside a second container with systemd, the two-job workflow, a restart on the same
volume, and `deploy/compose.yaml`. `.github/workflows/publish.yml` publishes: on every push to
`main` the timestamp tag and `edge`, on a tag `v<version>` that names the workspace version the
version and `latest`, both architectures in one manifest list, built by a BuildKit builder in a
container that the job creates and removes.

**What a host needs.** A container runtime (podman, docker, or Kubernetes), Postgres 15 or newer,
and a reverse proxy that terminates TLS (nginx, Caddy, HAProxy, Envoy, or a Kubernetes gateway). The
proxy must speak HTTP/2 to Bosun for the agent and plugin streams: `grpc_pass` for
`/bosun.agent.v1.` and `/bosun.plugin.v1.` paths in nginx, an `h2c://` upstream in Caddy. It needs
no request timeout on those paths and an idle timeout above the keepalive interval of 30 seconds,
and it passes the request's `Host` and sets `X-Forwarded-Proto`, from which the server tells an
agent host its own address ([Agents](#agents)). `docs/install/` in the repository has the
examples.

**The data directory.** `server.data_dir` (`/bosun-data` in the image, `/var/lib/bosun` for a
binary on a host) holds what a single host needs and nothing else:

| Path | Content |
|------|---------|
| `etc/bosun.toml` | the configuration file, read when no `--config` is given and `/etc/bosun/server.toml` does not exist |
| `keys/kek` | the KEK, generated on the first start when the configuration names none ([07-storage.md](07-storage.md)) |
| `store/` | the directory backend of the object store, used when no S3 store is configured |
| `admin.token` | the first owner's token, written on the first start ([08-access.md](08-access.md)) |
| `admin.password` | the first owner's password, written next to `admin.token`; both go when that password changes ([08-access.md](08-access.md#local-sign-in)) |

So a single host needs exactly two settings to start, the database address and the data directory,
and a mounted volume to keep its state. The key and the token are written to a temporary file in
their directory and renamed into place, with mode `0600`, so a crash leaves either the whole file or
none; the directory `keys/`, and the data directory when the server creates it, get mode `0700`.
Starts after the first reuse both and write nothing. A KEK that the configuration names, in either
form, means nothing is generated, and `bootstrap.admin` means no `admin.token`.

**Quick start on one host:**

```sh
mkdir -p /srv/bosun/data
chown 210:210 /srv/bosun/data
podman run -d --name bosun -p 8080:8080 \
  -v /srv/bosun/data:/bosun-data:Z \
  -e BOSUN_DATABASE_URL=postgres://bosun:change-me@db.example.org:5432/bosun \
  docker.io/globalartltd/bosun:latest
podman exec bosun cat /bosun-data/admin.token
```

`deploy/compose.yaml` brings up Bosun and Postgres together, and `deploy/bosun.container` is a
quadlet that runs the image as a systemd service under rootless podman. A secret such as the database
address goes through the runtime's secret mechanism rather than the command line.

**Several replicas** need what one host does not: an S3 store instead of the directory backend, and a
KEK given by the configuration, the same for every replica. A replica that would have to generate a
KEK, or that finds the directory backend configured next to other replicas on other hosts, is a
deployment mistake the documentation warns about; the server cannot detect it.

**Kubernetes.** A Helm chart `bosun` in `GlobalArtInc/helm-charts` deploys the image as a Deployment
with two replicas, a Service, an `HTTPRoute` for the interface, the API, the hooks and the runtime
services (including `/twirp/` at the root of the host), and a `GRPCRoute` for agents and external
plugins, behind the cluster's Envoy Gateway, with the gateway's timeouts set as above through its
traffic policy. This is how the team runs it on prod1: a Flux `HelmRelease` in the gitops
repository rolls it out, and the secrets (the KEK, the database address, the object store
credentials, the App's private key) come from Kubernetes secrets encrypted in the gitops repository.

**On a host without containers.** The binary from the release as `bosun-server.service` under the
user `bosun`, with `/etc/bosun/server.toml` and the data directory `/var/lib/bosun`.

## The server process

`bosun-server migrate` brings the schema up to date and exits. `bosun-server admin create <login>`
brings the schema up to date the same way, creates an owner and prints its first personal token
([08-access.md](08-access.md)); neither command needs the KEK. `bosun-server secrets rewrap` reads the KEKs
as `serve` does, without ever generating one, brings the schema up to date, re-encrypts the data
key of every secret that is not under the current KEK, prints
`re-encrypted the data keys of <n> secrets under the KEK <id>`, and exits with 1 and an error that
names each secret it could not re-encrypt ([07-storage.md](07-storage.md#secrets-at-rest)). It logs
to standard error. `bosun-server serve` loads the KEK,
or generates it into the data directory when the configuration names none,
connects to the database, brings the schema up to date ([07-storage.md](07-storage.md)), creates the
owner of `bootstrap.admin`, or without it the owner `admin` with its token in `admin.token`, when
the database holds no user ([08-access.md](08-access.md)), and then serves the REST API, the health
endpoints and gRPC on one listener, which speaks HTTP/1.1 and HTTP/2 without TLS (the gateway
terminates TLS); requests whose content type is `application/grpc` go to the gRPC services, which
answer `UNIMPLEMENTED` for a service they do not have. It refuses to start when the schema belongs
to a newer major version, when a configured KEK is unreadable or malformed, or when the object
store cannot be used: before it connects to the database, it opens the store, writes the empty
object `probes/start` and deletes it again, so an S3 endpoint that does not answer, a bucket that
does not exist and credentials the store refuses all stop it. It says which in an error line of its
log before it exits with status 1.

`bosun-server check` reads the configuration as `serve` does, asks `GET /readyz` of the address
of `server.listen` (the loopback address when that is every address) with a timeout of five
seconds, and exits with 0 and the line `the server at <address> is ready` when the answer is `200`,
or with 1 and an error line that says why: no answer, or the `reason` of the `503`. It logs to
standard error and is the health check of the image.

On `SIGTERM` or `SIGINT` the server logs the signal, stops accepting connections, ends every gRPC
stream with the status `UNAVAILABLE` and the message `the server is shutting down: received
SIGTERM` (or `SIGINT`), so that agents reconnect to another replica, waits for the requests in
flight to finish, and exits with status 0. It sets no deadline of its own for that wait; the
platform's grace period before it kills the process is the deadline.

## Agents

An agent goes onto any Linux server that can reach the server's address. The pool page of the
interface, and `bosun pool token create`, print a ready command with a fresh registration token:

```sh
curl -fsSL https://bosun.example.org/install-agent.sh | sudo sh -s -- --token brt_… [--instances 4] [--label deploy]
```

The server serves `install-agent.sh` and the agent binaries of its own version at
`/_runtime/agent/linux-<arch>`, so the host downloads nothing from anywhere else, and the agent always
matches the server. Neither needs a token. The binaries are the files `linux-x86_64` and
`linux-aarch64` of `agents.binary_dir`; one that is missing answers `404` with the path it looked
for. The served script names the server and its version: the address is the one the host fetched
the script from, the `Host` of the request (or the authority of an HTTP/2 request) with `https`
when `X-Forwarded-Proto` says so and `http` otherwise, and a host with characters other than
letters, digits, `-`, `.`, `:`, `[` and `]` is refused with `422`, so the address is safe inside
the script. `bosun pool token create` builds the command from the server address of its profile.

On a Debian or Ubuntu host with systemd, run as root, the script:

- downloads the agent for the host's architecture, refuses it unless `bosun-agent --version`
  names the server's version, and installs it to `/usr/local/bin/bosun-agent` by a rename;
- creates `bosun-agent-<n>` users with subordinate id ranges and lingering, the group
  `bosun-cache`, and the directories `/etc/bosun`, `/var/lib/bosun/agents/<n>` (mode `0700`, of the
  instance's user) and `/var/lib/bosun/cache` (group `bosun-cache`, mode `2775`);
- checks the prerequisites for the `podman` capability (podman 4.4 or newer, cgroups v2,
  `netavark`) and leaves the capability out with a message when they are missing;
- installs the template unit `bosun-agent@.service`, which runs instance `<n>` as
  `bosun-agent-<n>` with `Delegate=yes` and `KillMode=mixed`, so the agent gets its cgroup for the
  jobs and its `SIGTERM` alone on a stop, and, from M2 when `bosun-agent gc` exists, the timer for
  it;
- registers each instance whose `/etc/bosun/agent-<n>.toml` does not exist yet, named
  `<host name>-<n>`, gives the file to the instance's user, and enables and restarts its unit.

Everything the script does is in functions that run after the whole script has arrived, so a
download cut short runs nothing.

Upgrading an agent is running the same command again, which replaces the binary with the server's
version and restarts the instances. A restart cancels a running job as `SIGTERM` does
([05-execution.md](05-execution.md#registration-and-identity)), so the agents of the host are
drained first and upgraded once they are idle; they come back drained, and lifting the drain gives
them jobs again ([05-execution.md](05-execution.md#draining-and-deleting-agents)).

## Configuration

`server.toml`, every key overridable by an environment variable `BOSUN_<SECTION>_<KEY>`:

```toml
[server]
listen = "0.0.0.0:8080"
data_dir = "/var/lib/bosun"
public_url = "https://ci.example.org"
trusted_proxies = ["10.0.0.0/8"]

[database]
url = "postgres://bosun@db/bosun"
max_connections = 32

[store]
kind = "s3"
endpoint = "https://s3.example.org"
bucket = "bosun"
region = "us-east-1"
path_style = true

[keys]
kek_file = "/etc/bosun/kek"
previous_kek_files = []

[bootstrap]
admin = "roman"

[github]
app_id = 123456
private_key_file = "/etc/bosun/github-app.pem"
webhook_secret_file = "/etc/bosun/github-webhook-secret"
server_url = "https://github.com"
api_url = "https://api.github.com"

[[oidc]]
name = "keycloak"
issuer = "https://sso.example.org/realms/main"
client_id = "bosun"
client_secret_file = "/etc/bosun/oidc-secret"
groups_claim = "groups"

[tools]
node_upstream = "https://nodejs.org/dist"
node_musl_upstream = "https://unofficial-builds.nodejs.org/download/release"
node12 = "24.22.1"
node16 = "24.22.1"
node20 = "24.22.1"
node24 = "24.22.1"

[agents]
binary_dir = "/usr/lib/bosun/agent"
heartbeat_interval = "30s"
offline_after = "60s"
assignment_lease = "30s"

[limits]
failed_sign_ins = 10
max_queue_time = "24h"
max_pause = "24h"
max_hold = "2h"
agent_grace = "10m"
```

The file is `--config <file>`, or `/etc/bosun/server.toml` when that exists, or `etc/bosun.toml` in
the data directory that `BOSUN_SERVER_DATA_DIR` or the default names, when that exists; without any
of them the server takes its configuration from the environment alone. A key of the file that the
server does not know is refused, together with the file's name and the line. The variable of a key
is its section and its name in capitals, so `[database] max_connections` is
`BOSUN_DATABASE_MAX_CONNECTIONS`, and a variable wins over the file. Variables under `BOSUN_` that
name no key, such as `BOSUN_TEST_DATABASE_URL`, are left alone.

- `server.listen` defaults to `0.0.0.0:8080`; port 0 takes a free port, which the `listening` line
  of the log names.
- `server.public_url` is the address people and agents reach the server at, an http or https
  address with a host and maybe a path, without a user, a query or a fragment. It has no default;
  without it the `bosun.url` and `bosun.run_url` of expressions are empty
  ([03-expressions.md](03-expressions.md)).
- `database.url` has no default. `database.max_connections` defaults to 32 and is at least 2,
  because the migrations at start hold two pooled connections at once.
- The KEK is 64 hexadecimal digits (`openssl rand -hex 32`), in the file `keys.kek_file` or
  directly in `keys.kek`, which is meant for `BOSUN_KEYS_KEK`. At most one of them is set; with
  neither, the server uses `keys/kek` in the data directory and generates it on the first start.
  Surrounding whitespace, such as the newline at the end of a file, does not count.
- `keys.previous_kek_files` lists the files of earlier KEKs, in the same form, whose data keys the
  server still opens while `bosun-server secrets rewrap` has not re-encrypted them under the current
  KEK ([07-storage.md](07-storage.md#secrets-at-rest)); it is empty by default. Its variable is a
  list separated by commas. A file that cannot be read or does not hold a KEK stops the server as
  the current KEK's does.
- `server.data_dir` defaults to `/var/lib/bosun`; the image sets `/bosun-data`. A path setting
  that is empty is refused.
- `agents.binary_dir` is the directory of the agent binaries the server hands out,
  `/usr/lib/bosun/agent` by default, where the image puts them.
- `server.trusted_proxies` lists the reverse proxies whose `X-Forwarded-For` names the address of
  a sign-in, each an address such as `10.0.0.7` or a network such as `10.0.0.0/8`; it is empty by
  default, so the header counts for nothing. Its variable is a list separated by commas, such as
  `BOSUN_SERVER_TRUSTED_PROXIES=10.0.0.0/8,fd00::/8`.
- `limits.failed_sign_ins`, 10 by default and at least 1, is the number of failed sign-ins of an
  account or an address after which it is locked ([08-access.md](08-access.md#local-sign-in)).
- `store.kind` is `directory`, the default, or `s3`. `store.path` is the root of the directory
  backend, `store/` in the data directory by default, so `/var/lib/bosun/store` on a host.
- For `s3`, `store.endpoint` is the address of the S3 API, http or https with a host and maybe a
  port but no path, such as `https://s3.eu-central-1.amazonaws.com`, and `store.bucket` a bucket
  that exists; neither has a default. `store.region` is the region requests are signed for,
  `us-east-1` by default. `store.path_style`, `false` by default, names the bucket in the path
  rather than in the host, which most self-hosted S3 servers and every endpoint given as an IP
  address need. `store.access_key_id` and `store.secret_access_key` are set together, usually as
  `BOSUN_STORE_ACCESS_KEY_ID` and `BOSUN_STORE_SECRET_ACCESS_KEY` from a secret; with neither, the
  server takes `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` from its environment, and with
  neither of those it refuses to start. A key of one kind set while `store.kind` is the other is
  refused too, so a bucket configured without `kind = "s3"` cannot leave the server on the
  directory backend unnoticed. The certificate of an https endpoint is verified against the public
  web PKI built into the binary.
- The bucket of an S3 store has a lifecycle rule that aborts incomplete multipart uploads after a
  day, which removes the uploads of a replica that died while it wrote
  ([07-storage.md](07-storage.md#the-object-store)), and its credentials allow `s3:GetObject`,
  `s3:PutObject`, `s3:DeleteObject`, `s3:ListBucket` and `s3:AbortMultipartUpload` on it.
- `bootstrap.admin` has no default. When it is set it is the login of the owner that `serve`
  creates on a start against a database without users ([08-access.md](08-access.md)), and a value
  that is not a login is refused like any other setting the server cannot use.
- Durations are a whole number followed by `ms`, `s`, `m` or `h`, such as `30s`, and are more than
  zero.
- `[github]` configures the GitHub App ([06-runtime-services.md](06-runtime-services.md#the-forge)).
  `github.app_id` is the App's number, `github.private_key_file` the file of its private key, in
  PEM as the forge hands it out, and `github.webhook_secret_file` the file of the webhook secret
  the App signs its deliveries with, whose surrounding whitespace does not count. The three are set
  together or not at all, and a server without them has no forge: it takes no webhooks and its
  organisations bind no repositories. `github.server_url` is the forge's own address,
  `https://github.com` by default, and `github.api_url` the address of its REST API, which
  defaults to `https://api.github.com` for GitHub and to `<server_url>/api/v3` for any other
  server, the address at which an enterprise server serves it; both are http or https addresses
  with a host and without a user, a query or a fragment, and setting either without the App is
  refused. The runs of repository-bound projects see both in `github.server_url` and
  `github.api_url`, and `github.graphql_url` is the API's address with a final `v3` replaced by
  `graphql`, or with `graphql` appended when it has none, as the forge serves it. `serve` reads
  both files and builds the App's client before it connects to the database, so an unreadable
  file, a key the client refuses and an empty secret stop it with an error line that names the
  file.
- `[tools]` configures the actions and tools the server hands to agents
  ([05-execution.md](05-execution.md#actions-and-tools)). `tools.node12`, `tools.node16`,
  `tools.node20` and `tools.node24` are the runtime map: the exact version of Node, three numbers
  such as `24.22.1`, that runs an action whose `using` names that runtime. Each defaults to
  `24.22.1`, so every Node action runs on Node 24 as on current hosted runners, and a run notes an
  action that runs on another major version than it declares. `tools.node_upstream`, by default
  `https://nodejs.org/dist`, is where the server fetches the glibc builds of those versions, and
  `tools.node_musl_upstream`, by default `https://unofficial-builds.nodejs.org/download/release`,
  the musl builds, each with its `SHASUMS256.txt`; either is an http or https address with a host,
  or a file address of a directory with the same layout, such as a mirror on a host without access
  to the internet, without a user, a password, a query or a fragment, so that no credential
  reaches the messages that name an upstream address. The server fetches an archive the first
  time an agent asks for it, with the proxy of its environment (`HTTPS_PROXY`), and keeps it in the
  object store. Actions come from the forge through the GitHub App, so a server without one runs no
  job that uses an action.
- `agents.heartbeat_interval` defaults to 30 seconds and `agents.offline_after` to 60 seconds,
  which must be longer than the interval; `limits.agent_grace` defaults to 10 minutes
  ([05-execution.md](05-execution.md)). `agents.assignment_lease`, the time an agent has to accept
  an assignment before its job returns to the queue, defaults to 30 seconds, and
  `limits.max_queue_time`, after which a job no agent took is cancelled with `no_agent`, to 24
  hours ([04-runs.md](04-runs.md#dispatch)). Tests shorten them.

`agent-<n>.toml` holds the server address, the agent's directory, the agent id and credential,
labels, `allow_terminal`, proxy settings, the log spool limit and `cache_max_bytes`:

```toml
url = "https://ci.example.org"
dir = "/var/lib/bosun/agents/1"
agent_id = "0199a5f2-2c6f-7a3e-8a3b-5f0d4c1e2b3a"
credential = "bat_…"
labels = ["deploy"]
http_proxy = "http://proxy.example.org:3128"
https_proxy = "http://proxy.example.org:3128"
no_proxy = "localhost,.example.org"
spool_max_bytes = 268435456
```

`bosun-agent register` writes the first five keys ([05-execution.md](05-execution.md)); the others
are optional and set by hand, and `dir` is an absolute path. The proxy keys reach every step and
`spool_max_bytes` defaults to 256 MiB. `allow_terminal` and `cache_max_bytes` arrive with the debug
terminal and cache volumes; until then, as any key the agent does not know, they are refused with
the file's name, the line and the column.

## Scaling

Replicas share nothing but Postgres and the object store, so the server scales by adding replicas;
each holds the streams of the agents that happened to connect to it. The load that grows with
usage is log ingestion and event fan-out; both go through Postgres, which is the component to size.
Connection pools are per replica, and each replica holds one more connection that listens for
notifications, so `max_connections` plus one, times the replica count, must stay below the
database's limit.

## Observability

- `/metrics` in Prometheus format: queue length and waiting time by pool and label set, jobs by
  status, run and job durations, agents by state, outbox backlog and failures, log ingestion rate,
  webhook processing time, plugin call durations and failures.
- Logs: JSON lines on standard output (on standard error for the `admin` commands, whose standard
  output is their result) with the request id, the run, the job and the agent where they apply,
  collected by the cluster's log pipeline. Each line carries `timestamp`, `level`, `message`,
  `target` and the fields of the event; a line written while a request is handled carries the
  request in `span` (`request_id`, `method`, `path`), and every request ends with a line `request`
  with its `status` and `duration_ms`. The request id is the `X-Request-Id` of the request when it
  is 1 to 128 letters, digits, `-`, `_` or `.`, such as the one the gateway sets, and a fresh UUIDv7
  otherwise; the response carries it back in `X-Request-Id`. The level is `info`, except for the
  database driver, which logs every statement at `info` and is kept to `warn` and above.
- Webhooks log `received a webhook delivery` with its `delivery`, `event` and whether it was
  `stored`, `processed a webhook delivery` with its `duration_ms`, a warning for a delivery whose
  signature is missing or wrong, and a warning for each failed processing with its `error`
  ([06-runtime-services.md](06-runtime-services.md#deliveries)).
- The outbox logs `delivered an outbox row` with its `row`, `kind`, `key`, the rows it `dropped`
  and `duration_ms`, a warning for each failed delivery with its `error` and `wait_ms`, and an
  error for each row that dies ([07-storage.md](07-storage.md#the-outbox)).
- Alerts worth having from day one: jobs queued longer than 10 minutes, no online agent in a pool
  that has queued jobs, an outbox row dead, webhook verification failures, agents on a version the
  next server release will refuse.
- `/healthz` says the process is alive; `/readyz` says the database and the object store answer.
  `/healthz` answers `200 {"status": "alive"}` without touching the database. `/readyz` answers
  `200 {"status": "ready"}`, or `503 {"status": "unready", "reason": "the database does not
  answer"}` when the database does not answer a ping within one second, or `503 {"status":
  "unready", "reason": "the object store does not answer"}` when a read of `probes/readiness`, a
  key that never holds an object, fails otherwise than with "not found" or takes more than a
  second. It logs the cause as a warning and recovers by itself when the database or the store
  returns. Probes give it a timeout above two seconds.

## Backups

- Postgres, with the cluster's usual database backups.
- The object store bucket, with versioning or replication.
- **The KEK.** Without it every secret is lost. It is kept outside the cluster that runs Bosun; on a
  single host with a generated key, that means a copy of `keys/kek` from the data directory, which
  the server's log asks for on the start that generated it. After a rotation, the backups of the
  database taken before `bosun-server secrets rewrap` need the earlier KEK.

A restore needs the three together. Logs and artifacts missing from a restored bucket show as
"expired" on the run page rather than failing it.

## Upgrades

Migrations are additive inside a major version, so replicas of version N and N+1 can run side by
side during a rolling update, and a rollback of the binary is safe. A server accepts agents of its
own minor version and the previous one; the agents page marks older ones, and the release notes say
when the next release will refuse them.

## Taking over the team's CI

1. Until Bosun runs jobs, its own CI runs on GitHub Actions on GitHub's hosted runners, which
   cost a public repository nothing.
2. From M1, an agent instance on the runner host runs Bosun's simple workflows next to the GitHub
   runners.
3. From M2, the CI of Keel runs on Bosun from `.bosun/workflows/`, a copy of its GitHub workflows,
   while GitHub Actions keeps running in parallel until the results agree for a week of pushes.
4. Then the project setting `github_workflows` is switched on, GitHub Actions is disabled for the
   repository, the required checks of its branch protection are bound to Bosun's App instead of
   GitHub Actions (their names stay the same), and the copy in `.bosun/` is deleted. Repository by
   repository, the same steps.
