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
The migrations seed the trusted pool `default` ([05-execution.md](05-execution.md)). The future of
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
| identity | `user`, `user_identity`, `team`, `team_member`, `role`, `role_binding`, `api_token`, `session` |
| tenancy | `organisation` (with its settings, among them the action policy), `forge_installation`, `project`, `environment`, `environment_rule`, `secret`, `variable` |
| workflows | `workflow`, `workflow_revision`, `workflow_draft`, `schedule`, `upstream_subscription` |
| runs | `run`, `attempt`, `job`, `execution`, `step`, `run_event`, `approval`, `annotation`, `step_summary`, `terminal_session` |
| agents | `pool`, `pool_access`, `agent`, `registration_token`, `control_message`, `label_alias` |
| logs | `log_chunk`, `log_object` |
| artifacts and caches | `artifact`, `artifact_block`, `cache_entry` |
| actions and tools | `action_ref`, `tool_object` |
| delivery | `webhook_delivery`, `outbox`, `webhook_subscription`, `idempotency_key` |
| plugins | `plugin`, `plugin_instance`, `plugin_kv` (from M5) |
| audit | `audit_event` |
| schema | `schema_version`, next to sqlmig's ledger `migrations` and `migrations_checksum` |

The central columns:

```
workflow     id, project_id, name, source_kind, enabled, revision_id, last_run_number, created_at
run          id, project_id, workflow_id, revision_id, ir jsonb, number, global_number,
             event, trigger jsonb, ref, sha, inputs jsonb, draft_id, derived_from,
             status, conclusion, cancel_requested_at, created_at, started_at, completed_at
attempt      id, run_id, number, reason, actor jsonb, created_at
job          id, attempt_id, key, name, group_id, matrix jsonb, needs text[], kind,
             status, waiting_reason, conclusion, conclusion_source, outputs jsonb,
             priority, labels text[], pool_ids uuid[], pinned_agent_id, carried_from,
             queued_at, started_at, completed_at
execution    id, job_id, number, agent_id, lease_until, status, conclusion, reason, started_at,
             completed_at, log_bytes, log_compacted_at
step         id, execution_id, parent_step_id, index, name, status, outcome, conclusion,
             conclusion_source, outputs jsonb, started_at, completed_at
log_chunk    execution_id, seq, step_id, data bytea, dropped
log_object   step_id, execution_id, key, size, lines, frames bigint[], created_at
run_event    seq bigserial, run_id, attempt_id, job_id, execution_id, step_id, kind,
             actor jsonb, data jsonb, at
```

A run's `trigger` holds the actor and the payload of the event, `{"actor": …, "payload": …}`; for
`workflow_dispatch` the payload is `{"inputs": …}`. A run's `cancel_requested_at` is the time an
accepted `run.cancel` reached it. A job is created with its key, display name and `needs`; its
`labels` and `pool_ids` are written when the planner evaluates `runs-on` as the job becomes ready
([04-runs.md](04-runs.md)). `labels` are lowercased; `pool_ids` are the pools the job may run in:
the pool `runs-on.group` names, none when no pool has that name, and without a group every pool
open to the project, which until pool access lists arrive in M1 is every pool.

A milestone creates the columns it uses. The columns above that M0 does not use (`draft_id` and
`derived_from` of runs, `reason` of attempts, `group_id`, `matrix`, `kind`, `outputs`,
`pinned_agent_id` and `carried_from` of jobs, `parent_step_id` and `outputs` of steps) arrive with
their milestones as additive migrations. The indexes of the queue arrived with the claim (step 11
of [../plan.md](../plan.md)), as did the partial index on `execution (lease_until) where status =
'assigned'` that the lease expiry reads. The partial index on `execution (completed_at) where status =
'completed' and log_compacted_at is null` that the compaction of logs reads arrived with the logs
(step 13).

An agent records its current replica in `agent.replica_id`, which no row references: replicas are
processes, not rows. `control_message` holds the control messages sent to agents
([05-execution.md](05-execution.md)): `id, execution_id, kind, created_at, acknowledged_at,
took_effect`; its agent is the agent of its execution.

Statuses, conclusions, reasons, trust levels, capabilities and event kinds are `text` holding the
names `bosun-types` serialises them to, with neither Postgres enum types nor check constraints, so
a later milestone adds a value without altering a type. Hashes of credentials, of workflow
sources and of idempotent requests are SHA-256 in `bytea`.

A row that another row owns references its owner with `ON DELETE CASCADE`: a project its
organisation, a workflow its project, a revision its workflow, a run its project and its workflow,
an attempt its run, a job its attempt, an execution its job, a step its execution, a journal event
its run, a log chunk its execution, a log object its step, a control message its execution, a
personal token its user and a registration token its pool. Deleting a run under `run_retention`
therefore removes everything it owns in one statement. The other references inside one run (the
attempt, job, execution and step of a journal event, the step of a log chunk, the execution of a
log object) carry no foreign key, because the owner already bounds them and a cascade would have
to check each of them.
References to rows that are not owners (the current revision of a workflow, the revision of a
run, the pool of an agent, the agent of an execution) are plain foreign keys, which refuse to delete what is still referenced. Every
foreign key column leads an index, which a test asserts.

