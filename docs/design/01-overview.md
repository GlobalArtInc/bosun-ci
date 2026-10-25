# Overview

This document states what Bosun is, what it deliberately is not, which processes it consists of,
how the codebase is layered and named, and how its versions relate. The other documents in
`docs/design/` specify each subsystem; this one gives the frame they fit into.

## What Bosun is

Bosun is a CI/CD server. It runs workflows when a forge reports an event, on a schedule, when a
person or a script asks, when another workflow finishes, or when a plugin says so. The work runs
on agents the team owns, on its own machines, and every result, log and artifact stays on the
team's infrastructure.

Three properties are the reason it exists rather than a configuration of an existing server:

- **Compatible syntax.** Workflows are written in the workflow syntax of GitHub Actions. A
  repository that already has `.github/workflows/` runs on Bosun without a rewrite, and the actions
  of the public ecosystem (`uses: owner/repo@ref`) work, including the ones that talk to the cache
  and artifact services.
- **A run is controllable while it runs.** An operator can pause a run or one job, set a
  breakpoint before a step, skip a step or a job, mark a job as succeeded with outputs of their
  choosing, re-run one job or only the failed ones, hold a failed job alive and open a terminal
  inside it, retry the failed step in the same environment, and approve a gate while giving it
  input values that flow into the jobs after it.
- **Customisable from the outside.** The server is extended by plugins (triggers, notifiers,
  admission policies, secret backends, executors), workflows are edited as a graph in the browser
  and run from a draft before they are committed, and everything the browser can do is available
  through the API, the CLI and an MCP endpoint.

## Goals and non-goals

Goals, in order of priority:

1. Run the existing workflows of `GlobalArtInc` and `OwnLate` unchanged on agents the team owns,
   starting with the CI of Keel.
2. Give operators full control over a running run, with every intervention recorded.
3. Make the server extensible without forking it, and every operation scriptable.
4. Stay operable by one engineer: one database, one object store, stateless server replicas,
   agents that are a single static binary.

Non-goals:

- A hosted multi-tenant service. One installation serves the organisations of one owner.
- Windows and macOS agents. The agent is written so that they can be added; none is built.
- Kubernetes pods and on-demand virtual machines as executors. The executor boundary is a plugin
  extension point ([10-plugins.md](10-plugins.md)); neither executor is built.
- Compatibility with other CI servers' pipeline languages.
- Reproducing the preinstalled software of hosted runner images. A label that names a hosted
  image is mapped to an image or a pool the team chooses ([05-execution.md](05-execution.md)).
- The parts of GitHub Actions that are GitHub's product rather than workflow execution: the
  marketplace, billing, hosted larger runners, deployment protection apps.

## Processes

```
 forge (GitHub App) ──webhooks──▶ ┌───────────────────────────┐ ◀──gRPC, opened by the agent── bosun-agent
                                  │ bosun-server (N replicas) │                              ├ shell executor
 browser ──HTTP, SSE, WebSocket──▶│ API, planner, scheduler,  │ ◀──gRPC── external plugins     └ podman executor
 bosun CLI, MCP clients ─────────▶│ runtime services, plugins │
                                  └────────────┬──────────────┘
                                   Postgres    │    object store (S3 or a directory)
```

- **bosun-server** holds no state of its own. Everything lives in Postgres and in the object
  store, so replicas can be added and removed freely. It serves the web interface, the REST API,
  the event stream, the forge webhooks, the runtime services jobs call, the MCP endpoint and the
  gRPC endpoint for agents and external plugins.
- **bosun-agent** runs jobs. It opens the connection to the server itself and keeps it open, so an
  agent behind NAT or in a closed network needs no inbound access. One agent process is one slot:
  it runs one job at a time.
- **bosun** is the CLI. Besides being an API client, it runs a workflow locally with the same
  execution engine the agent uses and the same runtime services the server offers (`bosun exec`).
- **Postgres** is the only database. It also carries the job queue and the notifications between
  replicas; there is no message broker.
- **The object store** keeps logs after a job finishes, artifacts, caches, downloaded actions and
  tools. It is S3-compatible storage, or a local directory for a single-replica installation.

## Names

