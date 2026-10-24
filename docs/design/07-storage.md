# Storage

This document specifies what Bosun keeps and where: the Postgres schema in outline, migrations and
identifiers, the job queue, notifications between replicas, periodic tasks, the outbox, the object
store, the log pipeline, retention, and secrets at rest.

## Postgres

One database `bosun` with one schema `bosun`, Postgres 15 or newer. Entities are sea-orm models
over sqlx; the queue, the notifications and the hot log path use sqlx directly, because their
queries depend on `FOR UPDATE SKIP LOCKED`, `LISTEN` and `NOTIFY`.

**Migrations** are SQL files applied by sqlmig with its ledger. Inside a major version they are
additive only: new tables, new nullable columns or columns with defaults, new indexes created
concurrently. `bosun-server migrate` applies them; `bosun-server serve` applies them at start under
an advisory lock, so several replicas starting at once apply them once. A server starts against a
schema that a newer version of the same major migrated, because additive migrations leave
everything it uses in place, which is what makes rolling updates and rollbacks safe; it refuses to
start against a schema of a newer major version.

The files are `crates/db/migrations/<milliseconds>-<slug>.up.sql`, embedded in the binary. There
are no down files, and a test refuses `DROP`, `ALTER … DROP` and `ALTER … TYPE` in any of them.
`bosun-db` creates the schema `bosun` before the first migration and sets `search_path` to it on
every connection, so the ledger of sqlmig (`migrations` and `migrations_checksum`) lives there too.
The migrations seed the trusted pool `default` ([05-execution.md](05-execution.md)). The future of
sqlmig's `apply` is not `Send`, so the server awaits the migrations on the task that starts it
rather than spawning them.

**Identifiers** are UUIDv7, ordered by creation time. Two numbers exist for compatibility: a run's
number among the runs of its workflow (`github.run_number`) and a global number from a sequence
(`github.run_id`). Times are `timestamptz`.

### Tables

| Area | Tables |
|------|--------|
| identity | `user`, `user_identity`, `team`, `team_member`, `role`, `role_binding`, `api_token`, `session` |
| tenancy | `organisation` (with its settings, among them the action policy), `forge_installation`, `project`, `environment`, `environment_rule`, `secret`, `variable` |
| workflows | `workflow`, `workflow_revision`, `workflow_draft`, `schedule`, `upstream_subscription` |
| runs | `run`, `attempt`, `job`, `execution`, `step`, `run_event`, `approval`, `annotation`, `step_summary`, `terminal_session` |
| agents | `pool`, `pool_access`, `agent`, `registration_token`, `label_alias` |
| logs | `log_chunk`, `log_object` |
| artifacts and caches | `artifact`, `artifact_block`, `cache_entry` |
| actions and tools | `action_ref`, `tool_object` |
| delivery | `webhook_delivery`, `outbox`, `webhook_subscription`, `idempotency_key` |
| plugins | `plugin`, `plugin_instance`, `plugin_kv` (from M5) |
| audit | `audit_event` |

The central columns:

```
run          id, project_id, workflow_id, revision_id, ir jsonb, number, global_number,
             event, trigger jsonb, ref, sha, inputs jsonb, draft_id, derived_from,
             status, conclusion, created_at, started_at, completed_at
attempt      id, run_id, number, reason, actor jsonb, created_at
job          id, attempt_id, key, name, group_id, matrix jsonb, needs text[], kind,
             status, waiting_reason, conclusion, conclusion_source, outputs jsonb,
             priority, labels text[], pool_ids uuid[], pinned_agent_id, carried_from,
             queued_at, started_at, completed_at
execution    id, job_id, number, agent_id, lease_until, status, conclusion, reason, started_at,
             completed_at
step         id, execution_id, parent_step_id, index, name, status, outcome, conclusion,
             conclusion_source, outputs jsonb, started_at, completed_at
run_event    seq bigserial, run_id, attempt_id, job_id, execution_id, step_id, kind,
             actor jsonb, data jsonb, at
```

A milestone creates the columns it uses. The columns above that M0 does not use (`draft_id` and
`derived_from` of runs, `reason` of attempts, `group_id`, `matrix`, `kind`, `outputs`,
`pinned_agent_id` and `carried_from` of jobs, `parent_step_id` and `outputs` of steps) arrive with
their milestones as additive migrations, and the indexes of the queue arrive with the claim
(step 11 of [../plan.md](../plan.md)).

Statuses, conclusions, reasons, trust levels, capabilities and event kinds are `text` holding the
names `bosun-types` serialises them to, with neither Postgres enum types nor check constraints, so
a later milestone adds a value without altering a type. Hashes of credentials, of workflow
sources and of idempotent requests are SHA-256 in `bytea`.

