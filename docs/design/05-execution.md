# Execution

This document specifies the agent: how instances are laid out on a host, how an agent registers
and stays connected, the protocol between agent and server, what a job assignment carries, how the
execution engine runs a job, the two executor capabilities, cache volumes, where actions and tools
come from, local execution with the CLI, and the debug terminal. The services a running job calls
back into are in [06-runtime-services.md](06-runtime-services.md).

## Agent instances

One agent process is one slot and runs one job at a time. A host that should run several jobs
runs several instances, `bosun-agent@1.service`, `bosun-agent@2.service` and so on, each under its
own unix user `bosun-agent-<n>` with its own home, its own rootless podman and its own directory
`/var/lib/bosun/agents/<n>`. Separate users are what isolates concurrent jobs on one host from each
other's processes, files and containers.

An agent belongs to exactly one pool ([08-access.md](08-access.md)). Its labels are:

- automatic: `self-hosted`, the operating system (`Linux`), the architecture (`X64`, `ARM64`), and
  one label per capability it has (`shell`, `podman`);
- configured: any further labels in its configuration file.

The pool's name is not a label; `runs-on.group` selects it.

A pool has a name, a trust level (`trusted` or `untrusted`, [08-access.md](08-access.md)), an access
list of organisations and projects that may use it, whether jobs get the podman socket, whether
privileged container options are allowed, optionally the address ranges its agents may connect
from, and optionally an executor plugin instance that provides its agents
([10-plugins.md](10-plugins.md)). An installation starts with one trusted pool, `default`, open to
every project.

An agent started with `--ephemeral` takes exactly one job and then unregisters and exits. This is
the building block for executors that provide a fresh machine per job ([10-plugins.md](10-plugins.md)).

## Registration and identity

1. An administrator creates a registration token `brt_…` for a pool, with an expiry and either
   single or repeated use, in the web interface, the CLI or the API
   (`POST /api/v1/registration-tokens` with the pool's name, `reusable` and an optional
   `expires_at`, by default 24 hours ahead and at most one year).
2. `bosun-agent register --url <server> --token brt_… --name <name> [--label <label>]…` calls
   `POST /api/v1/agents/register`. The server records the agent and returns its id and its
   credential `bat_…`, shown once. A token that is malformed, unknown, expired or, for a single-use
   token, already used is refused with `401 unauthenticated`, and the message says which. An agent
   name is 1 to 64 characters without spaces or control characters; a label is the same without
   commas either. Labels are stored lowercased.
3. The agent writes both into `/etc/bosun/agent-<n>.toml`, readable only by its user.

The server can ask an agent to rotate its credential in the `Welcome` message; the old credential
stays valid until the agent has confirmed the new one. Deleting an agent revokes its credential;
a job it was running is completed as `failure` with the reason `agent_removed`.

## Protocol

The agent opens one gRPC stream to the server and keeps it open:

```
service Agent {
  rpc Connect(stream AgentMessage) returns (stream ServerMessage);
}
```

The stream runs over HTTP/2 with TLS; the credential travels as a bearer token in the request
metadata. Keepalive pings go every 30 seconds in both directions. On any failure the agent
reconnects with jittered exponential backoff from 1 to 60 seconds and resolves the server's name
anew on every attempt, so a server that moved to another address is found without restarting the
agent.

| Agent to server | Carries |
|-----------------|---------|
| `Hello` | agent version, protocol version, capabilities, labels, OS, architecture, host name, and the executions it is still running with the last log sequence number of each |
| `Idle` | the slot is free |
| `Accept`, `Reject` | answer to an assignment, with a reason for a rejection |
| `StepStarted`, `StepFinished` | step identity, times, outcome, conclusion, outputs, annotations, the reference of the uploaded step summary |
| `Log` | execution, step, sequence number, a chunk of masked log lines |
| `Paused`, `Held` | the job stopped at a boundary or after a failure |
| `JobFinished` | conclusion and job outputs |
| `ControlAck` | the id of a control message and whether it took effect |
| `TerminalOutput` | session id and bytes |
| `Heartbeat` | load, free disk, image cache size |

| Server to agent | Carries |
|-----------------|---------|
| `Welcome` | agent id, server version, settings (log chunk size, heartbeat interval), an optional request to rotate the credential |
| `Assignment` | a job assignment, below |
| `Control` | an id, the execution, and one of `cancel`, `force_cancel`, `pause`, `resume`, `skip_step`, `breakpoint`, `abort_step`, `retry_step`, `continue`, `release`, `extend` |
| `TerminalOpen`, `TerminalInput`, `TerminalResize`, `TerminalClose` | a debug terminal session |
| `LogAck` | the highest log sequence number stored |
| `Drain` | finish the current job and take no more |