| What | Name |
|------|------|
| Product | Bosun, id `bosun` |
| Binaries | `bosun-server`, `bosun-agent`, `bosun` (CLI) |
| Crates | `bosun-*` |
| Frontend packages | `@globalart/bosun-web`, `@globalart/bosun-wasm` |
| Database and schema | database `bosun`, schema `bosun` |
| Image | `globalartltd/bosun`, uid and gid 210, port 8080, data directory `/bosun-data` |
| Directories on a server host | `/etc/bosun`, `/var/lib/bosun` |
| Directories on an agent host | `/etc/bosun/agent-<n>.toml`, `/var/lib/bosun/agents/<n>`, `/var/lib/bosun/cache` |
| Unix users | `bosun` for a server on a host, `bosun-agent-<n>` per agent instance |
| systemd units | `bosun-server.service`, `bosun-agent@<n>.service` |
| Session cookie | `__Host-bosun_session` |
| Workflow directory | `.bosun/workflows/` in a repository |
| Token prefixes | `bst_` personal token, `brt_` agent registration token, `bat_` agent credential, `bpt_` plugin credential, `bss_` session |
| Agent protocol | `bosun.agent.v1` |

Bosun is a working name. Renaming it is a mechanical change while no installation exists.

## Technology

| Aspect | Choice |
|--------|--------|
| Backend | Rust 1.98, tokio, axum 0.8, tonic for gRPC, sea-orm over sqlx, sqlmig for migrations |
| Storage | Postgres 15 or newer; S3-compatible object store or a local directory |
| Frontend | React 19, Vite, Radix, TanStack Query, React Router; React Flow for graphs; Monaco for YAML |
| Agent | one static musl binary per architecture; podman for the container executor |
| Delivery | one container image that runs on any Linux host with Postgres (podman, docker, compose, a quadlet, or the Helm chart in Kubernetes), or the server binary with a systemd unit; agents installed on any host with one command that fetches the binary from the server |

The frontend is built into the server binary, so one artifact carries the whole server.

## Codebase

| Crate | Role | May depend on |
|-------|------|---------------|
| `bosun-types` | identifiers, statuses, the workflow IR, API and event shapes, error codes | nothing of ours |
| `bosun-expr` | the expression language: parser, evaluator, context availability | `bosun-types` |
| `bosun-workflow` | YAML with source spans, validation, diagnostics, lowering to the IR | `bosun-types`, `bosun-expr` |
| `bosun-proto` | protobuf definitions of the agent and plugin protocols | `bosun-types` |
| `bosun-db` | entities, migrations, queries, the queue | `bosun-types` |
| `bosun-blob` | the object store interface with S3 and directory backends | nothing of ours |
| `bosun-forge` | forge clients: the GitHub App first | `bosun-types` |
| `bosun-services` | the runtime services jobs call (artifacts, caches, block uploads) over the object store and an index interface | `bosun-types`, `bosun-blob` |
| `bosun-plugin-host` | WebAssembly host for plugins (from M5) | `bosun-types` |
| `bosun-server` | HTTP and gRPC endpoints, planner, scheduler, the database index of the runtime services | every crate above |
| `bosun-server-bin` | binary `bosun-server`: command line, configuration, start | `bosun-types`, `bosun-server` |
| `bosun-runner` | the execution engine: steps, actions, executors, workflow commands, masking | `bosun-types`, `bosun-expr`, `bosun-workflow`, `bosun-proto` |
| `bosun-agent` | agent binary: registration, connection, slot | `bosun-types`, `bosun-runner`, `bosun-proto` |
| `bosun-cli` | CLI binary `bosun` | `bosun-types`, `bosun-workflow`, `bosun-runner`, `bosun-services`, `bosun-blob` |
| `bosun-wasm` | WebAssembly bindings of `bosun-workflow` and `bosun-expr`, published as `@globalart/bosun-wasm` (from M4) | `bosun-workflow`, `bosun-expr` |
| `bosun-testkit` | database per test, server harness, scripted fake agent, fake forge | anything, dev-only |

Rules that CI enforces with `hack/check-deps.sh`, which reads the graph from `cargo metadata` with
all features on:

- a crate depends directly only on the crates of ours its row allows, counting normal and build
  dependencies; dev-dependencies are free;
