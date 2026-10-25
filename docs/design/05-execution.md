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

## Pools and label aliases

Step 18 of M1 ([../plan.md](../plan.md)) settled the parts of pools that M1 uses, their name, trust
level and access list, and the label aliases; the endpoints are in
[11-api.md](11-api.md#pools-and-label-aliases).

**Access lists.** A pool's access list is its rows of `pool_access`. A row opens the pool to one
project, to every project of one organisation, or, naming neither, to every project of the
installation, which is the row the migrations give the pool `default`. A pool is created with at
least one row. When the planner evaluates `runs-on`, it writes into the job's `pool_ids` only the
pools open to the job's project ([07-storage.md](07-storage.md#tables)), and the claim checks the
access list of the agent's pool again ([07-storage.md](07-storage.md#the-queue)), so an access list
that is narrowed later keeps the queued jobs of a project it no longer admits from that pool's
agents at once.

**Trust.** A run is trusted or not (`run.trusted`); the runs of pull requests from forks are
untrusted, as step 29 of [../plan.md](../plan.md) settled, and every other run is trusted. The
claim matches the trust of a job's run with the trust level of the agent's pool in both directions,
whatever the labels say: a job of a trusted run, which carries secrets, never reaches an untrusted
pool, and a job of an untrusted run never reaches a trusted one
([08-access.md](08-access.md#code-from-outside-the-team)).

**Capabilities.** A job needs the capabilities its label aliases name, stored in the job's
`capabilities`. The claim offers a job only to an agent that holds each of them, and a job that
needs none to an agent that holds either; an agent that holds no capability is offered no job.

**The `shell` of an untrusted pool.** An agent of an untrusted pool never holds `shell`. When its
`Hello` announces the capability, or the label `shell` among its configured labels, the server drops
both, records the agent without them, and logs a warning that names the agent and its pool, so the
agent is offered no job that needs `shell`, whether by an alias or by the label. In M1 the agent has
no `podman` yet ([../plan.md](../plan.md)), so only scripted fake agents claim from an untrusted
pool.

**Label aliases** are rows of `label_alias`: a label, stored lowercased, and either an organisation
or none for the installation, each pair once, with an optional pool, capability and default image.
An image makes the capability `podman`. When the planner evaluates `runs-on`
([04-runs.md](04-runs.md#planning)), it looks each label up among the aliases of the job's
organisation and of the installation, and the organisation's alias wins. An aliased label leaves the
job's labels; the alias's pool, when it names one, restricts the job's pools to that pool, and its
capability joins the job's capabilities. With several aliased labels the job may run only in a pool
that every one of them names and needs every capability they name, so aliases that name two pools,
or a pool that `runs-on.group` does not name or that is not open to the project, leave the job no
pool, and it is cancelled with `no_agent` after `max_queue_time` like a label no agent offers. An
alias that names no pool and no capability only removes its label, so `ubuntu-latest` can mean any
agent. Aliases are read once, when the job becomes ready; changing an alias does not move jobs that
are already queued. An alias that names `podman` or an image is stored for M2: its jobs need
`podman`, which no agent of M1 holds, so they wait for one, and the image is not used before job
containers arrive in M2.

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
a job it was running is completed as `failure` with the reason `agent_removed`
([Draining and deleting agents](#draining-and-deleting-agents)).

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
| `StepStarted`, `StepFinished` | step identity, times, outcome, conclusion, outputs, annotations and the step summary |
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
| `Drain` | the agent is drained: it finishes the current job and is offered no other ([Draining and deleting agents](#draining-and-deleting-agents)) |

The protobuf definitions live in `proto/bosun/agent/v1/agent.proto` and are compiled by
`bosun-proto` with a protobuf compiler written in Rust, so building needs no `protoc`. The first
message on a stream is the `Hello`, which names the protocol as `bosun.agent.v1` and the agent's own
version; the server answers with `Welcome` before anything else. A message the server reads from an
agent is at most 8 MiB, which step 23 of [../plan.md](../plan.md) set: the summary and the
annotations a `StepFinished` carries come to at most 2.5 MiB
([File commands and workflow commands](#file-commands-and-workflow-commands)), so the limit leaves
more than 5 MiB for its outputs and the rest, where the 4 MiB of the gRPC library would not have
held a report with long outputs. The server refuses a `Hello` from
an agent whose protocol version it does not speak, and from an agent of a minor version other than
its own and the previous one, by ending the stream with `FAILED_PRECONDITION` and a message that
names both versions. The server composes the agent's labels from the `Hello`: `self-hosted`, the
operating system, the architecture and one label per capability, followed by the configured labels,
all lowercased; they replace the labels given at registration.

**Reports.** The server applies what an agent reports about an execution it holds, each report in
a transaction of its own under the run's lock ([07-storage.md](07-storage.md)), and plans the run
after it ([04-runs.md](04-runs.md)). `Accept` starts the execution and its job and creates the
job's steps; `StepStarted` and `StepFinished` move a step, with the times the agent gives, and
`StepFinished` stores on the step the outputs it reports, and its annotations and summary
([06-runtime-services.md](06-runtime-services.md#step-summaries-and-annotations));
`JobFinished` completes the execution with its conclusion, the steps it left open and the job, and
stores on the job the outputs it reports for the names the job declares; the server logs and drops
any other name, so a dependent's `needs` hold only declared outputs. A
report about an execution of another agent, or one that does not fit the execution's state, such as
a `StepStarted` for a step that already started or an `Accept` for an assignment a cancel already
completed, is ignored and logged at warning level; a report whose execution id or conclusion is
malformed, or that carries an annotation of no kind, ends the stream like any other breach of the
protocol. `Idle`, `Reject` and the lease of an assignment belong to the queue
([07-storage.md](07-storage.md#the-queue)): an `Idle` makes the replica that holds the stream
claim a job for the agent, and again whenever a job is queued, until it sends the agent an
`Assignment`; a `Reject` of an execution still `assigned` returns its job to
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
as lost. Every step the engine reaches is reported: one whose `if` does not hold with a
`StepFinished` of outcome and conclusion `skipped` and no `StepStarted`, every other one with both.
The steps the engine does not reach, after the job's timeout or after a job that could not be
prepared, are not reported, and the server completes them when the job ends
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

## Draining and deleting agents

Step 19 of M1 ([../plan.md](../plan.md)) settled how an administrator takes an agent out of
service; the endpoints are in [11-api.md](11-api.md#agents) and the commands in
[11-api.md](11-api.md#the-cli).

**Draining.** `PATCH /agents/{agent}` with `"drain": true` stores the drain on the agent
(`agent.drain`), so it outlasts a reconnect and a restart of the agent or of the server, and the
claim takes nothing for a drained agent ([07-storage.md](07-storage.md#the-queue)). The claim is
what keeps jobs away from it: the agent finishes the job it runs, reports it as always, and is
offered no other, whichever replica it connects to. The transaction that stores the drain announces
it on `bosun_replica_<id>` of the agent's current replica, with the agent's id alone as the payload
([07-storage.md](07-storage.md#notifications-between-replicas)), so the replica that holds the
stream learns of it whichever replica took the request, reads the agent and sends `Drain`. It also
sends `Drain` right after the `Welcome` of every stream of a drained agent and, after its listening
connection was re-established, to every drained agent it holds; a stream carries one `Drain` until
the drain is lifted. The agent logs the `Drain` and changes nothing else, since the server already
offers it no job; an agent of the previous minor version does not know the message and drops it,
and is kept from jobs all the same. A drain or its lifting that changes the stored value also writes
an `agent.status` event in the transaction of the change ([11-api.md](11-api.md#the-event-stream)),
so the agents page follows it without a request.

**A drained agent once it is idle** stays connected and idle: it keeps its stream and its
heartbeats, sends `Idle` as before, is listed as online and drained, and is offered no job until the
drain is lifted or the agent is deleted. It does not exit. Under its unit, which restarts it only on
a failure ([12-operations.md](12-operations.md#agents)), an agent that exited would leave the host
without an agent until someone started it by hand, and one that exited with a failure would be
started again and drained again in a loop; staying connected lets an administrator see from the
agents page when it went idle, lift the drain without touching the host, or delete it.

**Lifting a drain.** `"drain": false` lifts it at once, through the same announcement: the replica
that holds the stream claims a job for the agent when the agent is idle, as after an `Idle`, and
sends the agent nothing, since the drain changed nothing on it. A drain lifted while the agent has
no stream applies when it connects. Draining and lifting are therefore how an agent is taken out of
service for a while, for the upgrade of its host among other things, without being registered
again.

**Deleting.** `DELETE /agents/{agent}` marks the agent deleted (`agent.deleted_at`) and offline in
one statement, which writes an `agent.status` event of the agent as offline. From then on its credential is refused when it connects, with `UNAUTHENTICATED` and
``the agent was deleted, which revoked its credential``, it is not listed, the claim takes nothing
for it, and every request that names it is `404`. Its row stays, because its executions keep naming
it through a plain foreign key ([07-storage.md](07-storage.md#tables)). The server then ends each
execution of the agent that is not completed, each in a transaction of its own under its run's
lock: a running one completes as `failure` with the reason `agent_removed`, and its job with it,
after which the planner takes the run on; one still `assigned`, which the agent never started,
returns to the queue with the same reason and keeps its place, as after an expired lease. Each also
gets a `cancel` control message, and its events carry the caller as their actor. Last, the server
announces the agent on its current replica's channel; that replica sends the agent its
unacknowledged control messages, the cancels among them, and then ends the stream with
`UNAUTHENTICATED` and the message above, so a job still running on the host is cancelled there as by
a `cancel`, and nothing it reports afterwards reaches the server. The agent keeps trying to
connect with its backoff and is refused every time, until its instance is removed from the host.
A deletion that its replica abandoned halfway is finished by the others: the sweep of agents ends
the open executions of agents deleted longer than `agents.offline_after` ago the same way, with the
actor `system`, so it does not race the replica that is still at it, and a heartbeat of a
deleted agent that has no open execution left ends its stream as the announcement would, after its
unacknowledged control messages, so the cancels still arrive first.

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
- every `uses` resolved to a commit and to a download address on the server, with the action's
  metadata and the Node that runs it ([Actions and tools](#actions-and-tools));
- breakpoints set before the job started, and the cache volumes it mounts.

`secrets` holds only the secrets the job's expressions reference, plus `GITHUB_TOKEN`. When a job
reads secrets by a computed name, it receives every secret in its scope, and the validator has
already warned about that. The references are those the IR records for the job template
([02-workflows.md](02-workflows.md)), which include those of the workflow's `env`. The server reads
and decrypts them in the transaction that builds the assignment, from the organisation and the
project of the run, a project's secret shadowing its organisation's of the same name, and nowhere
else ([07-storage.md](07-storage.md#secrets-at-rest)). `vars` holds every variable of the
organisation and the project, as the server's own expressions see them
([03-expressions.md](03-expressions.md#the-contexts-the-server-supplies)). `GITHUB_TOKEN` arrives
with the forge token (step 34 of [../plan.md](../plan.md)). The assignment of a job of an untrusted
run reads no secret at all, so its `secrets` is empty whatever the job references, and its `vars`
are those of any run ([08-access.md](08-access.md#code-from-outside-the-team)).

**Encoding.** The identity travels in fields of `Assignment`. Everything else travels in four JSON
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
  `defaults.run`, `timeout_minutes` and `continue_on_error`. A setting of `defaults.run` the job does
  not set is the workflow's, as the target lets the more specific default of the same name win.
  `env` is the workflow's `env`,
  evaluated when the job is assigned with the contexts that key sees
  ([03-expressions.md](03-expressions.md#where-each-expression-is-evaluated)), with the job's `env`,
  which the planner evaluated when the job became ready, merged over it, so a name in both takes the
  job's value. When the job's `env` or `defaults.run` read secrets, the assignment evaluates them,
  rather than the planner ([03-expressions.md](03-expressions.md#the-contexts-the-server-supplies)).
- `actions`, a list with one entry per distinct `uses` of the job's steps, in the order of the steps:
  `uses`, the step's reference as the job template holds it, an object with `owner`, `repository`,
  `path` (`null` for an action at the root of its repository) and `ref`, not the text the step
  writes; `commit`, the commit it resolved to; `download`, the path of its tarball on the server
  (`/_runtime/actions/<owner>/<repo>/<commit>.tar.gz`), which the agent asks of the server address
  it connects to; `metadata`, the action's metadata as `bosun_workflow::action` lowers it; and
  `node`, the version of Node that runs it, such as `24.22.1`
  ([Actions and tools](#actions-and-tools)). A job without a `uses` has an empty list.

JSON rather than protobuf messages carries these for two reasons. The IR is already serialised as
JSON for the run's snapshot, so the server hands over the template it stores, and the agent reads it
with the same types from `bosun-types`. And a JSON reader can refuse what it does not know, where
protobuf drops an unknown field without a word, which would let an agent run a job without a part it
never saw. The documents replaced the structured steps and the text `env` of M0, whose field
numbers are reserved.

An expression of the workflow's `env` that fails to evaluate, an expression that reads a secret and
fails, and a secret that does not decrypt fail the job at its assignment, as an error of the planner
does ([04-runs.md](04-runs.md#planning)): after an error notice at the
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
it cannot run: a field it does not know, a step that uses an action its `actions` do not resolve,
an action whose owner, repository, commit or path cannot name a directory under `work/_actions`,
whose `download` is not a path on the server or whose `node` is not a version of three numbers, two
actions whose versions of Node would share one directory of `externals/`
([Node actions](#node-actions)), a custom shell without `{0}`, or slugs that are not slugs. After a
`Reject` with its slot free, and whenever its slot becomes free, it sends `Idle`.

## The execution engine

`bosun-runner` executes one job in four phases.

1. **Prepare.** Create the directories, fetch the actions and tools the job uses from the server,
   and, for jobs that use containers, pull images, create the job's network, start the service
   containers and wait for their health checks, and start the job container.
2. **Pre steps.** For each step that uses a Node action with `pre`, in the order of the steps, run
   `pre` when `pre-if` holds (default `always()`).
3. **Main steps.** In order: evaluate `if`, assemble `env`, render the templates, honour
   breakpoints and pause requests at the boundaries, run with `timeout-minutes` and `retry`,
   process the file commands, record outputs and annotations.
4. **Post steps** for each step that uses a Node action with `post`, in the reverse order of the
   steps, when `post-if` holds (default `always()`). Then evaluate the job outputs, tear down, and
   report `JobFinished`, so that a job reported as finished has no process left on the agent and
   the slot is free for the next one.

### Main steps

**Conditions.** A job has one status, as in the target: `success` until a step concludes `failure`
or `timed_out`, then `failure`, and `cancelled` from the moment a cancel reaches the agent, whatever
it was before. Before a step starts, the agent evaluates its `if` with `bosun_expr::holds`:
`success()` is true while the status is `success`, `failure()` while it is `failure` and
`cancelled()` while it is `cancelled`, so a step without `if`, or whose `if` calls no status
function, runs only while the status is `success`. A step whose `if` does not hold is skipped. An
`if` that fails to evaluate fails the step, without `continue-on-error`, and its error is the step's
log.

**Templates.** Once the step starts, the agent evaluates `timeout-minutes`, `env`,
`working-directory` and `run`, in this order, and `continue-on-error` when the step failed or timed
out. A key that is one `${{ }}` takes the type of its value, any other text is a string:
`timeout-minutes` must be a whole number of at least 0 and `continue-on-error` a boolean. An
expression that fails, or a value its key cannot use, fails the step, with the error and its line and
column as the step's log; `continue-on-error` applies to such a failure as to any other, as the
target applies it to every failure of a step whose condition held, and one that fails to evaluate
counts as false. Each value of the step's `env` sees the `env` context without the step's own
variables; `working-directory`, `run` and `continue-on-error` see it with them. The agent does not
render `name`, since no report carries the display name of a step and the server names steps when
it creates them; a step that uses an action renders its `with` after its `env` and sees what `run`
sees ([Node actions](#node-actions)), and `shell` takes no expression
([02-workflows.md](02-workflows.md)).

**Defaults.** A step without `shell` runs with the `shell` of `defaults.run` in `evaluated`, or else
with the default shell below. A step without `working-directory`, or whose `working-directory`
renders empty, runs in the `working_directory` of `defaults.run`, or else in the workspace.

**Contexts.** The expressions of a step see the contexts of the assignment and those the agent
keeps:

- `github`, the job's with `workspace` and the properties of the step
  ([06-runtime-services.md](06-runtime-services.md#the-github-context-and-the-environment));
- `env`: what earlier steps wrote to `GITHUB_ENV`, under the job's `env`, under the step's own once
  it is evaluated. A value from the file does not replace one of the job's, in the context as in the
  environment, as in the target, where the job's `env` is laid over the file's values before each
  step;
- `job`: `{ status }`, the status above;
- `steps`: for each step with an `id` that the engine reached, `{ outputs, outcome, conclusion }`,
  where a timed-out step shows `failure`, the target's only value for it. A step without an `id` has
  no entry, as in the target, though its outputs are reported;
- `runner`: `os`, `arch`, `name`, `environment` (`self-hosted`), `tool_cache`, `temp` and
  `workspace`, from the facts of the runner. `debug` is `1` on a debug run (M3) and absent
  otherwise, as the target leaves it out.

`jobs` is `null` in a step.

**Outcome and conclusion.** A step's outcome is `success` when its shell exits with 0 and its file
commands are valid, `failure` otherwise, `cancelled` when a cancel ended it, and `timed_out` when a
timeout ended it. Its conclusion is its outcome, except that `continue-on-error` turns `failure` and
a `timed_out` of the step's own timeout into `success`.

**Timeouts.** A step's `timeout-minutes` counts from the start of the step and the job's (default
360) from the start of the job on the agent, which until pauses and holds arrive (M3) is all of the
job's time in progress. Zero sets no limit, as the target's step timeout does, and for a job too.
When a limit is reached, the agent ends the running step with the signals of a cancel
([`shell`](#shell)) and logs which limit it was. The two limits end differently, and the cases in
`contracts/fixtures/steps/` pin both:

- A step over its own `timeout-minutes` concludes `timed_out`, or `success` under
  `continue-on-error`. The job's status becomes `failure` unless `continue-on-error` holds, and the
  later steps follow their `if`, so `if: failure()` and `if: always()` run after it. The job then
  concludes `failure`, not `timed_out`: the step ran out of time, the job did not.
- A job over its own `timeout-minutes` ends its running step, which concludes `timed_out` whatever
  its `continue-on-error`, starts no further step, not even one whose `if` admits a failure or a
  cancellation, since the job has no time left, and concludes `timed_out`. Its dependents see it as
  `failure` in `needs.<job>.result`, or as `success` when the job may fail under its own
  `continue-on-error` ([03-expressions.md](03-expressions.md#the-contexts-the-server-supplies)),
  and it fails the run ([04-runs.md](04-runs.md#matrices)). A test of the server pins
  `needs.<job>.result`.

**Cancellation.** A cancel ends the running step with the signals of [`shell`](#shell), and the step
concludes `cancelled`. The agent then evaluates the `if` of every remaining step with the status
`cancelled`, so only a step whose `if` calls `always()` or `cancelled()`, or negates a status
function, runs. A step that starts after the cancel runs to its end, within its own timeout and the
job's.

**The job.** After the last step the agent renders each of the job's `outputs` with the contexts of a
step, `github` without the properties of a step, and reports those that are not empty in
`JobFinished`, as the target leaves an empty output out. An output that fails to evaluate fails the
job, none is reported, and the error goes to the log of the last step that started. The job
concludes with the first of these that holds: `timed_out` after its timeout, `cancelled` after a
cancel, `failure` when its status is `failure` or its outputs failed, and `success`.

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
  from the facts of the runner, and `BOSUN_*`), then the proxy settings of the agent, what earlier
  steps wrote to `GITHUB_ENV`, the job's `env` and the step's `env`, each overriding the ones before
  it; the directories earlier steps wrote to `GITHUB_PATH` come first in `PATH`, the latest first.
  The facts of the runner are the
  agent's name, which the `Welcome` carries, its operating system and architecture as its labels
  name them, and `RUNNER_TEMP` (`work/_temp`), `RUNNER_TOOL_CACHE` (`tools/`) and
  `RUNNER_WORKSPACE`, which the agent creates before the first step. Before the first step it also
  writes the payload, `github.event`, to `RUNNER_TEMP/_github_workflow/event.json`, the file
  `GITHUB_EVENT_PATH` names, and teardown removes it with the rest of `RUNNER_TEMP`. The cases in
  `contracts/fixtures/steps/` pin these rules.
- **Node action.** Run with the Node runtime the assignment names for it, from the agent's
  `externals/`, or mounted into the job container at `/__e/node20` or `/__e/node24`; for images
  based on musl the musl build is mounted. Inputs arrive as `INPUT_<NAME>`, with the name uppercased
  and spaces replaced by `_`, defaults applied, and a warning for a missing required input
  ([Node actions](#node-actions)).
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

### Node actions

Step 33 of M1 ([../plan.md](../plan.md)) settled how the agent runs Node actions; the fixture
actions in `compat/fixtures/actions/`, built with the published toolkit, pin it.

**Stages.** A job's steps run as stages, which `bosun_types::ir::stages` derives from the job's
steps and the metadata of their actions, so that the server and the agent count them alike: first
a pre stage for each step that uses an action with `pre`, in the order of the steps; then a main
stage for every step; last a post stage for each step that uses an action with `post`, in the
reverse order of the steps. The index of a stage is the `step_index` of every report and log chunk
about it, and the server creates one step per stage ([04-runs.md](04-runs.md#parts-of-a-run)). The
target registers a post step when its action's pre or main step starts and runs them as a stack,
so an action with `pre` has its post step after those of actions without one; Bosun keeps the plain
reverse order of the steps, which the target produces whenever the actions agree on `pre`.

**Conditions.** A pre stage runs when `pre-if` holds, `always()` without one; a main stage when the
step's `if` holds, as for any step; a post stage when `post-if` holds, `always()` without one, and
only after its pre or main stage started, as the target runs only the post steps it registered: a
post stage whose pre and main stages both did not start, such as that of a skipped step whose action
has no `pre`, is reported `skipped` without a `StepStarted`. A condition sees the job's status as
the `if` of a step does, so a post stage after a cancel runs with its `always()` and runs to its
end, and `post-if: failure()` runs only once the job has failed. A job over its timeout runs no further stage, post
stages included. The step's `env`, `timeout-minutes` and `continue-on-error` apply to each of its
stages, as the target applies them; only the main stage gives `steps.<id>` its entry.

**Inputs.** Each stage renders the step's `with`, after its `env`, and every input of the metadata
that `with` does not name, compared ignoring case, gets its default rendered with the contexts of the
step, or the empty string without one. Each becomes `INPUT_<NAME>`, the name uppercased and spaces
replaced by `_`, laid over the environment of a step together with `STATE_<name>` for the state
saved so far. In the main stage, an input that is required, not named in `with` and without a
default gives the warning ``the action `<uses>` requires the input `<name>`, which the step does not
supply``, and a deprecated input that `with` names gives ``the input `<name>` of the action `<uses>`
is deprecated: <message>``, each an annotation of the step with its line in the log. The toolkit's
`getInput` would refuse a missing required input only when the action asks it to; the warning makes
the omission visible either way.

**State.** Each step that uses an action keeps a state of its own: what its pre stage saves, through
`GITHUB_STATE` or `::save-state`, reaches its main and post stages as `STATE_<name>`, and what its
main stage saves reaches its post stage. What a post stage saves goes nowhere. Two steps that use
the same action keep two states.

**Running.** A stage runs `<node> <directory>/<entry>`, the entry being the metadata's `pre`, `main`
or `post`, in the workspace, with the process handling, file commands and workflow commands of a
`run` step. `github.action` is the step's `id`, or `__<owner>_<repo>` for a step without one;
`github.action_path` is the action's directory, `github.action_repository` its `<owner>/<repo>` and
`github.action_ref` its ref as the step writes it, each mirrored as `GITHUB_ACTION_*`.

**Fetching.** After it accepted a job and before its first stage, the agent fetches what the job's
actions need from the server it connects to, with its credential as a bearer token, directly and
without the proxy its configuration gives steps, as its stream connects:

- each action's tarball, once per repository and commit, from its `download`, into
  `work/_actions/<owner>/<repo>/<commit>` with the owner and the repository in lower case, the
  tarball's top directory becoming that directory, and an action's `path` below it as its directory.
  A directory that exists is complete and is used again by later jobs, since its commit pins it;
- each version of Node the actions run on, from
  `/_runtime/tools/node/<version>/<platform>.tar.xz`, into `externals/node<major>`, its top
  directory becoming that directory and `bin/node` its program. `externals/node<major>.version`
  records `<version> <platform>`, so a directory of another version, or of a fetch that stopped
  before it recorded one, is fetched again and replaced. Two versions of one major version in one
  assignment would share a directory, and the agent rejects the assignment.

`<platform>` is `<os>-<arch>`, `linux-x64` or `linux-arm64`, with `-musl` on a host whose `/lib` or
`/lib64` holds the loader of musl, `ld-musl-*`, and no loader of glibc, `ld-linux*`, since a host of
glibc may carry musl's loader too. An agent built for macOS, which only developers run, asks for
`darwin-<arch>`, which the server does not serve. An archive is written to `.<name>.download` beside its directory,
unpacked into `.<name>.partial` with every entry kept inside it, and only then renamed into place,
so an interrupted fetch leaves no directory that looks complete. A fetch that fails, an answer other
than `200` among them, fails the job in its first stage with ``the job could not be prepared on
this agent: <address> answered <status>: <message>`` and runs no stage after it.

### File commands and workflow commands

Before each step the engine creates fresh, empty files for `GITHUB_ENV`, `GITHUB_OUTPUT`,
`GITHUB_PATH`, `GITHUB_STATE` and `GITHUB_STEP_SUMMARY`, and after the step, whatever its outcome,
it reads the first four. `GITHUB_ENV`, `GITHUB_OUTPUT` and `GITHUB_STATE` hold one entry after
another: a line `NAME=VALUE`, split at its first `=`, or a line `NAME<<DELIMITER` followed by the
lines of the value and a line that is exactly the delimiter. The value of the second form is those
lines without the line break before the delimiter; a line ends at `\n` or `\r\n`, empty lines between
entries are skipped, and neither names nor values are trimmed, as in the target. A line of neither
form, an entry without a name or a delimiter, and a value whose delimiter never comes fail the step,
with a line in its log that names the file. `GITHUB_PATH` holds a directory per line, and empty
lines are skipped.

What `GITHUB_ENV` sets reaches the environment and the `env` context of the later steps, below the
job's `env`; what `GITHUB_PATH` adds goes to the front of their `PATH`, and a directory added again
moves to the front. Neither changes the step that wrote it. `GITHUB_OUTPUT` sets the outputs of the
step, the last value of a name winning, which `StepFinished` reports and `steps.<id>.outputs` shows
from the next step on. `GITHUB_STATE` is read and checked like the others; its values, and those of
`::save-state`, are the state that the pre and main steps of a Node action pass on to the steps of
the same action after them ([Node actions](#node-actions)), and a script's state goes nowhere, as in
the target. The cases in `contracts/fixtures/file-commands/` pin these rules.

Step 23 of M1 ([../plan.md](../plan.md)) settled the step summary, the workflow commands and the
problem matchers below.

**The step summary.** After each step, whatever its outcome, the engine reads `GITHUB_STEP_SUMMARY`;
an empty file gives the step no summary. The text is masked ([Masking](#masking)), a sequence that
is not UTF-8 replaced by U+FFFD, and cut at 1 MiB: at the start of a masked value that would cross
the cut, and at a character boundary. A cut summary gives the step the notice `the step summary is
longer than 1 MiB, so only its first 1 MiB is kept`. The target drops a summary over 1 MiB with an
error instead; Bosun keeps its start. The summary travels in `StepFinished`, not as an upload the
report refers to ([06-runtime-services.md](06-runtime-services.md#step-summaries-and-annotations)).

**Workflow commands.** Every whole line of a step's output of at most 64 KiB, without a `\r` that
ends it, is read for a command. A line is a command when, after the white space at its start, it
begins with `::`, holds a second `::`, and the text between them up to its first space names one of
the commands below, in any case. The rest of that text is the properties, `key=value` separated by
commas, the key in any case, where one without a key or a value is ignored; everything after the
second `::` is the data. Data is unescaped from `%0D`, `%0A` and `%25` in this order, a property
also from `%3A` and `%2C` before `%25`, the reverse of the toolkit's escaping, so `%250A` stays
`%0A`. Any other line is output, and so is a line longer than 64 KiB, which the log splits. The
line of a command is not logged; each command writes what its entry says:

- `::debug::` writes each line of its data after `##[debug]` when step debug is on, and nothing
  otherwise. Step debug is on when `ACTIONS_STEP_DEBUG` is `true` in the `vars` or `secrets`
  context or in the step's environment, or `RUNNER_DEBUG` is `1` in the step's environment.
- `::notice`, `::warning` and `::error` give the step an annotation of their kind whose message is
  the data, and write the message after `##[notice]`, `##[warning]` or `##[error]`. The properties
  `title`, `file`, `line`, `endLine`, `col` and `endColumn` become the annotation's; a number that is
  not a whole number of at least 0 is left out, and the rest are corrected as the target corrects
  them: `endLine` without `line` is both, `endColumn` without `col` is both, columns without a line
  are left out, and so are the columns when `line` and `endLine` differ, the lines when `endLine` is
  before `line` and the columns when `endColumn` is before `col`. An absolute `file` inside the
  workspace becomes relative to it. Empty data gives neither an annotation nor a line, since the
  forge refuses an annotation without a message.
- `::group::` writes `##[group]` and its data, `::endgroup::` writes `##[endgroup]`, and the log
  viewer folds the lines between them ([09-frontend.md](09-frontend.md)).
- `::add-mask::` adds its data to the masker, which masks it in every text that leaves the agent
  from then on, later lines of the step and later steps included. White space alone masks nothing
  and gives a warning.
- `::stop-commands::<token>` writes its own line, and every later line is output, read by the
  problem matchers but not for commands, until a line that is a command named `<token>`, in any
  case, which is written too and ends the pause. A token that is empty, `pause-logging` or the name
  of a command fails the command, unless the step's environment sets
  `ACTIONS_ALLOW_UNSECURE_STOPCOMMAND_TOKENS` to `true`.
- `::echo::on` and `::echo::off` turn echoing on and off until the step ends; each step starts with
  it off. While it is on, `set-output`, `save-state`, `set-env`, `add-path`, `add-matcher`,
  `remove-matcher` and `echo` write their own line before they act; the other commands never do.
  Data other than `on` and `off`, in any case, fails the command.
- `::add-matcher::` and `::remove-matcher` add and remove problem matchers, below.
- `::set-output name=<name>::<value>` sets an output of the step, and `::save-state` with a `name`
  is read like an entry of `GITHUB_STATE`. Both are deprecated, and each use gives a warning. Since
  the files are read after the step, a value the file `GITHUB_OUTPUT` gives the same name wins.
- `::set-env name=<name>::<value>` and `::add-path::<directory>` act as an entry of `GITHUB_ENV`
  and of `GITHUB_PATH` do, applied before the files, when the step's environment sets
  `ACTIONS_ALLOW_UNSECURE_COMMANDS` to `true`; otherwise they fail.

A boolean of the step's environment or of a context is `true` as the target reads it, in any case
and with white space around it. A command that fails, by those rules or because `set-output`,
`save-state` or `set-env` has no `name` or a document of problem matchers cannot be read or breaks
their rules, gives the error ``the command `<line>` could not be processed: <reason>`` and fails
the step, as in the target, while its script goes on.

**Problem matchers.** `::add-matcher::<file>` reads a document of problem matchers in the target's
format, its path relative to the workspace: `problemMatcher`, a list of matchers, each with an
`owner`, an optional default `severity` of `error`, `warning` or `notice`, and `pattern`, a list
of patterns, each with a regular expression `regexp`, the group numbers of the properties `file`,
`fromPath`, `line`, `column`, `severity`, `code` and `message`, and `loop`; other keys are ignored.
The document is refused when a matcher has no owner, an owner names two matchers ignoring case, a
default severity is another, a matcher has no pattern, `loop` is set on a pattern that is not the
last of several or that sets no `message`, a `regexp` does not compile, a property is set in two
patterns of a matcher or to a group its pattern does not have, or no pattern of a matcher sets
`message`. Matchers belong to the job: those a document adds come first, replace any matcher of the
same owner ignoring case, and apply from the next line on, in this step and the later ones, and the
log gets ``added the problem matchers `<owner>`, …``. `::remove-matcher owner=<owner>::` removes
the matcher of that owner and `::remove-matcher::<file>` those of the owners a document names, and
the log gets ``removed the problem matchers …`` or `removed no problem matcher`; a command with both
an owner and a file, or with neither, only gives a warning. An empty `::add-matcher::` gives a
warning as well.

Every line that is not a command is matched, without its colour codes (`ESC [`, digits and `;`,
then `m`), against the job's matchers in order, as the target matches it. A matcher of one pattern
matches the lines its expression matches. In a matcher of several, a pattern after the first is
tried only on the line after the one its predecessor matched, the last completes a match, and a
last pattern with `loop` keeps what the patterns before it captured, so each further line it
matches is another match until a line breaks the sequence. A property comes from the first pattern
that captures it. When one matcher completes a match, every other one starts over. The match
becomes an annotation when its message is not blank and its severity, or else the matcher's, is
empty, `error`, `warning` or `notice` in any case, an empty one meaning `error`; the line is then
written after `##[<kind>]`, colour codes removed, in place of the line, and the first matcher whose
match gives an annotation takes the line. `line` and `column` count only when they are digits;
`code` is read but not reported, as the forge shows none. `file` is resolved as the target resolves
it: a relative path from the directory of `fromPath` when the match has one, then from the
workspace, with `.` and `..` resolved. The file must exist and belong to the workflow's repository:
the nearest directory above it that holds `.git/config` must name `<server_url>/<repository>` or
`git@<host>:<repository>.git` as a `url`, ignoring case, with `server_url` and `repository` from
`github`, and the path becomes relative to that directory; otherwise the annotation has no file.
Patterns are expressions of `fancy-regex`, a backtracking engine like the target's that has
lookaround and backreferences too, so the matchers of the setup actions in use compile. A line on
which matching exceeds its backtracking limit removes the matcher from the job with a warning, as
the target removes a matcher that keeps timing out. The cases in
`contracts/fixtures/matchers/` run the matchers that the setup actions of eslint, tsc and gcc
register.

**Annotations.** A step keeps its first 10 annotations of each kind and a job its first 50, as the
target keeps them, and the log keeps the line of every one. `StepFinished` carries a step's
annotations with their message, title and file masked and each cut at 4096 characters: the target
cuts the message there, and Bosun cuts the two others too, so the 30 annotations of a step stay
under 1.5 MiB and with a summary of 1 MiB fit a message of the stream ([Protocol](#protocol)) with
room to spare. The cases in `contracts/fixtures/workflow-commands/` pin the commands, the
annotations, the summary and the log.

### Masking

Every text passes through the job's masker before it leaves the agent: the lines of a step's log,
the agent's own lines in it, error messages among them, the outputs `StepFinished` reports, the
annotations and the step summary of a step, and the output of a debug terminal. The execution
engine masks as it produces each text, so the log spool on disk holds masked lines too. Annotations
and summaries are masked like log lines in every text they carry, the message, the title, the file
and the summary itself, before `StepFinished` carries them (step 23 of
[../plan.md](../plan.md)). Of the messages about a job, only a `Reject` leaves without the masker,
since the agent rejects an assignment before it builds one. Its reason may quote the execution id
and the slugs of the assignment, the `repository` of its `github` context, a field or a setting of
the job, and the error of a document that does not parse, which can quote a value of that document,
`evaluated.env` included. The server writes the documents from the types the agent reads and never
writes one whose values fail to parse, so the parse error a reason carries is that of a field the
agent does not know, which names the field and no value (see *Fields an agent does not know*).

**Values.** The masker holds every secret in the assignment's `secrets` context, the forge token,
the job token and every value given to `::add-mask::`, whatever their length; a value of
`::add-mask::` joins it from the next text on. A value is taken without the white space around it,
as in the target, and so is each of its lines, split at `\r` and `\n`; one that is only white space
masks nothing. A multi-line value is so masked as a whole, as a message of the agent can quote it,
and line by line, as a step prints it.

**Encoded forms.** For each value it also masks the encoded forms the target masks:

- JSON-escaped, as `toJSON` escapes a string, without the quotes around it;
- URL-encoded: every byte of the UTF-8 text but `A`–`Z`, `a`–`z`, `0`–`9`, `-`, `.`, `_` and `~`
  as `%XX` with upper-case digits;
- XML-escaped: `<`, `>`, `"`, `'` and `&` as `&lt;`, `&gt;`, `&quot;`, `&apos;` and `&amp;`;
- escaped for an expression string, with `'` doubled, and for a command line, with `"` as `\"`;
- for PowerShell, the two sections of a value with `&` that PowerShell prints apart, or wraps in
  colour codes, when the value given to it as a command fails. A value that holds `&+` is cut at
  its first `&+`: the first section runs through the `&+`, and the second starts after it and after
  the one character PowerShell colours there. Any other value is cut at its last `&`: the first
  section runs through that `&`, and the second is the rest. The target masks a section only from
  six characters on; Bosun applies the rule on short forms below, so it masks one from four bytes
  on. Beyond the target, Bosun also masks the parts before and after the value's first `&`, so a
  value printed with white space around its first `&` is masked too;
- without its surrounding quotes, when the value starts and ends with `"`;
- base64 at the three byte alignments. For each alignment, with 0, 1 or 2 bytes before the value in
  its group of three, the masker takes the encoding without its first characters that also depend
  on the bytes before the value, in two forms: up to the last character that depends on the value
  alone, which matches whatever follows the value, such as the newline `echo` adds; and to its end
  with its padding, which matches a value that ends the encoded data. The target masks only padded
  encodings, of the value and of the value without its first byte or two, which a value followed by
  other data escapes; Bosun masks those characters and more. A value after one leading byte shows
  as two characters and `***`, as `Lm***` for `printf '.%s' "$TOKEN" | base64`, and after two
  leading bytes as three characters and `***`.

An encoded form shorter than four bytes is not masked, since it would match ordinary text: the
expression escape of a lone `'`, or the encoding of one byte at an alignment that leaves one
character of it. The value itself is masked at any length.

**Matching.** All forms of all values are the patterns of one Aho–Corasick automaton, built when
the job starts and again when a value is added, so the cost of masking a line grows with the line
and not with the number of values. Every occurrence is found, overlapping ones included;
occurrences that overlap or touch merge, and each merged range becomes `***`, so `ababab` with the
value `abab` is `***`. An automaton that cannot be built, because its states would outnumber its
state identifiers, masks every text as a whole, so a failure to mask lets nothing through. A line
is masked before it is split at 64 KiB, so the split never cuts a value in two. A line still being
written is split only once the longest form, less one byte, has arrived after the point of the
split; where an occurrence crosses that point, the line is split where the occurrence starts, or,
when its range starts the piece, the piece is masked whole and the rest of the range is carried
into the next piece as masked.

**Outputs.** `StepFinished` reports the outputs of a step with their names and values masked, while
`steps.<id>.outputs` keeps the values for the later steps of the job. A job output whose value
contains a masked value is dropped, as in the target: `JobFinished` does not report it, a line in
the log of the last step that started names it, and a dependent job's
`needs.<job>.outputs.<name>` is empty. The cases in `contracts/fixtures/masking/` pin these rules.

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
  externals/node<major>                Node runtimes for actions, such as node24
  externals/node<major>.version        the version and platform each of them holds
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
trust level `trusted` ([08-access.md](08-access.md)), and the server drops it from the agents of
untrusted pools ([Pools and label aliases](#pools-and-label-aliases)).

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
`owner/repo@ref` and `owner/repo/path@ref` to a commit through the forge, stores the tarball in the
object store keyed by the commit, reads the action's metadata from it, and gives the agent an
address on the server. A tag that moves between assignment and execution has no effect on a running
job.

The organisation's action policy decides which actions may be used: all, only those owned by the
organisation, or those matching listed patterns. A job that uses a forbidden action fails before it
reaches an agent, naming the action. The policy arrives in M2.

Node runtimes reach agents the same way: the agent fetches
`/_runtime/tools/node/<version>/<os>-<arch>[-musl].tar.xz` from the server, which caches it from
the configured upstream. Tools that actions download for themselves, such as language toolchains,
use the job's network and proxy settings, not Bosun.

Step 32 of M1 ([../plan.md](../plan.md)) settled how.

**Resolution.** When a replica has claimed a job for an agent, and before it takes the run's lock,
it prepares the job's actions: each distinct `uses` of its steps once, in their order. The owner
and the repository are compared ignoring case, as the forge compares them, and are kept in lower
case wherever Bosun stores them; the ref is compared exactly.

- An organisation that an installation of the GitHub App maps to
  ([06-runtime-services.md](06-runtime-services.md#installations-and-repositories)) resolves
  through that installation's token, so the private actions of the organisation resolve, and so
  does every public repository, which every installation token can read. A private repository of
  another account does not: the forge answers as if it did not exist.
- An organisation without an installation, such as one that has only repository-less projects,
  resolves without credentials, within the forge's rate limit for requests without a token. Only
  public repositories resolve that way; a private one fails with a message that says the
  organisation has no installation. The two other rules were refused: resolving through another
  organisation's installation would let one organisation read the private actions of another and
  spend another organisation's rate limit, and refusing every action would keep repository-less
  projects from `actions/setup-node` and its kind. The forge answers a conditional request that
  finds nothing new with `304`, which does not count against the limit, so a steady use of a few
  public actions stays well within it.
- A server without a GitHub App resolves no action, and every job that uses one fails.

The ref resolves to a commit through the forge's `commits/<ref>`
([06-runtime-services.md](06-runtime-services.md#the-github-app-client)). The first assignment of
a job pins its commits: the server writes them to the job's `actions`
([07-storage.md](07-storage.md#tables)) as a list of owner, repository, ref and commit, in the
transaction that sends the assignment. A later assignment of the same job, after a `Reject` or an
expired lease, takes the pinned commits and resolves nothing, so moving the tag after a job was
first assigned never changes the commit that job receives; another job resolves the tag again and
gets the commit it names then.

**The store.** The tarball of a commit is fetched once into `actions/<owner>/<repo>/<commit>.tar.gz`
and recorded in `action_ref`, with its size, its SHA-256 and every `action.yml` and `action.yaml` it
holds below its top directory, keyed by its path in the repository: the text of each of at most
1 MiB, and the size of a larger one, which the server does not read. A replica that finds no row
takes a transaction-level advisory lock on the key and checks again, so of two assignments of the
same new action at once, by two replicas or by one replica for two of its agents, one downloads it
and the other waits for its row; a waiting replica asks for the lock again every 100 milliseconds,
holding no snapshot in between, for at most ten minutes. The tarball streams from the forge into the
store while its digest is taken; it is then read back once to collect the metadata files, and only
after both is the row written, in the lock's transaction. A tarball that is not gzip-compressed tar
is deleted from the store again.

**Metadata and runtimes.** The metadata of each `uses` is the `action.yml` of the action's directory
in the row, or else its `action.yaml`, lowered with `bosun_workflow::action`
([02-workflows.md](02-workflows.md#lowering-action-metadata)) at every assignment. Its `using` names
the Node that runs it through the installation's runtime map, the settings `tools.node12`,
`tools.node16`, `tools.node20` and `tools.node24` ([12-operations.md](12-operations.md#configuration)),
each an exact version of Node, by default all the same version of Node 24, as current hosted
runners run them. An action whose `using` names another major version than the one it is assigned
adds the warning `node_substituted` to the run's notices at its step, once, when the job is first
assigned ([04-runs.md](04-runs.md#run-notices)).

**Failures.** The job fails before it reaches an agent when one of its actions:

- cannot be resolved, or its tarball cannot be downloaded or read: `action_unresolved`. A forge that
  refuses, is rate limited or cannot be reached fails the job too, with the forge's error in the
  message, rather than leaving it queued;
- has no metadata in its directory, metadata larger than 1 MiB, which the message names with its
  size, or metadata with errors: `action_invalid`, quoting the first error with its file, line and
  column;
- declares `using: composite` or `using: docker`: `not_implemented`, naming M2.

Each such action adds an error notice at its step whose message names it, and then, in one
transaction under the run's lock, the execution and the job complete as `failure` with the reason
`action_unavailable`, the planner takes the run on, the agent is sent nothing, and the replica claims
the next job for it at once, as after an error in the workflow's `env`. A failure of the object store
or of the database while the actions are prepared fails nothing: the replica logs it and does not
offer the job, whose lease returns it to the queue for another attempt. The lease counts while the
actions are prepared, so a download that outlasts it returns the job to the queue as well, and the
tarball it stored serves the next claim.

**Downloads.** An agent downloads with its credential, `Authorization: Bearer bat_…`, which the
server checks as it checks the stream's. A request without one, with a malformed one or with an
unknown one is `401 unauthenticated`.

- `GET /_runtime/actions/<owner>/<repo>/<commit>.tar.gz` serves the stored tarball only to an agent
  that holds an execution, not yet completed, whose job pinned that commit of that repository;
  another agent, and the same agent once its execution completed, gets `403 forbidden`. An action
  is code that runs with the job's secrets, and a private action is the organisation's code, so an
  agent reads only what a job it runs uses.
- `GET /_runtime/tools/node/<version>/<platform>.tar.xz` serves Node to any agent, since Node is
  public; `<platform>` is `linux-x64`, `linux-arm64`, `linux-x64-musl` or `linux-arm64-musl`, and the
  version must be one the runtime map names, so an agent cannot make the server fetch arbitrary
  files. Anything else is `404 not_found`. The archive is fetched once, under the same lock as an
  action's tarball, from `tools.node_upstream` for glibc and `tools.node_musl_upstream` for musl,
  as `<upstream>/v<version>/node-v<version>-<platform>.tar.xz`, and checked against the line for it
  in `<upstream>/v<version>/SHASUMS256.txt`, the checksum list Node publishes. It streams into
  `tools/node/<version>/<platform>.tar.xz` while its digest is taken, and a digest that differs from
  the list fails the store's put, so nothing of it is kept; the request is then `500 internal` with
  a message that names both digests. A stored archive, recorded in `tool_object`, is served from the
  store to every later request. An upstream is an http or https address, or a file address of a
  directory laid out the same way, for a host without access to the internet.

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