A row that another row owns references its owner with `ON DELETE CASCADE`: a project its
organisation, a workflow its project, a revision its workflow, a run its project and its workflow,
an attempt its run, a job its attempt, an execution its job, a step its execution, a journal event
its run, a log chunk its execution, a log object its step, a personal token its user and a
registration token its pool. Deleting a run under `run_retention` therefore removes everything it
owns in one statement. The other references inside one run (the attempt, job, execution and step
of a journal event, the step of a log chunk, the execution of a log object) carry no foreign key,
because the owner already bounds them and a cascade would have to check each of them.
References to rows that are not owners (the current revision of a workflow, the revision of a
run, the pool of an agent, the agent of an execution) are plain foreign keys, which refuse to delete what is still referenced. Every
foreign key column leads an index, which a test asserts.

`idempotency_key` keeps, for 24 hours, the principal, the key, a hash of the request and the
response of every write that carried an `Idempotency-Key` ([11-api.md](11-api.md)). `label_alias`
maps a label such as `ubuntu-latest` to a pool, a capability and a default image, per installation
or per organisation.

## The queue

The queue is the set of `job` rows in status `queued`. A partial index on
`(priority desc, queued_at) where status = 'queued'` orders them and a GIN index on `labels`
filters them; labels are stored lowercased.

An idle agent claims a job in one transaction: select the candidates whose labels are contained
in the agent's labels, whose pools include the agent's pool, and whose trust matches the pool's
(jobs of untrusted runs only in untrusted pools, jobs of trusted runs only in trusted pools),
order them by priority, by the
project's ratio of running jobs to its weight, and by `queued_at`, take the first with
`FOR UPDATE SKIP LOCKED`, create its execution with `lease_until` 30 seconds ahead, and set the job
to `assigned`. Two replicas claiming for two agents at the same moment never take the same job and
never wait on each other.

A job that becomes `queued` is announced with `NOTIFY bosun_queue`; every replica with idle agents
attempts a claim for them. A sweep every 5 seconds does the same, so a lost notification costs at
most a few seconds.

## Notifications between replicas

| Channel | Payload | Listener |
|---------|---------|----------|
| `bosun_queue` | job id | replicas with idle agents |
| `bosun_replica_<id>` | agent id and control message id | the replica holding that agent's stream |
| `bosun_events` | run id and event sequence number | replicas with subscribers on the event stream |
| `bosun_log` | execution id and highest chunk sequence number | replicas with followers of that log |

Payloads carry identifiers only; the data is read from the tables. A listener that reconnects
reads everything newer than the last sequence number it saw, so a dropped connection loses nothing.

## Periodic tasks

Periodic tasks run in every replica but each takes a Postgres advisory lock first, so exactly one
replica performs each at a time: the cron scheduler (minute resolution), lease expiry, offline
detection of agents, the queue timeout, retention, cache eviction, and the expiry of tokens and
sessions. Outbox delivery is not a singleton: workers in every replica claim rows with
`SKIP LOCKED`.

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
| `logs/<run>/<attempt>/<execution>/<step>.log.zst` | compacted step logs |
| `summaries/<execution>/<step>.md` | step summaries |
| `artifacts/<project>/<artifact>` | finalised artifacts |
| `blocks/<upload>/<block>` | staged blocks of an upload in progress |
| `caches/<project>/<entry>` | cache entries |
| `actions/<owner>/<repo>/<commit>.tar.gz` | downloaded actions |
| `tools/<tool>/<version>/<platform>.tar.xz` | Node runtimes and other tools |
| `terminals/<session>.cast` | recorded debug terminal sessions |

A key is a path of segments separated by `/`: at most 1024 bytes of UTF-8 without control
characters, every segment non-empty, neither `.` nor `..`, and at most 255 bytes. Every backend
follows the same rules, which one conformance suite in `bosun-blob` pins for each of them:

- a put streams its body and replaces the object whole: until it completes, readers see the
  earlier object or none, and a put that fails or is abandoned halfway leaves nothing that a get
  or a listing sees, and the earlier object in place;
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

Downloads of artifacts and caches are streamed by the server from the store, so the store needs
no public address and the signed addresses jobs use are Bosun's own.

## Logs

While an execution runs, its log lives in `log_chunk`, keyed by execution and sequence number and
inserted with `ON CONFLICT DO NOTHING`, so a chunk the agent sends twice is stored once. A chunk
holds whole lines, each prefixed with an RFC 3339 timestamp with nanoseconds; the text is the raw
output with its ANSI sequences, already masked by the agent. Lines longer than 64 KiB are split.

A follower of a live log reads the stored chunks from its offset, then subscribes to `bosun_log`
and reads new chunks as they are announced. When the execution completes, a compactor writes each
step's log to the object store as one zstd stream with a line index every 1000 lines for ranged
reads, records it in `log_object`, and deletes the chunks. The server serves a compacted log by
range, and searches it by streaming decompression for the API and MCP.

An execution's log is limited to 128 MiB; beyond it the agent stops sending lines and the log ends
with a notice of how much was dropped.

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
decrypt. The KEK is 32 bytes from the server's configuration, given as a file or an environment
variable, never stored in the database. The configuration can hold the current KEK and earlier
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