- a crate without a row fails the check, so a new crate arrives together with its row here;
- `bosun-types`, `bosun-expr` and `bosun-workflow` do no I/O, reach no async runtime, database,
  HTTP or gRPC crate, and build for `wasm32-unknown-unknown`: the editor in the browser runs the
  same parser ([09-frontend.md](09-frontend.md));
- `bosun-runner`, `bosun-agent`, `bosun-cli` and `bosun-wasm` reach neither `bosun-db` nor
  `bosun-server`, directly or through other crates. An agent never talks to Postgres;
- `bosun-server` reaches neither `bosun-runner` nor `bosun-agent`. The server never executes a
  step;
- `bosun-testkit` is only ever a dev-dependency.

## Repository layout

```
crates/      bosun-types, bosun-expr, bosun-workflow, bosun-db, ... (lib crates)
bin/         bosun-server, bosun-agent, bosun (bin crates)
proto/       .proto files of the agent and plugin protocols
web/         the frontend (from M1)
contracts/   golden fixtures, the workflow JSON Schema, generated openapi.json
compat/      the compatibility corpus: workflows with their expected results
deploy/      Dockerfile, systemd units, install-agent.sh, sample configs
docs/        design and plan
hack/        scripts CI runs
```

## Versions

All binaries share one version, taken from the workspace. Three other version planes exist and
move independently of it:

- **REST API** under `/api/v1`. Inside `v1` changes are additive.
- **Agent protocol** `bosun.agent.v1`. The agent announces its version and capabilities in its
  first message; the server accepts agents of its own minor version and the previous one, refuses
  any other and tells it why, so a server upgrade never strands a fleet without a message in its
  log.
- **Database schema**. Migrations inside a major version are additive only
  ([07-storage.md](07-storage.md)).

The workflow syntax has no version of its own: it is the GitHub Actions syntax plus the extensions
listed in [02-workflows.md](02-workflows.md).

## Glossary

| Term | Meaning |
|------|---------|
| organisation | top-level owner of projects; maps to a forge organisation or stands alone |
| project | a repository bound to Bosun, or a repository-less project for server-side workflows |
| workflow | a named definition; its source is a file in the repository or a record on the server |
| revision | an immutable version of a workflow definition, identified by the hash of its source |
| draft | an unsaved edit of a workflow kept on the server; it can be run |
| run | one execution of a workflow revision for one trigger |
| attempt | one pass of a run; a re-run adds an attempt and carries over the results it does not re-run |
| job template | a job as written; with a matrix it expands to several jobs |
| job | the unit of scheduling: runs on one agent, or is a gate that runs on none |
| step | the unit of execution inside a job |
| gate | a job without steps that waits for approval and turns approver input into outputs |
| agent, slot | a process that runs jobs; one agent process has one slot |
| pool | a named set of agents with an access list and a trust level |
| label | a string an agent advertises and `runs-on` selects by |
| executor | how an agent runs a job: `shell` on the host, or `podman` in containers |
| environment | a deployment target with protection rules, secrets and variables |
| operator command | an intervention in a run by a person, a token or a plugin |
| journal | the append-only record of everything that happened to a run |
| hold | a failed job kept alive for inspection instead of being torn down |

## Documents

- `01-overview.md`: this document
- `02-workflows.md`: workflow sources, the compatibility surface, extensions, the IR
- `03-expressions.md`: the expression language and where each expression is evaluated
- `04-runs.md`: runs, attempts, the state machine, operator commands, gates, re-runs
- `05-execution.md`: the agent, its protocol, the execution engine and the executors
- `06-runtime-services.md`: the job token, artifacts, caches, actions, statuses on the forge
- `07-storage.md`: Postgres, the queue, the object store, logs, retention, secrets at rest
- `08-access.md`: identity, roles, tokens, environments, untrusted code, audit
- `09-frontend.md`: the web interface and the workflow editor
- `10-plugins.md`: extension points, WebAssembly and external plugins
- `11-api.md`: REST conventions, the event stream, outgoing webhooks, the CLI, MCP
- `12-operations.md`: deployment, configuration, scaling, observability, upgrades
- `13-decisions.md`: decisions, alternatives, risks, open questions
