# Plugins

This document specifies how Bosun is extended without changing its code: the extension points,
plugins that run inside the server as WebAssembly components, plugins that run as external
processes, the manifest, distribution, instances and configuration, routing of events, and the
first plugins Bosun ships.

## What plugins are for

Steps already have an extension mechanism with an ecosystem behind it: actions. Plugins therefore
do not add step types. They extend the parts of the server that a workflow cannot reach: where runs
come from, who hears about them, what may run, where secrets live, and where jobs execute.

| Extension point | Runs as | Called | Answers |
|-----------------|---------|--------|---------|
| webhook trigger | WebAssembly | on a request to `/hooks/plugins/<instance>` | zero or more trigger events |
| poll trigger | WebAssembly | on the instance's schedule | zero or more trigger events |
| stream trigger | external | continuously, for example consuming a Kafka topic | trigger events |
| notifier | WebAssembly | for routed run, job and approval events | delivered or failed |
| admission policy | WebAssembly | when a job reaches admission ([04-runs.md](04-runs.md)) | admit, deny with a reason, wait with a reason and a recheck time, or require approval from named approvers |
| expression functions | WebAssembly | for `plugin(name, ...)` in a server-evaluated expression | a value |
| event sink | WebAssembly | for every journal event matching its filter | nothing |
| secret backend | external | when an assignment is built and a secret is a reference | the value |
| executor | external | when a job is queued for a pool backed by the executor | a fresh agent for the job |

## WebAssembly plugins

A WebAssembly plugin is a component run by wasmtime inside `bosun-server`, in the crate
`bosun-plugin-host`. The WIT package `bosun:plugin@1.0.0` defines one world per extension point;
one component may export several of them.

```wit
package bosun:plugin@1.0.0;

interface types {
  record run-summary {
    project: string,
    workflow: string,
    run-id: string,
    attempt: u32,
    status: string,
    conclusion: option<string>,
    url: string,
  }

  record admission-request {
    run: run-summary,
    job-key: string,
    environment: option<string>,
    ref: string,
    changed-paths: list<string>,
    actor: string,
  }

  variant decision {
    admit,
    deny(string),
    wait(tuple<string, u32>),
    require-approval(list<string>),
  }
}

world policy {
  import host;
  use types.{admission-request, decision};
  export admit: func(request: admission-request) -> result<decision, string>;
}

world notifier {
  import host;
  use types.{run-summary};
  export notify: func(kind: string, run: run-summary, detail: string) -> result<_, string>;
}
```

The `host` interface the server provides:

| Import | Gives |
|--------|-------|
| `log` | structured log lines into the server's log, tagged with the instance |
| `config` | the instance's configuration as JSON |
| `kv` | a key-value store private to the instance, limited in size |
| `http` | outgoing HTTP to the hosts the manifest declares, and nowhere else |
| `secrets` | the secrets bound in the instance's configuration |
| `api` | a narrow client of Bosun: read runs and jobs, dispatch a workflow, give the commands the manifest declares |
| `clock`, `random` | wall clock and randomness |

Every call runs with a deadline enforced by epoch interruption (default 2 seconds for policies and
functions, 10 seconds for the others) and a memory limit (default 64 MiB). A trap or a timeout is an
error in the server's log and the instance's status page. An admission policy that fails decides
by its instance's failure mode: `deny` by default, `admit` when configured. Each plugin is compiled
and pre-instantiated once; every call gets a fresh instance from it, so no call sees another call's
memory, and state that must outlive a call lives in `kv`.

## External plugins

An external plugin is a process in any language that speaks `bosun.plugin.v1` over gRPC. Like an
agent, it opens the connection to the server, authenticates with its `bpt_` credential and
announces which services it implements. It runs wherever is convenient: as a deployment next to
the server or as a unit on a host.

- **Secret backend.** A secret in Bosun can be a reference instead of a value:
  `{ backend: <instance>, path: <backend-specific path> }`. When an assignment is built, the server
  asks the backend for the values, with a cache whose lifetime the backend returns.
- **Stream trigger.** The server sends the plugin its configuration; the plugin streams trigger
  events back.
- **Executor.** A pool can be backed by an executor instance. When a job is queued for that pool,
  the server asks the executor to provision a machine for it and hands it a single-use
  registration token; the machine starts `bosun-agent --ephemeral`, registers in the pool, takes
  that job, and exits; the server then asks the executor to remove the machine. This is how
  Kubernetes pods or virtual machines would become executors; neither is built
  ([01-overview.md](01-overview.md)).

## Manifest

A plugin package carries `bosun-plugin.toml`:

```toml
id = "telegram"
version = "1.2.0"
points = ["notifier"]
bosun = ">=0.5"

[permissions]
http = ["api.telegram.org"]
secrets = true
api = []

[config]
schema = "config.schema.json"
```

The server refuses a plugin whose manifest asks for an import it does not declare, and shows the
declared permissions before installation.

## Distribution and installation

Plugins are OCI artifacts: a manifest whose layers are the component and the plugin manifest, in
any OCI registry; the team publishes its own to Astrum. `bosun plugin install oci://<reference>`
pulls the artifact by digest, verifies its Ed25519 signature against the publisher keys configured
in the installation (an unsigned plugin installs only when an owner passes `--allow-unsigned`),
stores it in the object store and records the version. Upgrades are explicit; every instance is
pinned to a version until it is moved.

## Instances and configuration

A plugin has instances, each attached to an organisation or a project, each with a name and a
configuration validated against the plugin's JSON Schema. The interface renders the configuration
form from the schema; fields marked `"x-bosun-secret": true` are stored as secrets and given to the
instance through the `secrets` import. Workflows refer to instances by name, resolved in the
project first and in the organisation second.

## Routing

- **Notifiers** receive events from the `notify` routes of workflows ([02-workflows.md](02-workflows.md))
  and from subscriptions on the instance itself, such as "every failure on the default branch of
  the organisation". `fixed` means a success after the previous run of the same workflow on the same
  branch failed.
- **Triggers** emit events with a type and a payload. A workflow subscribes with
  `on.plugin: { <instance>: { types: [...] } }`. The run's `github.event_name` is `plugin`,
  `github.event` is the payload, and `bosun.trigger` names the instance.
- **Admission policies** are attached to an organisation, a project or an environment, and apply
  to every job they cover.

## First plugins

M5 ships these as separate packages built in this repository:

- notifiers for Telegram, Matrix and email (SMTP);
- a Kafka stream trigger, as an external plugin;
- an admission policy for change windows ("no deploys to production on Friday after 15:00",
  freeze periods);
- an admission policy that requires approvers when the changed paths match patterns, for example
  two approvals for migrations.

## Stability

The WIT package and the plugin protocol are versioned. Inside a major version changes are
additive; the server hosts plugins of its major version and of the previous one.