The protobuf definitions live in `proto/bosun/agent/v1/agent.proto` and are compiled by
`bosun-proto` with a protobuf compiler written in Rust, so building needs no `protoc`. The first
message on a stream is the `Hello`, which names the protocol as `bosun.agent.v1` and the agent's own
version; the server answers with `Welcome` before anything else. The server refuses a `Hello` from
an agent whose protocol version it does not speak, and from an agent of a minor version other than
its own and the previous one, by ending the stream with `FAILED_PRECONDITION` and a message that
names both versions. The server composes the agent's labels from the `Hello`: `self-hosted`, the
operating system, the architecture and one label per capability, followed by the configured labels,
all lowercased; they replace the labels given at registration.

The heartbeat interval is the setting `agents.heartbeat_interval` (default 30 seconds), which the
`Welcome` carries; a `Heartbeat` refreshes the agent's `last_seen_at`. A stream on which the agent
sends nothing for `agents.offline_after` (default 60 seconds) is ended by the server with
`UNAVAILABLE`, so a hung agent reconnects.

**Logs.** Every log chunk carries a sequence number per execution. The server stores chunks
idempotently and acknowledges them; the agent keeps unacknowledged chunks on disk, up to 256 MiB by
default, and sends them again after a reconnect. A reconnect therefore loses no log lines and
duplicates none.

**A lost connection.** A job keeps running when the stream drops. The server marks the agent
offline after `agents.offline_after` (default 60 seconds) without a heartbeat; a periodic sweep
does it, every heartbeat interval, on one replica at a time
([07-storage.md](07-storage.md)). When the agent reconnects within the grace period
(`agent_grace`, default 10 minutes), its `Hello` lists the executions it is running and the server
reconciles them: the executions it holds for the agent that the `Hello` does not list are completed
as `failure` with the reason `agent_lost`, since the agent came back without them. When it does
not reconnect in time, the sweep completes its executions the same way, or re-queues them when the
job's `retry.when` includes `agent-lost`. Completing an execution this way also completes its job
as `failure` with the reason `agent_lost`, each with its journal event.

**Several server replicas.** An agent's stream ends on one replica, which records itself as the
agent's current replica (`agent.replica_id`) and clears that when the stream ends. Control messages
for that agent are rows of `control_message`, published with Postgres `NOTIFY` on the channel
`bosun_replica_<id>` of the agent's current replica, in the transaction that writes the row, and
that replica forwards them over the stream. A replica also sends every unacknowledged control
message of an agent when the agent connects and after its listening connection was re-established,
so a message published while the agent had no stream, or while a notification was lost, still
arrives; the agent answers each with `ControlAck`, which the server records on the row, and ignores
an id it has seen. No replica keeps state another replica needs.

## The job assignment

An assignment carries everything an agent needs and nothing more:

- identity: run, attempt, job, execution, job key, matrix values;
- the job part of the IR: steps as templates, outputs, `retry`, `pause` and hold settings;
- values the server already evaluated ([03-expressions.md](03-expressions.md)): `runs-on`,
  `container` and `services` images and credentials, `env` (workflow and job merged), `defaults`,
  `timeout-minutes`;
- contexts: `github`, `needs`, `strategy`, `matrix`, `inputs`, `vars`, `secrets`, `bosun`;
- the runtime: the job token, the addresses of the runtime services and the forge token
  ([06-runtime-services.md](06-runtime-services.md));
- every `uses` resolved to a commit and to a download address on the server;
- breakpoints set before the job started, and the cache volumes it mounts.

`secrets` holds only the secrets the job's expressions reference, plus `GITHUB_TOKEN`. When a job
reads secrets by a computed name, it receives every secret in its scope, and the validator has
already warned about that.

## The execution engine

`bosun-runner` executes one job in four phases.

1. **Prepare.** Create the directories, fetch the actions and tools the job uses from the server,
   and, for jobs that use containers, pull images, create the job's network, start the service
   containers and wait for their health checks, and start the job container.
2. **Pre steps.** For each Node action with `pre`, in the order the actions appear, run `pre`
   when `pre-if` holds (default `always()`).
3. **Main steps.** In order: evaluate `if`, assemble `env`, render the templates, honour
   breakpoints and pause requests at the boundaries, run with `timeout-minutes` and `retry`,
   process the file commands, record outputs and annotations.
