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
2. `bosun-agent register --config <file> --url <server> --token brt_… --name <name> --dir <dir>
   [--label <label>]…` calls `POST /api/v1/agents/register`. The server records the agent and
   returns its id and its credential `bat_…`, shown once. A token that is malformed, unknown,
   expired or, for a single-use token, already used is refused with `401 unauthenticated`, and the
   message says which. An agent name is 1 to 64 characters without spaces or control characters; a
   label is the same without commas either. Labels are stored lowercased.
3. The agent writes both into its configuration file, `/etc/bosun/agent-<n>.toml` on a host the
   install script set up, created with mode `0600` so only its user reads it, together with the
   server's address, the directory and the labels
   ([12-operations.md](12-operations.md#configuration)). `register` refuses to overwrite an existing
   file, so a second registration cannot orphan the first; a refusal of the server is printed with
   the server's message. `bosun-agent run --config <file>` then connects and runs jobs until
   `SIGTERM` or `SIGINT`; on either it cancels a running job as a `cancel` would, reports it while
   it has a stream, and exits.

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
metadata; an `https` address, for the registration as for the stream, is verified against the
public root certificates built into the agent, so a host needs no certificate bundle of its own.
Keepalive pings go every 30 seconds in both directions. On any failure the agent reconnects with
jittered exponential backoff from 1 to 60 seconds: after n failures in a row it waits a random time
between half of and all of the smaller of 2ⁿ⁻¹ and 60 seconds, and never less than one second, so
1 second after the first failure, 1 to 2 seconds after the second and 2 to 4 after the third; a
`Welcome` resets the count. Every attempt opens a new channel and so resolves the server's name
anew, so a server that moved to another address is found without restarting the agent.

| Agent to server | Carries |
|-----------------|---------|
| `Hello` | agent version, protocol version, the revision of the assignment it reads, capabilities, labels, OS, architecture, host name, and the executions it is still running with the last log sequence number of each |
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
| `Welcome` | agent id and name, server version, settings (the heartbeat interval), an optional request to rotate the credential |
| `Assignment` | a job assignment, below |
| `Control` | an id, the execution, and one of `cancel`, `force_cancel`, `pause`, `resume`, `skip_step`, `breakpoint`, `abort_step`, `retry_step`, `continue`, `release`, `extend` |
| `TerminalOpen`, `TerminalInput`, `TerminalResize`, `TerminalClose` | a debug terminal session |
| `LogAck` | the sequence number of a log chunk the server handled, which releases every chunk up to it |
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

**Reports.** The server applies what an agent reports about an execution it holds, each report in
a transaction of its own under the run's lock ([07-storage.md](07-storage.md)), and plans the run
after it ([04-runs.md](04-runs.md)). `Accept` starts the execution and its job and creates the
job's steps; `StepStarted` and `StepFinished` move a step, with the times the agent gives, and
`StepFinished` stores on the step the outputs it reports;
`JobFinished` completes the execution with its conclusion, the steps it left open and the job, and
stores on the job the outputs it reports for the names the job declares; the server logs and drops
any other name, so a dependent's `needs` hold only declared outputs. A
report about an execution of another agent, or one that does not fit the execution's state, such as
a `StepStarted` for a step that already started or an `Accept` for an assignment a cancel already
completed, is ignored and logged at warning level; a report whose execution id or conclusion is
malformed ends the stream like any other breach of the protocol. `Idle`, `Reject` and the lease of
an assignment belong to the queue ([07-storage.md](07-storage.md#the-queue)): an `Idle` makes the
replica that holds the stream claim a job for the agent, and again whenever a job is queued, until
it sends the agent an `Assignment`; a `Reject` of an execution still `assigned` returns its job to
the queue with the reason `rejected_by_agent`, and the agent is not offered that job again on the
same stream; an `Accept` after the lease returned the job is ignored like any report that does not
fit. A replica sends an assignment only to an agent that said it is idle and has no execution that
is not `completed`.

The heartbeat interval is the setting `agents.heartbeat_interval` (default 30 seconds), which the
`Welcome` carries; a `Heartbeat` refreshes the agent's `last_seen_at`. A stream on which the agent
sends nothing for `agents.offline_after` (default 60 seconds) is ended by the server with
`UNAVAILABLE`, so a hung agent reconnects.

**Logs.** Every log chunk carries a sequence number per execution, counted from 1. A chunk holds
whole lines of one step, each prefixed with its RFC 3339 time in nanoseconds and a space
([07-storage.md](07-storage.md#logs)); the agent closes a chunk 200 milliseconds after its first
line, at 64 KiB, and at every step boundary, so a live log lags by at most that much. The server
stores chunks idempotently and acknowledges each one it handled with its own sequence number, which
releases it and every chunk below it ([07-storage.md](07-storage.md#logs)); the agent keeps
unacknowledged chunks on disk, up to 256 MiB by default (`spool_max_bytes`), as one file
per chunk under `spool/<execution>/` in its directory, written in full and synced before it is
renamed into place, and sends them again after a reconnect and after a restart of the agent, in
order and before anything else that follows the `Welcome`. A reconnect or a restart therefore
loses no log lines and duplicates none. While the spool is full the agent drops output instead of
stopping the job, and the next chunk it stores begins with a line that says how many bytes were
dropped.

**Reports.** `StepStarted`, `StepFinished` and `JobFinished` are sent in the order they happen,
after the log chunks that precede them. While the agent has no stream it keeps them in memory and
sends them after the next `Welcome`, after the spooled chunks; its `Hello` lists the execution it
runs and every execution whose `JobFinished` it still holds, so the server does not complete them
as lost. Only steps that started are reported: a failing or cancelled step ends the job, the steps
after it do not start, and the server completes them when the job ends
([04-runs.md](04-runs.md#states)).

**A lost connection.** A job keeps running when the stream drops. The server marks the agent
offline after `agents.offline_after` (default 60 seconds) without a heartbeat; a periodic sweep
does it, every heartbeat interval, on one replica at a time
([07-storage.md](07-storage.md)). When the agent reconnects within the grace period
(`agent_grace`, default 10 minutes), its `Hello` lists the executions it is running and the server
reconciles them: the executions it holds for the agent that the `Hello` does not list are completed
as `failure` with the reason `agent_lost`, since the agent came back without them. When it does
not reconnect in time, the sweep completes its executions the same way, or re-queues them when the
job's `retry.when` includes `agent-lost`. Completing an execution this way also completes its job
as `failure` with the reason `agent_lost`, each with its journal event, and the planner takes the
run on in the same transaction. The sweep finds such executions under its advisory lock and fails
each one in a transaction of its own under its run's lock, after checking again that its agent is
still offline; a reconnecting agent's executions are failed the same way, after its `Hello`.

**Several server replicas.** An agent's stream ends on one replica, which records itself as the
agent's current replica (`agent.replica_id`) and clears that when the stream ends. Control messages
for that agent are rows of `control_message`, published with Postgres `NOTIFY` on the channel
`bosun_replica_<id>` of the agent's current replica, in the transaction that writes the row, and
that replica forwards them over the stream. A replica also sends every unacknowledged control
message of an agent when the agent connects and after its listening connection was re-established,
so a message published while the agent had no stream, or while a notification was lost, still
arrives; the agent answers each with `ControlAck`, which the server records on the row, and does not
act again on an id it has seen: it repeats the acknowledgement it gave, for the last 1024 ids. A
`cancel` of the execution the agent runs takes effect; any other id is acknowledged as having no
effect. No replica keeps state another replica needs.

## The job assignment

An assignment carries everything an agent needs and nothing more:

- identity: run, attempt, job, execution, job key, matrix values, and the slugs of the job's
  organisation and project, which name its workspace;
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

**Encoding.** The identity travels in fields of `Assignment`. Everything else travels in three JSON
documents beside it, which the agent reads with every field it does not know refused:

- `job`, the job's template from the IR exactly as the run's snapshot stores it
  ([02-workflows.md](02-workflows.md)), its steps as templates;
- `contexts`, an object with the server's contexts `github`, `needs`, `strategy`, `matrix`,
  `inputs`, `vars`, `secrets` and `bosun`, built for the job as
  [03-expressions.md](03-expressions.md#the-contexts-the-server-supplies) describes. `github.workspace`
  is empty there, and the agent fills it in with the job's workspace; `strategy` and `matrix` are the
  job's as the server evaluated them ([04-runs.md](04-runs.md#matrices)): the strategy of a single
  job and `null` for a job without a matrix;
- `evaluated`, the values the server evaluated: `env`, `shell` and `working_directory` of
  `defaults.run`, `timeout_minutes` and `continue_on_error`. `env` is the workflow's `env`,
  evaluated when the job is assigned with the contexts that key sees
  ([03-expressions.md](03-expressions.md#where-each-expression-is-evaluated)), with the job's `env`,
  which the planner evaluated when the job became ready, merged over it, so a name in both takes the
  job's value.

JSON rather than protobuf messages carries these for two reasons. The IR is already serialised as
JSON for the run's snapshot, so the server hands over the template it stores, and the agent reads it
with the same types from `bosun-types`. And a JSON reader can refuse what it does not know, where
protobuf drops an unknown field without a word, which would let an agent run a job without a part it
never saw. The documents replaced the structured steps and the text `env` of M0, whose field
numbers are reserved.

An expression of the workflow's `env` that fails to evaluate fails the job at its assignment, as an
error of the planner does ([04-runs.md](04-runs.md#planning)): after an error notice at the
expression's span, the execution and the job complete as `failure` with the reason
`evaluation_failed` in the same transaction, the agent is sent nothing, and the replica claims the
next job for it at once.

**Revisions.** The assignment has a revision, `1` from M1 on, and the agent announces the revision
it reads in its `Hello` (`assignment_revision`). The revision rises when the assignment changes so
that an agent of the previous revision would misread it rather than refuse it. The assignment of M1
is such a change: an agent of M0 reads neither the documents nor any field it does not know, so it
would see a job without steps and report it successful. An agent of M0 does not know the field
either, and so announces `0`. A replica admits an agent of another revision as the version rule
says, so it can still report the job it was running when the server was upgraded under it, logs a
warning that names both revisions, and never claims a job for it: the agent stays connected and
idle until it is upgraded, and no job is offered to it, in a loop or otherwise.

**Fields an agent does not know.** Within a revision a newer server may hand over something an
agent does not know, such as a key of the IR that a later step lowers. The agent answers `Reject`
with a reason that names the document and the field, as in ``the document `job` of the assignment
is not one this agent reads: unknown field `container`, …``. Like every rejection this returns the
job to the queue, and the replica does not offer that job to that agent again on the stream, so it
goes to an agent that reads it, or is cancelled with `no_agent` after `max_queue_time`.

A step's `shell` travels as the IR holds it: `bash`, `sh`, `pwsh`, `python`, or a custom template,
which must contain `{0}`. The agent answers `Accept` before it starts a job, or `Reject` with the
reason when it is running another job, when it is stopping, or when the assignment holds something
it cannot run: a field it does not know, a step that uses an action (until step 33 of
[../plan.md](../plan.md)), a custom shell without `{0}`, or slugs that are not slugs; and, until the
agent evaluates the expressions of steps and applies their keys (step 21), a step whose `run`,
`env` or `working-directory` holds an expression, or that sets `if`, `continue-on-error` or
`timeout-minutes`. After a `Reject` with its slot free, and whenever its slot becomes free, it sends
`Idle`.

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
   `always()`). Then evaluate the job outputs, tear down, and report `JobFinished`, so that a job
   reported as finished has no process left on the agent and the slot is free for the next one.

### Kinds of steps

- **`run`.** The script is written to a file in `RUNNER_TEMP` and run through the shell template.
  Without `shell` the template is `bash -e {0}` when `bash` is on the `PATH` where the step runs,
  and `sh -e {0}` otherwise. An explicit `shell: bash` is `bash --noprofile --norc -eo pipefail {0}`,
  which differs from the default as it does in the target; `sh` is `sh -e {0}`, `python` is
  `python {0}`, `pwsh` is `pwsh -command ". '{0}'"`, and any other value is a template that must
  contain `{0}`. A template is split into words at white space, a double-quoted part stays one word
  without its quotes and `\"` is a quote, as the target splits it; the first word is the program
  and `{0}` becomes the script's path in every word. The script is `RUNNER_TEMP/step-<index>` with
  the extension the target gives its program, the first word as written in any case: `.sh` for
  `bash` and `sh`, `.py` for `python`, `.ps1` for `pwsh` and `powershell`, `.cmd` for `cmd`, and
  none for any other program, `python3` and `/bin/bash` included. `working-directory` is relative
  to the workspace and must exist, or the step fails. Standard output and standard error share one pipe, so their lines
  keep the order they were written in; lines longer than 64 KiB are split. A step ends when its
  shell exits; a process it left in the background keeps running into the next steps, as in the
  target, its output still counts as the output of its step, and teardown ends it. The step's
  environment is the agent's own, then the variables of a step of
  [06-runtime-services.md](06-runtime-services.md#the-github-context-and-the-environment) (`CI`,
  `GITHUB_ACTIONS`, the `GITHUB_*` of the step's context, `GITHUB_WORKSPACE` among them, `RUNNER_*`
  from the facts of the runner, and `BOSUN_*`), then the proxy settings of the agent, the job's
  `env` and the step's `env`, each overriding the ones before it. The facts of the runner are the
  agent's name, which the `Welcome` carries, its operating system and architecture as its labels
  name them, and `RUNNER_TEMP` (`work/_temp`), `RUNNER_TOOL_CACHE` (`tools/`) and
  `RUNNER_WORKSPACE`, which the agent creates before the first step. Before the first step it also
  writes the payload, `github.event`, to `RUNNER_TEMP/_github_workflow/event.json`, the file
  `GITHUB_EVENT_PATH` names, and teardown removes it with the rest of `RUNNER_TEMP`. The cases in
  `contracts/fixtures/steps/` pin these rules.
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
  spool/<execution>/<seq>-<step>       unacknowledged log chunks
```

The organisation and project slugs in the path keep two repositories of the same name apart on an
agent that serves several organisations. The repository's name is the part of `github.repository`
after its owner, and the agent rejects an assignment whose `github.repository` is not an owner and a
name, or whose name is `.` or `..`. A repository-less project has no repository name, so its
project slug stands in for it: `work/<org>.<project>/<project>`.

The systemd unit runs the agent with `Delegate=yes`, and the agent puts every job in a cgroup of
its own, so teardown kills every process the job started, including ones that detached. The cgroup
is `bosun-job-<execution>` under the agent's own cgroup of the unified hierarchy
(`/proc/self/cgroup`, mounted at `/sys/fs/cgroup`); every step's process enters it before it
executes the shell, so no child escapes it, and teardown writes `cgroup.kill`, or kills the members
one by one on kernels older than 5.14, and removes the cgroup. At start the agent probes whether it
may create cgroups and move processes into them, and logs what it found. Where it may not, because
the agent's cgroup is not delegated (an agent started by hand or under a unit without
`Delegate=yes`) or because the host is not Linux, it tracks jobs by process group alone: teardown
sends `SIGKILL` to the group of every step, which ends background processes, `nohup` included, but
not a process that left its group with `setsid`. When the agent cannot create a job's directories
or its cgroup, the job fails in its first step, whose log says why.

Each step runs in its own process group. Cancelling a step sends `SIGINT` to the group, then
`SIGTERM` after 7.5 seconds, then `SIGKILL` after 2.5 more, so even a step that ignores both
signals ends within 10 seconds; the step and the job conclude `cancelled`. The agent passes
`HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` from its configuration into every step, and the same
values as `http_proxy`, `https_proxy` and `no_proxy`, since many tools read only those.

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
