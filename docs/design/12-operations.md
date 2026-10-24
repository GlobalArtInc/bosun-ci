# Operations

This document specifies how Bosun is deployed and run: the server in a cluster or on a host, the
agents, configuration, scaling, observability, backups, upgrades, and the path by which Bosun takes
over the CI of the team's own repositories.

## The server

**In a cluster.** The image `globalartltd/bosun-server` carries the binary with the embedded
interface. A Helm chart `bosun` in `GlobalArtInc/helm-charts` deploys it as a Deployment with two
replicas, a Service, an `HTTPRoute` for the interface, the API, the hooks and the runtime services
(including `/twirp/` at the root of the host), and a `GRPCRoute` for agents and external plugins,
behind the cluster's Envoy Gateway. The gateway
needs idle and request timeouts long enough for agent streams (no request timeout on the gRPC
route, a stream idle timeout above the keepalive interval), which the chart sets through the
gateway's traffic policy. A Flux `HelmRelease` in the gitops repository rolls it out. Secrets (the
KEK, the database address, the object store credentials, the App's private key) come from
Kubernetes secrets encrypted in the gitops repository.

**On a host.** The same binary as `bosun-server.service` under the user `bosun`, with
`/etc/bosun/server.toml`, and the directory backend of the object store under `/var/lib/bosun`
when there is one replica.

`bosun-server migrate` brings the schema up to date and exits. `bosun-server serve` checks the KEK,
connects to the database, brings the schema up to date ([07-storage.md](07-storage.md)), and then
serves the REST API, the health endpoints and gRPC on one listener, which speaks HTTP/1.1 and
HTTP/2 without TLS (the gateway terminates TLS); requests whose content type is
`application/grpc` go to the gRPC services, which answer `UNIMPLEMENTED` for a service they do not
have. It refuses to start when the schema belongs to a newer major version, when the KEK is
missing, or when the object store is unreachable, and says which in an error line of its log
before it exits with status 1.

On `SIGTERM` or `SIGINT` the server logs the signal, stops accepting connections, ends every gRPC
stream with the status `UNAVAILABLE` and the message `the server is shutting down: received
SIGTERM` (or `SIGINT`), so that agents reconnect to another replica, waits for the requests in
flight to finish, and exits with status 0. It sets no deadline of its own for that wait; the
platform's grace period before it kills the process is the deadline.

## Agents

`deploy/install-agent.sh <server> <registration-token> [--instances <n>] [--label <l>]...`
on a Debian or Ubuntu host:

- creates `bosun-agent-<n>` users with subordinate id ranges and lingering, the group
  `bosun-cache`, and the directories;
- checks the prerequisites for the `podman` capability (podman 4.4 or newer, cgroups v2,
  `netavark`) and leaves the capability out with a message when they are missing;
- installs the binary to `/usr/local/bin/bosun-agent`, the template unit `bosun-agent@.service`
  with `Delegate=yes`, and the timer for `bosun-agent gc`;
- registers each instance and starts it.

Upgrading an agent is replacing the binary and restarting the instances; an instance that is
running a job finishes it first when it receives `Drain`.

## Configuration

`server.toml`, every key overridable by an environment variable `BOSUN_<SECTION>_<KEY>`:

```toml
[server]
listen = "0.0.0.0:8080"
public_url = "https://ci.example.org"
trusted_proxies = ["10.0.0.0/8"]

[database]
url = "postgres://bosun@db/bosun"
max_connections = 32

[store]
kind = "s3"
bucket = "bosun"
endpoint = "https://s3.example.org"
path_style = true

[keys]
kek_file = "/etc/bosun/kek"
previous_kek_files = []

[github]
app_id = 123456
private_key_file = "/etc/bosun/github-app.pem"
webhook_secret_file = "/etc/bosun/github-webhook-secret"

[[oidc]]
name = "keycloak"
issuer = "https://sso.example.org/realms/main"
client_id = "bosun"
client_secret_file = "/etc/bosun/oidc-secret"
groups_claim = "groups"

[limits]
max_queue_time = "24h"
max_pause = "24h"
max_hold = "2h"
agent_grace = "10m"
```