4. **Post steps** in the reverse order of their actions, when `post-if` holds (default
   `always()`). Then evaluate the job outputs, report `JobFinished`, and tear down.

### Kinds of steps

- **`run`.** The script is written to a file in `RUNNER_TEMP` and run through the shell template.
  Without `shell` the template is `bash -e {0}` when `bash` is on the `PATH` where the step runs,
  and `sh -e {0}` otherwise. An explicit `shell: bash` is `bash --noprofile --norc -eo pipefail {0}`,
  which differs from the default as it does in the target; `sh` is `sh -e {0}`, `python` is
  `python {0}`, `pwsh` is `pwsh -command ". '{0}'"`, and any other value is a template that must
  contain `{0}`. `working-directory` is relative to the workspace.
- **Node action.** Run with the Node runtime its `using` names, from the agent's tool directory, or
  mounted into the job container at `/__e/node20` or `/__e/node24`; for images based on musl the
  musl build is mounted. Inputs arrive as `INPUT_<NAME>`, with the name uppercased and spaces
  replaced by `_`, defaults applied, and a warning for a missing required input.
- **Composite action.** Its steps run nested under the step that used it, with the `inputs`
  context bound to the action's inputs and with `GITHUB_ACTION_PATH` set; its outputs map from its
  steps.
- **Container action.** Built from its Dockerfile, tagged by the action's commit so it is built
  once per agent, or pulled for `docker://`. It runs on the job's network with the workspace at
  `/github/workspace`, home at `/github/home`, the event payload under `/github/workflow`, the file
  commands under `/github/file_commands`, `RUNNER_TEMP` at `/github/runner_temp`, the `INPUT_*` and
  `GITHUB_*` environment, and its `entrypoint` and `args`.
- **`uses: ./path`** loads the action from the workspace; **`uses: docker://image`** runs an image
  as a step.

### File commands and workflow commands

Before each step the engine creates fresh files for `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`,
`GITHUB_STATE` and `GITHUB_STEP_SUMMARY`, and reads them after the step, including the multi-line
`name<<DELIMITER` form. The step summary is limited to 1 MiB and uploaded to the server.

Lines on standard output are scanned for workflow commands: `::debug::`, `::notice`, `::warning`
and `::error` with `file`, `line`, `endLine`, `col`, `endColumn` and `title`, `::group::` and
`::endgroup::`, `::add-mask::`, `::stop-commands::<token>` with its matching end, `::echo::on` and
`::echo::off`, and `::add-matcher::` and `::remove-matcher::`, whose problem matchers turn matching
output lines into annotations, as the setup actions of language toolchains expect. The deprecated
`::set-output` and `::save-state` are honoured with a warning; `::set-env` and `::add-path` are
refused unless the step's environment sets `ACTIONS_ALLOW_UNSECURE_COMMANDS=true`. Annotations are
reported with `StepFinished` within the target's limits: 10 of each kind per step and 50 per job.

### Masking

The agent masks before a log line or terminal output leaves the process. The values it masks are
every secret in the assignment, the forge token, the job token and every value given to
`::add-mask::`, whatever their length. For each value it also masks the encoded forms the target
masks: JSON-escaped, URL-encoded, XML-escaped, escaped for an expression string, escaped for a
command line and for PowerShell, with surrounding quotes trimmed, and base64 at all three byte
alignments. A multi-line value is masked as a whole and line by line. A job output that contains a
masked value is dropped with a warning, as in the target behaviour.

### Workspace

The workspace of a project persists between jobs on the same agent instance, so a checkout is
incremental; the project setting `workspace: clean` empties it before each job. `RUNNER_TEMP` is
emptied after every job. On agents of an untrusted pool nothing persists: the workspace, the tool
cache and the downloaded actions are removed after every job, so one job cannot leave hooks,
toolchains or patched actions for the next.

## Capabilities

An agent has one or both capabilities. **`shell`** means steps may run directly on the host as the
agent's user. **`podman`** means the agent may run containers through rootless podman. A job's
steps run in a job container when the job declares `container`, when its label alias names an
image, or when the agent lacks `shell`; otherwise they run on the host. Service containers and
container actions need `podman` in every case.

### `shell`

```
/var/lib/bosun/agents/<n>/
  work/<org>.<project>/                RUNNER_WORKSPACE
  work/<org>.<project>/<repository>    GITHUB_WORKSPACE
  work/_temp                           RUNNER_TEMP
  work/_actions/<owner>/<repo>/<commit>
  tools/                               RUNNER_TOOL_CACHE
  externals/node20, node24             Node runtimes for actions
```

