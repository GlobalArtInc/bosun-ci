# Storage

This document specifies what Bosun keeps and where: the Postgres schema in outline, migrations and
identifiers, the job queue, notifications between replicas, periodic tasks, the outbox, the object
store, the log pipeline, retention, and secrets at rest.

## Postgres

One database `bosun` with one schema `bosun`, Postgres 15 or newer. Entities are sea-orm models
over sqlx. The queue runs raw SQL through the same connection rather than entities, because its
queries depend on `FOR UPDATE SKIP LOCKED` and `NOTIFY`; the listener of the notifications and the
hot log path use sqlx directly, the listener because it depends on `LISTEN`.

**Migrations** are SQL files applied by sqlmig with its ledger. Inside a major version they are
additive only: new tables, new nullable columns or columns with defaults, new indexes created
concurrently. `bosun-server migrate` applies them; `bosun-server serve` applies them at start under
an advisory lock, so several replicas starting at once apply them once. A server starts against a
schema that a newer version of the same major migrated, because additive migrations leave
everything it uses in place, which is what makes rolling updates and rollbacks safe; it refuses to
start against a schema of a newer major version.

The files are `crates/db/migrations/<milliseconds>-<slug>.up.sql`, embedded in the binary. There
are no down files, and a test refuses `DROP`, `ALTER … DROP` and `ALTER … TYPE` in any of them.
`CREATE INDEX CONCURRENTLY` refuses to run inside a transaction or next to another statement, so a
file that creates an index holds that one statement and opens with sqlmig's directive
`-- migrator:no-transaction`, the only line of a migration that is not SQL to run.
`bosun-db` creates the schema `bosun` before the first migration and sets `search_path` to it on
every connection, so the ledger of sqlmig (`migrations` and `migrations_checksum`) lives there too.
The migrations seed the trusted pool `default` ([05-execution.md](05-execution.md)) and the four
built-in roles ([08-access.md](08-access.md#roles-and-bindings)). The future of
sqlmig's `apply` is not `Send`, so the server awaits the migrations on the task that starts it
rather than spawning them.

`bosun-db` wraps the whole migration in an advisory lock of its own, held on a connection it takes
out of the pool for the purpose: the creation of the schema, the check of the major version,
sqlmig's `apply` and the record of the major version. A replica that finds the lock taken asks
again with `pg_try_advisory_lock` every 100 milliseconds, idle in between, and gives up after 150
seconds with an error that says so. It does not wait inside `pg_advisory_lock`, because a session
blocked in a statement holds a snapshot, `CREATE INDEX CONCURRENTLY` waits for every older snapshot
in the database, and the replica holding the lock would wait for the one waiting for it until that
one gave up. sqlmig's own lock covers only the files; `CREATE SCHEMA IF NOT EXISTS` and the creation
of sqlmig's ledger run before it and fail with a unique violation when two replicas start against
an empty database at once.
Because sqlmig holds two pooled connections at once while it applies, the pool of a server has at
least two.

The major version a schema belongs to is the table `schema_version`, one row per major version of
Bosun that ever migrated it, written after the migrations of that version applied. Before it
applies anything, a migration refuses a schema whose highest row is above the server's own major
version (the major of the workspace version), and the refusal names both. A newer minor version of
the same major only adds migrations to the ledger that an older server does not carry, which
sqlmig ignores, so the older server starts.

**Identifiers** are UUIDv7, ordered by creation time. Two numbers exist for compatibility: a run's
number among the runs of its workflow (`github.run_number`) and a global number from a sequence
(`github.run_id`). The run's number is the workflow's `last_run_number`, incremented in the
transaction that creates the run, so the row lock orders concurrent dispatches of one workflow and
a number is never given twice, even after runs are deleted. Times are `timestamptz`.

### Tables

| Area | Tables |
|------|--------|
| identity | `user`, `user_identity`, `team`, `team_member`, `role`, `role_binding`, `api_token`, `api_token_scope`, `session`, `sign_in_failure`, `oidc_state` |
| tenancy | `organisation` (with its settings, among them the action policy), `forge_installation`, `forge_repository`, `project`, `environment`, `environment_rule`, `secret`, `variable` |
| workflows | `workflow`, `workflow_revision`, `workflow_draft`, `schedule`, `upstream_subscription` |
| runs | `run`, `attempt`, `job`, `execution`, `step`, `run_event`, `stream_event`, `approval`, `annotation`, `step_summary`, `terminal_session` |
| agents | `pool`, `pool_access`, `agent`, `registration_token`, `control_message`, `label_alias` |
| logs | `log_chunk`, `log_object` |
| artifacts and caches | `artifact`, `artifact_block`, `cache_entry` |
| actions and tools | `action_ref`, `tool_object` |
| delivery | `webhook_delivery`, `outbox`, `forge_check_run`, `webhook_subscription`, `idempotency_key` |
| plugins | `plugin`, `plugin_instance`, `plugin_kv` (from M5) |
| audit | `audit_event` |
| schema | `schema_version`, next to sqlmig's ledger `migrations` and `migrations_checksum` |

The central columns:

```
workflow     id, project_id, name, source_kind, enabled, revision_id, last_run_number, created_at
run          id, project_id, workflow_id, revision_id, ir jsonb, number, global_number,
             display_name, event, trigger jsonb, ref, sha, inputs jsonb, draft_id, derived_from,
             trusted, status, conclusion, cancel_requested_at, created_at, started_at,
             completed_at
attempt      id, run_id, number, reason, actor jsonb, created_at
job          id, attempt_id, key, name, group_id, matrix jsonb, strategy jsonb, needs text[], kind,
             status, waiting_reason, conclusion, conclusion_source, continue_on_error,
             evaluated jsonb, outputs jsonb, priority, labels text[], pool_ids uuid[],
             capabilities text[], actions jsonb, pinned_agent_id, carried_from,
             queued_at, started_at, completed_at
execution    id, job_id, number, agent_id, lease_until, status, conclusion, reason, started_at,
             completed_at, log_bytes, log_compacted_at
step         id, execution_id, parent_step_id, index, name, status, outcome, conclusion,
             conclusion_source, outputs jsonb, started_at, completed_at
log_chunk    execution_id, seq, step_id, data bytea, dropped
log_object   step_id, execution_id, key, size, lines, frames bigint[], created_at
run_event    seq bigserial, run_id, attempt_id, job_id, execution_id, step_id, kind,
             actor jsonb, data jsonb, at, xact
stream_event seq bigserial, xact, subject, project_id, run_id, kind, data jsonb, at
```

A run's `trigger` holds the actor and the payload of the event, `{"actor": …, "payload": …}`; for
`workflow_dispatch` the payload is the one [06-runtime-services.md](06-runtime-services.md)
describes, which `github.event` shows. A run's `display_name` is its rendered `run-name` or its
default name ([04-runs.md](04-runs.md#planning)); runs created before the column existed got the
workflow's name. A run's `cancel_requested_at` is the time an accepted `run.cancel` reached it, and
`trusted` whether its jobs may go to trusted pools only or to untrusted pools only
([05-execution.md](05-execution.md#pools-and-label-aliases)). A job is created with its key, display
name and `needs`; its `labels`, `pool_ids` and `capabilities` are written when the planner evaluates
`runs-on` as the job becomes ready ([04-runs.md](04-runs.md)), together with its rendered `name`,
its `continue-on-error`, which the conclusion of the run reads, and in `evaluated` the values its
assignment carries: `{"timeout_minutes", "env", "shell", "working_directory"}`, where `env` maps
names to rendered values and the other two are `null` when `defaults.run` does not set them.
`labels` are lowercased, without the labels a label alias replaced. `pool_ids` are the pools the job
may run in, among those whose access list admits its project: the pool `runs-on.group` names, none
when no such pool has that name, and without a group every one of them, each narrowed to the pool
its label aliases name. `capabilities` are those its label aliases name, empty when either
capability will do ([05-execution.md](05-execution.md#pools-and-label-aliases)). A job's `outputs`
are the outputs its agent reported in `JobFinished` for the names the job declares, `{}` until then;
a job group's are those of its members combined when it completes
([04-runs.md](04-runs.md#matrices)). A step's `outputs` are the outputs its agent reported in
`StepFinished`, `{}` until then.

A member of a job group is a row of its own with the template's key: `group_id` is the job of its
group, which carries no foreign key since it is a reference inside one run, and `matrix` its
combination as a JSON object. `strategy` is the `strategy` context of the job,
`{"fail-fast", "job-index", "job-total", "max-parallel"}`, written when the member is created or,
for a job whose strategy has no matrix, when the job is evaluated; it is `null` for a job without a
strategy, which sees the context of a single job. Postgres keeps the keys of a `jsonb` object in an
order of its own, so a member's `matrix` keeps its values but not their order, which named the
member when it was created ([04-runs.md](04-runs.md#matrices)).

A milestone creates the columns it uses. The columns above that M0 does not use (`draft_id` and
`derived_from` of runs, `reason` of attempts, `kind`, `pinned_agent_id` and `carried_from` of jobs,
`parent_step_id` of steps) arrive with their milestones as additive migrations; the display name of
runs and the evaluated values and outputs of jobs arrived with the planner's expressions (step 15 of
[../plan.md](../plan.md)), `group_id`, `matrix` and `strategy` of jobs with matrices (step 16), and
the outputs of steps with the assignment of M1 (step 20), and the trust of runs and the
capabilities of jobs with pools (step 18). The indexes of the queue arrived with the
claim (step 11 of [../plan.md](../plan.md)), as did the partial index on `execution (lease_until) where status =
'assigned'` that the lease expiry reads. The partial index on `execution (completed_at) where status =
'completed' and log_compacted_at is null` that the compaction of logs reads arrived with the logs
(step 13).

A project's settings ([11-api.md](11-api.md#project-settings)) are columns of `project`, one per
setting, with the defaults of the settings as the columns' defaults: `github_workflows`,
`fork_policy` and `report` as a boolean and two names, `token_permissions` as a `jsonb` object of
permission names to levels. `settings_version` is the version their `ETag` carries; it starts at 1
and grows by one with each change, which is an `UPDATE` conditioned on the version the change was
made from. They arrived with step 25 of [../plan.md](../plan.md); a later setting arrives as a
column with its default, so a project that existed before takes the default.

Secrets and variables are rows of `secret` and `variable`, each owned by an organisation and, for
one of a project, by that project as well: `organisation_id` and a nullable `project_id`, the pair
referencing the project as the access lists do, so that deleting the organisation or the project
deletes them. `name` is stored in upper case and unique within its organisation and project, a
`NULL` project counting as one. A variable holds its `value` as text. A secret holds no value in
clear: `ciphertext`, `data_key` and `kek_id` are its value sealed as
[Secrets at rest](#secrets-at-rest) describes. Both tables have `created_at` and `updated_at`, the
time the value was last set. They arrived with step 24 of [../plan.md](../plan.md).

The forge's installations and repositories, `forge_installation` and `forge_repository`, are keyed
by the forge's own ids, as `bigint`, since they mirror rows of the forge rather than rows Bosun
creates; a project's `repository_id` names its repository, a plain foreign key with a unique index,
so a repository is bound to one project at most
([06-runtime-services.md](06-runtime-services.md#installations-and-repositories)).
`webhook_delivery` holds `id, delivery, event, payload jsonb, received_at, attempts,
next_attempt_at, last_error, processed_at`, unique by `delivery`, the forge's id of the delivery.
`outbox` holds `id bigserial, kind, collapse_key, payload jsonb, created_at, attempts,
next_attempt_at, last_error, failing_since, dead_at` ([The outbox](#the-outbox)), with a partial
index on `(next_attempt_at, id) where dead_at is null` for the claim and one on
`(collapse_key, id)` for collapsing; `forge_check_run` maps a job to the id of its check run on the
forge, `job_id, id, created_at`, owned by the job. Both arrived with step 28 of
[../plan.md](../plan.md).

A workflow's `source_kind` is `server` or `repository`. A repository workflow is named by the path
of its file and is unique by project and path, which the partial unique index on
`workflow (project_id, name) where source_kind = 'repository'` keeps, so two replicas that read a
new file at once create one workflow; a revision of it carries `blob_sha`, the forge's hash of its
blob, by which the file is found again without reading it, and which is `null` for the revisions of
server-side workflows ([02-workflows.md](02-workflows.md#repository-workflows)). A run's `ref` and
`sha` are the ref and the commit of its event, `null` for a run of a project without a repository.
They arrived with step 27 of [../plan.md](../plan.md).

A job's `actions` are the commits its first assignment pinned for the actions its steps use, a list
of `{"owner", "repository", "ref", "commit"}` with the owner and the repository in lower case, and
`null` until then or for a job without a `uses`; they arrived with the action store (step 32 of
[../plan.md](../plan.md)), together with two tables of what the object store keeps for agents.
`action_ref` holds `id, owner, repository, sha, key, size, sha256, metadata jsonb, created_at`, one
row per commit of an action repository, unique by owner, repository and commit, where `key` is the
tarball's object, `sha256` its digest and `metadata` maps the path of every `action.yml` and
`action.yaml` in it to its text, a JSON string, or to its size in bytes, a JSON number, when it is
larger than the 1 MiB the server reads. `tool_object` holds `id, tool, version, platform, key, size,
sha256, created_at`, one row per archive of a tool, unique by tool, version and platform, such as
`node`, `24.22.1` and `linux-x64` ([05-execution.md](05-execution.md#actions-and-tools)). Neither
table references another, and no row is deleted in M1.

An agent records its current replica in `agent.replica_id`, which no row references: replicas are
processes, not rows. `agent.drain` says whether the agent is drained and `agent.deleted_at` when it
was deleted, `null` while it exists
([05-execution.md](05-execution.md#draining-and-deleting-agents)); a deleted agent keeps its row,
which its executions reference, and both columns arrived with step 19 of [../plan.md](../plan.md).
`control_message` holds the control messages sent to agents ([05-execution.md](05-execution.md)):
`id, execution_id, kind, created_at, acknowledged_at, took_effect`; its agent is the agent of its
execution.

Statuses, conclusions, reasons, trust levels, capabilities and event kinds are `text` holding the
names `bosun-types` serialises them to, with neither Postgres enum types nor check constraints, so
a later milestone adds a value without altering a type. Hashes of credentials, of workflow
sources and of idempotent requests are SHA-256 in `bytea`.

A row that another row owns references its owner with `ON DELETE CASCADE`: a project its
organisation, a workflow its project, a revision its workflow, a run its project and its workflow,
an attempt its run, a job its attempt, an execution its job, a step its execution, a journal event
its run, a stream event its project and its run, a log chunk its execution, a log object its step,
a control message its execution, a personal token its user, an entry of a token's restriction its
token and the organisation and project it names, a registration token its pool, an entry of an
access list its pool and the organisation and project it names, a label alias its organisation,
and a secret or a variable its organisation and project. Deleting a run under `run_retention`
therefore removes everything it owns in one statement. The other references inside one run (the
attempt, job, execution and step of a journal event, the step of a log chunk, the execution of a
log object) carry no foreign key, because the owner already bounds them and a cascade would have
to check each of them.
References to rows that are not owners (the current revision of a workflow, the revision of a
run, the pool of an agent, the agent of an execution, the pool of a label alias) are plain foreign keys, which refuse to delete what is still referenced. Every
foreign key column leads an index, which a test asserts.

`idempotency_key` keeps, for 24 hours, the principal (`user:<id>`), the key, a hash of the request
and the status and body of the response of every write that created something with an
`Idempotency-Key` ([11-api.md](11-api.md)). The row is written in the transaction of the write; a
row older than 24 hours is ignored and replaced when its key comes again. `label_alias`
maps a label such as `ubuntu-latest` to a pool, a capability and a default image, per installation
or per organisation, and `pool_access` holds the access lists of pools
([05-execution.md](05-execution.md#pools-and-label-aliases)).

### The journal

`run_event.seq` is a `bigserial`, and Postgres draws its value when the row is inserted, not when
the transaction commits. Without more, a reader that asks for the events after the last number it
saw could miss an event that commits later with a lower number. Bosun gives every reader of one
run's journal a commit-ordered view with a lock per run: a transaction that changes the state of a
run first takes the run's row lock (`SELECT … FROM run … FOR NO KEY UPDATE`), appends to its
journal only while it holds it, and holds it until it ends. The transactions that change one run
therefore run one after another, each draws its sequence numbers after the previous one committed,
and within a run the order of the numbers is the order of the commits: a reader that resumes after
the last number it saw of a run misses nothing and repeats nothing. The same lock is what the
planner needs, since two jobs completing at once in two transactions would otherwise both miss the
other and leave their dependent `pending`. `FOR NO KEY UPDATE` rather than `FOR UPDATE` leaves
inserts of rows that reference the run free, because their foreign key check takes
`FOR KEY SHARE`. Creating a run needs no lock, since no other transaction sees the run before the
creating one commits.

Rows are locked in one order to rule out deadlocks: the run, then its jobs, executions and steps,
then agents, whose row a control message takes `FOR SHARE`. A transaction that holds an agent's row,
such as the sweep that marks agents offline, takes no run's lock; the sweep fails abandoned
executions afterwards, one transaction each. The claim of the queue takes the job and its run's
lock with `SKIP LOCKED`, so it never waits for a run that another transaction is changing; it takes
the agent's row first, and never waits after that, so it closes no cycle.

The lock orders the journal of one run, not the journal of all runs. A reader across runs, such as
the event stream filtered by project ([11-api.md](11-api.md#the-event-stream)), arrives with a
cursor of its own: each event records its transaction (`pg_current_xact_id()`), and the stream reads
only the events of transactions older than the oldest one still running
(`pg_snapshot_xmin(pg_current_snapshot())`), in the order of transaction and sequence number,
resuming after the pair it last delivered. Every transaction below that horizon has ended, so
nothing appears behind the cursor; a long transaction delays the stream by its length.

Step 37 of M1 ([../plan.md](../plan.md)) added the column, `run_event.xact`, and settled what it
holds. A transaction's id is drawn when it first writes or locks a row, which can be before it takes
a run's lock: the claim of the queue locks the agent's row first, and another transaction may change
the run and commit between that moment and the claim's lock of the run. Ordered by the bare id, the
claim's events would come before the events that committed ahead of them. So `xact` is the
transaction's id raised to the `xact` of the run's latest event when that is higher, written as
`bigint`: within one run it never falls, its order is the order of the journal, and it is never
lower than the id of the transaction that wrote it, so every event below the horizon is final. The
pair (`xact`, `seq`) is an event's position. The insert reads the latest event through
`run_event (run_id, seq)`, under the run's lock, and the stream reads through an index on
`(xact, seq)`. Events written before the column existed have `xact` 0, below every horizon; the
column's default is the bare id of the writing transaction, so the events that a replica of an
earlier version writes during a rolling update reach the stream too, in the order of the bare ids.

The events of the stream that belong to no run's journal ([11-api.md](11-api.md#the-event-stream))
are rows of `stream_event`, a second sequence with positions of the same kind: `seq bigserial, xact,
subject, project_id, run_id, kind, data jsonb, at`. `subject` is the agent of an `agent.status` or
the job of a `queue.job`, and `xact` is raised to that of the subject's latest event, which the
insert reads through `stream_event (subject, seq)`. Writers of one subject run one after another,
since the events of an agent are written in the transaction that inserts or updates its row, under
that row's lock, and the events of a job under its run's lock, in the transaction of the
`job.status` event that moves it into, within or out of the queue, so the subject's events are
ordered as the journal of a run is. A `queue.job` belongs to its project and its run, which delete
it with `ON DELETE CASCADE`, so `run_retention` removes it with its run. An `agent.status` belongs
to neither, and an agent writes a few for each of its connections; retention deletes those older
than 7 days ([Retention](#retention)), from step 36 of M1 on, and a client that resumes after a
longer gap reads `GET /agents` again. Each sequence is read up to the horizon of the statement that
reads it, and a reader that reaches an event above it stops there and tries again shortly
([11-api.md](11-api.md#the-event-stream)). The horizon counts the transactions of every database of
the Postgres server, so a long transaction anywhere on it delays the stream.

## The queue

The queue is the set of `job` rows in status `queued`. A partial index on
`(priority desc, queued_at) where status = 'queued'` orders them and a GIN index on `labels`, partial
on the same condition, filters them; labels are stored lowercased. `GET /queue` lists the queued and
the waiting jobs together ([11-api.md](11-api.md#the-queue)) through a partial index on `job (id)
where status in ('queued', 'waiting')`, which step 37 of M1 added.

An idle agent claims a job in one transaction: select the candidates whose labels are contained
in the agent's labels, whose pools include the agent's pool, whose project the access list of the
agent's pool admits, whose capabilities the agent holds, and whose trust matches the pool's
(jobs of untrusted runs only in untrusted pools, jobs of trusted runs only in trusted pools),
order them by priority, by the
project's ratio of running jobs to its weight, and by `queued_at`, take the first with
`FOR UPDATE SKIP LOCKED`, together with the lock of its run ([The journal](#the-journal)), create
its execution with `lease_until` 30 seconds ahead (`agents.assignment_lease`), and set the job
to `assigned`. Two replicas claiming for two agents at the same moment never take the same job and
never wait on each other. The conditions of access, capabilities and trust arrived with pools (step
18 of [../plan.md](../plan.md)); an agent that holds no capability claims nothing. The ratio arrives
with fair share in M2; until then the order is priority, then `queued_at`, then the job's id.

An agent is idle from the `Idle` it sends until the replica that holds its stream sends it an
assignment ([05-execution.md](05-execution.md)). That replica keeps the state in memory, for its
own streams only, and claims for one agent at a time: a reason to claim that arrives while a claim
for the agent runs makes it try once more when that claim finds nothing. The claim itself checks the
database too: it takes the agent's row `FOR NO KEY UPDATE`, which orders two claims for one agent,
and takes nothing for an agent whose current replica is another one, that is drained or deleted
([05-execution.md](05-execution.md#draining-and-deleting-agents)), or that still has an execution
that is not `completed`. It looks for those executions in a statement of its own after the lock is
held, so a claim that waited for another one of the same agent sees the execution that one created.
A job an agent rejected on its current stream is passed over for that agent until it reconnects, so
an assignment it cannot run is not offered to it again and again.

A claim that finds no candidate it could lock, while candidates exist, took none only because
other transactions held their runs, most often other claims for jobs of the same run. It says so,
and the replica tries again after a pause between half of and all of 10 milliseconds, doubled with
every such claim in a row up to 1 second, instead of leaving the agent idle until the next sweep.

A job that becomes `queued` is announced with `NOTIFY bosun_queue` in the transaction that queues
it, at dispatch, when its needs complete, and when it returns to the queue; every replica with idle
agents attempts a claim for them. A sweep every 5 seconds does the same, so a lost notification
costs at most a few seconds, and runs the lease expiry and the queue timeout below.

An assignment whose agent did not answer `Accept` before `lease_until` returns to the queue. The
lease expiry finds such executions under its advisory lock and returns each in a transaction of its
own under its run's lock, after checking again that it is still `assigned` and past its lease: the
execution completes as `cancelled` with the reason `lease_expired`, the job goes back to `queued`
with its `queued_at` unchanged, so it keeps its place, and a `cancel` control message goes to the
agent in case it starts the job after all. A `Reject` does the same at once with the reason
`rejected_by_agent`, without the control message. The queue timeout finds, under its own advisory
lock, the jobs `queued` since longer than `limits.max_queue_time` and cancels each in a transaction
of its own under its run's lock, after checking again, with the reason `no_agent`
([04-runs.md](04-runs.md#dispatch)); the planner then takes the run on.

## Notifications between replicas

| Channel | Payload | Listener |
|---------|---------|----------|
| `bosun_queue` | job id | replicas with idle agents |
| `bosun_replica_<id>` | agent id and control message id, separated by a space; or the agent id alone, when the agent was drained, its drain was lifted or it was deleted | the replica holding that agent's stream |
| `bosun_events` | `run` and the run's id, or `agent` and the agent's id, separated by a space | replicas with subscribers on the event stream |
| `bosun_log` | execution id and the sequence number of the chunk stored, separated by a space | replicas with followers of that log |
| `bosun_webhook` | the id of a stored webhook delivery | the webhook worker of every replica with a GitHub App |
| `bosun_outbox` | the id of an outbox row | the outbox worker of every replica with a GitHub App |

Payloads carry identifiers only; the data is read from the tables. A listener that reconnects reads
everything newer than the last sequence number it saw, so a dropped connection loses nothing; for
`bosun_replica_<id>` that is every unacknowledged control message of the agents the replica holds,
and the drain or deletion of each of them, and for `bosun_events` every stream reads after its position. Every journal event and every stream
event announces itself in the statement that inserts it, and since Postgres delivers a payload once
per transaction however often it was sent, a transaction announces each run and each agent it wrote
events of once. The `<id>` of a replica is a UUIDv7 it draws at start, written as 32 hexadecimal
digits, and the `listening` line of its log names it. A replica listens on one connection of its
own, outside its pool.

## Periodic tasks

Periodic tasks run in every replica: the cron scheduler (minute resolution), lease expiry, offline
detection of agents together with the end of their grace period and the open executions of deleted
agents, the queue timeout, log compaction, retention, cache eviction, the removal of idempotency
keys older than 24 hours, the expiry of tokens, and the removal of ended sessions together with
forgotten failed sign-ins and the used states of sign-ins through identity providers that have
ended ([08-access.md](08-access.md#local-sign-in), [OpenID Connect](08-access.md#openid-connect)). Each takes a Postgres advisory lock first, so exactly one replica at a time finds the
work of each task, and each change it finds checks its condition again under the lock of what it
changes. Outbox delivery is not a singleton: workers in every replica claim rows with
`SKIP LOCKED`, and so does the processing of webhook deliveries
([06-runtime-services.md](06-runtime-services.md#deliveries)).

Each task has a lock of its own, a transaction-level `pg_try_advisory_xact_lock` whose key spells
the task in eight bytes (`bosunagt` for the sweep of agents, `bosunlse` for the lease expiry,
`bosunqto` for the queue timeout, `bosunlog` for log compaction, `bosunses` for the removal of
sessions). A replica that finds the lock taken skips the round. The transaction that holds it only
finds the rows to change; each change then runs in a transaction of its own under the lock of its
run, or, for log compaction, which changes no run, under an advisory lock of its execution and then
its execution's row, and checks its condition again, so a change never waits for a lock while holding
the task's, and a round that overlaps another changes nothing twice. The removal of sessions changes
no run and decides by time alone, so it deletes in the transaction that holds its lock: a session
that ended stays ended, a count of failed sign-ins whose last failure lies a day back and that
holds no lock any more is forgotten whether or not the task removes it, and a used state whose
sign-in has ended is refused by the cookie's own time before its row matters.

## The outbox

Side effects that leave the system — status reports to the forge, notifications, outgoing webhooks,
events for plugins — are rows in `outbox`, written in the same transaction as the journal event
that causes them: kind, a collapse key, payload, number of attempts, the time of the next attempt,
the last error. A worker claims a row, delivers it, and deletes it. A failure is retried with
exponential backoff from 1 second to 1 hour; after 24 hours the row is marked dead and shown in the
administration pages. When a newer row with the same collapse key exists, an older one is dropped
without delivery, so a job that changes state five times while the forge is down is reported once,
with its latest state.

Step 28 of M1 ([../plan.md](../plan.md)) settled how. In M1 every row is a report to the forge
([06-runtime-services.md](06-runtime-services.md#reporting-to-the-forge)), of kind `check_run` or
`commit_status`, and `journal::append` writes it right after the event that causes it, so no code
path that changes a run can forget it. The collapse key is the kind and the subject of the report,
`check_run/job/<job>` or `commit_status/run/<run>`; since the rows of one job are written under its
run's lock, the order of their ids, drawn from a sequence, is the order of their commits, as for the
journal ([The journal](#the-journal)). Each row inserts with a notification on `bosun_outbox`.

Every replica with a GitHub App runs one outbox worker, woken by `bosun_outbox` and otherwise every
second, which is the resolution of the backoff. One delivery is two short transactions, the claim
and the result, with the requests to the forge between them and outside any transaction, so a slow
forge neither holds a database connection nor delays the event stream's horizon:

1. **The claim** takes, with `FOR UPDATE SKIP LOCKED`, the row due first (`next_attempt_at` at or
   before now on the server's clock, which tests inject, then the lowest id) among those that are
   the oldest row of their collapse key not marked dead. Only that row of a key can ever be
   claimed, so the rows of one key are delivered one after another, by one worker at a time,
   whichever replicas run. In the same transaction the worker takes the newest row of the key,
   deletes the older ones, the claimed one included, without delivering them, and leases the
   newest: it moves its `next_attempt_at` 5 minutes ahead, counts
   the attempt, and carries over the attempts, `failing_since` and `last_error` of the oldest, so
   the backoff and the deadline of a report do not restart when the job moves on. A newer row that
   arrives meanwhile waits behind the leased one.
2. **The delivery** runs with a timeout of 2 minutes, well inside the lease, so a row is claimed
   again only when its worker died, hung or could not write the result, and a delivery repeats
   only then.
3. **The result** is written only while the row still holds the lease the claim gave it, which is
   how a worker that overran its lease leaves the row to the claim that took it over:
   - delivered: the row is deleted, with the dead rows of its key that it supersedes, and the id of
     a check run it created is recorded in `forge_check_run`; the worker logs
     `delivered an outbox row` with `row`, `kind`, `key`, the number of rows it `dropped` and
     `duration_ms`;
   - a failure that may pass (the database, the forge unreachable, a `5xx`, a rate limit, the
     timeout): `failing_since` keeps the time of the first failure and the row waits
     1 second after its first attempt, twice as long after each further one, up to an hour, and at
     least until a rate limit resets, with a warning that names it. A failure 24 hours or more
     after `failing_since` marks the row dead instead;
   - a failure that cannot pass (a `4xx` that is no rate limit, a payload this version cannot read,
     a repository that no installation of the App sees any more) marks the row dead at once.

A dead row keeps its `dead_at` and `last_error`, is never claimed again, and no longer blocks its
key: a newer row of the key is delivered and deletes it. Until the administration page of M3
(step 32 of M3) lists, retries and discards dead rows, each one is logged at error level as
`an outbox row is dead and is no longer tried`, with its `row`, `kind`, `key`, `attempts` and
`error`, and `/metrics` counts the rows whose `dead_at` is set (step 39 of M1). Rows of a kind the
replica does not know, written by a newer version during a rolling update, are left to the replicas
that know it.

## The object store

`bosun-blob` defines streaming put with multipart upload, ranged get, delete and listing by prefix,
with two backends: S3-compatible storage (path-style addressing configurable) and a local
directory. The directory backend serves replicas on one host only, such as the end-to-end tests;
replicas on several hosts would split the data between them.

| Prefix | Content |
|--------|---------|
| `logs/<run>/<attempt>/<execution>/<chunks>/<step>.log.zst` | compacted step logs |
| `summaries/<execution>/<step>.md` | step summaries |
| `artifacts/<project>/<artifact>` | finalised artifacts |
| `blocks/<upload>/<block>` | staged blocks of an upload in progress |
| `caches/<project>/<entry>` | cache entries |
| `actions/<owner>/<repo>/<commit>.tar.gz` | downloaded actions |
| `tools/<tool>/<version>/<platform>.tar.xz` | Node runtimes and other tools |
| `terminals/<session>.cast` | recorded debug terminal sessions |
| `probes/start`, `probes/readiness` | an empty object a server writes and deletes as it starts, and a key that never exists, which `/readyz` reads ([12-operations.md](12-operations.md)) |

A key is a path of segments separated by `/`: at most 1024 bytes of UTF-8 without control
characters, every segment non-empty, neither `.` nor `..`, and at most 255 bytes. Every backend
follows the same rules, which one conformance suite in `bosun-blob` pins for each of them:

- a put streams its body and replaces the object whole: until it completes, readers see the
  earlier object or none, and a put that fails or is abandoned halfway leaves nothing that a get
  or a listing sees, and the earlier object in place; once it has failed, or soon after it was
  abandoned, nothing of it is left behind either, neither a staged file nor a multipart upload in
  progress;
- a ranged get returns the bytes of the range, cut at the end of the object, together with the
  object's size; a range that starts past the end, or ends before it starts, is refused;
- deleting an object that does not exist succeeds;
- a listing returns, in byte order, every key that begins with the prefix as a string, so
  `logs/r1` also finds `logs/r10/…`.

The directory backend keeps each object as a file under `objects/` in its root. A put writes the
body to a file it creates exclusively under `staging/`, syncs it, renames it into place and syncs
the directory, so an object appears whole or not at all, also to another process on the same
root, and two processes never write into each other's staged files. A crash leaves at most a file
in `staging/`, which is never an object. A delete removes the directories it leaves empty. Because
a file cannot also be a directory, the directory backend refuses a key below an object, such as
`a/b/c` while `a/b` exists, and a key above objects, such as `a/b` while `a/b/c` exists; the
layout above never produces either.

The S3 backend speaks the S3 REST API over HTTP/1.1 and signs every request with Signature
Version 4 in the `Authorization` header, the SHA-256 of the payload included. The client is Bosun's
own, a few hundred lines over reqwest, rather than an SDK: the AWS SDK brings a large dependency
tree for nine requests, and `object_store` lists by path segments rather than by string prefix,
which breaks the listing rule above. The backend names the bucket in the host
(`<bucket>.<endpoint host>`) by default, or in the path with `path_style`, which an endpoint given
as an IP address needs. A put reads its body in parts of 8 MiB. A body that ends within the first
part is one `PutObject`; a longer one becomes a multipart upload of 8 MiB parts, at most 10,000 of
them (about 78 GiB), which `CompleteMultipartUpload` turns into the object once the body ends, so
memory holds one part per put in flight. A put that fails aborts its upload before it returns the
error; a put that is abandoned, its future dropped, aborts it from a task it spawns on the runtime,
so neither leaves an upload in progress. A replica that dies during an upload leaves one behind,
which is never an object, and the bucket's lifecycle rule `AbortIncompleteMultipartUpload` removes
it ([12-operations.md](12-operations.md#configuration)), as a crash leaves a file in `staging/` of
the directory backend.

A ranged get sends `Range` and takes the size of the object from `Content-Range`. A range that
`Range` cannot express, an empty one or one that ends before it starts, and a range the store
refuses with `416`, such as one that starts at the end of the object, ask `HEAD` for the size
instead, so the rules above hold for them too. A get tells `NoSuchKey`, which is "not found", from
`NoSuchBucket` and every other answer, which are errors, so a deleted bucket makes `/readyz`
unready rather than ready. A listing pages through `ListObjectsV2` with the prefix, 1000 keys a
page, and sorts the keys, since S3-compatible servers do not all list in byte order; a key that
breaks the rules above is left out. Unlike the directory backend, S3 keeps `a/b` and `a/b/c` side
by side. The backend does not retry: a request that fails fails the operation, and its caller tries
again as it would with the directory backend, as the next sweep compacts a log again. A request
fails after 10 seconds without a connection or 60 seconds without a byte. The credentials are a
static access key, from the configuration or from the environment
([12-operations.md](12-operations.md#configuration)); session tokens, instance roles and web
identities are not supported.

Downloads of artifacts and caches are streamed by the server from the store, so the store needs
no public address and the signed addresses jobs use are Bosun's own.

## Logs

While an execution runs, its log lives in `log_chunk`, keyed by execution and sequence number and
inserted with `ON CONFLICT DO NOTHING`, so a chunk the agent sends twice is stored once. A chunk holds
whole lines, each prefixed with an RFC 3339 timestamp with nanoseconds; the text is the raw output
with its ANSI sequences, already masked by the agent. Lines longer than 64 KiB are split. A chunk
whose last line lacks its newline is given one, so the lines of a step are the same however its
chunks were cut.

**Ingestion.** The replica that holds the agent's stream stores each `Log` in a transaction of its
own that takes the row of the chunk's execution `FOR NO KEY UPDATE` and no other lock, which orders
the chunks of one execution against each other and against the compaction that ends them. The
chunk's step is the step of that index of the execution. A chunk of an execution another agent
holds, of an execution or a step that does not exist, or of an execution whose log is already
compacted, is not stored, and is logged at warning level. Every chunk the server handled, stored or
not, is acknowledged after its transaction ended with a `LogAck` carrying its own sequence
number; a stream carries the chunks of an execution in order, so every chunk below it was handled
too. When the database fails, the server does not acknowledge the chunk and ends the stream with
`UNAVAILABLE`, so the agent sends it again after reconnecting. A stored chunk is announced with
`NOTIFY bosun_log` in the transaction that stores it. A sequence number of 0, or one above the
largest `bigint`, is a breach of the protocol and ends the stream.

**The limit.** An execution's log is limited to 128 MiB, and the server enforces the limit as it
ingests chunks, because the agent sends every line. `execution.log_bytes` counts the bytes of every
chunk recorded, stored or dropped. A chunk that would take it past the limit is acknowledged without
being stored: it is recorded as a row of `log_chunk` without data and with its size in `dropped`,
and counted, so `log_bytes` is past the limit from then on, every chunk after it is dropped too and
the stored log stays a prefix of the output. The first dropped chunk of an execution is therefore
the one that finds `log_bytes` within the limit, also when the stored chunks fill the limit exactly,
and the server logs at warning level that the log reached its limit then. A dropped chunk that the
agent sends again conflicts like any other and is counted once. Every step whose
chunks were dropped ends with the line `<completed_at> <n> bytes of output were dropped because the
log of the execution reached its limit of 128 MiB`, dated when the step completed, so the log of
the completed execution ends with such a notice. Readers add the line once the step is completed,
and the compactor writes it into the object, so it reads the same before and after compaction.

**Reading.** The lines of a step's log are numbered from 1, in the order of its chunks. A reader
takes, in one statement, whether the execution's log is compacted, whether the step is completed and
the next 64 chunks of the step after the last one it read, so it sees the log either as chunks or as
objects and never as half of each. Both hold the same lines under the same numbers, so a reader that
was reading chunks when the log was compacted goes on in the object at the line it reached.

**Following.** A follower subscribes to `bosun_log` before its first read and reads from its offset.
When it has read every stored chunk, it waits for an announcement of a chunk of its execution, for
the notification connection to be re-established, or for a second to pass, which is how it learns
that its step completed, and reads again. It ends once its step is completed and it has delivered
every line stored by then, the notice included. Each replica hears every announcement on its one
listening connection and wakes only the followers of that execution.

**Compaction.** A sweep every 5 seconds takes the advisory lock `bosunlog`, finds up to 100
executions that completed at least 10 seconds ago and whose `log_compacted_at` is not set, through a
partial index on `execution (completed_at)`, and compacts each one. The delay leaves room for chunks
an agent sends right after it reconnects, such as the spooled chunks of an execution the server
completed as lost because the agent came back without it.

The compaction of an execution is one transaction. It begins with `pg_try_advisory_xact_lock` on a
key made of the last eight bytes of the execution's id, which the variant bits of a UUID make
negative, so it never meets the key of a periodic task. A compactor that finds the lock taken skips
the execution, since another replica's sweep found it too, so one compactor at a time works on an
execution and only the holder of the lock can record its objects. Under the lock it checks again
that the execution completed and that its log is not compacted, then reads the chunks in pages of 256
through a cursor, which sees them as they were when it was declared, so the pages never mix two
states of the log. It writes the log of each step that has lines to
`logs/<run>/<attempt>/<execution>/<chunks>/<step>.log.zst`, the segments being the ids of the four
and the number of chunks read, as one zstd stream made of one frame per 1000 lines. The chunks of an
execution only grow in number until its compaction deletes them, so that number names the chunks an
object was made from: a compactor that lost its connection, and with it the lock, and still writes,
never replaces an object recorded from other chunks. Last, in the same transaction, it takes the
execution's row, counts its chunks again, records each object in `log_object` with its size, its
number of lines and the byte offset of every frame in `frames`, deletes the chunks and sets
`log_compacted_at`. When the count changed, a chunk arrived while it worked; it records nothing,
deletes the objects it wrote, and the next sweep compacts the execution again. The transaction takes
no row lock before that last step, so chunks go on being ingested while the compactor reads and
writes. A chunk that arrives after its execution's log was compacted is acknowledged and not
stored.

The server serves a compacted log by range, decompressing only the frames that hold the lines asked
for, and searches it by streaming decompression for the API and MCP.

## Retention

Project settings, each capped by an installation maximum:

| What | Default |
|------|---------|
| logs and step summaries | 90 days |
| artifacts | 90 days, or `retention-days` of the upload |
| caches | 7 days without a read; 10 GiB per project, least recently used first |
| terminal recordings | 30 days |
| inbound webhook payloads | 30 days |
| audit events | 1 year |
| runs, attempts, jobs, steps and the journal | kept, unless `run_retention` is set, which deletes runs older than it with everything they own |
| drafts | until deleted |

Besides these, retention deletes the `agent.status` events of the event stream that are older than
7 days ([The journal](#the-journal)); they belong to no project, so no setting applies to them.

## Secrets at rest

Every secret value is encrypted with AES-256-GCM under its own random data key; the data key is
encrypted with the key-encryption key (KEK) and stored next to the value. The associated data binds
the ciphertext to the secret's scope and name, so a ciphertext copied onto another secret fails to
decrypt. Step 24 of [../plan.md](../plan.md) settled the details:

- **The data key** is 32 random bytes drawn for each value written, so replacing a secret's value
  replaces its data key too. The server holds it only while it seals or opens that value.
- **The layout.** `ciphertext` is a 12-byte random nonce followed by the AES-256-GCM encryption of
  the value's UTF-8 bytes under the data key with its 16-byte tag; `data_key` is a nonce followed by
  the encryption of the data key under the KEK with its tag. The library is `ring`, which the forge
  client already uses for the App's key.
- **The binding.** The associated data of both encryptions is a label (`bosun secret value` or
  `bosun secret data key`, each ended by a zero byte) followed by the 16 bytes of the organisation's
  id, the 16 bytes of the project's id or 16 zero bytes for a secret of the organisation, and the
  name in upper case. Copying a row's `ciphertext`, `data_key` and `kek_id` onto another secret, of
  another name or scope, makes both decryptions fail, and so does any change to them.
- **The KEK's id** is the first 16 hexadecimal digits of the SHA-256 of `bosun kek id`, a zero byte
  and the 32 bytes of the KEK. It names a KEK without revealing it, needs no setting of its own, and
  is the same wherever the same KEK is configured; `kek_id` records it for each data key.
- **Reading.** A secret's value is decrypted only for the assignment of a job that reads it
  ([05-execution.md](05-execution.md#the-job-assignment)); the server never answers with a value
  and never logs one. A data key under a KEK the configuration does not hold, or a ciphertext that
  does not decrypt, fails the job at its assignment with the notice `secret_unreadable`
  ([04-runs.md](04-runs.md#run-notices)). The KEK is 32 bytes from the server's configuration, written as 64 hexadecimal digits in
a file (`keys.kek_file`) or in the environment variable `BOSUN_KEYS_KEK`, never stored in the
database. When the configuration names none, the server uses `keys/kek` in its data directory and,
on the first start, generates it there from the operating system's random source, with mode
`0600`, and logs at warning level that it did and that the file must be backed up; this is what lets
a single host start with nothing but a database address ([12-operations.md](12-operations.md)). The configuration holds the current KEK and earlier
ones, `keys.previous_kek_files`, each known by its id; a data key is opened with the KEK its
`kek_id` names, and every new one is sealed under the current KEK. `bosun-server secrets rewrap`
brings the schema up to date and re-encrypts, in pages of 100, every data key whose `kek_id` is not
the current KEK's, leaving the values themselves as they are. Each update is conditioned on the
data key it read, so a value that a server sets meanwhile is not overwritten; such a secret, and one
whose KEK the configuration does not hold, is named in the error with which the command ends, after
it re-encrypted all the others, and running it again finishes the work. It never generates a KEK:
without a configured one it uses `keys/kek` of the data directory, and refuses when that does not
exist. Rotating the KEK is therefore three moves: configure the new KEK as `keys.kek_file` and the
old one in `keys.previous_kek_files` on every replica, run `bosun-server secrets rewrap` once, and
remove the old KEK from the configuration.

Further key material:

- the private key of the GitHub App comes from the configuration;
- the Ed25519 key that signs job tokens is generated by the server, stored encrypted under the
  KEK, and rotated; earlier public keys are kept until the last token they signed has expired;
- the HMAC key of signed addresses is derived from the KEK with HKDF and a fixed label.

Losing the KEK loses every secret, and the backup instructions say so
([12-operations.md](12-operations.md)). The API accepts secret values and never returns them.

## Tests

Tests run against Postgres only, on the server that `BOSUN_TEST_DATABASE_URL` names; there is no
in-memory substitute. `bosun-testkit` gives each test a database of its own:
`TestDatabase::migrated()` creates it with `CREATE DATABASE … TEMPLATE` from a template that the
test binary migrates when a test first asks for one, and `TestDatabase::empty()` creates one with
nothing in it. Dropping the handle drops the database, and the template is dropped with the last
database made from it, so a test binary that ends leaves nothing on the server. While tests overlap
the template is migrated once; tests that run strictly one after another rebuild it each time.

Several test processes, from one checkout or from several, share one server at the same time.
Each process draws an owner, a UUIDv7, at start, names what it creates `bosun_test_<owner>_<n>`
and `bosun_test_<owner>_template`, and holds a session with the `application_name`
`bosun_test_<owner>` for as long as it runs, opened before it creates anything. Before its first
database, a process lists the databases and then the sessions, and drops every database of that
pattern whose owner has no session: what a crashed or killed process left. Names outside the
pattern are never touched.

The S3 backend is tested against RustFS (`rustfs/rustfs:1.0.1`), an S3-compatible server under the
Apache 2.0 licence, on the server that `BOSUN_TEST_S3_URL` names, with its access key and secret as
the user and password of the address, such as `http://bosun:bosun-secret@127.0.0.1:9000`. RustFS
keeps keys as S3 does, so `a/b` and `a/b/c` coexist and a key is never a directory; it verifies
Signature Version 4, lists the multipart uploads in progress, and starts from environment variables
alone, which lets CI run it as a service container next to Postgres. MinIO no longer publishes its
images, Garage needs a layout and keys created by commands before it serves, and Versity's gateway
keeps objects as files, with the directory backend's limits. `TestBucket::create()` in
`bosun-testkit` gives a test a bucket of its own, `bosun-test-<owner>-<n>` with an owner drawn per
process as for databases, and dropping the handle deletes its objects and the bucket. A bucket that
a killed process left stays until the container is replaced: started with `--rm` and no volume,
RustFS keeps its data in the container, and a new container starts empty. CI runs the server as a service of the job; on a developer's
machine it runs in a container, started as `CLAUDE.md` shows, and one server serves every checkout
and test process at once.
