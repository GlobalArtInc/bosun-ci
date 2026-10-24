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

`bosun-server serve` refuses to start when the schema belongs to a newer major version, when the KEK
is missing, or when the object store is unreachable, and says which.

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
  they apply, collected by the cluster's log pipeline.
- Alerts worth having from day one: jobs queued longer than 10 minutes, no online agent in a pool
  that has queued jobs, an outbox row dead, webhook verification failures, agents on a version the
  next server release will refuse.
- `/healthz` says the process is alive; `/readyz` says the database and the object store answer.

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