The file is `--config <file>`, or `/etc/bosun/server.toml` when that exists; without either the
server takes its configuration from the environment alone. A key of the file that the server does
not know is refused, together with the file's name and the line. The variable of a key is its
section and its name in capitals, so `[database] max_connections` is
`BOSUN_DATABASE_MAX_CONNECTIONS`, and a variable wins over the file. Variables under `BOSUN_` that
name no key, such as `BOSUN_TEST_DATABASE_URL`, are left alone.

- `server.listen` defaults to `0.0.0.0:8080`; port 0 takes a free port, which the `listening` line
  of the log names.
- `database.url` has no default. `database.max_connections` defaults to 32 and is at least 2,
  because the migrations at start hold two pooled connections at once.
- The KEK is 64 hexadecimal digits (`openssl rand -hex 32`), in the file `keys.kek_file` or
  directly in `keys.kek`, which is meant for `BOSUN_KEYS_KEK`. Exactly one of them is set;
  surrounding whitespace, such as the newline at the end of a file, does not count.

`agent-<n>.toml` holds the server address, the agent id and credential, labels, `allow_terminal`,
proxy settings, the log spool limit and `cache_max_bytes`.

## Scaling

Replicas share nothing but Postgres and the object store, so the server scales by adding replicas;
each holds the streams of the agents that happened to connect to it. The load that grows with
usage is log ingestion and event fan-out; both go through Postgres, which is the component to size.
Connection pools are per replica, so `max_connections` times the replica count must stay below the
database's limit.

## Observability

- `/metrics` in Prometheus format: queue length and waiting time by pool and label set, jobs by
  status, run and job durations, agents by state, outbox backlog and failures, log ingestion rate,
  webhook processing time, plugin call durations and failures.
- Logs: JSON lines on standard output with the request id, the run, the job and the agent where
  they apply, collected by the cluster's log pipeline. Each line carries `timestamp`, `level`,
  `message`, `target` and the fields of the event; a line written while a request is handled
  carries the request in `span` (`request_id`, `method`, `path`), and every request ends with a
  line `request` with its `status` and `duration_ms`. The request id is the `X-Request-Id` of the
  request when it is 1 to 128 letters, digits, `-`, `_` or `.`, such as the one the gateway sets,
  and a fresh UUIDv7 otherwise; the response carries it back in `X-Request-Id`. The level is
  `info`, except for the database driver, which logs every statement at `info` and is kept to
  `warn` and above.
- Alerts worth having from day one: jobs queued longer than 10 minutes, no online agent in a pool
  that has queued jobs, an outbox row dead, webhook verification failures, agents on a version the
  next server release will refuse.
- `/healthz` says the process is alive; `/readyz` says the database and the object store answer.
  `/healthz` answers `200 {"status": "alive"}` without touching the database. `/readyz` answers
  `200 {"status": "ready"}`, or `503 {"status": "unready", "reason": "the database does not
  answer"}` when the database does not answer a ping within one second, and logs the cause as a
  warning; it recovers by itself when the database returns. Probes give it a timeout above that
  second.

## Backups

- Postgres, with the cluster's usual database backups.
- The object store bucket, with versioning or replication.
- **The KEK.** Without it every secret is lost. It is kept outside the cluster that runs Bosun.

A restore needs the three together. Logs and artifacts missing from a restored bucket show as
"expired" on the run page rather than failing it.

## Upgrades

Migrations are additive inside a major version, so replicas of version N and N+1 can run side by
side during a rolling update, and a rollback of the binary is safe. A server accepts agents of its
own minor version and the previous one; the agents page marks older ones, and the release notes say
when the next release will refuse them.

## Taking over the team's CI

1. Until Bosun runs jobs, its own CI runs on GitHub Actions on the self-hosted runners.
2. From M1, an agent instance on the runner host runs Bosun's simple workflows next to the GitHub
   runners.
3. From M2, the CI of Keel runs on Bosun from `.bosun/workflows/`, a copy of its GitHub workflows,
   while GitHub Actions keeps running in parallel until the results agree for a week of pushes.
4. Then the project setting `github_workflows` is switched on, GitHub Actions is disabled for the
   repository, the required checks of its branch protection are bound to Bosun's App instead of
   GitHub Actions (their names stay the same), and the copy in `.bosun/` is deleted. Repository by
   repository, the same steps.