The organisation and project slugs in the path keep two repositories of the same name apart on an
agent that serves several organisations.

The systemd unit runs the agent with `Delegate=yes`, and the agent puts every job in a cgroup of
its own, so teardown kills every process the job started, including ones that detached. Each step
runs in its own process group. Cancelling a step sends `SIGINT` to the group, then `SIGTERM` after
7.5 seconds, then `SIGKILL` after 2.5 more. The agent passes `HTTP_PROXY`, `HTTPS_PROXY` and
`NO_PROXY` from its configuration into every step.

The `shell` capability offers no isolation beyond the unix user, so it is only allowed in pools of
trust level `trusted` ([08-access.md](08-access.md)).

### `podman`

For each execution the agent creates a network `bosun-<execution>`, starts each service with the
service's key as its network alias, publishes ports only when `ports` asks, and waits for the
health checks given in `options`. The job container runs `tail -f /dev/null` as its entrypoint and
gets these mounts, so that actions written for the target environment find their paths:

| Host | Container |
|------|-----------|
| `work/` | `/__w` |
| `externals/` (read-only) | `/__e` |
| `tools/` | `/__t` |
| the job's home | `/github/home` |
| the event payload directory | `/github/workflow` |

The workspace inside the container is `/__w/<org>.<project>/<repository>`. Steps run through
`podman exec`. Root in a rootless container is the agent's user on the host, so files the job
creates belong to that user and teardown can remove them.

`options` of `container` and `services` are passed to `podman create`, except flags that weaken
isolation (`--privileged`, `--cap-add`, `--device`, `--pid=host`, `--network=host`, and volume mounts
outside the workspace), which are refused unless the pool sets `allow_privileged`.

The agent's podman socket is given to steps as `DOCKER_HOST`, and mounted into the job container as
`/var/run/docker.sock` when the pool allows it, so `docker build` inside a job works. Because every
agent instance has its own user, the socket sees only that instance's containers.

Images are pulled by podman itself; registry mirrors are configured in the host's
`registries.conf`, which is how agents behind a closed network pull through the team's registry
proxy. Teardown removes the job's containers, network and anonymous volumes; `bosun-agent gc`,
run by a systemd timer, prunes images and build cache beyond the configured size.

## Cache volumes

A cache volume ([02-workflows.md](02-workflows.md)) is a directory
`/var/lib/bosun/cache/<organisation-id>/<project-id or _organisation>/<name>/<key>` owned by the
group `bosun-cache`, which every agent user of the host belongs to, with the setgid bit and mode
`2775`. `exclusive` mode takes an advisory lock on a lock file next to the directory for the
duration of the job; a job waiting for the lock logs that it is waiting. `bosun-agent gc` evicts
the least recently used volumes when the host's cache exceeds `cache_max_bytes`.

The scope in the path keeps projects from mounting each other's volumes by name, but every agent
user of the host can write every volume. Cache volumes therefore exist only in trusted pools, and
the pool is the boundary: projects that must not be able to poison each other's caches belong in
different pools.

## Actions and tools

Agents do not download actions from the internet. At assignment the server resolves every
`owner/repo@ref` to a commit through the forge, using the installation's forge credentials so that
private actions of the organisation work, stores the tarball in the object store keyed by the
commit, and gives the agent an address on the server. A tag that moves between assignment and
execution has no effect on a running job.

The organisation's action policy decides which actions may be used: all, only those owned by the
organisation, or those matching listed patterns. A job that uses a forbidden action fails before it
reaches an agent, naming the action.

Node runtimes reach agents the same way: the agent fetches
`/_runtime/tools/node/<version>/<os>-<arch>[-musl].tar.xz` from the server, which caches it from
the configured upstream. Tools that actions download for themselves, such as language toolchains,
use the job's network and proxy settings, not Bosun.

## Local execution

`bosun exec [<workflow>] [--job <key>] [--event <name>] [--input <k=v>]… [--secrets <file>]`
runs a workflow on the developer's machine with the same `bosun-runner`. The CLI plays the
server: it fills the contexts from the local git repository and the options, and serves the
runtime services of `bosun-services` on a local port over a temporary directory, with an index kept
in memory. No run is recorded anywhere.

## Debug terminal

On `TerminalOpen` the agent starts a pseudo-terminal: `podman exec -it` into the job container
with the job's environment, or, for `shell` and only when the agent's configuration says
`allow_terminal = true`, a login shell in the workspace as the agent's user with the job's
environment. Input, output and resizes travel as terminal messages on the stream. A session closes
after 30 minutes without input, when the job is released, or on `TerminalClose`.