`idempotency_key` keeps, for 24 hours, the principal (`user:<id>`), the key, a hash of the request
and the status and body of the response of every write that created something with an
`Idempotency-Key` ([11-api.md](11-api.md)). The row is written in the transaction of the write; a
row older than 24 hours is ignored and replaced when its key comes again. `label_alias`
maps a label such as `ubuntu-latest` to a pool, a capability and a default image, per installation
or per organisation.

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
the event stream filtered by project ([11-api.md](11-api.md)), arrives with a cursor of its own:
each event records its transaction (`pg_current_xact_id()`), and the stream reads only the events of
transactions older than the oldest one still running (`pg_snapshot_xmin(pg_current_snapshot())`),
in the order of transaction and sequence number, resuming after the pair it last delivered. Every
transaction below that horizon has ended, so nothing appears behind the cursor; a long transaction
delays the stream by its length. The column arrives with the stream.

## The queue

The queue is the set of `job` rows in status `queued`. A partial index on
`(priority desc, queued_at) where status = 'queued'` orders them and a GIN index on `labels`, partial
on the same condition, filters them; labels are stored lowercased.

An idle agent claims a job in one transaction: select the candidates whose labels are contained
in the agent's labels, whose pools include the agent's pool, and whose trust matches the pool's
(jobs of untrusted runs only in untrusted pools, jobs of trusted runs only in trusted pools),
order them by priority, by the
project's ratio of running jobs to its weight, and by `queued_at`, take the first with
`FOR UPDATE SKIP LOCKED`, together with the lock of its run ([The journal](#the-journal)), create
its execution with `lease_until` 30 seconds ahead (`agents.assignment_lease`), and set the job
to `assigned`. Two replicas claiming for two agents at the same moment never take the same job and
never wait on each other. The trust condition arrives with trust levels in M1 and the ratio with
fair share in M2; until then the order is priority, then `queued_at`, then the job's id.

An agent is idle from the `Idle` it sends until the replica that holds its stream sends it an
assignment ([05-execution.md](05-execution.md)). That replica keeps the state in memory, for its
own streams only, and claims for one agent at a time: a reason to claim that arrives while a claim
for the agent runs makes it try once more when that claim finds nothing. The claim itself checks the
database too: it takes the agent's row `FOR NO KEY UPDATE`, which orders two claims for one agent,
and takes nothing for an agent whose current replica is another one or that still has an execution
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
| `bosun_replica_<id>` | agent id and control message id, separated by a space | the replica holding that agent's stream |
| `bosun_events` | run id and event sequence number | replicas with subscribers on the event stream |
| `bosun_log` | execution id and the sequence number of the chunk stored, separated by a space | replicas with followers of that log |

Payloads carry identifiers only; the data is read from the tables. A listener that reconnects
reads everything newer than the last sequence number it saw, so a dropped connection loses nothing;
for `bosun_replica_<id>` that is every unacknowledged control message of the agents the replica
holds. The `<id>` of a replica is a UUIDv7 it draws at start, written as 32 hexadecimal digits,
and the `listening` line of its log names it. A replica listens on one connection of its own,
outside its pool.

## Periodic tasks

Periodic tasks run in every replica: the cron scheduler (minute resolution), lease expiry, offline
detection of agents together with the end of their grace period, the queue timeout, log compaction,
retention, cache eviction, the removal of idempotency keys older than 24 hours, and the expiry of
tokens and sessions. Each takes a Postgres advisory lock first, so exactly one replica at a time finds the
work of each task, and each change it finds checks its condition again under the lock of what it
changes. Outbox delivery is not a singleton: workers in every replica claim rows with
`SKIP LOCKED`.

Each task has a lock of its own, a transaction-level `pg_try_advisory_xact_lock` whose key spells
the task in eight bytes (`bosunagt` for the sweep of agents, `bosunlse` for the lease expiry,
`bosunqto` for the queue timeout, `bosunlog` for log compaction). A replica that finds the lock
taken skips the round. The transaction that holds it only finds the rows to change; each change then
runs in a transaction of its own under the lock of its run, or, for log compaction, which changes no
run, under an advisory lock of its execution and then its execution's row, and checks its condition
again, so a change never waits for a lock while holding the task's, and a round that overlaps another
changes nothing twice.

## The outbox

Side effects that leave the system — status reports to the forge, notifications, outgoing webhooks,
events for plugins — are rows in `outbox`, written in the same transaction as the journal event
that causes them: kind, a collapse key, payload, number of attempts, the time of the next attempt,
the last error. A worker claims a row, delivers it, and deletes it. A failure is retried with
exponential backoff from 1 second to 1 hour; after 24 hours the row is marked dead and shown in the
administration pages. When a newer row with the same collapse key exists, an older one is dropped
without delivery, so a job that changes state five times while the forge is down is reported once,
with its latest state.

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

## Secrets at rest

Every secret value is encrypted with AES-256-GCM under its own random data key; the data key is
encrypted with the key-encryption key (KEK) and stored next to the value. The associated data binds
the ciphertext to the secret's scope and name, so a ciphertext copied onto another secret fails to
decrypt. The KEK is 32 bytes from the server's configuration, written as 64 hexadecimal digits in
a file (`keys.kek_file`) or in the environment variable `BOSUN_KEYS_KEK`, never stored in the
database. When the configuration names none, the server uses `keys/kek` in its data directory and,
on the first start, generates it there from the operating system's random source, with mode
`0600`, and logs at warning level that it did and that the file must be backed up; this is what lets
a single host start with nothing but a database address ([12-operations.md](12-operations.md)). The configuration can hold the current KEK and earlier
ones, each with an id; `bosun-server secrets rewrap` re-encrypts every data key under the current
one.

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
