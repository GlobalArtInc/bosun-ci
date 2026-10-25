# Plan

This document orders the work: the milestones M0 to M5 with their scope and exit criteria, a
breakdown of M0 to M3 into steps of one change each, the working rules of the repository, and the
exit checklists of M0 to M3. The design lives in `design/`; this document only says in which order
it is built and how each step is shown to be done. Nothing here carries a date or a duration.

## Milestones

Milestones are strictly sequential and each ends in a state that can be demonstrated. Three
ordering rules follow from [design/13-decisions.md](design/13-decisions.md):

- the riskiest plumbing (an agent stream across replicas, the Postgres queue, logs that survive a
  reconnect) is proven end to end in M0, before any compatibility work depends on it;
- the first real payoff, the CI of Keel moving to Bosun, comes at M2, before run control, the
  editor and plugins;
- the compatibility corpus starts in M1 and grows with every milestone after it.

### M0 — Walking skeleton

Scope:

- The workspace, CI on the self-hosted runners, the dependency rules of
  [design/01-overview.md](design/01-overview.md).
- `bosun-types`, and `bosun-workflow` for a subset without expressions: `name`,
  `on: workflow_dispatch` without inputs, literal `env`, jobs with `name`, `runs-on`, `needs`, `env`
  and `steps`, steps with `id`, `name`, `run`, `shell`, `working-directory`, `env`.
- `bosun-db` with the tables M0 needs, `bosun-testkit`, `bosun-blob` with the directory backend.
- `bosun-server`: configuration, `serve` and `migrate`, health, the error envelope, personal tokens
  and the bootstrap owner, repository-less projects, server-side workflows, dispatch, the planner
  and the state machine with the journal, the queue with leases, the agent endpoint, log ingestion,
  following and compaction.
- `bosun-proto`, `bosun-runner` and `bosun-agent` with the `shell` capability.
- `bosun` with `login`, `workflow push`, `workflow dispatch --watch`, `run cancel`, `job logs`,
  `agent list`, `pool token create`.
- `hack/e2e.sh` running the M0 scenarios against real binaries, with two replicas on one host
  sharing the directory backend.
- The image `globalartltd/bosun` and the install on any Linux host of
  [design/12-operations.md](design/12-operations.md): the data directory with a generated KEK, the
  directory store and the first owner's token, `install-agent.sh` served by the server, compose and
  quadlet files, `docs/install/`.

Done when:

- a pushed workflow with two jobs, the second needing the first, runs on a registered agent after
  `bosun workflow dispatch --watch`, with its logs streamed live into the terminal;
- a failing step fails its job, the dependent job is skipped and the run fails;
- `bosun run cancel` stops a running `sleep 600` step within 10 seconds and the run ends cancelled;
- a restart of the server in the middle of a job loses no log line and duplicates none;
- two agents with different labels receive only the jobs their labels satisfy;
- with two server replicas on one database, a cancel issued through one replica reaches an agent
  connected to the other;
- the CI commands are green, including the WebAssembly build of the pure crates;
- the published image, started on a clean host with nothing but a Postgres address, takes an agent
  installed on another host with the one command the server prints, and runs the two-job workflow.

### M1 — GitHub and the job language

Scope:

- `bosun-expr` in full; `bosun-workflow` for the whole compatibility surface of
  [design/02-workflows.md](design/02-workflows.md), with execution of what is listed here and,
  for the rest, an error that names the milestone it arrives in. Its warnings reach the run page
  as run notices in the journal, and `POST /api/v1/lint` and `bosun workflow lint` show them
  outside it.
- The GitHub App: webhooks, installations, projects bound to repositories, workflow files read at
  the commit, the triggers `push`, `pull_request`, `pull_request_target`, `workflow_dispatch` with
  inputs, `schedule` with `timezone`, `repository_dispatch`, `merge_group`, generic events, path
  filters, the fork policy with `run.admit` for its approval mode, no runs from the App's own
  events, check runs and commit statuses through the outbox.
- Jobs: `if`, matrix expansion with `include`, `exclude`, `fail-fast` and `max-parallel`, outputs,
  `concurrency` with `queue`, `timeout-minutes`, `continue-on-error`, `defaults`, label aliases.
- Pools with trust levels and access lists, enforced where jobs are claimed; draining and deleting
  agents. Runs from forks are kept to untrusted pools and receive no secrets but a read-only forge
  token; since agents of untrusted pools hold no `shell`, admitted runs from forks execute from M2
  on, with `podman`.
- Steps: every context, file commands, workflow commands, annotations, step summaries, masking.
  The job token and the runtime service variables come with `bosun-services` in M2.
- Node actions with their pre and post steps: the action store on the server, Node runtimes for
  agents, the forge token, so that `actions/checkout` and other Node actions work.
- Secrets and variables at organisation and project scope, encrypted under the KEK.
- Sign-in with local passwords and OpenID Connect, sessions, roles and bindings, scoped tokens.
- The S3 backend; the event stream; log search; retention of logs, step summaries, webhook
  payloads and runs; `/metrics`, where dead outbox rows are counted until M3 gives them a page.
- `contracts/openapi.json` generated from the handlers and checked in CI.
- The CLI commands of these resources: tokens, pools and label aliases, draining and deleting
  agents, secrets and variables, project settings, projects, workflows and runs, `run admit`,
  dispatch inputs, log search and `job cancel`.
- The web interface: sign-in, projects, runs, the run page with the graph, live logs, annotations
  and summaries, the queue and agents page, secrets and variables. Its OwnLate project.
- The compatibility corpus: the inventory of every `uses:` and every key used in the workflows of
  both organisations, and the expression cases with results recorded on GitHub's hosted runners.

Done when:

- a push to a test repository runs a workflow with `actions/checkout`, a matrix and job outputs on
  the runner host's Bosun agent, and its pull request shows a check run per job with the right
  conclusions;
- the expression corpus passes and matches the recorded results;
- a secret printed raw, as JSON and as base64 appears masked in the log;
- a fork pull request in a project with the default policy creates no run;
- a viewer cannot dispatch and a developer cannot write a secret;
- Bosun's simple workflows run on Bosun next to GitHub Actions.

### M2 — The CI of Keel

Scope:

- The `podman` capability: job containers, service containers, container actions, the podman
  socket, cache volumes. The execution rules of untrusted pools, under which admitted runs from
  forks execute; the project setting `workspace: clean`.
- Composite actions, `uses: ./path`, `uses: docker://`.
- `bosun-services`: job tokens and the runtime service variables, the artifact service and both
  cache protocols with the block blob upload endpoint, and `cache-mode`; `bosun exec` on top of it.
- Reusable workflows, local and from other repositories.
- `priority`, fair share and project caps; `on.upstream` and `workflow_run`; the action policy;
  `run.rerun`, `run.rerun-failed` and `job.rerun`, also when requested from the forge, with the
  forge account link that maps the forge user to a Bosun user.
- The web interface for what M2 adds: attempts, re-runs, artifacts, nested steps and call groups on
  the run page; priorities and caps on the queue page; the action policy, the project settings,
  the pool settings and the forge account link in the settings.
- A load test of log ingestion and dispatch with many agents, its numbers recorded in
  `docs/`.

Done when:

- the CI of Keel runs unchanged from a copy under `.bosun/workflows/` with the same results as on
  GitHub Actions;
- a re-run requested from a check on the forge creates the next attempt, with the linked Bosun user
  as its actor;
- `actions/upload-artifact@v4` and `actions/download-artifact@v4` move a file between jobs,
  `actions/cache@v4` restores what it saved, and so does `actions/cache@v3.3.3`, a release from
  before the current cache protocol;
- a job with a Postgres service container passes its tests against it;
- Bosun's own CI runs on Bosun.

### M3 — Run control

Scope:

- Every remaining operator command of [design/04-runs.md](design/04-runs.md), from the API, the
  CLI and the run page, with the journal: its endpoint with the filter of interventions, and the
  timeline on the run page. The check run of a job an operator completed names the operator and
  the command.
- Gates with inputs, environments with protection rules, the approval inbox; dispatch inputs of
  type `environment` checked against the project's environments.
- `retry`, `pause` and breakpoints, `debug.hold-on-failure`, the debug terminal and its recording,
  debug re-runs, "run again with changes" with edited inputs and a ref.
- TOTP for local accounts; the audit trail, recorded from M3 on and shown in the interface; the
  settings pages for teams, roles, role bindings and personal tokens.
- Agent credential rotation, the address ranges of pools, and the administration page of dead
  outbox rows.
- The MCP endpoint.

Done when:

- a scenario test exists and passes for every command in the command table;
- an approver's gate input reaches the dependent job's step;
- a held job's terminal session is recorded, `hold.retry-step` after a manual fix lets the job
  succeed, and the run page shows who did what;
- an MCP client dispatches a run, reads a failing step's log with `grep`, pauses and resumes a job.

### M4 — The workflow editor

Scope:

- `@globalart/bosun-wasm`; the graph, the inspector and the YAML view on one document; drafts with
  autosave; running a draft, and running a run again with changes from a draft; saving as a commit
  or a pull request with a three-way merge, under the user's own authorisation of the App, whose
  token the forge account link stores from M4 on; server-side workflows with revisions and export;
  the action metadata endpoint.

Done when:

- for every workflow of the corpus, loading it and saving it without changes produces the same
  bytes, and renaming a job in the graph rewrites its references and nothing else;
- a draft run does not appear on the forge unless asked to;
- saving a draft opens a pull request with exactly the edited file.

### M5 — Plugins

Scope:

- `bosun-plugin-host` and the WIT package `bosun:plugin@1.0.0`; the external plugin protocol;
  installation from OCI with signatures; instances with configuration forms; `notify` routes,
  `on.plugin` triggers, admission policies, `plugin()` functions, secret backends; outgoing
  webhooks next to the notifier plugins.
- The first plugins: Telegram, Matrix and email notifiers, the Kafka trigger, the change-window and
  changed-paths policies.

Done when:

- a failure and the following fix are both delivered to Telegram;
- a change-window policy keeps a deploy job waiting until the window opens;
- a Kafka message starts a run whose `github.event` is the message;
- a policy plugin that traps keeps its job out, and a plugin without an `http` permission cannot
  reach the network.

After M5, the open questions of [design/13-decisions.md](design/13-decisions.md) decide what comes
next.

## M0 work breakdown

M0 is split into fifteen steps. Each step is one change to `main`, ordered by dependency; a step
is done when its acceptance holds in CI. Where a step names a mechanism the design leaves open, it
says so.

### 1. Repository scaffolding and CI

**Goal.** Turn the repository into a workspace that CI builds and tests. The cargo workspace
declares `members = ["crates/*"]` with shared dependency versions; `bin/*` joins the list with the
first binary in step 6, because cargo refuses a member pattern that matches nothing, and for the
same reason the workspace starts with `bosun-types` as an empty crate that step 2 fills. The
toolchain is pinned by `rust-toolchain.toml` with the `wasm32-unknown-unknown` and
`x86_64-unknown-linux-musl` targets, and CI installs exactly that channel. `.github/workflows/ci.yml`
runs on `[self-hosted, Linux, X64]` with a shared `CARGO_TARGET_DIR` under the runner's cache
directory, starts a Postgres 15 service on a free port, waits for it with `pg_isready`, and runs
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` with `BOSUN_TEST_DATABASE_URL`, `hack/check-deps.sh`, which asserts the
dependency rules of [design/01-overview.md](design/01-overview.md) from `cargo metadata`, and
`hack/test-check-deps.sh`, which proves on scratch workspaces that those rules refuse what they
must.

**Touches.** `Cargo.toml`, `rust-toolchain.toml`, `crates/types/`, `.github/workflows/ci.yml`,
`hack/check-deps.sh`, `hack/test-check-deps.sh`, `contracts/`.

**Out of scope.** Release builds and images, the frontend, the WebAssembly build (step 3 adds it
with the first crate it applies to).

**Acceptance.**

- CI runs the commands above on the workspace and is green.
- The workflow waits for the Postgres service with `pg_isready` and fails if it does not come up.
- `hack/check-deps.sh` passes on the workspace and refuses a workspace crate without a rule.
- `hack/test-check-deps.sh` shows `hack/check-deps.sh` failing, with the right message, on scratch
  workspaces where the runner reaches the database crate, the server reaches the runner, a pure
  crate reaches an async runtime, and a crate depends on `bosun-testkit` outside dev-dependencies;
  and passing on an allowed graph.

### 2. bosun-types

**Goal.** The shared vocabulary as plain types: identifiers (UUIDv7 newtypes per entity);
statuses and conclusions of runs, jobs, executions and steps with the conclusion source; waiting
reasons; the actor; the IR of [design/02-workflows.md](design/02-workflows.md) with `Template`,
`Expr` and `Evaluable<T>` already present but carrying only literals in M0, for the M0 subset of
step 3; the journal event kinds M0 emits; the error codes and their HTTP statuses from
[design/11-api.md](design/11-api.md); the API shapes of the resources M0 serves. Golden fixtures in
`contracts/fixtures/`.

**Touches.** `crates/types/`, `contracts/fixtures/`.

**Out of scope.** Expression ASTs (M1), event kinds of later milestones, plugin types, the shapes
of tokens (step 7) and of logs (step 13), which arrive with the steps that serve them.

**Acceptance.**

- `cargo tree -p bosun-types` shows no async runtime, no database driver and no HTTP crate.
- Every fixture deserialises and re-serialises without loss, and a fixture that no test maps to a
  type fails the suite; an unknown value of a closed enum fails to deserialise; the IR refuses
  unknown fields.
- The status enums of runs, jobs, executions and steps reject every transition outside the tables
  of [design/04-runs.md](design/04-runs.md), through a `can_transition` function tested
  exhaustively against the tables read from that document.
- The error codes and their statuses equal the table of [design/11-api.md](design/11-api.md), and
  the waiting reasons equal the table of [design/04-runs.md](design/04-runs.md).

### 3. bosun-workflow, the M0 subset

**Goal.** Parse workflow YAML 1.2 with spans, report diagnostics in the shape of
[design/02-workflows.md](design/02-workflows.md), validate the job graph, and lower the M0 subset to
the IR. A key of the compatibility surface outside the subset yields an error that names the
milestone it arrives in; an unknown key yields `unknown_key` with a suggestion. The YAML library is
chosen in this step by two criteria: spans on every node, and YAML 1.2 core schema semantics for
`on`. CI gains `cargo build -p bosun-types -p bosun-workflow --target wasm32-unknown-unknown`.

**Touches.** `crates/workflow/`, `contracts/fixtures/workflows/`, `.github/workflows/ci.yml`.

**Out of scope.** Expressions, triggers other than `workflow_dispatch`, reading from a forge.

**Acceptance.**

- Fixture workflows lower to committed IR snapshots.
- A cycle, an unknown `needs`, a repeated step id and an invalid job id each produce their
  diagnostic with the right line and column.
- `on: workflow_dispatch` parses as a trigger, not as a boolean.
- The WebAssembly build is green.

### 4. bosun-db and bosun-testkit

**Goal.** The schema of M0 as sqlmig migrations (`organisation`, `project`, `user`, `api_token`,
`workflow`, `workflow_revision`, `run`, `attempt`, `job`, `execution`, `step`, `run_event`, `pool`,
`agent`, `registration_token`, `log_chunk`, `log_object`, `idempotency_key`), with the trusted pool
`default` seeded, sea-orm entities, and the query layer the server needs. `bosun-testkit` migrates a template database once per test binary and gives each test
its own database created from it.

**Touches.** `crates/db/`, `crates/testkit/`, `crates/db/migrations/`.

**Out of scope.** Tables of later milestones; the queue claim (step 11).

**Acceptance.**

- Migrations apply to an empty database and a second application is a no-op.
- Two tests that write the same rows run in parallel without interfering.
- A test asserts that every migration file contains no `DROP`, no `ALTER … DROP` and no
  `ALTER … TYPE`.

### 5. bosun-blob with the directory backend

**Goal.** The object store interface of [design/07-storage.md](design/07-storage.md) (streaming
put, ranged get, delete, list by prefix) with the directory backend, and a conformance test suite
that every backend runs.

**Touches.** `crates/blob/`.

**Out of scope.** The S3 backend (M1).

**Acceptance.**

- The conformance suite passes for the directory backend.
- A put interrupted halfway leaves no visible object.

### 6. Server skeleton

**Goal.** `bosun-server serve` and `bosun-server migrate`: configuration from `server.toml` and
`BOSUN_*` variables, migrations under an advisory lock at start, axum on one listener prepared to
carry gRPC as well, `/healthz` and `/readyz`, the error envelope, JSON logs with a request id,
graceful shutdown that stops accepting, drains requests and closes agent streams with a reason.

**Touches.** `bin/server/`, `crates/server/`, `crates/db/` (the migration lock and the table
`schema_version`), `crates/testkit/` (the address of a test database), `Cargo.toml` (adds `bin/*`
to the members).

**Out of scope.** Authentication (step 7), the interface, the object store (step 13).

**Acceptance.**

- The server refuses to start against a schema of a newer major version, and without a KEK,
  naming the reason; it starts against a schema migrated by a newer minor version.
- Two servers started at once against an empty database both come up and the migrations ran once.
- `/readyz` fails while the database is unreachable and recovers when it returns.

### 7. Tokens and the bootstrap owner

**Goal.** Users without sign-in yet, the owner created by `bosun-server admin create` or by
`BOSUN_BOOTSTRAP_ADMIN` together with a first token printed once, personal tokens `bst_` in the
format of
[design/08-access.md](design/08-access.md) stored as SHA-256, bearer authentication middleware, and
a provisional rule for M0: the owner may do everything, nobody else exists.

**Touches.** `crates/server/`, `crates/db/`, `bin/server/` (the command `admin create`),
`crates/types/` and `contracts/fixtures/api/` (the shapes of users and of `GET /api/v1/me`).

**Out of scope.** Passwords, OpenID Connect, sessions, roles (M1).

**Acceptance.**

- A request without a token gets `401 unauthenticated`; with a revoked or expired token too.
- The token's checksum is verified before any database lookup.

### 8. Projects, server-side workflows and dispatch

**Goal.** Organisations and repository-less projects; server-side workflows created and updated
through the API, each save validated and stored as a revision; `POST /workflows/{id}/dispatch`
creating a run with its first attempt and its pending jobs, and the IR snapshot.

**Touches.** `crates/server/`, `crates/db/` (the queries and the counter `last_run_number` of
workflows), `crates/types/` and `contracts/fixtures/api/` (the request shapes of organisations,
projects, workflows and sources), `Cargo.toml` (`bosun-workflow` for the server).

**Out of scope.** Repository-bound projects and triggers other than dispatch (M1).

**Acceptance.**

- An invalid workflow is refused with its diagnostics and creates no revision.
- Saving the same source twice yields one revision.
- Dispatch with an `Idempotency-Key` repeated returns the same run.

### 9. Planner and state machine with the journal

**Goal.** The transitions of [design/04-runs.md](design/04-runs.md) for M0: readiness when `needs`
complete, skipping on a failed dependency, queueing, completion of jobs and runs with their
conclusions, and `run.cancel` and `job.cancel` through the command endpoint. Every transition is a
journal event written in the same transaction as the status columns.

**Touches.** `crates/server/` (the planner, the command endpoint, and the agent's `Accept`,
`StepStarted`, `StepFinished` and `JobFinished`), `crates/db/` (the lock of a run that orders its
journal, the transitions, the column `cancel_requested_at` of runs), `crates/testkit/` (the fake
agent sends any message), `Cargo.toml` (`proptest` for the property test).

**Out of scope.** `if`, matrices, gates, environments, every other command; `Idle`, `Reject` and
the lease of an assignment, which belong to the queue (step 11).

**Acceptance.**

- A property test drives random job graphs with random step results and checks that the projected
  statuses always equal a replay of the journal.
- A cancelled run never leaves a job outside `completed`.

### 10. bosun-proto and the agent endpoint

**Goal.** The protobuf definitions of `bosun.agent.v1` for the messages M0 uses, compiled with
tonic and a pure-Rust protobuf compiler so that no `protoc` is needed; registration tokens
`brt_`, `POST /api/v1/agents/register` returning `bat_`; the `Connect` stream with `Hello`,
`Welcome`, heartbeats, offline detection and the grace period; the replica routing of control
messages through `NOTIFY`.

**Touches.** `proto/`, `crates/proto/`, `crates/server/`, `crates/db/`.

**Out of scope.** Terminal messages, credential rotation, `Drain`.

**Acceptance.**

- A scripted fake agent from `bosun-testkit` registers, connects, goes offline after missed
  heartbeats and back online on reconnect.
- An agent of an unsupported protocol version is refused with a message naming both versions.
- A control message published on replica A arrives at a fake agent connected to replica B.

### 11. The queue

**Goal.** Claiming of queued jobs by idle agents as in [design/07-storage.md](design/07-storage.md)
(labels contained in the agent's labels, pool, priority, age), assignment leases, lease expiry,
`NOTIFY bosun_queue` and the periodic sweep, `max_queue_time`, advisory locks for the periodic
tasks. The `Assignment` sent to the agent carries the slugs of the job's organisation and project,
which name its workspace, and each step's `shell` as the text `bosun_types::ir::Shell` displays; the
agent of step 12 rejects an assignment without valid slugs
([design/05-execution.md](design/05-execution.md#the-job-assignment)).

**Touches.** `crates/server/`, `crates/db/`.

**Out of scope.** Fair share and project caps (M2), trust levels (M1).

**Acceptance.**

- Fifty fake agents claiming concurrently from three replicas never receive the same job.
- An assignment that is not confirmed returns the job to the queue after the lease.
- A job whose labels no agent offers is cancelled with `no_agent` after a shortened
  `max_queue_time`.

### 12. bosun-runner and bosun-agent with the shell capability

**Goal.** The agent binary: `register`, `run`, the configuration file, reconnection with backoff
and fresh name resolution, the log spool on disk with sequence numbers and resending after a
reconnect. The engine runs `run` steps on the host with the default shell rules of
[design/05-execution.md](design/05-execution.md), `env`, `working-directory`, a cgroup per job,
cancellation by `SIGINT`, `SIGTERM`, `SIGKILL`, and reports step and job results.

**Touches.** `bin/agent/`, `crates/runner/`.

**Out of scope.** Expressions, actions, file and workflow commands, masking (M1), podman (M2).

**Acceptance.**

- A step that starts a detached background process leaves no process behind after the job.
- A step that ignores `SIGINT` and `SIGTERM` is killed within 15 seconds of a cancel.
- Unacknowledged log chunks survive an agent restart and are delivered once.

### 13. Logs

**Goal.** Ingestion of log chunks with `ON CONFLICT DO NOTHING` and acknowledgement, reading a
step's log by range, following it as an event stream across replicas through `NOTIFY bosun_log`,
compaction into the object store after the execution completes, and the 128 MiB limit, which the
server enforces at ingestion, since the agent sends every line
([design/07-storage.md](design/07-storage.md#logs)). The object store enters the server here, as its
first user: the `[store]` section with the directory backend, the refusal to start when the store is
unreachable, and its probe in `/readyz` ([design/12-operations.md](design/12-operations.md)), which
step 6 leaves out.

**Touches.** `crates/server/`, `crates/db/`.

**Out of scope.** Search, the interface, retention (M1).

**Acceptance.**

- A follower attached in the middle of a step receives every line exactly once, before and after
  compaction.
- A follower connected to replica A sees lines ingested by replica B.

### 14. The CLI and the end-to-end scenarios

**Goal.** `bosun login` storing a token, `workflow push`, `workflow dispatch --watch` with live
logs, `run cancel`, `job logs --follow`, `agent list`, `pool token create`; and `hack/e2e.sh`, which
builds the binaries, starts Postgres, two server replicas and two agents with different labels,
runs the M0 scenarios through the CLI, and runs in CI.

**Touches.** `bin/cli/`, `hack/e2e.sh`, `.github/workflows/ci.yml`.

**Out of scope.** Every other command (later milestones), `bosun exec` (M2).

**Acceptance.**

- `hack/e2e.sh` runs every scenario of the M0 exit checklist and is green in CI.

### 15. The image and an install on any host

**Goal.** Make Bosun installable on any Linux server, as
[design/12-operations.md](design/12-operations.md) describes under Delivery and Agents. The
`server.data_dir` setting with the layout of that section: `serve` uses `keys/kek` when the
configuration names no KEK and generates it on the first start, uses the directory backend under
`store/` when no S3 store is configured, and, on a first start against a database without users
and without `bootstrap.admin`, creates the owner `admin` and writes its token to `admin.token`
([design/08-access.md](design/08-access.md)). `bosun-server check` for health checks. A multi-stage
`Dockerfile` that builds static musl binaries of `bosun-server` and of `bosun-agent` for `x86_64` and
`aarch64`, and an image `globalartltd/bosun` for `linux/amd64` and `linux/arm64` that runs as uid
210 with `/bosun-data`. The server serves `install-agent.sh` and the agent binaries of its own
version under `/_runtime/agent/`; `install-agent.sh` takes `--token`, `--instances` and `--label`
and does what the Agents section lists. `deploy/compose.yaml` with Postgres, `deploy/bosun.container`
as a quadlet, and `docs/install/` with the requirements, the quick start, compose, the quadlet,
reverse proxy examples for nginx and Caddy that carry the agent stream over HTTP/2, and adding
agents. CI builds the image on every commit, starts it against its Postgres service, waits for
`bosun-server check`, reads `admin.token`, registers an agent with the served script inside a
second container and runs the two-job workflow through it; on `main` it publishes the tags of
the Delivery section to Docker Hub with the secrets `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN`
of the repository.

**Touches.** `bin/server/`, `crates/server/`, `bin/agent/`, `Dockerfile`, `.dockerignore`,
`deploy/`, `docs/install/`, `.github/workflows/`.

**Out of scope.** The Helm chart and the prod1 rollout (operations, after M0), release automation
beyond tagging, S3 (M1), passwords and `admin.password` (M1), Windows and macOS agents.

**Acceptance.**

- On a clean Linux host with podman or docker, the quick start of design/12-operations.md brings the
  server up with only `BOSUN_DATABASE_URL` and a volume, and `admin.token` signs the CLI in.
- A restart with the same volume keeps the KEK, the stored objects and the owner, and writes no new
  token; a start with a configured KEK generates nothing.
- `docker compose -f deploy/compose.yaml up` starts Bosun and Postgres together and becomes ready.
- The one-line command from `bosun pool token create` installs and registers an agent on another
  host, which then runs the two-job workflow; the binary it installed has the server's version.
- The CI job above is green, and `main` publishes `globalartltd/bosun` with a timestamp tag and
  `edge` for both architectures.

## M1 work breakdown

M1 is split into forty-six steps, numbered so that each depends only on steps before it. Each step
is one change to `main`; a step is done when its acceptance holds in CI. The steps form tracks that
proceed side by side once their first steps are in: the API document and access (1, 8–12),
expressions and the parser (2–7), the forge and its events (13, 25–31), the job language on the
server and on the agent, with pools and secrets (15–24), actions and the forge token (32–34), and
results, events and the interface (35, 37, 38, 40–44). The S3 backend (14), retention (36) and
metrics (39) stand apart; the corpus (45) and the end-to-end scenarios (46) close the milestone.
Where a step names a mechanism the design leaves open, it settles it and records the choice in the
design document it names, as the M0 steps do. Remarks that hold for every step:

- `Depends on` lists only what a step needs: code it builds on, or files it would otherwise change
  at the same time as another step. Steps without a dependency on each other may run at the same
  time.
- A behaviour of the target syntax lands together with the case that pins it, in
  `contracts/fixtures/` or `compat/`. Keys of later milestones stay `not_implemented` and name their
  milestone.
- Tests never reach GitHub or the internet. The forge is the fake of step 13 (its REST API, its
  token endpoints and its webhooks), the identity provider is the fake of step 11, and the object
  store is the directory backend or the local S3-compatible server of step 14. Actions and Node
  runtimes are fixtures served locally.
- From step 1 on, `contracts/openapi.json` is generated from the handlers. A step that adds or
  changes an endpoint regenerates it with `hack/openapi.sh`, and therefore depends on step 1,
  directly or through another step. The file is regenerated, never merged by hand.
- From step 9 on, every endpoint declares the permission it needs through step 9's check and has its
  row in the endpoint table of `docs/design/11-api.md`. A step that adds an endpoint therefore
  depends on step 9, directly or through another step, and adds its row.
- A step that adds a journal event kind, a status, a waiting reason or a reason, a run notice code,
  an error code or a diagnostic code also adds it to the table in `docs/design/` that the tests
  read.
- New tables and columns are additive migrations.
- Steps that change `proto/bosun/agent/v1/agent.proto` add messages and fields under new numbers and
  never renumber. The CLI keeps one module per command group. Steps that touch either therefore
  rebase onto each other without depending on each other.
- From step 40 on, `pnpm lint && pnpm test && pnpm build` in `web/` is part of the verification. A
  step that adds user-visible strings registers their keys in the interface's OwnLate project with
  the ownlate-translations skill before it pushes, because the build pulls the dictionaries.
  Playwright scenarios are files under `web/e2e/`, which `hack/e2e.sh` runs from step 40 on, so
  later steps add scenarios without changing the script.
- Behaviour that exists only on Linux is asserted by tests that run in CI.

| # | Step | Depends on | Review |
|---|------|------------|--------|
| 1 | OpenAPI from the handlers | — | no |
| 2 | Expression syntax and static checks | — | yes |
| 3 | Expression evaluation | 2 | yes |
| 4 | Jobs and steps of the compatibility surface | 2 | yes |
| 5 | Triggers and event matching | — | yes |
| 6 | Action metadata, workflow-wide warnings and the JSON Schema | 4, 5 | no |
| 7 | The `github` context and the step environment | — | yes |
| 8 | Local sign-in and sessions | 1 | yes |
| 9 | Permissions, roles and teams | 8 | yes |
| 10 | Users, teams, roles, bindings and scoped tokens | 9 | yes |
| 11 | OpenID Connect | 9 | yes |
| 12 | Lint through the API and the CLI | 6, 9 | no |
| 13 | bosun-forge and the fake forge | — | yes |
| 14 | The S3 backend | — | no |
| 15 | Planner with expressions and run notices | 1, 3, 4, 7 | yes |
| 16 | Matrix strategies | 15 | yes |
| 17 | Concurrency groups | 16 | yes |
| 18 | Pools, label aliases and trust in the claim | 9, 15 | yes |
| 19 | Draining and deleting agents | 9 | yes |
| 20 | The M1 job assignment | 15 | yes |
| 21 | Steps on the agent | 20 | yes |
| 22 | Masking | 21 | yes |
| 23 | Workflow commands, problem matchers and step summaries | 21, 22 | yes |
| 24 | Secrets and variables | 4, 9, 21, 22 | yes |
| 25 | Project settings | 9 | no |
| 26 | Webhooks, installations and repository-bound projects | 9, 13 | yes |
| 27 | Repository workflows and forge events | 6, 15, 25, 26 | yes |
| 28 | Reporting to the forge through the outbox | 16, 27 | yes |
| 29 | Pull requests, merge groups, forks and trust | 18, 24, 28 | yes |
| 30 | Dispatch inputs | 27 | yes |
| 31 | Schedules | 27 | yes |
| 32 | The action store and Node runtimes | 6, 20, 26 | yes |
| 33 | Node actions with pre and post steps | 23, 32 | yes |
| 34 | The forge token | 22, 29 | yes |
| 35 | Step results on the server | 23, 28 | no |
| 36 | Retention | 25, 35 | yes |
| 37 | The event stream and the queue | 9 | yes |
| 38 | Projects, workflows and runs in the CLI | 9, 26 | no |
| 39 | Metrics | 28 | no |
| 40 | The web interface: scaffold, embedding and sign-in | 1, 8 | no |
| 41 | Home, project and run pages | 15, 37, 38, 40 | no |
| 42 | The job graph and admission | 16, 29, 41 | no |
| 43 | Logs, annotations and step summaries on the run page | 35, 41 | yes |
| 44 | Queue, agents, secrets and variables pages | 18, 19, 24, 37, 40 | no |
| 45 | The compatibility corpus | 3, 6 | yes |
| 46 | End to end against the fake forge | 10, 33, 34, 45 | no |

### 1. OpenAPI from the handlers

**Goal.** Generate the OpenAPI document of [design/11-api.md](design/11-api.md) from the handlers
and commit it as `contracts/openapi.json`. The generator is chosen in this step by two criteria.
First, the document is derived from the request and response types the handlers use, so it cannot
drift from them. Second, the shared parts (the error envelope, cursor pages, `Idempotency-Key`,
`ETag` and `If-Match`) are described once. The choice is recorded in design/11-api.md.

The API shapes of `bosun-types` derive their schemas, and every endpoint served under `/api/v1` when
the step lands gets its operation. Paths outside `/api/v1` are not part of the document: `/hooks`,
`/_runtime`, `/twirp`, the health endpoints and gRPC. `bosun-server openapi` prints the document and
`hack/openapi.sh` writes it into `contracts/openapi.json`. A test fails when the generated document
differs from the committed one, and its message names the script.

**Touches.** `crates/server/` (an operation per handler), `crates/types/` (schemas of the API
shapes), `bin/server/` (`openapi`), `contracts/openapi.json` (new), `contracts/README.md`,
`hack/openapi.sh` (new), `Cargo.toml`, `docs/design/11-api.md`.

**Depends on.** nothing in M1.

**Out of scope.** The permission of each operation (step 9), the interface's client types (step 40),
serving the document over HTTP.

**Acceptance.**

- In CI, the generated document equals `contracts/openapi.json`.
- For every operation in the document, a request with the owner's token reaches a handler. None is
  answered with the `not_found` of an unserved path.
- Every fixture in `contracts/fixtures/api/` validates against the schema of the request or response
  it belongs to, and a fixture that belongs to none fails the suite.
- The error envelope and the cursor page are one component each, and every operation that returns
  them references it.
- The WebAssembly build of `bosun-types` stays green.

**Review.** no.

### 2. Expression syntax and static checks

**Goal.** Create `bosun-expr` with the syntax of
[design/03-expressions.md](design/03-expressions.md). A hand-written lexer and a precedence-climbing
parser produce the expression syntax tree with spans. The tree itself is plain data in
`bosun-types`, so the IR can hold it. `Expr` gains the full node set: every literal form the
document lists, context names, property and index access, the object filter `.*`, `!`, comparisons,
equality, `&&`, `||` and function calls. `Template` gains expression segments. Templates are split
at `${{ }}`, correctly when a string literal inside contains `}}`, and the three forms of `if` are
recognised.

Static checks run without evaluating anything: an unknown function, a wrong number of arguments, and
a context that is not available at the key. The availability table of the target syntax is data in
the crate, pinned by a fixture, together with the side that evaluates each key under the rule of the
document. Context, property and function names are case-insensitive in the target, which the
document does not state; cases pin it and the document gains the rule. `plugin()` is refused with a
diagnostic that names M5. CI's WebAssembly build gains `-p bosun-expr`.

**Touches.** `crates/expr/` (new), `crates/types/` (the expression tree, template segments),
`contracts/fixtures/expr/` (new), `.github/workflows/ci.yml`, `Cargo.toml`,
`docs/design/03-expressions.md`. The rows of `bosun-expr` in `hack/check-deps.sh` and
`docs/design/01-overview.md` already exist; the step checks them.

**Depends on.** nothing in M1.

**Out of scope.** Evaluation (step 3); expressions inside `bosun-workflow` (step 4).

**Acceptance.**

- Every syntax case in `contracts/fixtures/expr/` parses to its committed tree with spans, or fails
  with its committed error and span. The cases cover precedence (`!a == b`, `a || b && c`), every
  literal form, `''` inside a string and `'}}'` inside a template.
- `if: x`, `if: ${{ x }}` and `if: " ${{ x }} "` give the same expression; `if: "a ${{ x }}"` is a
  template.
- Each of these produces an error at the span of the call or the context: an unknown function,
  `contains(a)`, `case(a, b)` with an even number of arguments, and `steps.x` at a key that cannot
  see `steps`.
- The availability fixture equals the table in the crate. A test reads the key table of
  design/03-expressions.md and checks the evaluating side of every key it lists.
- The WebAssembly build of `bosun-types`, `bosun-expr` and `bosun-workflow` is green, and
  `cargo tree -p bosun-expr` shows no I/O crate.

**Review.** yes: the syntax and the availability table are the compatibility contract the corpus
pins.

### 3. Expression evaluation

**Goal.** The evaluator of `bosun-expr`, over a `Value` enum with the coercions of
design/03-expressions.md:

- truthiness;
- equality, with strings compared ignoring case and numeric conversion across types;
- comparisons involving `NaN`;
- `&&` and `||`, which return one of their operands;
- property lookup, which yields `null` for a missing property, and the object filter;
- conversion to a string, with numbers in the `G15` form.

It implements the functions `contains`, `startsWith`, `endsWith`, `format` with its brace escapes,
`join`, `toJSON`, `fromJSON` and `case`. The status functions and `hashFiles` are hooks the caller
implements, so the server and the agent each supply their own. Contexts come through a trait and are
resolved lazily. Rendering a template turns each segment's value into text. An evaluation error
carries the span of the expression. A runner executes every case in `contracts/fixtures/expr/` in
the format of the document (`expression`, `contexts`, `result` or `error`).

**Touches.** `crates/expr/`, `contracts/fixtures/expr/`.

**Depends on.** 2.

**Out of scope.** Availability of contexts at keys (step 2), the agent's `hashFiles` (step 21),
recording the cases on hosted runners (step 45).

**Acceptance.**

- Every evaluation case in `contracts/fixtures/expr/` yields its committed result or error. The
  cases include `'0x10' == 16`, `null == 0`, `'ABC' == 'abc'`, comparisons with `NaN`,
  `format('{{0}}', 1)`, `fromJSON` of invalid JSON, the indentation of `toJSON`, and the `G15`
  output of `3.10`, `1e21` and `0.1`.
- `a || b` and `a && b` return the operand, not a boolean, in one case per combination of
  truthiness.
- `fromJSON(toJSON(v))` equals `v` for generated values (a property test).
- The context trait is never asked for a context the expression does not name.

**Review.** yes: the corpus pins coercion and function semantics against the target.

### 4. Jobs and steps of the compatibility surface

**Goal.** Lower into the IR, with expressions, every workflow, job and step key that the table
"Where each key arrives" assigns to M1:

- Every string value becomes a `Template` and every `if` an `Expr`.
- `run-name`.
- `permissions` at both levels: `read-all`, `write-all`, `{}`, and the per-scope form with the
  target's scope names, `id-token` included.
- `defaults.run` with `shell` and `working-directory`.
- `concurrency` at both levels, as a string or as `group`, `cancel-in-progress` and `queue`.
  `cancel-in-progress` is an `Evaluable<bool>`, because workflows compute it, Bosun's own CI among
  them.
- Job `outputs`, `timeout-minutes` and `continue-on-error`, the last two as `Evaluable`.
- `strategy`, with `matrix` (vectors, `include` and `exclude`, or one expression), `fail-fast` and
  `max-parallel`.
- `runs-on` with expressions.
- Step `uses` in the forms `owner/repo@ref` and `owner/repo/path@ref`, `with`, `continue-on-error`
  and `timeout-minutes`.

Validation checks every expression against the availability of its key (step 2). It refuses `run`
together with `uses`. It also refuses `queue: max` together with a literal
`cancel-in-progress: true`; an expression there is checked when step 17 evaluates it. `./path` and
`docker://` stay `not_implemented` and name M2.

The step also computes, for each job template, the secrets its expressions reference, or that it
reads secrets by a computed name. The validator warns about the computed name, and step 24 uses the
result. The parser now returns warnings together with the IR. The diagnostic codes table of
design/02-workflows.md lists only errors today; the warning codes this step introduces are added to
it.

**Touches.** `crates/workflow/`, `crates/types/src/ir.rs`, `contracts/fixtures/workflows/`,
`contracts/fixtures/ir/`, `contracts/fixtures/diagnostics/`, `docs/design/02-workflows.md`.

**Depends on.** 2.

**Out of scope.** Triggers (step 5); action metadata and the warnings that need the whole file or
its path (step 6); evaluating anything (steps 15 and 21).

**Acceptance.**

- A fixture workflow per M1 key lowers to its committed IR, and every IR fixture round-trips through
  `bosun-types`.
- Each of these produces its diagnostic at the expression's span: `steps.a.outputs.x` in a job's
  `runs-on`, `matrix` in workflow `env`, and an unknown function in a step `if`.
- `uses: ./x` and `uses: docker://alpine` are `not_implemented` and name M2.
  `uses: actions/checkout` without a ref is `invalid_value`. `run` together with `uses` is refused.
- `queue: max` together with `cancel-in-progress: true` is `invalid_value`.
  `cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}` lowers to a template, also next to
  `queue: max`.
- `secrets.A` and `secrets['B']` in a job yield the set {A, B}. `secrets[format('X_{0}', matrix.k)]`
  yields "every secret" and a warning.
- An `if` with text around its `${{ }}` produces the warning of design/03-expressions.md.

**Review.** yes: the reference analysis decides which secrets a job receives, and the lowering of
`permissions` decides the scope of the forge token.

### 5. Triggers and event matching

**Goal.** Lower `on` in its string, list and mapping forms, for every event the table assigns to M1,
with its filters:

- `push`: `branches`, `branches-ignore`, `tags`, `tags-ignore`, `paths`, `paths-ignore`.
- `pull_request` and `pull_request_target`: `types`, `branches`, `branches-ignore`, `paths`,
  `paths-ignore`.
- `merge_group`: `types`, `branches`, `branches-ignore`.
- `schedule`: `cron` in the target's five-field syntax, parsed into a form step 31 evaluates, and
  `timezone` as an IANA name checked against a compiled-in time zone database.
- `workflow_dispatch` `inputs` of type `string`, `boolean`, `choice`, `number` and `environment`,
  with `description`, `required`, `default` and `options`.
- `repository_dispatch` `types`, and `types` on every generic event.

The `on` lowering moves into a module of its own. The parser also exposes it separately, so a file
whose jobs are broken still yields its triggers, as design/02-workflows.md requires for
`startup_failure`.

A pure matcher decides whether an event matches a workflow's triggers. The event is its name,
action, ref and base ref, and its changed files or their absence. For a merge group, `branches`
matches the base ref of the queue. The matcher follows the target: its glob patterns for refs and
paths, negation with `!`, order-sensitive evaluation of mixed lists, and the default `types` of each
event. `workflow_call`, `workflow_run` and `upstream` stay `not_implemented` naming M2, and `plugin`
naming M5.

**Touches.** `crates/workflow/` (a new module for `on`), `crates/types/src/ir.rs` (`Trigger`),
`contracts/fixtures/workflows/`, `contracts/fixtures/ir/`, `contracts/fixtures/diagnostics/`,
`contracts/fixtures/triggers/` (new).

**Depends on.** nothing in M1.

**Out of scope.** Reading changed files from the forge (steps 27 and 29); validating dispatch inputs
at dispatch time (step 30); firing schedules (step 31).

**Acceptance.**

- Each event in each form lowers to its committed IR, and an input's `description` is kept in it.
- Each of these produces its diagnostic at the value: `branches` together with `branches-ignore`, an
  invalid cron, an unknown time zone, a `choice` default outside its options, and a `number` default
  that is not a number.
- The cases in `contracts/fixtures/triggers/` (filter, event, expected) pass. Among them:
  - `['**', '!docs/**', 'docs/keep.md']`;
  - `*` not crossing `/`;
  - a tag push against `paths`;
  - `pull_request` without `types`, which matches `opened`, `synchronize` and `reopened` but not
    `closed`;
  - `merge_group: { branches: [main] }`, which matches a merge group whose base ref is
    `refs/heads/main` and not one into `release`.
- The triggers of a file whose `jobs` has errors are returned together with those errors.
- The WebAssembly build stays green with the time zone database.

**Review.** yes: filters decide whether a run exists at all, which is workflow semantics the corpus
pins.

### 6. Action metadata, workflow-wide warnings and the JSON Schema

**Goal.** Parse `action.yml` and `action.yaml` in `bosun-workflow`, with spans and diagnostics:

- `name`, `description`, `author` and `branding`;
- `inputs`, with `description`, `required`, `default` as a template, and `deprecationMessage`;
- `outputs`;
- `runs` with `using: node12`, `node16`, `node20` or `node24`, and `main`, `pre`, `post`, `pre-if`
  and `post-if`.

`composite` and `docker` are `not_implemented` and name M2. Whether an unknown key in an action's
metadata is an error, as it is in a workflow, is settled with cases taken from public actions and
recorded in design/02-workflows.md.

A lint entry point that knows the file's path adds the warnings that need the whole file or its
location. These are the `bosun` context and extension keys in a file under `.github/workflows/`, and
`pull_request_target` combined with a checkout of the pull request's head.

`contracts/schema/workflow.json` describes every key of the surface and every extension key, each
with the milestone of its From column as `x-bosun-from`. The schema therefore stays the same from
milestone to milestone, and the editor of M4 can say when a key arrives. `bosun-workflow` names the
current milestone. It also keeps a list of the keys whose milestone has been reached but which are
not implemented yet. This step leaves that list empty for M1; M2 step 1 and M3 step 3 move the
current milestone on and fill the list with the keys of their milestones.

A test reads the table "Where each key arrives" and the extensions table of design/02-workflows.md.
It checks three things:

- a key is implemented exactly when its From milestone is reached and the key is not on the list;
- every other key is `not_implemented` and names its From milestone;
- the schema lists exactly the keys of the two tables, each with its milestone.

**Touches.** `crates/workflow/`, `contracts/schema/workflow.json` (new),
`contracts/fixtures/actions/` (new), `contracts/fixtures/workflows/`,
`contracts/fixtures/diagnostics/`, `contracts/README.md`, `docs/design/02-workflows.md`.

**Depends on.** 4, 5.

**Out of scope.** Resolving and running actions (steps 32 and 33); the lint endpoint and command
(step 12); the action metadata endpoint (M4).

**Acceptance.**

- Fixture actions lower to their committed metadata. A `pre-if` naming a context its key cannot see,
  a `runs` without `main`, and `using: composite` each produce their diagnostic.
- `${{ bosun.run_url }}` in a file under `.github/workflows/` produces a warning, and under
  `.bosun/workflows/` none. A `pull_request_target` workflow whose checkout sets
  `ref: ${{ github.event.pull_request.head.sha }}` produces a warning.
- The table test passes with an empty list for M1.
- A workflow fixture the parser accepts validates against the schema, and a fixture with an unknown
  key does not; a JSON Schema validator runs as a dev-dependency.

**Review.** no: a pure parser addition, pinned by fixtures and by the table test.

### 7. The `github` context and the step environment

**Goal.** Pure functions in `bosun-workflow` that turn an event into what a job sees, as
[design/06-runtime-services.md](design/06-runtime-services.md) specifies:

- The `github` context, built from the event's name and payload and the facts of the run. The facts
  are the repository, the ref and commit where the payload does not give them, the workflow and its
  ref, the run's id, number and attempt, the actor and the triggering actor, the job, the forge
  addresses and the workspace. Every scalar property is derived exactly as the target derives it.
- The payloads Bosun produces itself, in the shape the target produces for the event:
  `workflow_dispatch` with its inputs as strings, `schedule` with its cron, and a
  `repository_dispatch` sent through the API.
- The environment variables of a step: `CI`, `GITHUB_ACTIONS`, the `GITHUB_*` mirrors of the
  context, the file command paths, `RUNNER_*` from the facts of the runner, and `BOSUN_*`.

The step settles, and records in design/06-runtime-services.md, what `github.repository`,
`github.ref` and the forge addresses are for a project without a repository.

The fixtures in `contracts/fixtures/github/` each hold an event name, a payload, the facts, and the
expected context and environment. There is one per event: a push to a branch, a branch creation, a
tag push, a pull request, a `pull_request_target`, a merge group, a dispatch, a schedule, a
repository dispatch and an `issues` event. Payloads are copied from the forge's documented webhook
examples.

**Touches.** `crates/workflow/` (a new module), `contracts/fixtures/github/` (new),
`docs/design/06-runtime-services.md`.

**Depends on.** nothing in M1.

**Out of scope.** The facts the server holds about a run (step 15) and carrying the context to the
agent (step 20); the `runner` context (step 21); the job token and the runtime service variables
(M2).

**Acceptance.**

- For each fixture, the derived context and environment equal the committed ones.
- The dispatch payload of the inputs `{ flag: true, count: 3 }` has `inputs.flag == 'true'` and
  `inputs.count == '3'`.
- A pull request's `github.ref` is its merge ref, and `head_ref` and `base_ref` are its branches; a
  push has empty `head_ref` and `base_ref`.
- A tag push has `ref_type` `tag` and `ref_name` equal to the tag.
- The WebAssembly build stays green.

**Review.** yes: the context and the variables are what actions read, and the corpus pins them.

### 8. Local sign-in and sessions

**Goal.** Sign-in with local passwords and browser sessions:

- passwords hashed with argon2id;
- `POST /auth/login` and `POST /auth/logout`;
- the session `bss_` in the format of design/08-access.md, stored as SHA-256 in `session`, with an
  idle timeout of 12 hours and an absolute one of 30 days;
- the cookie `__Host-bosun_session`, with `Secure`, `HttpOnly`, `SameSite=Lax` and path `/`;
- a CSRF token bound to the session, returned by `GET /me` and required as `X-Bosun-CSRF` on every
  mutating request the cookie authenticates.

Failed sign-ins are rate-limited per account and per address, with an exponentially growing lockout.
The address is taken from `X-Forwarded-For` only behind `server.trusted_proxies`.

On the first start the server writes `admin.password` next to `admin.token`. The first sign-in with
it requires a new password, and then deletes both files.

The step settles two things the design leaves open, and records them in design/08-access.md and
design/11-api.md. The first is how a user changes their own password. The second is how the sign-in
page learns the sign-in methods: `GET /api/v1/auth/methods` lists password sign-in and the
configured providers, and the provider list stays empty until step 11. A periodic task removes
expired sessions.

**Touches.** `crates/server/` (authentication, sessions, the auth routes), `crates/db/` (migrations:
password hash and state on `user`, `session`, failed sign-ins), `crates/types/`,
`contracts/fixtures/api/`, `contracts/openapi.json`, `bin/server/` (writing `admin.password`),
`docs/design/08-access.md`, `docs/design/11-api.md`.

**Depends on.** 1.

**Out of scope.** Roles and permissions (step 9), OpenID Connect (step 11), TOTP (M3), the sign-in
page (step 40).

**Acceptance.**

- A correct sign-in sets the cookie with its four attributes, and the session authenticates
  `GET /me`.
- A wrong password is `401`. After the configured number of failures, the account and the address
  are locked for a growing period. This also holds when the address arrives through a trusted proxy,
  but not through an untrusted one.
- A mutating request with the cookie is refused without `X-Bosun-CSRF` or with another session's
  token; the same request with a bearer token needs no CSRF token.
- With the clock injected, a session idle for 12 hours, or created 30 days ago, is refused. After
  logout it is refused at once.
- A fresh data directory gets `admin.password`. Signing in with it allows nothing but setting a new
  password, after which `admin.token` and `admin.password` are gone.
- A malformed `bss_` cookie is refused before any database query.

**Review.** yes: credentials, sessions and CSRF are security.

### 9. Permissions, roles and teams

**Goal.** The permissions and roles of [design/08-access.md](design/08-access.md), as data and as
one check:

- the tables `role`, `role_binding`, `team` and `team_member`;
- the built-in roles `viewer`, `developer`, `maintainer` and `admin`, and custom roles of an
  organisation as sets of permission keys;
- bindings of a user or a team to a role, on an organisation or on one project.

A person's permissions on a project are the union of their bindings on that project and on its
organisation, through their teams as well; installation owners hold everything. A token of M0 acts
with its user's permissions. One permission check replaces M0's provisional owner-only rule on every
endpoint served when the step lands, and lists return only what the caller may see.

Each handler declares its permission, or that it needs authentication only. The declaration is part
of its operation in `contracts/openapi.json` as `x-bosun-permission`. design/11-api.md gains a table
of endpoints and their permissions, which design/08-access.md cannot give because it maps roles to
permissions but not endpoints. `GET /me` gains the caller's permissions.

The step settles, and records in design/11-api.md, the shape of the permissions in `/me` (per
organisation and per project), and whether a resource the caller may not view answers `403` or
`404`.

**Touches.** `crates/server/` (the permission check on every route), `crates/db/` (migrations and
queries), `crates/types/`, `contracts/fixtures/api/`, `contracts/openapi.json`,
`docs/design/08-access.md`, `docs/design/11-api.md`.

**Depends on.** 8.

**Out of scope.** The resources that manage users, teams, roles, bindings and tokens (step 10); team
synchronisation from an identity provider (step 11); the audit trail (M3).

**Acceptance.**

- A test reads the endpoint table of design/11-api.md and checks that it equals the operations of
  `contracts/openapi.json` with their `x-bosun-permission`.
- A table-driven test reads that table and the role table of design/08-access.md. For every endpoint
  and every built-in role, it checks that a request is allowed or refused exactly as the two tables
  say.
- A viewer's dispatch is refused and a developer's succeeds; a binding on the organisation applies
  to each of its projects.
- A team's binding gives its members the role, and removing a member takes it away on their next
  request.
- A viewer of project P sees only P in the lists of projects and runs, and a request for a run of
  project Q answers as settled.
- `GET /me` lists the caller's permissions in the settled shape, pinned by a fixture.

**Review.** yes: permissions decide who can do what, everywhere.

### 10. Users, teams, roles, bindings and scoped tokens

**Goal.** The resources of identity and access in design/11-api.md:

- `/users`, managed by installation owners. A local user is created with a login, a display name, an
  email address and an initial password, which the first sign-in replaces through step 8's change of
  password.
- `/teams` with their members, `/roles` for custom roles, and `/role-bindings`, each managed with
  `organisation.manage` in the organisation it belongs to.
- `/tokens`. A personal token carries only permission keys its creator holds, may be restricted to
  organisations or projects, expires within a year, and is listed and revoked by its owner.
  `bosun token create` creates one.

The step records two rules in design/08-access.md. Installation owners manage users. A token acts
with the intersection of its keys and its creator's permissions at the time of each request, so a
creator who loses a permission takes it from their tokens as well.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `bin/cli/` (`token create`), `docs/design/08-access.md`,
`docs/design/11-api.md`.

**Depends on.** 9.

**Out of scope.** OpenID Connect (step 11), the settings pages (M3), the audit trail (M3).

**Acceptance.**

- A token restricted to `project.view` on one project reads that project's runs, cannot see another
  project, and cannot dispatch.
- Creating a token with a permission its creator lacks, or with an expiry beyond a year, is
  `invalid_input` naming the field.
- A token whose creator loses `run.dispatch` can no longer dispatch, although it carries the key.
- A custom role with an unknown permission key is `invalid_input`, and a binding of a custom role
  grants exactly its keys.
- An organisation's admin creates a team and a binding in that organisation and is refused in
  another. Only an owner creates a user.
- A user created with an initial password must set a new one at the first sign-in.

**Review.** yes: tokens and bindings grant access.

### 11. OpenID Connect

**Goal.** Sign-in through the `[[oidc]]` providers of design/12-operations.md:

- discovery of each provider's configuration;
- `GET /auth/oidc/{provider}/start`, with state, nonce and PKCE kept in a short-lived signed cookie;
- `GET /auth/oidc/{provider}/callback`, which exchanges the code and validates the ID token: the
  signature against the provider's keys, the issuer, the audience, the expiry and the nonce.

`user_identity` links the provider's subject to a user. A first sign-in creates the user when the
claims match the provider's allow rules (groups or email domains), and is refused otherwise. At each
sign-in, teams are synchronised from the groups claim through the configured mapping; the step
settles whether a mapped team that does not exist yet is created. `GET /auth/methods` lists the
providers. `bosun-testkit` gains a fake identity provider.

**Touches.** `crates/server/`, `crates/db/` (`user_identity`), `crates/testkit/` (the fake
provider), `contracts/openapi.json`, `docs/design/08-access.md`, `docs/design/12-operations.md` (the
configuration keys of the allow rules and the group mapping).

**Depends on.** 9.

**Out of scope.** Linking a forge account (M2), TOTP (M3).

**Acceptance.**

- Against the fake provider, a user in an allowed group signs in, receives a session, and is a
  member of the mapped team. A user outside the rules is refused, and no user is created.
- An ID token is refused when its audience, issuer or nonce is wrong, when it has expired, or when
  another key signed it.
- A callback is refused when its state was already used or belongs to another browser.
- Removing a user from the group at the provider removes them from the mapped team at their next
  sign-in.

**Review.** yes: an identity provider's tokens are security.

### 12. Lint through the API and the CLI

**Goal.** The places where the warnings of design/02-workflows.md are shown outside the run page:

- `POST /api/v1/lint` takes `source` and an optional `path`. It returns the diagnostics of step 6's
  lint entry point, warnings included, with `200` whether or not they hold errors. It needs
  authentication only.
- `bosun workflow lint [<file>...]` runs `bosun-workflow` locally on each file with its path.
  Without arguments it takes every file under `.bosun/workflows/` and `.github/workflows/` of the
  current directory. It prints each diagnostic as `file:line:column: severity: message`, or JSON
  with `--json`, and exits with status 1 when any file has an error.

**Touches.** `crates/server/`, `crates/types/`, `contracts/fixtures/api/`, `contracts/openapi.json`,
`bin/cli/`, `docs/design/11-api.md`.

**Depends on.** 6, 9.

**Out of scope.** The editor's diagnostics (M4) and MCP's `lint_workflow` (M3), which call the same
functions.

**Acceptance.**

- A source with an unknown key returns its diagnostic, pinned by a fixture. With the path
  `.github/workflows/x.yml`, a source that uses `bosun` returns the extension warning; with
  `.bosun/workflows/x.yml`, it returns none.
- A request without credentials is `401`.
- For every file in `contracts/fixtures/workflows/`, the CLI and the API return the same
  diagnostics.
- `bosun workflow lint` exits with status 1 on a file with an error, and with 0 on a file with
  warnings only.

**Review.** no.

### 13. bosun-forge and the fake forge

**Goal.** Create `bosun-forge` with the forge trait of design/06-runtime-services.md, limited to
what M1 uses:

- read a file and a directory at a commit, with their blob hashes;
- resolve a ref to a commit;
- compare two commits, with an indication that the forge truncated the list;
- list the files of a pull request;
- read the App's own identity: its slug and the account of its bot;
- mint an installation token for given repositories and permissions, and revoke a token;
- create and update a check run, and create a commit status;
- download a repository's tarball at a commit.

The GitHub App implementation signs App JWTs with RS256 from the configured private key and caches
installation tokens. It follows pagination, uses conditional requests, and turns rate-limit
responses into an error that carries the reset time.

`bosun-testkit` gains the fake forge, a local HTTP server with:

- in-memory repositories, commits, pull requests and installations;
- the token endpoints, which verify the App JWT against the App's public key and record each minted
  token's repositories and permissions;
- check runs and statuses, recorded for assertions;
- a sender of signed webhooks.

**Touches.** `crates/forge/` (new), `crates/testkit/`, `Cargo.toml`,
`docs/design/06-runtime-services.md`. The rows of `bosun-forge` in `hack/check-deps.sh` and
`docs/design/01-overview.md` already exist; the step checks them.

**Depends on.** nothing in M1.

**Out of scope.** The webhook endpoint (step 26), commits and pull requests (M4), the fake as a
process that serves git (step 46).

**Acceptance.**

- Against the fake: reading a directory at a commit returns names and blob hashes; a compare returns
  the changed files, with the truncation flag when the fake's limit is exceeded; pagination returns
  every file of a pull request.
- A minted token carries exactly the requested repository and permissions in the fake's record. The
  fake refuses a revoked token, and a JWT signed with another key.
- A rate-limited response becomes the error with its reset time. A `304` serves the cached body and
  does not count against the fake's rate limit.
- The `X-Hub-Signature-256` of the fake's webhooks matches an independent HMAC computation in the
  test.

**Review.** yes: the App's private key and the scope of installation tokens are security.

### 14. The S3 backend

**Goal.** The S3 backend of `bosun-blob`: streaming multipart put, ranged get, delete, and listing
by prefix. Path-style addressing is an option, and credentials come from the configuration or the
environment. The backend passes the conformance suite of M0, which gains the case of an abandoned
multipart upload.

In the server, `[store] kind = "s3"` selects the backend. Its probe joins `/readyz`, and the server
refuses to start while the store is unreachable. The step settles which S3-compatible server the
suite runs against, as a service in CI and in a container on a developer's machine. Its address is
in `BOSUN_TEST_S3_URL` next to `BOSUN_TEST_DATABASE_URL`. The choice is recorded in
design/07-storage.md and in `CLAUDE.md`.

**Touches.** `crates/blob/`, `crates/server/` (the store configuration), `crates/testkit/` (the
address of the test store), `.github/workflows/ci.yml`, `CLAUDE.md`, `docs/design/07-storage.md`,
`docs/design/12-operations.md`.

**Depends on.** nothing in M1.

**Out of scope.** Retention (step 36); downloads of artifacts and caches (M2).

**Acceptance.**

- In CI, the conformance suite passes for the S3 backend against the local S3 server, including that
  an abandoned multipart upload is aborted and leaves no object.
- A server configured with S3 compacts logs into it and serves them by range.
- The server refuses to start while the store is unreachable. `/readyz` reports it unready when the
  store stops answering, and ready when it answers again.

**Review.** no: a storage backend behind the conformance suite.

### 15. Planner with expressions and run notices

**Goal.** The server side of design/03-expressions.md. The planner builds the server's contexts:

- `github`, with step 7's derivation over the run's trigger and the facts the server holds;
- `inputs`, from the run's inputs, which stay empty until step 30;
- `vars`, empty until step 24;
- `needs`, with `result` and `outputs`;
- `bosun`.

It evaluates `run-name` when the run is created. When a job's `needs` are complete, it evaluates the
job's `if`, with the status functions over its ancestors and the implicit `success() &&`. It then
evaluates `name`, `runs-on`, `timeout-minutes`, `continue-on-error`, the job's `env` and `defaults`,
and stores them for the assignment.

During cancellation `cancelled()` is true, so a pending job whose `if` admits cancellation still
runs after `run.cancel`, as in the target. An evaluation error fails the job, with a reason the step
settles and adds to the reasons table of design/04-runs.md. The step also settles, with a case, what
an evaluation error in `run-name` does.

Jobs gain `outputs`. `JobFinished` gains the job's outputs, the server stores them, and the scripted
fake agent can report them. A job that fails under `continue-on-error` does not fail the run.

Run notices are new. The journal gains the kind `run.notice`. Its data is a diagnostic of
design/02-workflows.md, with the severity `error`, `warning` or `notice` and an optional job.
`GET /runs/{run}` returns a run's notices in the order of the journal. When a run is created, the
warnings of its revision become notices. An evaluation error adds an error notice at the
expression's span. Later steps add their notices through the same kind:

- the diagnostics of a `startup_failure` (step 27);
- a substituted Node runtime (step 32);
- a scope of the forge token that the App lacks (step 34).

design/04-runs.md gains `run.notice` in its kinds table and a table of the codes of runtime notices.

**Touches.** `crates/server/` (the planner, the run resource), `crates/db/` (migrations: job
outputs, evaluated values, the run's display name), `crates/types/`, `proto/` and `crates/proto/`
(outputs in `JobFinished`), `crates/testkit/` (the fake agent reports outputs),
`contracts/fixtures/events/`, `contracts/fixtures/api/`, `contracts/openapi.json`,
`docs/design/04-runs.md`.

**Depends on.** 1, 3, 4, 7.

**Out of scope.** Matrices (step 16), concurrency (step 17), label aliases (step 18), the workflow
`env` and carrying the values to the agent (step 20), rendering notices (step 41).

**Acceptance.**

- The property test of M0 drives random job graphs with random `if` expressions that use the status
  functions, and with random `continue-on-error`. The projected statuses always equal a replay of
  the journal.
- `if: failure()` runs only when an ancestor, direct or transitive, failed, and `if: always()` runs
  after a failed dependency. A plain `if: github.ref == 'refs/heads/main'` after a failed dependency
  completes as `skipped` with reason `if_false`.
- After `run.cancel`, a pending job with `if: always()` or `if: cancelled()` is queued, and the
  other pending jobs are cancelled.
- `timeout-minutes: ${{ fromJSON('x') }}` fails the job with the settled reason, and the run's
  notices hold an error at the expression's span.
- `runs-on: ${{ needs.setup.outputs.label }}` places the job by the output the fake agent reported
  for `setup` in `JobFinished`. `run-name: ${{ github.event_name }} by ${{ github.actor }}` becomes
  the run's display name.
- A run of a workflow whose step `if` has text around its `${{ }}` lists that warning among its
  notices, pinned by a fixture of `GET /runs/{run}`.

**Review.** yes: the run state machine and the journal.

### 16. Matrix strategies

**Goal.** Expand `strategy` when the job's `needs` are complete, as design/04-runs.md orders it. The
expansion covers vectors, `include` and `exclude`, and a matrix computed by an expression such as
`fromJSON(needs.setup.outputs.matrix)`, with the target's rules:

- an `include` entry extends every combination it matches, without overwriting an original value,
  and adds a combination when it matches none;
- an `exclude` entry removes every combination it partially matches.

The template becomes a job group (`group_id` and `matrix` on jobs). Each member is named with its
matrix values in parentheses, as the target names it, and sees the `matrix` and `strategy` contexts
(`fail-fast`, `job-index`, `job-total`, `max-parallel`). It then continues from step 3 of the
planning order in design/04-runs.md.

The group fails when the matrix is empty, naming the empty vector, and when it has more members than
the installation's limit of 256. `fail-fast`, true by default, cancels the remaining members when
one fails. `max-parallel` keeps the members beyond it `waiting`, with reason `max_parallel`. The
group's conclusion follows design/04-runs.md, taking each member's `continue-on-error` into account.
Expansion cases live in `contracts/fixtures/matrix/`.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/matrix/` (new),
`contracts/fixtures/events/`, `contracts/fixtures/api/`, `contracts/openapi.json`,
`docs/design/04-runs.md`.

**Depends on.** 15.

**Out of scope.** Job-level concurrency of members (step 17); check runs, which step 28 reports for
every job, members included; the graph (step 42).

**Acceptance.**

- The expansion cases pass. Among them are an `include` that extends combinations, an `include` that
  adds one, an `exclude` by a partial object, and a matrix from `fromJSON`.
- With `fail-fast`, a failing member cancels the queued and running members, and the group fails.
  With `fail-fast: false`, every member finishes.
- With `max-parallel: 2`, a property test never sees more than two members outside `waiting` and
  `completed`.
- 257 combinations fail the group with the limit in the message, and an empty vector fails it naming
  the vector.
- A member failing under `continue-on-error: ${{ matrix.experimental }}` leaves the group
  successful.

**Review.** yes: the run state machine, and matrix semantics the corpus pins.

### 17. Concurrency groups

**Goal.** The concurrency groups of design/04-runs.md. The workflow-level group applies to runs and
is evaluated when the run is created; the job-level group applies to jobs and is evaluated when the
job is ready. `cancel-in-progress` is evaluated together with its group.

- A group has at most one running member.
- With `queue: single`, it has at most one pending member; a newer arrival cancels it.
- `cancel-in-progress` also cancels the running member.
- With `queue: max`, up to 100 members wait in order of arrival.

Waiting members carry reason `concurrency`. A run held by its workflow-level group keeps its ready
jobs `waiting` with that reason, so the run is `waiting`, and its jobs are released together when
the group has room. The next member is released in the transaction that completes the running one.

The design leaves several things open, which the step settles with cases and records in
design/04-runs.md and design/07-storage.md:

- where groups are stored and locked, so that two replicas never admit two members;
- whether group names compare without regard to case, and whether runs and jobs share one namespace,
  as in the target;
- what happens to the 101st arrival under `queue: max`;
- what an evaluated `cancel-in-progress: true` next to `queue: max` does, and what an evaluation
  error in the workflow-level group does.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/concurrency/`
(new), `contracts/fixtures/events/`, `docs/design/04-runs.md`, `docs/design/07-storage.md`.

**Depends on.** 16.

**Out of scope.** Admission policies (M5) and environments (M3).

**Acceptance.**

- With two runs in one group, the second waits, its jobs with reason `concurrency` and the run
  itself `waiting`, and it starts when the first completes. Under `queue: single`, a third arrival
  cancels the waiting second.
- With `cancel-in-progress: true`, a new arrival cancels the running run, with the journal events of
  a cancellation.
- `cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}` cancels the running run of a branch
  on a new arrival, and leaves the running run of `main`.
- With `queue: max`, 100 members wait and start in order of arrival, and none is cancelled.
- Dispatches into one group that race through two replicas never leave two members running (a stress
  test with two server instances on one database).
- A job-level group with `${{ matrix.target }}` serialises only the members with equal values.

**Review.** yes: races between replicas and the run state machine.

### 18. Pools, label aliases and trust in the claim

**Goal.** Pools of design/05-execution.md with the parts M1 uses: name, trust level and access list
(`pool_access`). `/pools`, `bosun pool list` and `bosun pool create` manage them; design/11-api.md
gains `pool create` in its CLI listing.

Trust is enforced where jobs are claimed, as design/07-storage.md and design/08-access.md specify:

- Runs gain `trusted`, which is true for every run until step 29 marks the runs from forks.
- The claim matches the trust of a job's run with the pool's trust level in both directions,
  whatever the labels say. It offers a job only to agents of pools whose access list allows the
  job's project.
- The claim requires the capability a job needs: `podman` for a job whose alias names `podman` or an
  image, and either capability otherwise.
- An agent of an untrusted pool never holds `shell`. The server drops the capability, and its label,
  from the agent's `Hello`, logs a warning naming the agent and the pool, and so offers it no job
  that needs `shell`. In M1 only scripted fake agents can claim from an untrusted pool, because the
  agent has no `podman` yet.

`label_alias` maps a label such as `ubuntu-latest` to a pool, a capability and a default image, per
installation or per organisation; the organisation's alias takes precedence. `/label-aliases` and
`bosun alias set` manage them. When the planner evaluates `runs-on`, it replaces an aliased label by
the alias's pool and capability, so only that pool's agents claim the job. An alias that names
`podman` or an image is stored for M2, and its jobs wait for a `podman` agent.

**Touches.** `crates/server/`, `crates/db/` (the claim query, `pool_access`, `label_alias`, the
trust of runs), `crates/types/`, `crates/testkit/` (fake agents with chosen capabilities),
`bin/cli/`, `contracts/fixtures/api/`, `contracts/openapi.json`, `docs/design/05-execution.md`,
`docs/design/11-api.md`.

**Depends on.** 9, 15.

**Out of scope.** Marking runs from forks (step 29); the podman socket and privileged options of
pools (M2); address ranges of pools (M3); executor plugins (M5); draining and deleting agents (step
19).

**Acceptance.**

- `runs-on: ubuntu-latest` with an alias to pool `linux` is claimed only by an agent of that pool.
  Without an alias, the job is cancelled with `no_agent` after a shortened `max_queue_time`. An
  organisation's alias wins over the installation's.
- A pool's access list keeps the jobs of other projects away from its agents.
- Claim tests use a trusted and an untrusted pool whose scripted fake agents carry the same labels,
  with runs marked untrusted in the database. No trusted job reaches the untrusted pool, and no
  untrusted job reaches the trusted one.
- A fake agent that announces `shell` in an untrusted pool is listed without `shell`, and a queued
  job that needs `shell` is not offered to it.
- A developer cannot create a pool or an alias (`agent.manage`).

**Review.** yes: the trust boundary in the claim keeps untrusted code away from secrets.

### 19. Draining and deleting agents

**Goal.** Agent administration, which the agents page of design/09-frontend.md needs:

- `PATCH /agents/{agent}` with `drain`. Draining is stored on the agent, so it survives a reconnect,
  and is delivered as the `Drain` message through the control messages of the agent's current
  replica. The agent finishes its job and is offered no other.
- `DELETE /agents/{agent}`. It revokes the credential, ends the agent's stream on whichever replica
  holds it, and completes its execution as `failure` with reason `agent_removed`.
- `bosun agent drain` and `bosun agent delete`.

The step settles whether `drain: false` lifts a drain and what a drained agent does once it is idle,
and records both in design/05-execution.md.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `proto/` and `crates/proto/`
(`Drain`), `bin/agent/`, `bin/cli/`, `crates/testkit/` (the fake agent receives `Drain`),
`contracts/fixtures/api/`, `contracts/openapi.json`, `docs/design/05-execution.md`.

**Depends on.** 9.

**Out of scope.** Credential rotation (M3), the agents page (step 44).

**Acceptance.**

- An agent connected to replica B and drained through replica A finishes its job and receives no
  other, also after it reconnects.
- Deleting an agent in the middle of a job completes the job as `failure` with reason
  `agent_removed`, and the agent's credential is refused when it reconnects.
- A developer can neither drain nor delete an agent (`agent.manage`).

**Review.** yes: control messages across replicas, and the `agent_removed` path of the state
machine.

### 20. The M1 job assignment

**Goal.** The assignment of design/05-execution.md for M1. It carries:

- the job part of the IR;
- the contexts `github`, `needs`, `strategy`, `matrix`, `inputs`, `vars`, `secrets` and `bosun`;
- the values the server evaluated: `env`, `defaults`, `timeout-minutes` and `continue-on-error`. The
  workflow's `env` is evaluated at assignment, where design/03-expressions.md places it, and merged
  under the job's.

`StepFinished` gains the step's outputs, which the server stores.

The step settles two things the design leaves open, and records both in design/05-execution.md:

- the encoding: the IR and the contexts travel as JSON documents inside `Assignment`, since the IR
  is already serialised as JSON for the run snapshot. They replace the structured steps of M0.
- what an agent of the previous minor version does with an assignment it cannot read, so the job is
  not offered to it in a loop.

On the agent, every step receives the variables of step 7's environment function, with the facts of
the runner. The file that `GITHUB_EVENT_PATH` names holds the payload, and the workspace is
`work/<org>.<project>/<repository>`.

**Touches.** `proto/`, `crates/proto/`, `crates/server/` (building the assignment),
`crates/runner/`, `bin/agent/`, `crates/testkit/` (the fake agent reads the assignment),
`docs/design/05-execution.md`.

**Depends on.** 15.

**Out of scope.** Evaluating step expressions (step 21), secrets (step 24), resolved `uses` (step
32), the forge token (step 34), the variables of the runtime services (M2).

**Acceptance.**

- In a scenario with a real agent, a step's environment holds the variables of step 7's dispatch
  fixture with the values of the run, and `GITHUB_EVENT_PATH` holds the payload.
- A dependent job's assignment carries in `needs` the result of its dependency and the outputs the
  scripted fake agent reported for it in `JobFinished`.
- A key in both the workflow's and the job's `env` takes the job's value.
- An agent given an assignment with a field it does not know rejects it, naming the field, and the
  job is not offered to that agent again.

**Review.** yes: the agent protocol, and the environment variables the toolkit reads.

### 21. Steps on the agent

**Goal.** The main phase of the execution engine, with expressions. For each step, the agent:

- evaluates its `if`, with the status functions over the previous steps and the implicit
  `success()`;
- renders `name`, `run`, `env`, `with`, `shell` and `working-directory`;
- keeps the `steps` (outputs, outcome, conclusion), `job` (status), `runner` (name, os, arch, temp,
  tool cache, debug, environment) and `env` contexts;
- applies `defaults.run`, and `continue-on-error` with the step's outcome and conclusion;
- enforces the step's `timeout-minutes` and the job's, whose default is 360 and which counts only
  time in progress.

After a cancel, the agent still runs the remaining steps whose `if` admits cancellation.

Before each step, the agent creates fresh `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH`,
`GITHUB_STATE` and `GITHUB_STEP_SUMMARY` files. After the step it reads the first four, including
the `name<<DELIMITER` form. At the end of the job it evaluates the job's `outputs` and reports them.
`hashFiles` is implemented here as design/03-expressions.md specifies it.

The step settles, with cases, the conclusion a timeout gives the step and the job, and what
`needs.<job>.result` then shows. It records both in design/05-execution.md.

**Touches.** `crates/runner/`, `bin/agent/`, `contracts/fixtures/file-commands/` (new),
`contracts/fixtures/hash-files/` (new), `docs/design/05-execution.md`.

**Depends on.** 20.

**Out of scope.** Workflow commands and the step summary (step 23), masking (step 22), actions (step
33).

**Acceptance.**

- `if: failure()` runs only after a failed step, and `if: always()` runs after a cancel. A step
  whose `if` has no status function is skipped after a failure.
- A failing step with `continue-on-error: true` has outcome `failure` and conclusion `success`, and
  the job succeeds.
- A value written to `GITHUB_OUTPUT`, also in the heredoc form, is `steps.<id>.outputs.<name>` in a
  later step and in the job's `outputs`, and it reaches a dependent job's `needs`. `GITHUB_ENV` and
  `GITHUB_PATH` affect only later steps.
- With the agent's clock scaled in the test, a step over its `timeout-minutes` is cancelled with
  M0's signal sequence and ends with the settled conclusion. So does a job over its own.
- `hashFiles` cases over a fixture tree equal hashes that the test computes independently with
  `sha256sum`.

**Review.** yes: step semantics the corpus pins.

### 22. Masking

**Goal.** The masker of design/05-execution.md. It holds every secret of the assignment, the forge
token, and every value step 23 adds with `::add-mask::`, whatever their length. It masks each value
together with its encoded forms:

- JSON-escaped, URL-encoded and XML-escaped;
- escaped for an expression string, for a command line and for PowerShell;
- with surrounding quotes trimmed;
- base64 at the three byte alignments.

A multi-line value is masked both as a whole and line by line.

Every text that leaves the agent passes through the masker. That covers log lines, error messages,
and the annotations and step summaries that step 23 adds. The design says this only of logs; the
step settles it for annotations and summaries and records it in design/05-execution.md. A job output
that contains a masked value is dropped with a warning in the job's log. Matching uses a
multi-pattern automaton, so the cost per line does not grow with the number of values.

**Touches.** `crates/runner/`, `bin/agent/`, `contracts/fixtures/masking/` (new),
`docs/design/05-execution.md`.

**Depends on.** 21.

**Out of scope.** Setting secrets (step 24), `::add-mask::` (step 23), the forge token (step 34).

**Acceptance.**

- A runner test uses an assignment carrying a secret that contains a quote, a backslash, a space and
  a newline. In the log the agent sends, `***` replaces all of these:
  - the raw value;
  - its `toJSON`;
  - its URL encoding;
  - its XML escaping;
  - its base64 with zero, one and two leading bytes.
- A multi-line secret is masked both when printed whole and when printed line by line.
- A job output containing a secret is dropped with a warning, and the dependent job sees an empty
  string.
- The message of a failed step's evaluation error that quotes a secret leaves the agent masked.

**Review.** yes: masking is security.

### 23. Workflow commands, problem matchers and step summaries

**Goal.** Lines on standard output are scanned for the workflow commands of design/05-execution.md:

- `::debug::`, shown when `ACTIONS_STEP_DEBUG` or `RUNNER_DEBUG` is set;
- `::notice`, `::warning` and `::error`, with `file`, `line`, `endLine`, `col`, `endColumn` and
  `title`;
- `::group::` and `::endgroup::`;
- `::add-mask::`, which feeds the masker of step 22;
- `::stop-commands::<token>` and its end;
- `::echo::on` and `::echo::off`;
- `::add-matcher::` and `::remove-matcher::`. Problem matchers turn output lines into annotations.
  Their patterns have `regexp`, `file`, `fromPath`, `line`, `column`, `severity`, `message`, `code`
  and `loop`.

Data and properties are unescaped the way the toolkit escapes them. The deprecated `::set-output`
and `::save-state` are honoured with a warning. `::set-env` and `::add-path` are refused unless the
step sets `ACTIONS_ALLOW_UNSECURE_COMMANDS=true`. Annotations are limited to 10 of each kind per
step and 50 per job.

The step summary is read after each step and cut at 1 MiB with a notice. Annotations and the summary
travel inline in `StepFinished`. This changes the design, which describes an uploaded summary
referenced from `StepFinished`. The step updates the protocol table of design/05-execution.md and
the section on step summaries of design/06-runtime-services.md. It also sets the message size limit
of the agent stream, so that a summary of 1 MiB together with the annotations of a step fits with
room to spare.

**Touches.** `crates/runner/`, `bin/agent/`, `proto/`, `crates/proto/`, `crates/server/` (the
message size limit), `contracts/fixtures/workflow-commands/` (new), `contracts/fixtures/matchers/`
(new), `docs/design/05-execution.md`, `docs/design/06-runtime-services.md`.

**Depends on.** 21, 22.

**Out of scope.** Storing annotations and summaries on the server (step 35), rendering them (step
43).

**Acceptance.**

- Fixture outputs using every command produce their committed annotations and log, including escaped
  data and properties.
- The eleventh warning of a step and the fifty-first annotation of a job are dropped, as the target
  drops them.
- Problem matcher fixtures in the formats that the setup actions of common toolchains register
  (eslint-stylish, tsc, gcc) turn fixture output into the committed annotations, also through a
  multi-line `loop` pattern. After `::remove-matcher::`, the matcher stops.
- `::add-mask::v` masks `v` in the lines after it. `::stop-commands::t` suspends commands until
  `::t::`.
- `::set-env` behaves as its committed case without the opt-in, and sets the variable with it.
- An annotation message and a step summary that contain a secret leave the agent masked.
- The server accepts a step summary of exactly 1 MiB with ten annotations of each kind without
  ending the stream. A larger summary arrives cut at 1 MiB, with the notice.

**Review.** yes: the toolkit's command protocol.

### 24. Secrets and variables

**Goal.** Secrets and variables at organisation and project scope, as design/07-storage.md and
design/08-access.md specify. They live in the tables `secret` and `variable`. Each secret value is
encrypted at rest:

- with AES-256-GCM under its own data key;
- the data key is encrypted under the KEK and stored with the KEK's id;
- the associated data binds the ciphertext to its scope and name.

`keys.previous_kek_files` keeps earlier KEKs readable, and `bosun-server secrets rewrap` re-encrypts
every data key under the current one.

`/secrets` accepts values and never returns them; `/variables` returns values. Both are scoped by
`?organisation=` or `?project=`, and guarded by `secret.manage` and `variable.manage`. Names follow
the target's rules, pinned by cases. `bosun secret set` reads the value from standard input;
`bosun var set` takes it as an argument. A project's secret or variable shadows the organisation's
of the same name.

The assignment carries only the secrets the job references (step 4). A job that reads secrets by a
computed name gets every secret in scope. `vars` is filled on the server and in the assignment. The
step settles whether viewers see secret names, and records it in design/08-access.md.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `bin/server/` (`secrets rewrap`), `bin/cli/`, `docs/design/07-storage.md`,
`docs/design/08-access.md`.

**Depends on.** 4, 9, 21, 22.

**Out of scope.** Environment secrets (M3), secret backends (M5), keeping secrets from untrusted
runs (step 29), the pages (step 44).

**Acceptance.**

- A ciphertext copied onto another secret's row fails to decrypt. No response, log line or table
  holds a secret's value in clear: a test scans a database dump and the server's log for the value.
- After `secrets rewrap` with a new KEK, every secret decrypts with the new KEK alone.
- A developer's write of a secret is refused, and a maintainer's succeeds.
- A job that references `secrets.A` receives `A` and not `B`; one that uses `secrets[format(...)]`
  receives both.
- In a scenario with a real agent, a secret is set through the API and a step prints it raw, as JSON
  and as base64. The stored log holds none of the three forms and shows `***` in their place.

**Review.** yes: secrets at rest and in jobs.

### 25. Project settings

**Goal.** Project settings as one document with `ETag` and `If-Match`, as design/11-api.md requires
for documents that people edit concurrently, at `GET` and `PATCH /projects/{project}/settings`. The
step settles that settings are a sub-resource of the project rather than fields of it, and records
it in design/11-api.md. M1's settings, with their defaults:

- `github_workflows`, off;
- `fork_policy`, one of `disabled`, `approval` and `automatic`, by default `disabled`;
- `token_permissions`, the forge token's permissions for workflows that declare none, by default
  `contents: read` and `metadata: read`;
- `report`, `checks` or `statuses`, by default `checks`.

Reading needs `project.view` and writing `project.manage`. Later steps add their settings to the
document with defaults; step 36 adds the retention periods.
`bosun project settings <org/project> [--set key=value]...` reads and changes the document.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `bin/cli/`, `docs/design/11-api.md`.

**Depends on.** 9.

**Out of scope.** Applying each setting (steps 27, 28, 29 and 34), the settings page (M2).

**Acceptance.**

- A new project's settings equal the committed fixture of defaults.
- An update with a stale `If-Match`, or without one, is `412 precondition_failed`.
- An unknown setting, and a `fork_policy` outside its values, are `invalid_input` naming the field.
- A viewer reads the settings, a maintainer cannot change them, and an admin can.
- `bosun project settings --set fork_policy=approval` changes the setting and prints the document.

**Review.** no: a stored document with optimistic concurrency; the steps that apply each setting are
reviewed.

### 26. Webhooks, installations and repository-bound projects

**Goal.** The forge enters the server. The configuration gains `[github]`: the App id, the private
key file, the webhook secret file, and the forge's `server_url` and `api_url`, which default to
GitHub's and which tests point at the fake.

`/hooks/github` works in this order:

1. It verifies `X-Hub-Signature-256` in constant time, before parsing anything.
2. It records the delivery id in `webhook_delivery`, so a redelivery is acknowledged but not
   processed again.
3. It stores the payload and answers.

Workers in every replica then claim stored deliveries with `SKIP LOCKED` and process them. This step
handles the installation events; steps 27 and 29 plan runs from the others.

`installation`, `installation_repositories` and `repository` events maintain `forge_installation`,
the repositories each installation can see, and each repository's default branch. The step settles
how an installation maps to an organisation, and records it in design/06-runtime-services.md. The
organisation is created from the account's login when none is mapped, and an owner binds one when
that slug is taken.

Projects are bound to a repository through `POST /projects` and `PATCH /projects/{project}`, and
follow a renamed repository by its id.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `docs/design/06-runtime-services.md`, `docs/design/12-operations.md`.

**Depends on.** 9, 13.

**Out of scope.** Planning runs from events (steps 27, 29, 30 and 31), the project settings (step
25).

**Acceptance.**

- A delivery with a wrong or missing signature is refused and stores nothing. A valid delivery is
  acknowledged, and stored once even when it arrives twice.
- A processing delay injected into the worker does not delay the acknowledgement.
- Deliveries stored through two replicas are each processed exactly once.
- An installation event maps the account to an organisation. Binding a repository the installation
  cannot see is refused.
- A repository renamed on the fake stays bound to its project.

**Review.** yes: webhook authentication, and processing across replicas.

### 27. Repository workflows and forge events

**Goal.** Runs from the forge's events, for repository-bound projects.

For each processed event, the server reads `.bosun/workflows/` at the event's commit through the
forge. When the project's `github_workflows` setting is on, it also reads `.github/workflows/`; a
file present in both is taken from `.bosun/workflows/` only. Each file is parsed and linted with its
path (step 6) once per blob hash, and a changed blob yields a new revision of a repository workflow.
Each file's `on` is matched with step 5's matcher, and a match plans a run with the payload and the
actor as its trigger. A disabled workflow (the column of M0) creates no run.

The events:

- `push` uses the changed files from the comparison of `before` and `after`. The step settles, with
  cases, the comparison for a push that creates a branch, for a tag push, for a deletion, and for a
  comparison the forge truncated, and records it in design/02-workflows.md.
- `repository_dispatch` arrives from the webhook, and from `POST /api/v1/projects/{project}/events`,
  which requires `run.dispatch` and builds step 7's payload.
- Every other delivered event matches by `types` against the payload's `action`.

An event whose sender is the App's own bot starts no run, unless it is `workflow_dispatch` or
`repository_dispatch`. A file with errors whose `on` matches produces a run that ends with
`startup_failure`. Its diagnostics become error notices of the run (step 15), and the warnings of
every linted file become warning notices of its runs. A file whose `on` cannot be parsed produces
such a run for every push that changes it.

**Touches.** `crates/server/`, `crates/db/` (repository workflows, the blob cache), `crates/types/`,
`contracts/fixtures/api/`, `contracts/openapi.json`, `docs/design/02-workflows.md`.

**Depends on.** 6, 15, 25, 26.

**Out of scope.** Pull requests and merge groups (step 29), dispatch inputs (step 30), schedules
(step 31), reporting (step 28), enabling and disabling through the API (step 38).

**Acceptance.**

- A push to `main` that changes `src/a.rs` runs a workflow filtered on `src/**` and not one filtered
  on `docs/**`. A tag push and a branch creation behave as their committed cases.
- A push whose sender is the App's bot starts no run, while a `repository_dispatch` it sends starts
  one.
- A file with a broken job and a matching `on` yields a run completed as `startup_failure`.
  `GET /runs/{run}` lists its diagnostics as error notices with their spans, pinned by a fixture. A
  file with a broken `on` yields such a run for a push that changes it, and none for a push that
  does not.
- A file under `.github/workflows/` that uses `bosun` produces runs whose notices hold the extension
  warning.
- A push that changes no workflow file causes no parse, as the cache's counters show.
- A file present in both directories runs once, from `.bosun/workflows/`.
- `issues` with `types: [opened]` runs for `opened` and not for `closed`.
- A disabled workflow whose `on` matches creates no run.
- `POST /projects/{project}/events` runs a `repository_dispatch` workflow, and is refused for a
  viewer.

**Review.** yes: which runs an event creates, `startup_failure` and the bot rule are workflow
semantics.

### 28. Reporting to the forge through the outbox

**Goal.** The outbox of design/07-storage.md and the reports of design/06-runtime-services.md.

Rows of `outbox` are written in the transaction of the journal event that causes them. Workers in
every replica claim them with `SKIP LOCKED` and retry with backoff from 1 second to 1 hour. A row is
marked dead after 24 hours. A row is dropped when a newer one with the same collapse key exists.

Every job of a forge-bound run is reported as a check run named as the target names it. The check
run carries Bosun's details address, the status and the conclusion. With `report: statuses` in the
project's settings, the same goes out as commit statuses with the context `bosun/<workflow>/<job>`.

The step settles how a `startup_failure` run is reported, and records it in
design/06-runtime-services.md. Until the administration pages exist, a dead row is logged at error
level and counted by step 39's metrics; design/07-storage.md says so.

**Touches.** `crates/server/`, `crates/db/`, `crates/forge/`, `crates/types/`,
`docs/design/06-runtime-services.md`, `docs/design/07-storage.md`.

**Depends on.** 16, 27.

**Out of scope.** Summaries and annotations in the check run (step 35), re-runs requested from the
forge (M2), approvals on the forge (M3), and the summary line for a job an operator completed (M3,
with the commands that set an operator conclusion).

**Acceptance.**

- While the fake forge fails, a job changes state five times. After the fake recovers, the job is
  reported once, with its latest state.
- Workers in two replicas never deliver one row twice, as the fake's request log shows.
- A job named `Build` with the matrix values `linux` and `18` is reported as `Build (linux, 18)`; a
  job without a name is reported by its key.
- With `report: statuses`, the fake records `bosun/ci/build` contexts and no check run.
- With the clock injected, a row that keeps failing for 24 hours is marked dead, logged at error
  level, and no longer attempted.
- A `startup_failure` run is reported as settled.

**Review.** yes: delivery across replicas, retries and collapsing.

### 29. Pull requests, merge groups, forks and trust

**Goal.** Runs for `pull_request`, `pull_request_target` and `merge_group`, with their filters:
`branches` against the base ref (for a merge group, the base ref of the queue), `paths` against the
pull request's files, and the default `types`. Each event gets its `github` context through step 7:

- for `pull_request`, the merge ref and the commit the server resolves it to through the forge;
- for `pull_request_target`, the base branch's commit and workflow file;
- for `merge_group`, the queue's ref and commit.

A pull request whose head repository differs from its base repository is from a fork, and the
project's fork policy applies:

- `disabled` creates no run, and reports a commit status saying so through the outbox. The step
  settles the status's context and records it in design/08-access.md.
- `approval` creates the run, and its jobs wait with reason `untrusted` until `run.admit`. The
  command arrives in this step with `bosun run admit` and requires `run.override`.
- `automatic` creates the run at once.

A run from a fork is untrusted:

- it is marked so, and step 18's claim keeps its jobs to untrusted pools;
- the assignment carries no secret except `GITHUB_TOKEN`, and keeps the variables;
- step 34 mints a read-only token for it.

`pull_request_target` runs are trusted. In M1 no real agent runs an admitted fork run, because
untrusted pools hold no `shell` agents (step 18) and `podman` arrives in M2. The tests therefore use
scripted fake agents.

**Touches.** `crates/server/` (planning, commands), `crates/db/` (the trust of runs),
`crates/types/`, `bin/cli/` (`run admit`), `contracts/fixtures/github/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `docs/design/04-runs.md`, `docs/design/08-access.md`.

**Depends on.** 18, 24, 28.

**Out of scope.** The read-only forge token (step 34), the execution rules of untrusted pools (M2),
environments (M3), the admit control on the run page (step 42).

**Acceptance.**

- A fork pull request in a project with the default policy creates no run, and the fake forge
  records a status that says why.
- Under `approval`, the jobs wait with reason `untrusted`. A developer's `run.admit` is refused,
  through the API and through `bosun run admit`. A maintainer's admits the run. Its jobs are then
  claimed by a scripted fake agent of an untrusted pool, and by no agent of a trusted pool with the
  same labels.
- An untrusted run's assignment holds no secret of the project, and a variable arrives.
- `pull_request` with `paths: [src/**]` runs only when the pull request's files touch `src/`, and
  its `github.sha` is the merge commit the fake reports. `pull_request_target` runs the base
  branch's file.
- `merge_group` with `branches: [main]` runs for a merge group into `main` and not for one into
  `release`.

**Review.** yes: untrusted code and secrets.

### 30. Dispatch inputs

**Goal.** `workflow_dispatch` accepts the declared `inputs`:

- values are checked against their types: `string`, `boolean`, `choice` within its options, `number`
  and `environment`;
- defaults are applied;
- a missing required input or an undeclared one is `invalid_input` naming `inputs.<name>`;
- `inputs` holds typed values, and `github.event.inputs` the strings of step 7's payload, as in the
  target;
- a repository-bound project takes a `ref`, a branch or a tag (the default branch when omitted),
  resolves it to a commit through the forge, and runs the workflow file at that commit;
- `bosun workflow dispatch --input k=v` passes them.

The step settles what an `environment` input accepts before environments exist in M3, and records it
in design/02-workflows.md.

**Touches.** `crates/server/`, `crates/types/`, `contracts/fixtures/api/`, `contracts/openapi.json`,
`bin/cli/`, `docs/design/02-workflows.md`.

**Depends on.** 27.

**Out of scope.** `workflow_call` inputs (M2), gate inputs (M3), schedules (step 31).

**Acceptance.**

- A dispatch with `flag=true` gives `inputs.flag == true` as a boolean and
  `github.event.inputs.flag == 'true'`. A number input is a number.
- An undeclared input, and a choice outside its options, are `invalid_input` naming the input.
- A dispatch with `ref: v1.0` on a repository-bound project runs the workflow file of the tag's
  commit, and one without `ref` runs the default branch's file.

**Review.** yes: typed `inputs` next to string `github.event.inputs` are workflow semantics the
corpus pins.

### 31. Schedules

**Goal.** The runs Bosun starts on a schedule. `schedule` rows are rebuilt from the default branch's
workflow files on every push to it, and from server-side workflows on every save. The cron scheduler
is a periodic task under an advisory lock, with minute resolution. It evaluates each cron in its
`timezone` and plans a run with step 7's payload. A disabled workflow's schedule creates no run.

The design leaves two things open, which the step settles with cases where the target defines them,
and records in design/02-workflows.md:

- how a cron behaves across a change to or from daylight saving time;
- the actor of a scheduled run.

**Touches.** `crates/server/`, `crates/db/` (`schedule`), `crates/types/`,
`docs/design/02-workflows.md`.

**Depends on.** 27.

**Out of scope.** `on.upstream` and `workflow_run` (M2), trigger plugins (M5).

**Acceptance.**

- With the clock injected, `30 2 * * *` in `Europe/Belgrade` fires on the settled instants on the
  nights of both daylight saving changes.
- With two replicas, every schedule fires once.
- A push to the default branch that removes a `schedule` stops it. A server-side workflow saved with
  a `schedule` fires.
- A disabled workflow's schedule creates no run.

**Review.** yes: the scheduler is a singleton across replicas.

### 32. The action store and Node runtimes

**Goal.** Actions and tools reach agents through the server, as design/05-execution.md specifies.

When a job is assigned, every `uses: owner/repo@ref` and `owner/repo/path@ref` is resolved to a
commit through the forge. Resolution uses the credentials of the organisation's installation, so
private actions of the organisation work. The tarball is fetched once per commit into
`actions/<owner>/<repo>/<commit>.tar.gz` (`action_ref`). The server reads the action's metadata from
the tarball with step 6's parser. The installation's runtime map then decides which Node version
runs the action's `using`. By default `node12`, `node16` and `node20` run on Node 24, and a
substitution adds a notice to the run (step 15).

For each action, the assignment carries the commit, a download address on the server, the metadata
and the Node version. Each of these fails the job before it reaches an agent, with a message naming
the action:

- a `uses` the installation cannot resolve;
- metadata with errors;
- `using: composite` or `docker`, which are `not_implemented` and name M2.

Node runtimes are served at `/_runtime/tools/node/<version>/<os>-<arch>[-musl].tar.xz`. Each is
fetched once from the configured upstream (`[tools]` in the configuration), checked against the
upstream's published checksums, and kept as `tool_object`.

The step settles, and records in design/05-execution.md:

- How an agent authenticates these downloads: with its credential, and only for the actions of an
  execution it holds.
- How an organisation without an installation resolves public actions. Such an organisation might
  have only repository-less projects. The candidate rules are through another installation of the
  server, unauthenticated within the forge's rate limit, or not at all, with a message naming the
  reason.

**Touches.** `crates/server/`, `crates/db/`, `crates/forge/`, `crates/types/`, `proto/` and
`crates/proto/` (resolved actions in the assignment), `contracts/fixtures/api/`,
`docs/design/05-execution.md`, `docs/design/12-operations.md`.

**Depends on.** 6, 20, 26.

**Out of scope.** Running actions (step 33), the action policy (M2), composite, container and local
actions (M2).

**Acceptance.**

- Two assignments of `actions/checkout@v4` download its tarball from the fake forge once. Moving the
  tag after an assignment does not change the commit that job receives.
- An agent's download of an action belonging to an execution it does not hold is refused.
- A Node archive whose checksum differs from the upstream's list is refused and not stored. A second
  agent's request for a stored archive is served from the store.
- A `uses` of a repository the installation cannot see, and an action declared `using: composite`,
  each fail the job with a message naming the action, and no assignment is made.
- An action declared `node16` is assigned Node 24, and the run's notices say so.
- A public action used by an organisation without an installation resolves, or fails, as the settled
  rule says.

**Review.** yes: the code that agents run with secrets, and who may fetch it.

### 33. Node actions with pre and post steps

**Goal.** The agent runs Node actions. For each job, it:

- fetches each action and the Node runtimes the assignment names from the server, into
  `work/_actions/<owner>/<repo>/<commit>` and `externals/node20` or `externals/node24` (the musl
  build on a musl host);
- runs `main` with the inputs as `INPUT_<NAME>`, uppercased with spaces as `_`. Defaults from the
  metadata in the assignment are evaluated on the agent, so `${{ github.token }}` works. A missing
  required input, and a deprecated one, produce a warning.

`pre` steps run before the main steps, in the order of their actions, when `pre-if` holds (default
`always()`). `post` steps run after them, in reverse order, when `post-if` holds (default
`always()`), with the `STATE_*` variables their main step saved. Pre and post steps are steps of
their own in the journal.

The agent sets `GITHUB_ACTION_PATH`, `GITHUB_ACTION`, `GITHUB_ACTION_REPOSITORY`,
`GITHUB_ACTION_REF` and the `github.action*` properties. Outputs come through `GITHUB_OUTPUT`.

The fixture actions are small JavaScript actions built with the published toolkit. They are
committed with their bundled `dist/`, so tests need no package registry. They live under
`compat/fixtures/actions/`, the only path under `compat/` this step adds, and the fake forge serves
them. Tests take the Node found on the host and pack it into a local upstream directory with its
checksum list.

**Touches.** `crates/runner/`, `bin/agent/`, `crates/server/` (pre and post steps in the journal),
`crates/testkit/` (the fake forge serves the fixture actions), `compat/fixtures/actions/` (new),
`docs/design/05-execution.md`.

**Depends on.** 23, 32.

**Out of scope.** Composite, container and local actions, and the execution rules of untrusted pools
(M2); the forge token (step 34).

**Acceptance.**

- The test uses two fixture actions that each have `pre`, `main` and `post`:
  - both pre steps run in order before the first main step;
  - both post steps run in reverse order after the last main step;
  - each post step sees the state its main step saved.
- `post-if: failure()` runs only after a failure, and a post step still runs after a cancel.
- A fixture action uses the toolkit's `getInput`, `setOutput`, `saveState`, `exportVariable`,
  `addPath` and `setSecret`. Every value round-trips, and the secret it sets is masked.
- A missing required input produces a warning. An action declared `node16` runs on the Node 24 the
  assignment names, as its printed `process.version` shows.

**Review.** yes: the toolkit's protocols, and action semantics the corpus pins.

### 34. The forge token

**Goal.** The forge token of design/06-runtime-services.md. For each execution, the server mints an
installation token restricted to the run's repository and to the permissions derived from the
workflow's and the job's `permissions`:

- without `permissions`, the project's `token_permissions` setting;
- `read-all`, `write-all` and the per-scope form, intersected with what the App holds, with a
  warning notice on the run (step 15) for every scope the App lacks;
- read-only for an untrusted run, whatever it asks.

The token is `secrets.GITHUB_TOKEN` and `github.token`, and it is masked. It is revoked when the
execution ends, is cancelled or is lost. An execution still running when its token approaches expiry
gets a fresh one.

The step settles, and records in design/05-execution.md and design/06-runtime-services.md:

- the protocol message by which the agent asks for a fresh token, and how later steps see the new
  value;
- what the jobs of a repository-less project receive.

**Touches.** `crates/server/`, `crates/forge/`, `crates/db/`, `proto/`, `crates/proto/`,
`crates/runner/`, `bin/agent/`, `crates/testkit/`, `docs/design/05-execution.md`,
`docs/design/06-runtime-services.md`.

**Depends on.** 22, 29.

**Out of scope.** The job token and the runtime services (M2); `id-token: write` beyond accepting
it.

**Acceptance.**

- A job with `permissions: { contents: read, issues: write }` receives a token that the fake records
  with exactly those permissions, on the run's repository alone.
- `write-all` is intersected with the App's permissions, and the missing scopes appear as a warning
  notice on the run.
- An untrusted run's token is read-only, whatever its workflow asks.
- The fake records the token's revocation when the execution completes, also after a cancel.
- With the fake's token lifetime shortened, a later step of a long job receives a fresh token that
  the fake accepts.
- The token is masked in the log.

**Review.** yes: the scope and the lifetime of a credential handed to jobs.

### 35. Step results on the server

**Goal.** The server stores and serves what steps report besides their logs:

- annotations from `StepFinished` go to `annotation`;
- step summaries go to the object store under `summaries/<execution>/<step>.md`, recorded in
  `step_summary`;
- executions and steps expose both through the API.

Each job's check run gains a summary built from its step summaries, cut to the forge's limit, and
its annotations, sent in batches of 50.

A step's log can be searched with `?search=` on the log endpoint, over live chunks and, by streaming
decompression, over compacted objects, as design/07-storage.md describes. The search returns the
matching lines with their numbers. `bosun job logs --grep` uses it.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `bin/cli/`, `docs/design/11-api.md`.

**Depends on.** 23, 28.

**Out of scope.** Rendering (step 43), retention (step 36), MCP (M3).

**Acceptance.**

- A job with 120 annotations shows all of them, with file and line, through the API, and the fake
  forge receives them in three requests of at most 50.
- A step summary is stored once and served back byte for byte. The check run's summary concatenates
  the job's summaries and is cut at the forge's limit, with a notice.
- A search returns the same lines with the same numbers before and after compaction, and
  `bosun job logs --grep` prints them.
- A user without `project.view` on the run's project cannot read its annotations, summaries or logs.

**Review.** no: storing and reading reports, with no new trust boundary.

### 36. Retention

**Goal.** Retention, as design/07-storage.md specifies it, for what exists in M1: logs and step
summaries, inbound webhook payloads, and runs under `run_retention`. The periods join the project
settings of step 25 with the defaults of design/07-storage.md, capped by installation maximums in
the configuration. A periodic task under an advisory lock removes what has expired, from the
database and from the object store. A webhook delivery keeps its id after its payload is removed, so
redeliveries are still recognised.

The step settles how the API answers for a log or a summary that retention removed, so the run page
can show it as expired. It records the answer in design/11-api.md and the configuration in
design/12-operations.md.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `docs/design/07-storage.md`, `docs/design/11-api.md`,
`docs/design/12-operations.md`.

**Depends on.** 25, 35.

**Out of scope.** Retention of artifacts and caches (M2), of terminal recordings and of audit events
(M3).

**Acceptance.**

- With the clock injected:
  - logs and summaries older than the project's setting are removed from the store and the database,
    and the API answers as settled;
  - webhook payloads older than 30 days are removed, while their delivery ids still deduplicate;
  - runs older than `run_retention` disappear with everything they own, and newer runs stay.
- A project setting above the installation's maximum is `invalid_input`.
- With two replicas, the task runs on one at a time, and each expired object is removed once.

**Review.** yes: a singleton task across replicas that deletes runs and objects.

### 37. The event stream and the queue

**Goal.** `GET /api/v1/events` of design/11-api.md: a server-sent event stream filtered by
`project`, `run` and `kind`. It is fed across replicas by `NOTIFY bosun_events` from the journal
writer. It carries the events of runs, and also events that belong to no run: agents changing state,
and the queue changing. Payloads are the objects the REST resources return, and every event is
filtered by the caller's permissions.

The design leaves three things open, which the step settles and records in design/11-api.md and
design/07-storage.md:

- where the events without a run are kept;
- how their sequence and the journal's are encoded together in one `Last-Event-ID`;
- who may receive them.

`GET /api/v1/queue` lists queued and waiting jobs with their reasons, their waiting time, and the
labels that no online agent offers.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/fixtures/events/`, `contracts/openapi.json`, `docs/design/07-storage.md`,
`docs/design/11-api.md`.

**Depends on.** 9.

**Out of scope.** The pages (steps 41 and 44), outgoing webhooks (M5).

**Acceptance.**

- A subscriber on replica A receives, once and in order, the events of a run driven through replica
  B.
- Reconnecting with `Last-Event-ID` after a gap delivers exactly the missed events, of both
  sequences.
- A viewer of project P receives no event of project Q, through any filter.
- `/queue` names the missing labels of a job whose labels no online agent offers.

**Review.** yes: delivery across replicas and reconnects, and permissions per event.

### 38. Projects, workflows and runs in the CLI

**Goal.** What M1's features need from the run list and the CLI:

- `PATCH /workflows/{workflow}` with `enabled`, which needs `workflow.manage`. A disabled workflow
  is refused at dispatch as in M0, and the triggers of steps 27 and 31 skip it.
- The run list gains the filters workflow, branch, event, status and actor. Each workflow carries
  its latest run, on the default branch when its project is bound to a repository.
- `bosun project list|show`, `bosun workflow list|show|enable|disable`, `bosun run list|show|watch`
  and `bosun job cancel`, printing tables, or JSON with `--json`.

The step settles the exit status of `bosun run watch` for each conclusion of the run, and records it
in design/11-api.md.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `bin/cli/`, `docs/design/11-api.md`.

**Depends on.** 9, 26.

**Out of scope.** `bosun run journal` and the commands of M3; `bosun exec` (M2).

**Acceptance.**

- A maintainer disables a workflow, and its dispatch is then refused with the reason. A developer's
  `PATCH` is refused.
- `/runs` filters by each of the five fields, alone and combined, pinned by fixtures.
  `bosun run list --status failure --json` prints what the API returns.
- A workflow of a bound project carries its latest run on the default branch, and not a newer run on
  another branch.
- `bosun job cancel` cancels one job, and its dependents see `cancelled`. `bosun run watch` follows
  a run to its end and exits with the settled status for a failed run.

**Review.** no.

### 39. Metrics

**Goal.** `/metrics` in Prometheus format, as design/12-operations.md lists it, for what exists in
M1:

- queue length and waiting time by pool and label set;
- jobs by status;
- run and job durations;
- agents by state;
- the outbox's backlog, failures and dead rows;
- the log ingestion rate;
- webhook processing time and verification failures.

Values that live in Postgres are read when the endpoint is scraped, and are the same on every
replica. Counters of a replica's own work belong to that replica. The step settles how `/metrics` is
kept off the public address, and records it in design/12-operations.md together with a table of the
series.

**Touches.** `crates/server/`, `crates/db/` (aggregate queries), `docs/design/12-operations.md`.

**Depends on.** 28.

**Out of scope.** Plugin metrics (M5); dashboards and alerts (an operator task).

**Acceptance.**

- A test reads the table of series and checks that every series appears in the exposition, which
  parses with a Prometheus text parser.
- A scenario has a queued job, an online agent, and an outbox row that the fake forge refuses for 24
  hours of injected clock. Afterwards, the queue length, the agents by state and the dead rows have
  the expected values.
- A webhook with a wrong signature increments the verification failures.
- A request that does not reach `/metrics` through the settled protection is refused.

**Review.** no.

### 40. The web interface: scaffold, embedding and sign-in

**Goal.** `web/` as design/09-frontend.md specifies it. The package is `@globalart/bosun-web`:

- pnpm, TypeScript in strict mode, React 19 and Vite;
- Radix Themes, lucide and Geist, with a light and a dark theme;
- TanStack Query and React Router 7;
- English and Russian strings from the dictionaries of the interface's OwnLate project, pulled by
  the build, with a test that fails on a key the dictionaries lack.

The API client's types are generated from `contracts/openapi.json` by the build, so a change of the
document that breaks the interface fails `pnpm build`. The client sends `X-Bosun-CSRF` and reads the
error envelope, and Vitest checks it against `contracts/fixtures/api/`. The shell has navigation,
the user menu, and the switches for theme and language. The sign-in page offers password sign-in,
the providers of `/auth/methods`, and the forced password change.

The built interface is embedded in `bosun-server`. Hashed assets are served as immutable, and
`index.html` with `no-cache`. The client's routes are answered with `index.html`, while `/api`,
`/hooks`, `/_runtime`, `/twirp` and gRPC keep their own answers.

The step settles two things and records them in design/09-frontend.md: how `cargo build` behaves
when `web/` has not been built, and how CI runs Playwright on the self-hosted runner. CI gains
`pnpm lint`, `pnpm test` and `pnpm build`. The image builds the interface. `hack/e2e.sh` gains a run
of the Playwright scenarios under `web/e2e/`, starting with the sign-in scenario.

**Touches.** `web/` (new), `crates/server/` (serving the interface), `Dockerfile`,
`.github/workflows/ci.yml`, `hack/e2e.sh`, `docs/design/09-frontend.md`.

**Depends on.** 1, 8.

**Out of scope.** Every page except the shell and sign-in (steps 41 to 44).

**Acceptance.**

- `pnpm lint && pnpm test && pnpm build` is green in CI. A key used in the code but missing from the
  pulled dictionaries fails `pnpm test`.
- The server answers:
  - `/assets/<hash>.js` with an immutable cache header;
  - `index.html`, and a client route such as `/runs/<id>`, with `no-cache` and the page;
  - an unknown `/api/v1/` path with the JSON `not_found`.
- In `hack/e2e.sh`, Playwright signs in with `admin.password`, sets a new password, reaches the home
  page and signs out, in both themes.
- An accessibility check of the sign-in page in both themes finds no contrast or focus violation.

**Review.** no.

### 41. Home, project and run pages

**Goal.** The pages where runs are found and watched:

- **Home:** recent runs of the projects the user can see, failing workflows, and the queue in short.
- **Project:** its workflows with the status of their latest run, and the run list with filters by
  workflow, branch, event, status and actor (step 38).
- **Run:**
  - the header: status and conclusion with their source, trigger, commit, actor, attempt and
    duration;
  - the run's notices, each with its severity, its message and the file and line of its span, errors
    first;
  - the jobs with their status, and waiting jobs with their reason, as a list that step 42 turns
    into a graph;
  - the job panel: steps with nested pre and post steps, durations and conclusions.

`run.cancel` and `job.cancel` appear where they are valid. Everything is live: the event stream
patches the query cache or invalidates it, and reconnects with `Last-Event-ID`.

**Touches.** `web/`, `docs/design/09-frontend.md`.

**Depends on.** 15, 37, 38, 40.

**Out of scope.** The graph and the admit control (step 42); logs, annotations and summaries (step
43); the commands of M3.

**Acceptance.**

- Vitest: each run, job and step event of a fixture stream patches the cached objects to the state
  the fixture expects.
- Vitest: a fixture run with an error notice and a warning notice renders both with their file and
  line, the error first.
- Playwright: the jobs of a dispatched two-job workflow turn green on the run page without a reload,
  and the cancel button cancels a running run.
- Playwright: a viewer sees no cancel button, and the API refuses the command anyway.

**Review.** no.

### 42. The job graph and admission

**Goal.** The graph of jobs of design/09-frontend.md on the run page, drawn with React Flow and laid
out with ELK:

- job groups collapsed with member counts, and expandable;
- edges from `needs`;
- colours by status;
- waiting jobs with their reason.

The graph is live. A run whose jobs wait with reason `untrusted` shows `run.admit` to those with
`run.override`.

**Touches.** `web/`.

**Depends on.** 16, 29, 41.

**Out of scope.** Gates (M3), reusable workflow groups (M2).

**Acceptance.**

- Vitest: the layout of a fixture run with a matrix group collapses the members into one node with
  their count, and expanding it shows each member named with its matrix values.
- Vitest: a job event of a fixture stream changes the status of its node.
- Vitest: for a fixture run waiting with reason `untrusted`, the admit control is shown with a
  maintainer's permissions and hidden with a developer's.
- Playwright: the graph of a dispatched workflow with `needs` draws one edge per dependency and
  colours each node as its job completes.

**Review.** no.

### 43. Logs, annotations and step summaries on the run page

**Goal.** The log viewer of the job panel, as design/09-frontend.md describes it:

- virtualised, with ANSI colours;
- collapsible groups from `::group::`;
- optional timestamps and links to lines;
- search inside a step, through step 35's search;
- raw download;
- following a running step through `?follow=true`.

Annotations appear at their lines, and as a list per job. Step summaries are rendered as sanitised
Markdown: no raw HTML except the tags the target allows, no scripts, and images only from `https`
addresses.

**Touches.** `web/`.

**Depends on.** 35, 41.

**Out of scope.** Artifacts (M2), the terminal (M3).

**Acceptance.**

- Vitest: a corpus of hostile summaries renders none of what it carries. The corpus includes
  `<script>`, event handler attributes, `javascript:` links, `http` images and nested markup. ANSI
  sequences and log lines cannot inject markup.
- Vitest: a log of 100 000 lines renders only the visible lines, and collapsing a group hides its
  lines.
- Playwright: an annotation of a failing step appears at its line, and a followed step appends its
  lines live.

**Review.** yes: summaries and logs come from job output, which is untrusted markup.

### 44. Queue, agents, secrets and variables pages

**Goal.** The queue and agents page:

- the live queue, with each job's waiting time and the labels no online agent offers;
- agents by pool, with their state, current job, version, labels and capabilities, and drain and
  delete;
- registration tokens, with the one-line install command shown once;
- pools and label aliases, for those with `agent.manage`. An untrusted pool's page says that Bosun
  cannot check the isolation of its network, and that agents there get no `shell`.

The settings of organisations and projects gain secrets and variables. Secrets are write-only: the
page shows names and dates, and offers set, replace and delete. Controls are shown only to those
whose permissions allow them.

**Touches.** `web/`.

**Depends on.** 18, 19, 24, 37, 40.

**Out of scope.** The project settings page (M2); environments, teams, roles and the audit trail in
the settings (M3).

**Acceptance.**

- Playwright: creating a registration token shows the install command once.
- Playwright: after setting a secret, only its name and date are shown.
- Playwright: draining an agent shows it as draining.
- Playwright: a developer sees no control to write a secret, and the API refuses the write anyway.
- Vitest: the queue's reasons and missing labels render from fixture events.

**Review.** no.

### 45. The compatibility corpus

**Goal.** `compat/`, the compatibility corpus of design/13-decisions.md. The step writes
`compat/README.md` with its layout:

- `workflows/`: the workflow files of the organisations' repositories, by repository;
- `inventory/`: what they use;
- `recordings/`: results recorded on GitHub's hosted runners;
- `fixtures/actions/`: Bosun's own fixture actions built with the toolkit (step 33);
- `actions/<owner>/<repo>/<ref>/`: vendored upstream actions, each with its upstream commit and
  SHA-256 (from step 46 on).

The inventory:

- `bosun workflow inventory <dir>` walks a directory of workflow files. It writes into
  `compat/inventory/` every `uses:`, with its versions and counts, and every key path, with its
  count and the milestone it arrives in according to the parser.
- `hack/compat-sync.sh` fetches the workflow files of every repository of the given organisations
  into `compat/workflows/`, for the operator to commit.
- A test parses every workflow under `compat/workflows/` and fails on an `unknown_key`.

The expression recordings:

- `hack/compat-recordings.py generate` turns the cases of `contracts/fixtures/expr/` into a workflow
  for the public compatibility repository, which prints each case's result between markers. With
  `--check` it fails when the committed workflow is stale, and CI runs it so.
- `hack/compat-recordings.py import` turns the log of that workflow's run into
  `compat/recordings/expr.json`.
- A test fails when Bosun's result differs from a recorded one.

The step settles which cases a hosted runner can reproduce, with contexts supplied only through
`env`, `matrix` and literals. It marks the others, and records the rule in design/03-expressions.md.

Bosun's own simple workflows go under `.bosun/workflows/`. They are the checks that need no service
container: formatting and the dependency rules. A test checks that they validate without
diagnostics.

**Touches.** `compat/` (new, except `compat/fixtures/actions/`), `bin/cli/` (`workflow inventory`),
`hack/compat-sync.sh` (new), `hack/compat-recordings.py` (new), `crates/expr/` (the comparison
test), `crates/workflow/` (the corpus test), `contracts/fixtures/expr/`, `.bosun/workflows/` (new),
`.github/workflows/ci.yml`, `docs/design/03-expressions.md`, `docs/design/11-api.md` (the CLI
listing).

**Depends on.** 3, 6.

**Out of scope.** Collecting the real workflows and running the recordings on hosted runners
(operator tasks); running the simple workflows (step 46).

**Acceptance.**

- The inventory of a fixture tree equals its committed inventory.
- The corpus test, pointed at a scratch directory that holds a workflow with an unknown key, fails
  naming the file and the key.
- `hack/compat-recordings.py generate --check` passes in CI.
- The importer turns a fixture log into recordings equal to a committed file. The comparison fails,
  naming the case, when one recorded result differs from Bosun's.
- Every file under `.bosun/workflows/` validates without diagnostics.

**Review.** yes: the corpus is what pins compatibility.

### 46. End to end against the fake forge

**Goal.** `hack/e2e.sh` runs the scenarios of the M1 exit checklist against real binaries, with the
fake forge running as a process.

The fake's address is `GITHUB_SERVER_URL`, set to a `*.localhost` host such as
`http://forge.localhost:<port>`. The toolkit then treats it as GitHub rather than as an enterprise
server, which M2's artifact and cache scenarios rely on. `GITHUB_API_URL` points at the fake's REST
paths.

The fake gains what `actions/checkout` needs. The first is the git smart HTTP protocol, served by
`git http-backend` over its repositories, which accepts the installation tokens the fake minted. The
second is the API paths the toolkit uses. The step settles how `forge.localhost` resolves for git on
the CI host, for example through git's `http.curloptResolve` in the agent's environment or through
nss-myhostname.

Nothing is fetched from the internet. `actions/checkout@v4` is vendored in
`compat/actions/actions/checkout/v4/`, the layout M2 uses, with its upstream commit and the SHA-256
of the tarball, and the fake serves it. The Node runtime is the CI host's Node, packed into the
configured upstream directory.

The scenarios:

- A push webhook for a repository whose `.bosun/workflows/ci.yml` checks out, runs a matrix of two,
  and passes job outputs to a dependent job. The run succeeds, with one check run per job on the
  fake.
- A fork pull request under the default policy creates no run.
- Through the CLI, a viewer's dispatch and a developer's write of a secret are refused. The scenario
  creates both users through the API, and each signs in with a password and creates a token.
- A secret printed raw, as JSON and as base64 is masked in the stored log.
- The current commit is pushed into the fake as `GlobalArtInc/bosun-ci`, and every workflow under
  `.bosun/workflows/` runs on the shell agent.

The step settles how the fake forge runs as a process, and how its address resolves, and records
both in design/12-operations.md.

**Touches.** `hack/e2e.sh`, `crates/testkit/` (the fake as a binary, git serving, the toolkit's API
paths), `compat/actions/actions/checkout/` (new), `.github/workflows/ci.yml`,
`docs/design/12-operations.md`.

**Depends on.** 10, 33, 34, 45.

**Out of scope.** The real GitHub App, the test repository, and Bosun's workflows on the real
`GlobalArtInc/bosun-ci` (operator tasks).

**Acceptance.**

- `hack/e2e.sh` runs the M1 and M0 scenarios and is green in CI.
- After the push scenario, the fake forge's record holds one check run per job, both matrix members
  included, each with conclusion `success`. The dependent job's log shows the output value.
- The `GITHUB_SERVER_URL` that a step of the push scenario prints has a host ending in `.localhost`.
- After the fork scenario, no run of the workflow exists, and the fake records the status of the
  fork policy.
- The stored log of the secret scenario contains none of the raw, JSON and base64 forms of the
  secret, and contains `***`.
- Every run of a workflow under `.bosun/workflows/` completes with conclusion `success`.

**Review.** no: a test harness over behaviour that the steps before it reviewed.

## M2 work breakdown

M2 is split into twenty-four steps. Each step is one change to `main` and is done when its
acceptance holds in CI. Four steps need nothing else in M2 and come first: step 1 lowers the M2
syntax, step 2 prepares the end-to-end harness, step 3 is the `bosun-services` crate and step 4 the
action policy. Four lines then run in parallel: the runtime services (5 to 8, and `bosun exec` in
21), the podman executor and the actions it runs (9 to 14), the planner, the queue and re-runs (15
to 20), and at the end the corpus of real workflows and the interface (22 to 24). Remarks that apply
to every step:

- M1 is assumed to deliver its scope in full: `bosun-expr`, the whole surface known to the parser,
  the GitHub App and the fake forge in `bosun-testkit` (REST, token endpoints, webhooks, tarballs
  and git over smart HTTP), the action store with resolution to a commit, Node runtimes under
  `/_runtime/tools/`, the outbox and check runs, masking, secrets, roles, trust levels in the claim,
  the event stream, the web interface with its OwnLate project, `contracts/schema/workflow.json`,
  `contracts/openapi.json` with its CI check, and `hack/e2e.sh` with `actions/checkout@v4` vendored
  under `compat/actions/`. When a piece is missing, the step that needs it adds the smallest form of
  it and says so in its commit message.
- Step 1 lowers every M2 key except the two triggers of step 16, and keeps every M2 key, the two
  triggers included, refused with `not_implemented` naming M2 through the pending list of M1 step 6
  in `crates/workflow/src/surface.rs`. The list also holds the action kinds `using: composite` and
  `using: docker`, which fail the job that uses them when its actions are resolved. The step that
  executes a key or a kind takes it off the list, so no run is ever created for something no agent
  can execute.
- Tests that need podman run when `BOSUN_TEST_PODMAN=1`, which CI sets. When it is unset they skip
  with a printed notice, as on macOS; when it is set and podman is unusable they fail. No test pulls
  an image: images come from `compat/images.lock`, pinned by digest and warmed by CI before the
  tests (step 2), and the harness's agent runs with the pull policy `never` (step 10). Actions and
  tools come from fixtures that the fake forge and the harness serve.
- Steps 10 to 14 all change `bosun-runner`. Each keeps its work in a module of its own (the job
  container, services, cache volumes, composite actions, container actions) and touches the step
  loop and the prepare phase only where it hooks in. Step 13 follows step 10 because the nested
  steps of a composite action must run in the job container too.
- No step of M2 records audit events. `audit_event` arrives with M3 step 4, whose audited settings
  cover the ones M2 adds: the action policy, the pool settings, the dispatch weight and the running
  cap.
- Every step:
  - a step that adds an endpoint regenerates `contracts/openapi.json`;
  - a step that adds a journal event kind adds its fixture under `contracts/fixtures/events/` and
    extends the replay property test of M0 step 9 to generate it;
  - a step that settles a mechanism the design leaves open records it in the design document its
    goal names;
  - a behaviour of the target syntax comes with a case in `contracts/fixtures/` or `compat/`. When
    the behaviour needs a recording on GitHub's hosted runners, the step commits the case with the
    expected result taken from the target's documentation and the open-source runner, and the same
    case goes to the public compatibility repository (operator), whose recording the exit checklist
    compares;
  - a step that adds strings to the interface registers their keys in OwnLate in English and Russian
    before it is pushed, and the build pulls the dictionaries.

| # | Step | Depends on | Review |
|---|------|------------|--------|
| 1 | Workflow and action syntax of M2 | — | yes |
| 2 | The end-to-end harness for the M2 cases | — | no |
| 3 | bosun-services: job tokens, block uploads, artifacts | — | yes |
| 4 | The action policy and resolution at readiness | — | yes |
| 5 | Signing keys and job tokens in the assignment | 3 | yes |
| 6 | Artifacts behind the server | 2, 5 | yes |
| 7 | Caches over the current protocol, and `cache-mode` | 1, 6 | yes |
| 8 | The earlier cache protocol | 7 | yes |
| 9 | The podman capability in dispatch | 1, 2 | yes |
| 10 | Job containers | 9 | yes |
| 11 | Service containers and the podman socket | 10 | yes |
| 12 | Cache volumes and `bosun-agent gc` | 1, 10 | yes |
| 13 | Composite actions and `uses: ./path` | 1, 2, 4, 10 | yes |
| 14 | Container actions and `uses: docker://` | 10, 11, 13 | yes |
| 15 | Reusable workflows, local and from other repositories | 1, 2, 4 | yes |
| 16 | `workflow_run` and `on.upstream` | 1 | yes |
| 17 | Dispatch order, fair share and project caps | 1, 9 | yes |
| 18 | The load test | 17 | no |
| 19 | Re-runs and attempts | 6, 15 | yes |
| 20 | Re-runs requested from the forge and the forge account link | 19 | yes |
| 21 | `bosun exec` | 8 | no |
| 22 | The CI of Keel and of Bosun as corpus cases | 6, 11 | yes |
| 23 | The run page of M2 | 6, 13, 15, 19 | no |
| 24 | Settings and the queue page of M2 | 4, 9, 11, 17, 20 | no |

### 1. Workflow and action syntax of M2

**Goal.** Lower into the IR of `bosun-types` every key that "Where each key arrives" and the
extensions table of [design/02-workflows.md](design/02-workflows.md) assign to M2, except
`on.workflow_run` and `on.upstream` (step 16):

- workflow and job `cache-mode`, workflow and job `priority`;
- job `container` and `services`, in the forms the target's schema accepts, with `image`,
  `credentials`, `env`, `ports`, `volumes` and `options`;
- job `cache-volumes`, as strings and as mappings;
- job `uses`, `with` and `secrets`, including `secrets: inherit`, as `JobKind::Call`, and
  `on.workflow_call` with `inputs`, `secrets` and `outputs`;
- step `uses: ./path` and `uses: docker://image`, with `with.args` and `with.entrypoint`.

Action metadata gains `using: composite`, whose `run` steps require `shell` and whose `outputs` map
from step outputs, and `using: docker` with `image`, `args`, `env`, `entrypoint`, `pre-entrypoint`
and `post-entrypoint`.

Validation adds two errors, a call combined with `runs-on` or `steps` and a `workflow_call` input
whose default does not fit its type, and a pure `check_call(caller, callee)` that reports an input
or a secret the callee does not declare, a missing required input and an input of the wrong type,
for step 15 to call when it plans.

`bosun-expr` gains the availability rows of the new keys from the table of
[design/03-expressions.md](design/03-expressions.md), and the `job.container` and `job.services`
properties. That table has no row for `cache-volumes.key`; this step settles that the server
evaluates it when the job's `needs` are complete, with the contexts available to job `env` (neither
`runner` nor `steps`), and records the row in `design/03-expressions.md`.

The current milestone of M1 step 6 becomes M2. Every key lowered here, both new action kinds and the
two triggers of step 16 enter its pending list, and the table test of M1 step 6 holds against that
list: a key on it is refused with `not_implemented` naming M2, a key of a later milestone still
names its own, and every other key lowers. The JSON Schema of M1 step 6 already lists these keys; it
accepts every fixture workflow that lowers.

**Touches.** `crates/types/`, `crates/workflow/`, `crates/expr/`, `contracts/fixtures/workflows/`,
`contracts/fixtures/ir/`, `contracts/fixtures/diagnostics/`, `contracts/fixtures/actions/`,
`contracts/fixtures/expr/`, `contracts/schema/workflow.json`, `docs/design/03-expressions.md`.

**Depends on.** nothing in M2.

**Out of scope.** `on.workflow_run` and `on.upstream` (step 16); executing any of the keys; looking
up a callee in another file (step 15).

**Acceptance.**

- Fixture workflows with each new key lower to committed IR snapshots, and fixture actions of both
  new kinds lower to committed metadata snapshots.
- A composite `run` step without `shell`, a call job with `runs-on`, a `workflow_call` input whose
  default does not fit its type, and an undeclared input passed to `check_call` each produce their
  diagnostic with the right span.
- An expression in `services.<id>.env` that uses `steps` is refused, `job.services.<id>.ports` in a
  step is accepted, and `runner` in `cache-volumes.key` is refused, as the availability fixture
  says.
- A workflow that uses a pending key is refused with `not_implemented` naming M2. The table test
  fails when the pending list names a key the surface does not know, and when the schema's keys
  differ from the keys of the surface tables.
- The WebAssembly build of the pure crates stays green.

**Review.** yes: `check_call`, the typing of `workflow_call` inputs and `secrets: inherit` decide
which secrets cross workflows, and the corpus pins their semantics.

### 2. The end-to-end harness for the M2 cases

**Goal.** Give `hack/e2e.sh` what the M2 cases need, short of an agent with `podman`, which step 9
adds once the agent can detect it.

`compat/images.lock` lists every image a test uses, pinned by digest. `hack/warm-images.sh` pulls
them through the runner host's registry mirror in a CI step that runs before the tests, and refuses
an entry without a digest. It also extracts Node 24 from the pinned `node:24-alpine` image and packs
it as the `linux-x64-musl` build in the harness's Node upstream, next to the glibc build M1 packs,
so job containers on musl images get Node without a download.

`actions/upload-artifact@v4`, `actions/download-artifact@v4`, `actions/cache@v4`,
`actions/cache@v3.3.3` and `actions/setup-node@v4` are vendored in the layout in which M1 step 46
vendored `actions/checkout@v4`, `compat/actions/<owner>/<repo>/<ref>/`, each as the tarball the
forge serves for that commit, with the upstream commit and the tarball's SHA-256 in a manifest that
the fake forge checks before it serves one. A Node 24 tool cache is laid out as `setup-node`
expects, with its `.complete` marker, so `setup-node` finds Node without a download.

The fake forge serves repositories over git's smart HTTP since M1 step 46; this step gives a case's
fixture repository its history and annotated tags, so `actions/checkout@v4` with `fetch-depth: 0`
and a later `git fetch origin` behave as they do against the target.

A case is a directory `compat/cases/<name>/` with the fixture repository as a list of commits and
tags, the agent's home tree, the events to deliver, the project's secrets and variables, and
`expected.json` with the jobs and their display names, conclusions, selected outputs, the check run
names and log assertions. The harness runs every case, or one with `--case`. It asserts that the
host of `GITHUB_SERVER_URL` ends in `.localhost`, so the toolkit never takes its enterprise path
([design/06-runtime-services.md](design/06-runtime-services.md)), and removes what a case wrote
outside its sandbox. Agent names carry a prefix that never collides with the runner host's own
directories, because workflows such as Keel's write under
`/opt/actions-runner/cache/<project>/$RUNNER_NAME`. With `BOSUN_TEST_PODMAN=1` the harness checks
that podman is usable and fails saying so when it is not; without it, it prints that podman cases
are skipped.

**Touches.** `hack/e2e.sh`, `hack/warm-images.sh`, `compat/images.lock`, `compat/actions/`,
`compat/cases/`, `compat/README.md`, `crates/testkit/` (the fake forge: vendored actions, history
and annotated tags), `.github/workflows/ci.yml`.

**Depends on.** nothing in M2.

**Out of scope.** The podman agent of the harness (step 9), the podman executor (step 10), and the
cases of features later steps build (each such step adds its own).

**Acceptance.**

- A case converted from M1's checkout scenario passes, and the same case with one conclusion changed
  in `expected.json` fails the harness naming the job.
- A vendored action whose tarball no longer matches its recorded hash fails the harness naming the
  action.
- `hack/warm-images.sh` refuses an entry of `compat/images.lock` that is not pinned by digest.
- A case that checks out with `fetch-depth: 0`, then runs
  `git fetch origin "refs/tags/<tag>:refs/tags/<tag>"` and `git cat-file -t <tag>`, prints `tag`.
- With the fake forge's host overridden to one outside `.localhost`, the harness fails before it
  runs a case.
- With `BOSUN_TEST_PODMAN=1` and no usable podman the harness fails with a message saying so;
  without the variable it says that podman cases are skipped.
- CI warms the images, sets `BOSUN_TEST_PODMAN=1` and is green.

**Review.** no.

### 3. bosun-services: job tokens, block uploads, artifacts

**Goal.** The crate `bosun-services` with the parts of
[design/06-runtime-services.md](design/06-runtime-services.md) that need no server: an index trait,
which the server implements over Postgres in step 6, and an index in memory for `bosun exec`; job
tokens, Ed25519 JWTs whose `scp` claim has the form the toolkit parses, minted with a given key and
verified against a set of public keys; signed addresses, HMAC-signed with an expiry, a purpose and
an object; the block blob endpoint with exactly the three requests of the design
(`PUT ?comp=block&blockid=`, `PUT ?comp=blocklist`, and a single `PUT` with
`x-ms-blob-type: BlockBlob`), which streams blocks into `blocks/<upload>/<block>`, assembles the
object on the block list, and answers downloads with `HEAD`, `Range` and `x-ms-range`; and the Twirp
service `github.actions.results.api.v1.ArtifactService` with `CreateArtifact`, `FinalizeArtifact`,
`ListArtifacts`, `GetSignedArtifactURL` and `DeleteArtifact`, as JSON.

This step settles the path shape of signed addresses that the toolkit's storage client parses: a
container segment and a blob segment, also when the server's address is an IP address with a port,
as in the tests. It records the shape in `design/06-runtime-services.md`. The crate already has its
row in `design/01-overview.md` and its rule in `hack/check-deps.sh`.

**Touches.** `crates/services/`, `Cargo.toml`, `contracts/fixtures/twirp/`,
`docs/design/06-runtime-services.md`.

**Depends on.** nothing in M2.

**Out of scope.** Key storage and the job token in the assignment (step 5), the Postgres index and
the routes of the server (step 6), the cache service (steps 7 and 8).

**Acceptance.**

- The Twirp request and response fixtures, taken from what the toolkit sends and expects, round-trip
  through the service with the in-memory index and the directory backend.
- An upload in staged blocks sent out of order yields the object in block-list order, and a single
  `PUT` yields the whole object.
- A block list that names a block never staged is refused, and no object appears.
- A signed address that is expired, altered, or used for another object or purpose is refused with
  `403`, and a job token of another run can neither list nor create artifacts of this one.
- A token verifies against a set that holds its key and fails against one that does not.
- `hack/check-deps.sh` passes with the crate in the graph.

**Review.** yes: the toolkit's artifact and block protocols, job tokens and signed addresses.

### 4. The action policy and resolution at readiness

**Goal.** The organisation's action policy of [design/05-execution.md](design/05-execution.md):
`all`; `organisation`, where the action's owner is the organisation's forge account; or `listed`,
with the patterns `owner/*`, `owner/repo@*`, `owner/repo@<ref>` and their path forms. It is a
setting of the organisation, changed through `PATCH /organisations/{org}` with
`organisation.manage`. This step settles what the policy covers and records it in
`design/05-execution.md`: remote actions at every level of nesting and remote reusable workflows,
but neither `./path` nor `docker://`. The check is one pure function that steps 13 and 15 call.

Resolution of remote `uses` moves from the assignment to the moment the job becomes ready, outside
the planner's transaction. This step settles how and records it in `design/04-runs.md` and
`design/05-execution.md`. The planner's transaction puts a ready job with unresolved remote
references into `waiting` with the new reason `resolving`. A resolver in every replica claims such
jobs with `SKIP LOCKED`, resolves their references through the forge with no transaction open, and
stores the commits with the job. One transaction then checks the policy and either lets the job
continue to its admission rules and `queued`, or completes it as `failure` with the new reason
`action_forbidden`, naming the action. A reference the forge reports as missing completes the job at
once as `failure` with the new reason `action_unresolved`. A forge that does not answer is retried
with backoff from 1 second to 1 minute; after 10 minutes the job completes the same way, naming the
forge's error. The assignment reuses the stored commits.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/` (the setting, the waiting reason, the
reasons), `contracts/fixtures/api/`, `contracts/fixtures/action-policy/`, `docs/design/04-runs.md`,
`docs/design/05-execution.md`.

**Depends on.** nothing in M2.

**Out of scope.** Nested references and references resolved for the agent (step 13), callees (step
15), the settings page (step 24), auditing the change (M3 step 4).

**Acceptance.**

- A fixture table of patterns against references matches the function exactly.
- A job using a forbidden action goes from `waiting` with reason `resolving` to `completed` as
  `failure` with reason `action_forbidden` naming the action, never reaches `queued`, and the
  journal shows both transitions.
- An allowed action resolves when the job becomes ready, and moving its tag afterwards does not
  change the commit the agent receives.
- With the fake forge stopped when the job becomes ready, the job waits with reason `resolving` and
  is queued once the forge returns; stopped beyond a shortened limit, the job fails with
  `action_unresolved`. While the fake forge hangs, the jobs of another run are planned and queued
  without waiting for it.
- A reference to a tag the fake forge does not have fails the job at once with `action_unresolved`.
- A maintainer gets `403` when changing the policy.
- The waiting reasons and the reasons of `bosun-types` still equal the tables of
  `design/04-runs.md`.

**Review.** yes: a security boundary on untrusted code, and new transitions of the job state machine
with forge calls kept outside the planner's transaction.

### 5. Signing keys and job tokens in the assignment

**Goal.** The Ed25519 key that signs job tokens lives in the table `signing_key`: the public key,
the private key encrypted under the KEK as secrets are
([design/07-storage.md](design/07-storage.md)), a creation time and a retirement time. A replica
generates it under an advisory lock on first need, a periodic task rotates it, and a retired public
key is kept until the last token it signed has expired. This step settles the table and the rotation
period and records them in `design/07-storage.md`.

At assignment the server mints one job token per execution, with the expiry of
[design/06-runtime-services.md](design/06-runtime-services.md), and the assignment carries it with
`ACTIONS_RUNTIME_URL`, `ACTIONS_RESULTS_URL`, `ACTIONS_CACHE_URL` and `ACTIONS_CACHE_SERVICE_V2`,
derived from `server.public_url`. The agent masks the token. The design says every step receives
these variables, while the target gives them to actions and not to `run` steps; this step settles it
as the target does, Node actions now and composite and container actions as steps 13 and 14 add
them, and corrects `design/06-runtime-services.md`.

The server gains the authentication of runtime requests that steps 6 to 8 put in front of their
routes: the signature against the current and the retired public keys, then the execution not yet
completed.

**Touches.** `crates/server/`, `crates/db/`, `proto/`, `crates/proto/`, `crates/runner/`,
`docs/design/06-runtime-services.md`, `docs/design/07-storage.md`.

**Depends on.** 3.

**Out of scope.** The routes of the runtime services and the key of signed addresses (step 6).

**Acceptance.**

- In `hack/e2e.sh`, a fixture Node action sees the five variables with addresses under
  `server.public_url`, a `run` step of the same job sees none of them, and the token appears masked
  when the action prints it.
- The authentication refuses with `401` a token of a completed execution, an altered token, and a
  token signed by a key removed after its tokens expired; it accepts a token signed by the previous
  key after a rotation.
- Two replicas that start at once against a database without a key end up with one key, and its
  private key in the table decrypts only with the KEK.

**Review.** yes: job tokens, key storage and rotation, and masking.

### 6. Artifacts behind the server

**Goal.** Put the artifact service and the block endpoint of `bosun-services` behind the server: the
routes `/twirp/` at the root of the public address and `/_runtime/blob/`, behind the authentication
of step 5; the HMAC key of signed addresses, derived from the KEK with HKDF and a fixed label
([design/07-storage.md](design/07-storage.md)); a Postgres index over `artifact` and
`artifact_block`, under which an artifact is visible to every job of its attempt; and the REST API
`GET /runs/{run}/artifacts` and `GET` and `DELETE` on `/artifacts/{artifact}`, streaming from the
store with ranges. Reading needs `project.view`. The design names no permission for deleting; this
step settles `run.override` and records it in `design/08-access.md`.

Retention removes artifacts past their `retention-days`, capped by the project's maximum, and past
the project's default otherwise. A periodic task removes uploads not finalised within 24 hours, a
limit this step settles and records in `design/07-storage.md`.

**Touches.** `crates/server/`, `crates/db/`, `contracts/fixtures/api/`, `compat/cases/artifacts/`,
`docs/design/07-storage.md`, `docs/design/08-access.md`.

**Depends on.** 2, 5.

**Out of scope.** Caches (steps 7 and 8), artifacts carried into later attempts (step 19), the run
page (step 23).

**Acceptance.**

- The case `artifacts` has a job upload a file with `actions/upload-artifact@v4` and a dependent job
  get the same bytes with `actions/download-artifact@v4`, and `overwrite: true` replaces an artifact
  of the same name.
- Through the server, a signed address replayed after its expiry or used for another artifact is
  `403`, and the token of a job of another run cannot list this run's artifacts.
- The REST download serves a range with `206`, a viewer can download but gets `403` on `DELETE`, and
  a `DELETE` with `run.override` removes the object from the store.
- Retention and the clean-up of abandoned uploads, with shortened limits, remove the rows, the
  staged blocks and the objects.

**Review.** yes: the toolkit's artifact and block protocols end to end, and signed addresses.

### 7. Caches over the current protocol, and `cache-mode`

**Goal.** The Twirp `CacheService` of [design/06-runtime-services.md](design/06-runtime-services.md)
in `bosun-services`, with `CreateCacheEntry`, `FinalizeCacheEntryUpload` and
`GetCacheEntryDownloadURL`, uploading through the block endpoint. The semantics follow the target:
an entry is identified by key and version; lookup tries the exact key, then the key as a prefix,
then each restore key as a prefix, newest first; an entry is immutable once committed; a ref sees
its own entries, then the default branch's, and a pull request also its base branch's; entries are
scoped per project.

`cache-mode` is enforced by the service from the job's effective mode. The defaults per trigger, and
the rule on which triggers may write into the default branch's scope, are transcribed from the
target into `contracts/fixtures/cache-mode/defaults.json`. This step settles three things and
records them in `design/06-runtime-services.md`: the job token carries the effective mode and the
ref scope as claims, so the server and `bosun exec` decide alike; `upstream` gets the defaults of
`workflow_run`; a repository-less project has one scope.

In the server: the `cache_entry` index, and eviction after 7 days without a read and beyond 10 GiB
per project, least recently used first, both project settings. `cache-mode` leaves the pending list.

**Touches.** `crates/services/`, `crates/server/`, `crates/db/`, `crates/workflow/` (the pending
list), `contracts/fixtures/cache-mode/`, `contracts/fixtures/twirp/`, `contracts/fixtures/api/`,
`compat/cases/cache-v4/`, `docs/design/06-runtime-services.md`.

**Depends on.** 1, 6.

**Out of scope.** The earlier protocol (step 8), cache volumes (step 12), cache pages in the
interface.

**Acceptance.**

- The case `cache-v4` saves with `actions/cache@v4` in one run and restores the exact key in the
  next, and the server's request log shows `CreateCacheEntry`, `FinalizeCacheEntryUpload` and
  `GetCacheEntryDownloadURL` and no request under `/_runtime/cache/`.
- A restore key matches the newest entry by prefix, and a branch sees the default branch's entry but
  not another branch's.
- With mode `read` the lookup finds the entry and the create is refused; with `write-only` the
  lookup finds nothing and the create succeeds; with `none` both are refused.
- A `pull_request_target` run with the default mode cannot write, as the fixture table says.
- A second create of a committed key and version is refused in the form the toolkit handles as
  "already exists".
- Eviction with shortened limits removes the least recently used entries first.

**Review.** yes: the toolkit's cache protocol, and cache poisoning through modes and scopes.

### 8. The earlier cache protocol

**Goal.** The protocol of [design/06-runtime-services.md](design/06-runtime-services.md) under
`ACTIONS_CACHE_URL`: `GET _apis/artifactcache/cache?keys=…&version=…`,
`POST _apis/artifactcache/caches` to reserve, `PATCH _apis/artifactcache/caches/<id>` with
`Content-Range` in parallel chunks, and `POST _apis/artifactcache/caches/<id>` to commit, over the
entries, semantics and modes of step 7. This step settles that `cache_entry` gets a numeric id for
this protocol, from a sequence in an additive column, and records it in
`design/06-runtime-services.md`.

**Touches.** `crates/services/`, `crates/server/`, `crates/db/`, `contracts/fixtures/cache-v1/`,
`compat/cases/cache-v3/`, `docs/design/06-runtime-services.md`.

**Depends on.** 7.

**Out of scope.** Restoring across the two protocols.

**Acceptance.**

- The case `cache-v3` saves with `actions/cache@v3.3.3` in one run and restores the exact key in the
  next, with every cache request under `/_runtime/cache/`.
- Chunks uploaded in parallel and out of order yield the entry byte for byte; a commit whose chunks
  leave a gap is refused, and nothing becomes visible.
- A second reserve of a committed key and version answers in the form `actions/cache@v3.3.3` logs as
  "already exists" without failing its step.
- With mode `read` the reserve is refused; with `none` the lookup finds nothing.

**Review.** yes: the earlier toolkit cache protocol.

### 9. The podman capability in dispatch

**Goal.** The `podman` capability of [design/05-execution.md](design/05-execution.md), up to the
claim. The agent detects podman 4.4 or newer, cgroups v2 and netavark, and advertises `podman` with
its label, or logs which prerequisite is missing. When a job becomes ready, the server adds the
capability it needs to its labels, so the containment check of the claim enforces it: `podman` for
`container`, `services` or a label alias that names an image, `shell` otherwise. This step settles
that a job without an image needs `shell`, where M1 step 18 accepts either capability, so it never
reaches an agent that cannot run it on the host, and records it in `design/05-execution.md`; step 14
adds container actions and `docker://` steps to the same computation.

Untrusted pools keep the rule of M1 step 18 ([design/08-access.md](design/08-access.md)): an agent
there holds no `shell`, whatever it advertises, and the agents list says so. This step proves the
rule with a real agent that has both capabilities. Pools gain `allow_privileged`, off by default,
set through `PATCH /pools/{pool}` with `agent.manage` and refused for untrusted pools. When
`BOSUN_TEST_PODMAN=1`, `hack/e2e.sh` also runs an agent with `shell` and `podman`.

**Touches.** `bin/agent/` and `crates/runner/` (detection), `crates/server/`, `crates/db/`
(`pool.allow_privileged`), `crates/types/` and `contracts/fixtures/api/` (the shapes of pools and
agents), `hack/e2e.sh`, `docs/design/05-execution.md`.

**Depends on.** 1, 2.

**Out of scope.** Running anything in a container (step 10), services (step 11), container actions
(step 14).

**Acceptance.**

- In CI the harness's podman agent appears in `bosun agent list` with `podman`; an agent on a host
  without podman starts without it and logs why.
- Fifty fake agents with and without `podman`, claiming from three replicas, never receive a job
  that needs `podman` on an agent without it, nor a job that needs `shell` on an agent without it.
- A job whose `runs-on` label resolves through an alias with an image needs `podman`.
- An agent of an untrusted pool that advertises `shell` receives no job that needs `shell`.
- Setting `allow_privileged` on an untrusted pool is `422 invalid_input` naming the field.

**Review.** yes: the claim across replicas, and the trust levels of pools.

### 10. Job containers

**Goal.** The job container of [design/05-execution.md](design/05-execution.md) on the agent. Per
execution the agent creates the network `bosun-<execution>`. The job container runs
`tail -f /dev/null`, gets the mounts of the table, and runs steps through `podman exec`, with the
paths, the file commands and the `GITHUB_*` variables translated into the container. Node actions
run from `/__e/node24`, with the musl build in images based on musl. `job.container.id` and
`job.container.network` are available.

`container.options` are passed to `podman create`, except the flags that weaken isolation, which
fail the job unless the pool sets `allow_privileged`; the assignment carries the setting.
`container.credentials` log in to the registry for the pull and are masked. The agent setting
`pull_policy`, `missing` by default or `never`, serves closed networks and the tests; this step
records it in `design/12-operations.md`, and the harness's agent runs with `never`. Teardown removes
the containers, the network and anonymous volumes. On an agent of an untrusted pool the workspace,
the tool cache and the downloaded actions are removed after every job. The project setting
`workspace: clean` of [design/05-execution.md](design/05-execution.md), off by default, joins the
settings document of M1 step 25 and empties the workspace before each job of the project, on `shell`
and on `podman`. `container` leaves the pending list.

**Touches.** `crates/runner/`, `bin/agent/`, `proto/`, `crates/proto/`, `crates/server/` (the
container, the pool setting and `workspace: clean` in the assignment), `crates/workflow/` (the
pending list), `contracts/fixtures/api/`, `hack/e2e.sh`, `docs/design/05-execution.md`,
`docs/design/12-operations.md`.

**Depends on.** 9.

**Out of scope.** Services and the socket (step 11), cache volumes (step 12), container actions
(step 14), the debug terminal (M3).

**Acceptance.**

- In CI, a job with `container` set to a warmed Alpine image runs its `run` steps inside it, in the
  workspace `/__w/<org>.<project>/<repository>`, and `actions/checkout@v4` runs in it on the musl
  Node from the harness's upstream.
- A step in the container that writes to `$GITHUB_ENV`, `$GITHUB_OUTPUT` and `$GITHUB_PATH` affects
  the next step and the job's outputs as on the host.
- After the job no container, network or volume labelled with the execution remains, and the agent's
  user can remove every file the job wrote.
- Each refused flag (`--privileged`, `--cap-add`, `--device`, `--pid=host`, `--network=host`, a
  volume outside the workspace) fails the job before its first step, naming the flag; with
  `allow_privileged` the job starts.
- The registry password of `container.credentials` never appears in the log.
- On an agent of an untrusted pool, the workspace, the tool cache and the actions are empty after
  each job.
- With `pull_policy = "never"`, an image missing from the host fails the job naming the image, and
  nothing is pulled.
- With `workspace: clean`, a file that one job of the project leaves in its workspace is gone when
  the next job starts on the same agent, on `shell` and in a container; without the setting it
  stays.

**Review.** yes: isolation of untrusted code, privileged options and registry credentials.

### 11. Service containers and the podman socket

**Goal.** Services of [design/05-execution.md](design/05-execution.md), for jobs on the host and in
a job container. Each service starts on the job's network with its key as its network alias; its
`env` is evaluated on the agent, its credentials are masked, and its options pass through the filter
of step 10. Ports are published only when `ports` asks, and `job.services.<id>.id`, `.network` and
`.ports` are filled in. The job waits for the health checks of `options`, and an unhealthy service
fails the job before its first step, with the service's last health output in the log.

The agent's podman socket is given to steps as `DOCKER_HOST`, and mounted at `/var/run/docker.sock`
in the job container, when the pool's new setting `podman_socket` allows it. The migration sets it
on the seeded pool `default`; untrusted pools refuse it.

This step settles three mechanisms and records them in `design/05-execution.md`: the agent runs
`podman system service` on a socket in its own directory, needing no user unit; the agent runs the
health check commands itself at their interval, retries and start period, needing no systemd timers;
ports are published on `0.0.0.0`, so `docker port <id> <port>/tcp` through the socket prints
`0.0.0.0:<port>` as Docker does. `services` leaves the pending list.

**Touches.** `crates/runner/`, `bin/agent/`, `proto/`, `crates/proto/`, `crates/server/`,
`crates/db/` (`pool.podman_socket`), `crates/workflow/` (the pending list),
`contracts/fixtures/api/`, `compat/cases/postgres-service/`, `compat/images.lock`,
`docs/design/05-execution.md`.

**Depends on.** 10.

**Out of scope.** Container actions on the job network (step 14), the pool settings page (step 24).

**Acceptance.**

- The case `postgres-service` passes in CI. Its job runs on the host with a Postgres service,
  `ports: ['5432']` and health options; a step finds the port with
  `docker port "${{ job.services.postgres.id }}" 5432/tcp` piped into the `awk` of Keel's CI and
  runs a SQL script against it. The same job in a container reaches `postgres:5432` by its alias.
- `job.services.postgres.ports['5432']` equals the published port.
- A service whose health check never passes fails the job before its first step, with its health
  output in the log.
- Without `podman_socket` steps have no `DOCKER_HOST` and the job container has no socket; with it
  they have both; an untrusted pool refuses the setting.
- Teardown removes the services.

**Review.** yes: the socket is a security boundary, and the port and health behaviour is pinned by
the corpus.

### 12. Cache volumes and `bosun-agent gc`

**Goal.** `cache-volumes` of [design/05-execution.md](design/05-execution.md). A volume is the
directory `/var/lib/bosun/cache/<organisation>/<project or _organisation>/<name>/<key>`, with group
`bosun-cache`, the setgid bit and mode `2775`. `exclusive` takes an advisory lock on a lock file for
the duration of the job and logs while it waits; `shared` takes none. On `shell` the volume is a
symbolic link at `path`, created before the first step and removed after the last; on `podman` it is
a bind mount. The server evaluates `key` with the availability of step 1, and the assignment carries
the volumes with their keys as values.

This step settles that on an agent of an untrusted pool the volumes are not mounted and the job gets
a warning annotation, and records it in `design/02-workflows.md`. `bosun-agent gc` evicts the least
recently used volumes beyond `cache_max_bytes`, skipping locked ones, and prunes images and build
cache beyond the agent's image limit. `cache-volumes` leaves the pending list.

**Touches.** `crates/runner/`, `bin/agent/`, `proto/`, `crates/proto/`, `crates/server/`,
`crates/workflow/` (the pending list), `docs/design/02-workflows.md`.

**Depends on.** 1, 10.

**Out of scope.** The `actions/cache` service (step 7), the timer unit (installed by M0's
`install-agent.sh`).

**Acceptance.**

- A file written by one job into a volume is seen by the next job on the same host, on `shell` and,
  in CI, on `podman`.
- Two exclusive jobs on one volume run one after the other, and the second logs that it waits; two
  shared jobs run together.
- `organisation` scope is shared between projects of the organisation and `project` scope is not;
  different keys are different directories.
- The created directories have the group of the configuration, the setgid bit and mode `2775`, and
  the link is gone after the job.
- On an agent of an untrusted pool nothing is mounted and the warning is on the job.
- `bosun-agent gc` with a small `cache_max_bytes` removes the oldest unlocked volume first and
  leaves a locked one.

**Review.** yes: locks between concurrent jobs on one host, and the trust boundary of shared
volumes.

### 13. Composite actions and `uses: ./path`

**Goal.** Composite actions as in [design/05-execution.md](design/05-execution.md). Nested steps run
with the `inputs` context bound to the action's inputs and with `GITHUB_ACTION_PATH`; a `steps`
context is scoped to the action; outputs map from nested steps; nesting stops at 10 levels. In a job
container the nested steps run in the container. The server resolves the remote `uses` inside remote
composite actions when the job becomes ready, with the resolver of step 4, and applies the action
policy at every level.

`uses: ./path` loads `action.yml` from the workspace when the step is reached, for any `using`. A
remote reference found in a local action is resolved through `POST /_runtime/actions/resolve`,
authenticated by the agent's credential and the execution it runs, and that endpoint applies the
policy. The agent announces a nested step when it reaches it; the server creates its row under
`parent_step_id` with `step.created` and `step.status` in one transaction. The lines of nested steps
go into the top-level step's log, in one group per nested step.

This step settles the endpoint, the creation of nested step rows and where their lines go, and
records them in `design/05-execution.md`. `./path` and `using: composite` leave the pending list.

**Touches.** `crates/runner/`, `bin/agent/`, `proto/`, `crates/proto/`, `crates/server/`,
`crates/db/` (`step.parent_step_id`, `step.outputs`), `crates/workflow/` (the pending list),
`compat/cases/composite/`, `compat/cases/composite-post/`, `docs/design/05-execution.md`.

**Depends on.** 1, 2, 4, 10.

**Out of scope.** Container actions, local ones included (step 14); nested steps in the interface
(step 23).

**Acceptance.**

- A fixture composite action with a `run` step, a nested Node action and an output gives the job
  `steps.<id>.outputs.<name>`, sees its inputs, and finds `GITHUB_ACTION_PATH`.
- Nested steps appear in the API under their parent, their lines are in the parent's log in one
  group each, and the replay property test, extended with nested steps, holds.
- A chain of 11 nested composite actions fails the step, naming the chain.
- `uses: ./.github/actions/<name>` after the checkout runs a local Node action and a local composite
  action.
- A forbidden action nested in a remote composite fails the job before it is queued; one nested in a
  local composite fails its step, naming the action; the resolve endpoint refuses an agent that does
  not run the execution.
- In a job with `container`, the nested `run` steps of a composite action run inside the job
  container.
- The case `composite-post` pins the order of the pre and post steps of nested Node actions as the
  target documents it.

**Review.** yes: the journal of nested steps, a policy check on the agent's path, and workflow
semantics pinned by the corpus.

### 14. Container actions and `uses: docker://`

**Goal.** Container actions as in [design/05-execution.md](design/05-execution.md). A remote
Dockerfile action is built once per agent, tagged by its commit. A `docker://` reference is pulled
under the pull policy. A local `./path` Dockerfile action is built per job and removed at teardown,
since its content is not fixed; that is settled here and recorded in `design/05-execution.md`.

The action runs on the job's network, which is now created for any job that runs container actions.
It sees the workspace at `/github/workspace`, home at `/github/home`, the event under
`/github/workflow`, the file commands under `/github/file_commands` and `RUNNER_TEMP` at
`/github/runner_temp`, with the `INPUT_*`, `GITHUB_*` and runtime service environment. `args` and
`entrypoint` come from the metadata, with input templates evaluated, or from `with.args` and
`with.entrypoint`; `pre-entrypoint` and `post-entrypoint` run in the pre and post phases. A job
whose resolved actions include a container action, or which has a `docker://` step, needs `podman`
in the computation of step 9; a local container action on an agent without `podman` fails its step
with a message. `docker://` and `using: docker` leave the pending list.

**Touches.** `crates/runner/`, `bin/agent/`, `crates/server/`, `crates/workflow/` (the pending
list), `compat/cases/container-action/`, `compat/cases/container-action-phases/`,
`compat/images.lock`, `docs/design/05-execution.md`.

**Depends on.** 10, 11, 13.

**Out of scope.** Pushing built images anywhere; image signatures.

**Acceptance.**

- In CI, a fixture Dockerfile action writes to `$GITHUB_OUTPUT` and the output reaches
  `steps.<id>.outputs`; a second job on the same agent reuses the image and logs so.
- A `docker://` step with `with.entrypoint` and `with.args` runs that command.
- The action reads a file from `/github/workspace` and the event from `GITHUB_EVENT_PATH`, and
  reaches a service by its alias.
- The case `container-action-phases` pins `pre-entrypoint`, the main entrypoint and
  `post-entrypoint` in the order the target documents.
- A job with a remote container action, or with a `docker://` step, is never claimed by an agent
  without `podman`.
- A local Dockerfile action's image is gone after the job.

**Review.** yes: the mount and environment semantics pinned by the corpus.

### 15. Reusable workflows, local and from other repositories

**Goal.** Calls of reusable workflows as in [design/04-runs.md](design/04-runs.md). At planning the
server reads the callee: a local one at the run's commit, or one from another repository with the
ref resolved to a commit through the forge. It validates the callee with `check_call` and the action
policy of step 4. A missing, invalid or forbidden callee ends the run with `startup_failure`, with
diagnostics at the caller's line. A run stops at 10 levels of nesting and 50 distinct callees, and
the run's IR snapshot holds every callee.

A call becomes a group whose members are the callee's jobs, keyed `caller/callee`; a matrix on the
call job gives one group per member. `with` and `secrets` are evaluated when the call job is ready,
typed, with defaults and with `inherit`. The outputs of `on.workflow_call` are evaluated when the
group ends and reach `needs.<caller>.outputs`. The callee sees the caller's `github` context, and
its `permissions` may only narrow the caller's.

This step settles three things and records them in `design/02-workflows.md`: local callees may live
in `.github/workflows/` or `.bosun/workflows/`; a callee from another repository must belong to the
same installation or be public; callee jobs are reported as check runs named
`<caller name> / <callee job name>`. Job `uses`, `with`, `secrets` and `on.workflow_call` leave the
pending list.

**Touches.** `crates/server/`, `crates/db/` (`job.group_id`, `job.kind`), `crates/forge/`,
`crates/testkit/` (a second fixture repository), `crates/workflow/` (the pending list),
`contracts/fixtures/`, `compat/cases/reusable/`, `docs/design/02-workflows.md`.

**Depends on.** 1, 2, 4.

**Out of scope.** Re-running members of a call (step 19), the graph of the interface (step 23).

**Acceptance.**

- A caller with `with`, explicit secrets and `secrets: inherit` runs the callee's jobs as
  `caller/callee` members. Inputs arrive typed with their defaults, an omitted secret is empty while
  `GITHUB_TOKEN` is present, and an output of the callee reaches a dependent of the caller.
- A callee from another repository at a tag resolves to a commit, and moving the tag after planning
  changes nothing in the run.
- A missing callee, a callee without `workflow_call`, an undeclared input, an 11th level of nesting
  and a forbidden callee each end the run with `startup_failure` and the diagnostic.
- A matrix of two on a call job yields two groups; `if: false` on a call skips its whole group, and
  dependents of a call wait for the group to complete.
- The fake forge receives check runs named `<caller> / <job>`, and the replay property test,
  extended with call groups, holds.
- The case `reusable` pins inputs, secrets and outputs as the target documents them.

**Review.** yes: the run state machine with groups, secrets passed between workflows, and semantics
pinned by the corpus.

### 16. `workflow_run` and `on.upstream`

**Goal.** Lower `on.workflow_run` (`workflows`, `types`, `branches`, `branches-ignore`) and the
extension `on.upstream` (`project`, `workflows`, `branches`, `conclusions`) into the IR, executable
at once, so neither enters the pending list. A transition of a run to `requested`, `in_progress` or
`completed` writes an outbox row in the same transaction. A worker matches it against
`upstream_subscription`, which is maintained when a workflow revision becomes current on the default
branch or a server-side workflow is saved, and creates the triggered runs idempotently, keyed by the
source run, its attempt, the target workflow and the type. `workflow_run` stays within the project
and carries the payload the target produces. `upstream` crosses projects when the triggering user of
the upstream run has `run.dispatch` on the subscriber; the triggering user of a forge event is the
Bosun user linked to its sender, which step 20 makes possible. Its `github.event_name` is
`upstream`, and its `github.event` is the upstream run's resource with its project and workflow.

This step settles four things and records them in `design/02-workflows.md`: the shape of the
`upstream` payload, pinned by a fixture; a run without a Bosun user, such as a schedule or a push by
an unlinked actor, fires no cross-project `upstream`, and the skip is a journal event of the
upstream run; subscribers are read from the default branch; chains of these triggers stop after
three levels, as the target's do.

**Touches.** `crates/workflow/`, `crates/types/`, `crates/server/`, `crates/db/`
(`upstream_subscription`), `contracts/fixtures/`, `contracts/schema/workflow.json`,
`docs/design/02-workflows.md`.

**Depends on.** 1.

**Out of scope.** `on.plugin` (M5); triggers from other forges.

**Acceptance.**

- The completion of a run of `CI` on `main` creates one run of a workflow with
  `workflow_run: { workflows: [CI], types: [completed], branches: [main] }`, whose
  `github.event.workflow_run.conclusion` is the source's conclusion; a run on another branch creates
  none.
- Two replicas processing the same outbox row, or a worker that dies after the commit, still yield
  exactly one triggered run.
- `upstream` from another project fires when the dispatching user of the upstream run holds
  `run.dispatch` on the subscriber; without it nothing is created, and the skip event names the
  reason.
- A `conclusions` filter excludes a failed upstream run.
- A fourth level of a chain creates no run.

**Review.** yes: run creation through the outbox, exactly once across replicas, and a permission
check across projects.

### 17. Dispatch order, fair share and project caps

**Goal.** The ordering of [design/04-runs.md](design/04-runs.md) in the claim: higher `priority`
first, where the job's value overrides the workflow's, stored as `-1`, `0` and `1`; then the project
with the lowest ratio of running jobs to its weight; then the oldest. Two project settings,
`dispatch_weight` (default 1) and `running_cap` (default none), keep jobs beyond a cap `queued`, and
`GET /queue` marks the jobs a cap holds back.

This step settles two things and records them in `design/07-storage.md`: how the claim counts
running jobs per project, choosing between a join and a `job.project_id` filled by an additive
migration by a benchmark of the claim with 10 000 queued jobs over 100 projects, whose numbers the
commit message gives; and how a cap stays exact under concurrent claims, by a transaction-scoped
advisory lock per project. `priority` leaves the pending list.

**Touches.** `crates/server/`, `crates/db/`, `crates/workflow/` (the pending list),
`contracts/fixtures/api/`, `docs/design/07-storage.md`.

**Depends on.** 1, 9.

**Out of scope.** `job.prioritise` (M3), the load test (step 18), the queue page (step 24).

**Acceptance.**

- A `high` job queued after a `normal` one is claimed first, and a job-level `low` under a
  workflow-level `high` is claimed after a `normal` one.
- With project A running three jobs and B none, at equal weights, the next idle agent takes B's job
  even though A's is older; with A's weight at 3, it takes A's.
- Fifty fake agents claiming from three replicas never run more jobs of a project than its cap, and
  never receive the same job twice.
- `GET /queue` shows a job held by a cap as such.

**Review.** yes: claims racing between replicas, the exactness of caps, and a data migration if the
column is chosen.

### 18. The load test

**Goal.** A driver in `bosun-testkit`, run by `hack/load-test.sh`, that starts replicas on one
database and many scripted fake agents which take jobs and stream log chunks at a set rate. It
measures ingestion throughput, acknowledgement latency, the time from `queued` to `assigned`, and
the load on Postgres (connections, transactions per second, and the slowest statements from
`pg_stat_statements` when the extension is there). `--smoke` runs a short form, which CI runs. The
numbers of a full run go into `docs/load-test.md` with the command that produced them, the host's
CPU and memory, the Postgres version and the settings that size it (`shared_buffers`,
`max_connections`, `work_mem`), and the number of replicas.

**Touches.** `crates/testkit/`, `hack/load-test.sh`, `.github/workflows/ci.yml`,
`docs/load-test.md`.

**Depends on.** 17.

**Out of scope.** Writing chunks to the object store directly, the fallback that
[design/13-decisions.md](design/13-decisions.md) names, unless the numbers call for it in a later
change.

**Acceptance.**

- The smoke mode passes in CI with no lost or duplicated log line and no job assigned twice, and
  fails when the driver is told to drop one acknowledged chunk.
- `docs/load-test.md` records the agents, the rates, the latencies, the database load, the host and
  the Postgres sizing of a full run, with its command.

**Review.** no.

### 19. Re-runs and attempts

**Goal.** `run.rerun`, `run.rerun-failed` and `job.rerun`, the last with `dependents`, as in
[design/04-runs.md](design/04-runs.md). A re-run is allowed once the run is completed and adds
attempt N+1 with its `reason` and actor. The chosen jobs start again from `pending`; every other job
is carried by reference through `job.carried_from`, with its conclusion, outputs, logs and
artifacts, so a re-run job downloads an artifact a carried job uploaded. `github.run_attempt` is the
new number, while `run_number` and `run_id` stay, and the re-run uses the run's revision and IR
snapshot, callees included. `debug` is refused, naming M3. Check runs go out for the jobs of the new
attempt. The CLI gains `bosun run rerun [--failed]` and `bosun job rerun [--no-dependents]`.

This step settles re-runs of group members and records them in `design/04-runs.md`: a re-run acts on
member jobs, a group is completed again from its members, and a matrix is not expanded again for
carried members.

**Touches.** `crates/server/`, `crates/db/` (`attempt.reason`, `job.carried_from`), `crates/types/`,
`bin/cli/`, `contracts/fixtures/`, `docs/design/04-runs.md`.

**Depends on.** 6, 15.

**Out of scope.** Debug re-runs and "run again with changes" (M3), re-runs requested from the forge
(step 20).

**Acceptance.**

- `run.rerun` creates attempt 2 with every job `pending`, and a workflow file changed after the run
  does not change what runs.
- `run.rerun-failed` re-runs only failed and cancelled jobs and their dependents; the others are
  carried with their conclusions and logs, and a re-run job downloads a carried job's artifact.
- `job.rerun` with `dependents: false` re-runs one job.
- In a matrix only the failed member re-runs, and the group's conclusion follows; in a call group
  only the failed callee job re-runs.
- A re-run of an incomplete run is `409 invalid_state` with `command.refused` in the journal, and a
  viewer gets `403`.
- The replay property test across attempts holds, and `hack/e2e.sh` re-runs a failed job through the
  CLI.

**Review.** yes: the run state machine and the journal across attempts.

### 20. Re-runs requested from the forge and the forge account link

**Goal.** The forge account link of [design/08-access.md](design/08-access.md):
`GET /api/v1/auth/forge/link` and `/auth/forge/callback` through the App's user authorisation,
storing the forge user's id and login in `user_identity`, one Bosun user per forge account. This
step settles that the configuration gains `github.client_id` and `github.client_secret_file`, and
that the link stores the identity only, leaving the user's token to M4; it records both in
`design/12-operations.md` and `design/08-access.md`.

A `check_run` webhook with `rerequested` maps to `job.rerun`. A `check_suite` with `rerequested`
maps to `run.rerun-failed` for every completed run of the suite that has a failed or cancelled job;
this step settles that, because one suite of the App covers every run of a commit, and records it in
`design/06-runtime-services.md`. The command's actor is the linked user. An unlinked forge user, or
one without `run.rerun`, gets no attempt, and `command.refused` naming the forge login goes into the
journal. A dispatch by a linked user gets that user's forge login as `github.actor`. The fake forge
gains the authorisation endpoints.

**Touches.** `crates/server/`, `crates/forge/`, `crates/db/`, `crates/testkit/` (the authorisation
endpoints of the fake forge), `contracts/fixtures/api/`, `docs/design/06-runtime-services.md`,
`docs/design/08-access.md`, `docs/design/12-operations.md`.

**Depends on.** 19.

**Out of scope.** Storing user tokens and commits from the editor (M4), the link button (step 24).

**Acceptance.**

- The link flow against the fake forge stores the identity, and linking the same forge account to a
  second user is `409 conflict`.
- `check_run.rerequested` by a linked user with `run.rerun` creates attempt 2 re-running that job
  and its dependents, with that user as actor.
- `check_suite.rerequested` re-runs the failed jobs of each run of the suite.
- An unlinked user, and a linked user without `run.rerun`, create no attempt, and the journal says
  why.
- A redelivered webhook creates no second attempt.
- A linked user's dispatch has `github.actor` equal to the forge login.

**Review.** yes: mapping forge identities to Bosun permissions.

### 21. `bosun exec`

**Goal.**
`bosun exec [<workflow>] [--job <key>] [--event <name>] [--input k=v]... [--secrets <file>]` of
[design/05-execution.md](design/05-execution.md). The CLI plays the server: it fills the contexts
from the local git repository and the options, and runs the jobs in the order of `needs` with
`bosun-runner`; `--job` runs a job together with the jobs it needs. It serves `bosun-services` on a
local port, both cache protocols included, over a temporary directory store and the in-memory index,
and mints job tokens with a key of its own.

It plans with the pure functions the server's planner uses: matrix expansion and `if` with the
status functions over `needs`. If M1 left one of them inside `bosun-server`, this step moves it into
`bosun-workflow` unchanged, keeps the server's planner tests as they are, and says so in its commit
message.

This step settles where actions and tools come from, actions through the forge's tarball API into a
cache under `~/.cache/bosun/` and Node runtimes from the configured server's `/_runtime/tools/` or
from the `PATH`, and records it in `design/05-execution.md`. It adds `bosun-expr` to the row of
`bosun-cli` in `design/01-overview.md` and in `hack/check-deps.sh`, since the CLI builds contexts as
its values. No run is recorded anywhere.

**Touches.** `bin/cli/`, `crates/services/`, `crates/workflow/` and `crates/server/` (only when a
planning function moves), `hack/check-deps.sh`, `docs/design/01-overview.md`,
`docs/design/05-execution.md`.

**Depends on.** 8.

**Out of scope.** Reusable workflow calls (refused with a message naming the job), gates and
environments, a debug terminal.

**Acceptance.**

- In a fixture git repository, a two-job workflow runs both jobs in order and exits non-zero when
  one fails.
- `actions/upload-artifact@v4`, `actions/download-artifact@v4`, `actions/cache@v4` and
  `actions/cache@v3.3.3` from the fixtures work against the local services, with no server
  reachable.
- A value from `--secrets` is masked in the output.
- `--event pull_request` and `--input` reach `github.event` and `inputs`.
- `contracts/fixtures/matrix/` and the `if` cases of the server's planner tests pass through the
  functions the CLI calls.
- A workflow with a reusable call is refused naming the job, before any job runs.
- `hack/check-deps.sh` passes with the new edge and still refuses `bosun-cli` reaching `bosun-db`.

**Review.** no.

### 22. The CI of Keel and of Bosun as corpus cases

**Goal.** Two cases for `hack/e2e.sh` with real podman in CI.

`compat/cases/keel/` copies the four workflows of `GlobalArtInc/keel` byte for byte, from a commit
recorded with their SHA-256 in `compat/cases/keel/SOURCE`. Around them sit a stand-in repository
(`hack/`, `frontend/`, `Cargo.toml` and the rest the steps call, with the history and the annotated
tag the events need) and an agent home with stand-ins under `.cargo/bin`, so every step runs its
real control flow: services and port discovery through the podman socket, file commands, the matrix
with `max-parallel`, concurrency, artifacts. Stand-ins leave what later steps read, such as a
release binary that prints the tag's version and a `frontend/storybook-static` directory, and no
command reaches a registry or the internet. The Node tool cache and the images of the services come
from step 2. The project carries fake values of Keel's secrets and variables. The events are a push
to `main`, a pull request, a tag `keel-v*` and a dispatch of `publish-npm.yml` with `version`.

`obi1kenobi/cargo-semver-checks-action@v2` is vendored with the other upstream actions under
`compat/actions/`, because the server resolves the actions of skipped steps too. The case keeps its
step skipped on purpose, since the action downloads `cargo-semver-checks` and uses the cache
service: the history of the stand-in repository adds `crates/keel/Cargo.toml` only in its last
commit, so the baseline step outputs `present=false` for every event and the step is skipped, as
GitHub Actions skips it for that history. The baseline step also asks the forge for runs of the
workflow and falls back when it gets none. `compat/README.md` records both as known limitations,
next to the actions that ask the forge about runs.

`expected.json` pins every job, its check run name (including the 23 members
`keel-system with <features>`) and its conclusion as the target's rules give them for this tree,
where every stand-in succeeds: the naming of check runs and the `if` of each job per event. The
comparison with Keel's real runs on GitHub Actions is the operator check of the exit checklist.

`.bosun/workflows/` of this repository, which held the simple workflows of M1 step 45, becomes a
copy of every file of `.github/workflows/`, kept identical by `hack/check-bosun-workflows.sh` in CI,
and the scenario of M1 step 46 that ran the simple workflows gives way to the case `bosun-ci`.
`compat/cases/bosun-ci/` runs `.bosun/workflows/ci.yml` for a pull request in the same way; building
and publishing the image are covered by the operator check only.

**Touches.** `compat/cases/keel/`, `compat/cases/bosun-ci/`, `compat/actions/`,
`compat/images.lock`, `compat/README.md`, `.bosun/workflows/`, `hack/check-bosun-workflows.sh`,
`hack/e2e.sh`, `.github/workflows/ci.yml`.

**Depends on.** 6, 11.

**Out of scope.** Switching the real repositories over (operator); building Keel itself; exercising
`cargo-semver-checks`.

**Acceptance.**

- The copies' hashes equal `SOURCE`, and a changed byte fails the case.
- For the push to `main`, the case yields the pinned jobs and conclusions, the artifact
  `keel-x86_64-linux-musl` with a retention of 14 days, and never more than four `system` members in
  progress at once, read from the journal.
- On the pull request the `rehearsal` job is skipped; on every event the semver step is skipped and
  its action was resolved.
- The tag runs `release.yml` and `publish-npm.yml`, the release job fetches the annotated tag from
  the fake forge, and the dispatch passes `version` to the publishing step.
- With the agent's proxy settings pointing at an address that refuses every connection and the
  harness's addresses in `NO_PROXY`, the case still passes.
- `hack/check-bosun-workflows.sh` fails when a file of `.bosun/workflows/` differs from its
  counterpart in `.github/workflows/` or is missing.
- The case `bosun-ci` yields its pinned jobs and conclusions.

**Review.** yes: the compatibility corpus pins the workflow semantics of the first real payoff.

### 23. The run page of M2

**Goal.** The run page of [design/09-frontend.md](design/09-frontend.md) for what M2 adds: the
attempt selector in the header, with carried jobs marked; re-run buttons for the run, the failed
jobs and a job, shown when the run is completed and the user holds `run.rerun`; the artifacts of the
attempt in the job panel, for download; nested composite steps in the job panel, with their log
groups; and call groups in the graph, collapsed with member counts and expandable.

**Touches.** `web/`.

**Depends on.** 6, 13, 15, 19.

**Out of scope.** Run control beyond re-runs (M3), the editor (M4).

**Acceptance.**

- Vitest covers the attempt selector, the visibility rules of the re-run buttons, the nested step
  list and the collapsed call group, against the API fixtures in `contracts/fixtures/api/`.
- A Playwright test against `hack/e2e.sh` re-runs a failed job from the run page and sees attempt 2
  with the other jobs carried.
- An artifact downloads from the run page with the bytes it was uploaded with.
- A viewer sees no re-run button.
- `pnpm lint && pnpm test && pnpm build` is green with the pulled dictionaries, and no key is
  missing in either language.

**Review.** no.

### 24. Settings and the queue page of M2

**Goal.** The pages of [design/09-frontend.md](design/09-frontend.md) for the settings M2 adds: the
queue page shows priorities and the jobs a project cap holds back; the organisation settings edit
the action policy; the project settings edit the settings document of M1 step 25, with the dispatch
weight, the running cap and `workspace: clean` that M2 adds to it; the pool settings edit
`podman_socket` and `allow_privileged`, both unavailable for untrusted pools; the personal settings
offer the button that links a forge account and show the linked login.

**Touches.** `web/`.

**Depends on.** 4, 9, 11, 17, 20.

**Out of scope.** The audit trail in the interface (M3), the cache pages.

**Acceptance.**

- Vitest covers the policy editor with its three modes and its patterns, the project settings with a
  stale `If-Match`, and the pool settings for trusted and untrusted pools, against the API fixtures.
- A Playwright test against `hack/e2e.sh` sets the policy to `listed`, dispatches a run that uses an
  action outside the list, and sees the job fail naming the action.
- A Playwright test links a forge account through the fake forge and sees the login in the personal
  settings.
- A viewer sees the settings read-only and cannot change the action policy.
- `pnpm lint && pnpm test && pnpm build` is green with the pulled dictionaries, and no key is
  missing in either language.

**Review.** no.

## M3 work breakdown

M3 is split into thirty-two steps. Each step is one change to `main`, ordered by dependency; a step
is done when its acceptance holds in CI. Every step assumes that M0, M1 and M2 are accepted, and
builds on what they leave:

- the command endpoint with `run.cancel` and `job.cancel`, `run.admit` with the fork policy (M1),
  and `run.rerun`, `run.rerun-failed` and `job.rerun` (M2), each with its CLI command;
- roles, teams and permission keys, and the API of users, teams, roles, role bindings and tokens;
- local passwords with their lockout;
- the event stream, the outbox with check runs, and `/metrics` with the count of dead outbox rows;
- secrets and variables under the KEK, and `workflow_dispatch` inputs;
- `POST /api/v1/lint` and log search;
- `contracts/schema/workflow.json` and the extension warning;
- the fake forge and the scripted fake agent of `bosun-testkit`, and the compatibility corpus in
  `compat/`;
- Playwright against `hack/e2e.sh`, and the run page of the interface.

Steps 1 to 4 have no prerequisite in M3 and lay down what the rest builds on: the vocabulary of
commands, journal kinds and API shapes (1), the agent protocol (2), the workflow keys and their IR
(3), and the audit trail (4). After them, three chains run side by side:

- the server side of run control goes 5, 6, 9, 11, then 12 and 13;
- the agent goes 7, 8, 10, then 19;
- gates and environments go 15 to 18.

The server chain waits for the agent chain at 9 and 11, because a workflow key is switched on only
once both sides execute it. The terminal on the server (20) follows the holds. Run again with
changes (14), TOTP (21) and the MCP endpoint (22) each follow one or two early steps. The interface
steps 24 and 26 to 28 depend only on contracts and test against fixtures, so they run beside the
server work; 23 and 25 follow the server steps whose shapes they render. Steps 29 and 30 take the
commands and the exit scenarios end to end on the real binaries. Steps 31 and 32 take up what M1
leaves to M3 for agents, pools and the outbox, and need only the audit trail. The same rules hold
for every step:

- every effect on a run is a journal event written in the transaction of the status columns it
  changes;
- a command that the server accepts, or refuses for the state of its target, is recorded in the
  journal, and from step 4 in the audit trail;
- a workflow key of M3 stays on the pending list of step 3 until the step named there removes it, so
  no run is created for a key that nothing executes yet;
- a field of the assignment that an agent must understand is named in the assignment's required
  features (step 2), and the agent adds the feature to the ones it implements in the step that
  implements it;
- each server step adds the CLI subcommands of the commands it makes work, and tests them against
  the server harness;
- `contracts/openapi.json` is regenerated, never merged by hand, when a step lands after another
  that changed it;
- an interface step registers its keys in OwnLate with the ownlate-translations skill before it is
  pushed, since the build pulls the dictionaries;
- tests reach neither GitHub nor the internet;
- behaviour that needs cgroups or podman is tested in CI only.

Where a step names a mechanism the design leaves open, it says so, settles it, and records the
decision in the design document it names.

| # | Step | Depends on | Review |
|---|------|------------|--------|
| 1 | Run control vocabulary | — | yes |
| 2 | The agent protocol of run control | — | yes |
| 3 | Workflow keys of run control | — | yes |
| 4 | The audit trail | — | yes |
| 5 | Pause, skip and mark-success before the start | 1 | yes |
| 6 | Priority, pins and step commands before the start | 5 | yes |
| 7 | Step boundaries on the agent | 2, 3 | yes |
| 8 | Step retry | 2, 3, 7 | no |
| 9 | Control of running jobs | 2, 6, 7 | yes |
| 10 | Holds on the agent | 8 | yes |
| 11 | Holds on the server | 9, 10 | yes |
| 12 | Job-level retry | 11 | yes |
| 13 | Debug re-runs | 11 | yes |
| 14 | Run again with changes | 1 | yes |
| 15 | Gates and the approval inbox | 3, 5 | yes |
| 16 | Environments and their rules | 1, 4 | yes |
| 17 | Environment admission | 15, 16 | yes |
| 18 | Environment URLs and forge deployments | 2, 17 | yes |
| 19 | The debug terminal on the agent | 10 | yes |
| 20 | The debug terminal on the server | 4, 11 | yes |
| 21 | TOTP for local accounts | 4 | yes |
| 22 | The MCP endpoint | 5, 15 | yes |
| 23 | The audit page | 4 | no |
| 24 | Teams, roles and tokens in the interface | — | no |
| 25 | TOTP in the interface | 21, 24 | no |
| 26 | Run page controls and history | 1 | no |
| 27 | Approvals and environments in the interface | 1 | no |
| 28 | Terminal drawer, run again with changes, occupied slots | 1 | no |
| 29 | Commands end to end | 6, 9, 11, 13, 17 | no |
| 30 | The exit scenarios end to end | 19, 20, 22, 26, 28, 29 | no |
| 31 | Agent credential rotation and pool address ranges | 4 | yes |
| 32 | Dead outbox rows | 4 | no |

### 1. Run control vocabulary

**Goal.** Write the vocabulary of M3 as plain types in `bosun-types`, so that the server, the CLI
and the interface are built in parallel against one contract.

- **Commands.** Every command of the command table of [design/04-runs.md](design/04-runs.md), with
  its arguments:
  - `debug` of `run.rerun`, `run.rerun-failed` and `job.rerun`;
  - `outputs` of `job.mark-success`;
  - `priority` of `job.prioritise`, and `agent` of `job.pin`;
  - `position` (`before` or `after`) and `clear` of `step.breakpoint`;
  - `as` of `step.abort` (`failure` or `skipped`) and of `hold.continue` (`skipped` or `success`);
  - `duration` of `hold.extend`;
  - `inputs` and `comment` of the approval commands.
- **The command table as data.** For each command: the statuses of its target in which it is valid,
  the permission it needs, and whether it needs a `reason`. The server checks this one table, and
  the interface offers only what the server will accept.
- **Journal kinds.** The kinds M3 adds, with their data, go into the kinds table of 04-runs.md:
  - `control.acknowledged`, `job.priority`, `job.pin`;
  - `step.breakpoint` and `step.skip`, for what is set against a step before it starts;
  - `step.conclusion`, for the conclusion that `hold.continue` replaces on a completed step, which
    is not a status transition;
  - `hold.started` and `hold.extended`, each with its deadline;
  - `approval.requested`, and `approval.decided` (approved, rejected or bypassed, with the reviewer,
    the comment and the inputs);
  - `job.environment`, for the environment a job was admitted to and, later, its URL;
  - `terminal.opened`, and `terminal.closed` with the recording.

  Existing kinds gain fields: `debug` on `attempt.created`, `derived_from` on `run.created`, and
  `outputs` on a `job.status` that an operator caused.
- **Reasons.** The reasons M3 adds to the reason table: `pause_expired`, `hold_expired`,
  `approval_rejected`, `gate_timed_out`, `branch_policy`, `job_retry`.
- **API shapes.**
  - the `commands` field of runs, jobs and steps, which names each command valid now, its arguments,
    and whether it needs a reason;
  - the page of `GET /runs/{run}/journal`;
  - approval requests as the inbox lists them;
  - environments with their rules;
  - terminal sessions, and the frames of the terminal WebSocket;
  - the request of `POST /runs/{run}/derive`;
  - the pause and hold fields of jobs and executions (since when, the deadline, the held step), a
    job's environment and its URL, and the fields of agents and of the queue that say which slots a
    paused or held job occupies.

The design leaves these questions open, and this step settles them:

- how a target names a step while its job has no execution yet: by the step's index in the job
  template, recorded in 04-runs.md;
- whether `job.rerun` takes `debug`, which 02-workflows.md implies and the command table does not:
  it does, under `job.debug`, recorded in the command table and in the CLI of
  [design/11-api.md](design/11-api.md);
- whether the commands that need `run.override` require a `reason`, as the interface asks for one
  and `bosun job mark-success` takes `--reason`: they do, recorded in 04-runs.md;
- what an inbox id names, which `bosun approval approve <id>` and MCP's `decide_approval` take,
  recorded in 11-api.md;
- the frames of the terminal WebSocket: bytes as binary frames, and the viewer's role, resizes and
  the end of the session as JSON text frames, recorded in 11-api.md.

The command endpoint refuses every command it does not implement yet with `422 invalid_input` naming
`type`, so the server builds against the new types and never accepts a command without its effect;
the step that implements a command lifts its refusal. Every shape gets a golden fixture.

**Touches.** `crates/types/`, `crates/server/` (the refusal of commands not implemented yet),
`contracts/fixtures/api/`, `contracts/fixtures/events/`, `contracts/fixtures/operator-commands/`,
`contracts/openapi.json`, `docs/design/04-runs.md`, `docs/design/11-api.md`.

**Depends on.** nothing in M3.

**Out of scope.** Every effect, which later steps implement; the agent protocol (step 2); the IR of
the new workflow keys (step 3); the shapes of the audit trail (step 4) and of TOTP (step 21).

**Acceptance.**

- A test reads the command table from design/04-runs.md and compares the commands, their permissions
  and whether they need a reason with the code.
- The statuses in which each command is valid equal
  `contracts/fixtures/operator-commands/validity.json`, and a test checks that file against the
  transition tables, so no command is valid where no transition exists for its effect.
- Every command has a request fixture that deserialises and re-serialises without loss. An unknown
  command, an unknown argument, `as: success` on `step.abort` and `as: failure` on `hold.continue`
  all fail to deserialise.
- The kinds table and the reason table of design/04-runs.md equal the kinds and reasons in the code,
  and every kind has a fixture.
- Each command the server does not implement yet is `422 invalid_input` naming `type`, and changes
  no status.
- The WebAssembly build of `bosun-types` is green, and `cargo tree -p bosun-types` shows no async
  runtime.

**Review.** yes: the journal kinds and the command table are contracts that every later step, every
stored journal and every client depend on.

### 2. The agent protocol of run control

**Goal.** Add the messages of run control to `proto/bosun/agent/v1/agent.proto`, additively within
`bosun.agent.v1`:

- the `ControlKind` values of [design/05-execution.md](design/05-execution.md) (`force_cancel`,
  `pause`, `resume`, `skip_step`, `breakpoint`, `abort_step`, `retry_step`, `continue`, `release`,
  `extend`), with their arguments: the step, the position, `clear`, `as` and the duration;
- `Paused` with the boundary it stopped at, and `Held` with the failed step and the deadline;
- the terminal messages: `TerminalOpen`, `TerminalInput`, `TerminalResize` and `TerminalClose` to
  the agent, and from it `TerminalOutput` and the end of a session with its reason;
- assignment fields for step `retry` and `pause`, the hold duration, the breakpoints and skips set
  before the start, and the template of `environment.url`;
- the environment URL in `JobFinished`.

**Required features.** proto3 drops the fields a receiver does not know, while 02-workflows.md
requires an agent to refuse a job that carries a field it does not know rather than run it as if the
field were absent, and an agent of the previous minor version may still be connected after the
server is updated. M1 step 20 meets that rule for the IR and the contexts, which travel as JSON
documents, but not for the fields of the assignment message itself, which this step adds. This step
settles it and records it in 05-execution.md: the assignment lists the features its job uses
(`breakpoints`, `pause`, `step_retry`, `hold`, `environment_url`), and an agent answers an
assignment that names a feature it does not implement with `Reject`, naming the feature. The agent
of this step implements none of them; steps 7, 8, 10 and 18 add them.

The server's stream accepts the new agent messages and changes nothing for them until steps 9, 11
and 20 handle them.

**Touches.** `proto/bosun/agent/v1/agent.proto`, `crates/proto/`, `crates/server/` (the stream),
`bin/agent/` (the check of required features), `docs/design/05-execution.md`.

**Depends on.** nothing in M3.

**Out of scope.** What the agent does with the controls (steps 7, 8, 10 and 19), what the server
does with the new messages (steps 9, 11 and 20).

**Acceptance.**

- A test reads the message tables and the control kinds of design/05-execution.md and compares them
  with `agent.proto`.
- Every new message survives a round trip with all of its fields set, and the build still needs no
  `protoc`.
- An agent given an assignment that names `pause` answers `Reject` with a reason naming `pause`, and
  the server returns the job to the queue with `rejected_by_agent`.
- A `Paused` sent by a fake agent leaves its stream open and changes no status.

**Review.** yes: the agent protocol is a contract with every agent in the field, and the required
features decide whether an agent may run a job it does not fully understand.

### 3. Workflow keys of run control

**Goal.** Parse, validate and lower to the IR in `bosun-types` the keys that the tables of
[design/02-workflows.md](design/02-workflows.md) assign to M3:

- job `environment`, as a name or as `name` and `url`;
- `retry` on steps: `attempts` from 2 to 10, `backoff`, `factor` of at least 1, and `when` within
  `failure` and `timeout`;
- `retry` on jobs: `attempts`, and `when` within `failure`, `timeout` and `agent-lost`, default
  `[agent-lost]`;
- `pause` on steps, `before` or `after`;
- `debug.hold-on-failure` on jobs;
- `gate` on jobs, with:
  - `approvers` (`user:<login>`, `team:<slug>`), `min-approvals`, `timeout` and `message`;
  - `inputs`, with the schema and the type checks of `workflow_dispatch` inputs;
  - `allow-self-approval`.

`gate` excludes `runs-on`, `steps` and `uses`, and a gate job does not require them. The design does
not decide which other job keys a gate may carry, the syntax of durations in workflow keys, or the
range of `attempts` on a job; this step settles all three and records them in 02-workflows.md.

- The IR gains `environment`, `retry`, `pause`, `hold_on_failure` and the job kind `Gate`.
- The availability table of `bosun-expr` gains gate `message`, the defaults of gate `inputs`, and
  `environment`, with its `url` evaluated on the agent, as
  [design/03-expressions.md](design/03-expressions.md) lists.
- `contracts/schema/workflow.json`, which lists the keys since M1 step 6, describes their forms.
- **Pending keys.** The current milestone of M1 step 6 becomes M3, and every key of this step goes
  on its pending list in `crates/workflow/src/surface.rs`. A pending key is validated and lowered,
  and then refused with `not_implemented` naming M3, so a workflow that uses it creates no run while
  nothing executes it. Each key leaves the list in the step that makes it work on the server and on
  the agent: step `retry` in 8, `pause` in 9, `debug.hold-on-failure` in 11, job `retry` in 12,
  `gate` in 15, `environment` without `url` in 17, and `environment.url` in 18. The fixture tests
  lower with pending keys admitted.

**Touches.** `crates/workflow/`, `crates/types/src/ir.rs`, `crates/expr/`, `crates/server/` and
`crates/runner/` where they match on the IR, `contracts/fixtures/workflows/`,
`contracts/fixtures/ir/`, `contracts/fixtures/diagnostics/`, `contracts/schema/workflow.json`,
`docs/design/02-workflows.md`.

**Depends on.** nothing in M3.

**Out of scope.** What the keys do at run time (steps 7 to 18).

**Acceptance.**

- A test reads the two tables of 02-workflows.md that have a From column, and the pending list.
  Every key from M3 or earlier is accepted or pending; a pending key with a valid value yields
  `not_implemented` naming M3; and every later key yields `not_implemented` naming its milestone.
- Fixtures that use every M3 key lower to committed IR snapshots.
- `environment: production` and `environment: { name: production }` lower to the same IR.
- Each validation rule has a fixture with its code and span, among them:
  - an `attempts` of 1 and one of 11;
  - `when: [agent-lost]` on a step;
  - a gate with `steps`;
  - a gate input whose default does not fit its type;
  - an approver that is neither `user:` nor `team:`;
  - a duration that does not parse.
- A gate `message` that names `steps` or `runner` is refused as a context unavailable at that key,
  while an `environment.url` that names `steps` is accepted.
- In a file under `.github/workflows/`, `retry`, `pause`, `debug` and `gate` each yield the
  extension warning, and `environment` does not.
- The JSON Schema accepts every fixture workflow that lowers.
- The WebAssembly build of `bosun-types`, `bosun-expr` and `bosun-workflow` is green.

**Review.** yes: `environment` is target syntax, the IR is the contract that the planner and every
agent read, and the pending list keeps a key from creating runs before anything executes it.

### 4. The audit trail

**Goal.** Record `audit_event` as [design/08-access.md](design/08-access.md) specifies: the actor,
the action, the target, the data with secrets masked, the address, the user agent and the time. Each
event is written in the transaction of the action it records, through one function of the server.

No milestone's scope names the recording, although 08-access.md requires it and M3 names its
interface, so no earlier step records anything. This step creates the table and makes every point of
the list that exists by now record:

- sign-ins and failed sign-ins;
- tokens created and revoked;
- secrets and variables written, by name and never by value;
- roles and bindings;
- pools, agents and registration tokens;
- workflow saves;
- settings;
- every operator command, at the command endpoint, so that the commands of later steps are recorded
  without further work.

Later steps add environments (16), terminal sessions (20) and the second factor (21) through the
same function.

- **API.** `GET /api/v1/audit` serves holders of `audit.view`, with cursor pagination and filters by
  actor, action, organisation, project, target and time. Its shapes and fixtures go into
  `bosun-types`.
- **Events outside any organisation.** The design does not say who reads events that belong to no
  organisation, such as a failed sign-in. This step settles that owners only do, and records it in
  08-access.md.
- **Retention.** One year, as a periodic task.

**Touches.** `crates/db/migrations/`, `crates/db/`, `crates/server/` (the recording function and the
handlers that call it), `crates/types/src/audit.rs`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `docs/design/08-access.md`.

**Depends on.** nothing in M3.

**Out of scope.** The audit page (step 23); recording environments, terminals and TOTP (steps 16, 20
and 21); plugins (M5).

**Acceptance.**

- Each action of the list records exactly one event, and an action whose transaction fails records
  none.
- Writing a secret records its name, and a test finds its value nowhere in `audit_event`.
- A user without `audit.view` gets `403 forbidden`.
- An organisation admin reads the events of their own organisation and none of another's, and events
  without an organisation are listed for owners only.
- Every filter narrows the list, and the cursor walks the whole list without repeating an event.
- The periodic task, under its advisory lock, deletes events older than the retention.

**Review.** yes: the trail holds security-relevant data, must never hold a secret, and reading it is
a permission boundary.

### 5. Pause, skip and mark-success before the start

**Goal.** Implement, through `POST /api/v1/runs/{run}/commands`, the commands that act on jobs that
have not started and need no agent. Each is checked against the table of step 1 and applied in one
transaction with its journal events.

- **`run.pause` and `run.resume`** for what has not started:
  - queued jobs go back to `waiting` with the reason `paused`;
  - jobs that become ready while the run is paused wait with the same reason;
  - the run is `paused` until it is resumed, and then takes the status its jobs imply;
  - running jobs are left to step 9.
- **`job.pause` and `job.resume`** before the start, with the same waiting reason.
- **`job.skip` and `job.mark-success`** of a job that has not started:
  - the conclusion source is `operator`;
  - outputs are checked against the job's declared output names, and dependents see them in
    `needs.<job>.outputs`;
  - dependents follow their `if`.
- **Expiry.** A periodic task cancels jobs that have been `paused`, or `waiting` with the reason
  `paused`, longer than `limits.max_pause`, with the reason `pause_expired`.
- **The `commands` field** of runs, jobs and steps lists the commands this server implements that
  are valid now and that the caller may give. Each later step adds its own.
- **Reporting.** The check run of a job an operator completed carries its conclusion and a summary
  line naming the operator and the command, as
  [design/06-runtime-services.md](design/06-runtime-services.md) describes and M1 step 28 leaves to
  M3.
- **The journal.** `GET /api/v1/runs/{run}/journal`, which no earlier milestone serves, with cursor
  pagination and `interventions=true`, which keeps only the events whose actor is a person, a token
  or a plugin.

The design leaves two questions open, and this step settles them in 04-runs.md: whether `job.resume`
resumes a job of a paused run, and whether a refusal for lack of permission is recorded in the
journal too.

The CLI gains `bosun run pause | resume | journal [--interventions]` and
`bosun job pause | resume | skip | mark-success`.

**Touches.** `crates/server/` (the command endpoint, the planner, the journal endpoint, the expiry
task, the check run of an operator's conclusion), `crates/db/` (the time a job was paused),
`bin/cli/`, `contracts/openapi.json`, `docs/design/04-runs.md`.

**Depends on.** 1.

**Out of scope.** Priority, pins and step commands (step 6), effects on running jobs (step 9), holds
(step 11), gates (step 15).

**Acceptance.**

- For each command of this step, a test gives it to its target in every status:
  - it is accepted exactly where the table allows it;
  - elsewhere it is refused with `409 invalid_state` and a `command.refused` event;
  - a caller without the permission gets `403 forbidden`.
- The property test of M0 interleaves random commands of this step with random step results, and the
  projected statuses still equal a replay of the journal.
- `job.mark-success` with an output the job does not declare is `422 invalid_input`, naming
  `args.outputs.<name>`. With declared outputs, a dependent's assignment carries them in `needs`,
  and `success()` holds for that dependent.
- An idle fake agent does not claim a queued job of a paused run, and claims it after `run.resume`.
- One replica cancels a job paused before its start for longer than a shortened `max_pause`, with
  the reason `pause_expired`.
- The `commands` of a job list exactly what the table allows in its status, what the caller's
  permissions cover, and no command that a later step implements.
- `interventions=true` returns the events of the commands and none of the agent's or the system's.
- The fake forge receives the check run of a skipped job with the operator's summary line.
- Each CLI subcommand of this step, run against the server harness, produces the journal event of
  its command.

**Review.** yes: the run state machine and the journal, and a permission check on every command.

### 6. Priority, pins and step commands before the start

**Goal.** Implement the remaining commands on jobs that have not started:

- **`job.prioritise`** of a job queued or waiting. The claim orders by the new priority.
- **`job.pin`** of a queued job, which restricts it to one agent through `pinned_agent_id`, honoured
  by the claim. A pin to an agent that does not satisfy the job's labels and pool is
  `422 invalid_input`, naming `args.agent`.
- **`step.skip` and `step.breakpoint`** of a job that has not started, naming the step by its index
  in the job template (step 1). They are stored with the job and listed with its steps; step 9
  carries them into the assignment.

The design leaves open how a pin is lifted, and what happens to a job pinned to an agent that goes
offline or is deleted; this step settles both in 04-runs.md.

The CLI gains `bosun job prioritise | pin` and `bosun step skip | breakpoint [--after] [--clear]`.

**Touches.** `crates/server/` (the command endpoint), `crates/db/` (`pinned_agent_id`, a job's
breakpoints and skips, the claim query), `bin/cli/`, `contracts/openapi.json`,
`docs/design/04-runs.md`.

**Depends on.** 5.

**Out of scope.** Honouring breakpoints and skips on the agent (steps 7 and 9), step commands on
running jobs (step 9).

**Acceptance.**

- For each command of this step, the validity and permission test of step 5 holds; `job.pin` needs
  `agent.manage`.
- Only its agent claims a pinned job, while another matching agent stays idle. Fifty fake agents
  claiming from three replicas still never receive the same job when some jobs are pinned.
- A pin to an agent without the job's labels is `422 invalid_input` naming `args.agent`.
- A job pinned to an agent that goes offline, or is deleted, does what this step settles.
- After `job.prioritise` to `high`, an idle agent claims that job before an older one of normal
  priority.
- `step.breakpoint` with `position: after` records `step.breakpoint` and lists it with the step, and
  `clear` removes it.
- The property test includes the commands of this step.
- Each CLI subcommand of this step, run against the server harness, produces the journal event of
  its command.

**Review.** yes: the claim query decides which agent runs which job, and pinning is guarded by
`agent.manage`.

### 7. Step boundaries on the agent

**Goal.** Teach `bosun-runner` and `bosun-agent` to stop at step boundaries and to act on the
controls that concern steps.

At every step boundary, before a step starts and after it ends, the engine checks for a pause
request and for a breakpoint. A breakpoint may come from the assignment (set before the start), from
`pause: before | after`, or from a control at run time. On a stop, the engine reports `Paused` with
the boundary and waits until `resume`.

It handles these controls:

- `pause` and `resume`.
- `skip_step`: the pending step completes as `skipped` with source `operator`, and counts as skipped
  for the status functions. A skip from the assignment does the same.
- `breakpoint`: sets or clears a breakpoint at run time.
- `abort_step`: the step's process group gets the cancel sequence of M0, and the step ends as
  `failure`, or as `skipped` with `as: skipped`.
- `force_cancel`: ends the job without its remaining steps. The design leaves open whether this
  kills the job's cgroup at once; this step settles it in
  [design/05-execution.md](design/05-execution.md).

A control that arrives too late to take effect, such as a breakpoint for a step that has already
started, is acknowledged with `took_effect` false, and so is a control kind the agent does not know.
The job's `timeout-minutes` stops counting while it is paused. The agent adds `breakpoints` and
`pause` to the features it implements.

**Touches.** `crates/runner/`, `bin/agent/`, `docs/design/05-execution.md`.

**Depends on.** 2, 3.

**Out of scope.** Step retry (step 8), holds (step 10), the terminal (step 19), the server side of
these controls (step 9).

**Acceptance.**

- Engine tests drive a job through a scripted control channel. A pause request sent during step 2
  lets step 2 finish and reports `Paused` before step 3, which starts only after `resume`.
- `pause: before` on step 2 reports `Paused` before step 2 starts, and `pause: after` reports it
  after step 2 ends.
- A breakpoint and a skip in the assignment take effect as if they had been sent at run time.
- A breakpoint for a step that has already started is acknowledged with `took_effect` false, and
  nothing pauses.
- `abort_step` ends a step that ignores `SIGINT` and `SIGTERM` within 15 seconds, as `failure`, or
  as `skipped` with `as: skipped`.
- After `force_cancel` during a step, neither a following `if: always()` step nor a post step runs,
  and no process of the job is left.
- A job with a shortened timeout, paused for longer than that timeout, does not time out.
- An assignment that names `pause` and `breakpoints` is accepted.

**Review.** yes: control messages race with step boundaries, and abort and force-cancel kill
processes.

### 8. Step retry

**Goal.** Make `retry` on steps work, on the agent and in the assignment.

The engine:

- re-executes the step on `failure` and, when `when` lists it, on `timeout`, waiting `backoff` after
  the first attempt and multiplying the wait by `factor` after each further one;
- discards a failed attempt's writes to `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH` and
  `GITHUB_STATE`;
- never retries a cancelled or aborted step;
- applies a pause or a breakpoint after the step only once its last attempt has ended;
- makes each attempt a section of the step's log, whose marker this step settles and records in
  [design/05-execution.md](design/05-execution.md).

The server carries step `retry` in the assignment with the feature `step_retry`, the agent adds that
feature to the ones it implements, and step `retry` leaves the pending list.

**Touches.** `crates/runner/`, `bin/agent/`, `crates/server/` (the assignment),
`crates/workflow/src/surface.rs`, `docs/design/05-execution.md`.

**Depends on.** 2, 3, 7. Step 7 is a prerequisite because both steps rewrite the engine's step loop.

**Out of scope.** Job-level retry (step 12).

**Acceptance.**

- A step with `retry: { attempts: 3 }` that fails twice and then passes succeeds:
  - `GITHUB_OUTPUT` holds only the third attempt's writes;
  - its log has three sections;
  - with `backoff: 1s` and `factor: 2`, the third attempt starts no sooner than 3 seconds after the
    first one ended.
- A step that times out is retried only when `when` lists `timeout`.
- A step cancelled or aborted during its first attempt is not retried.
- With `pause: after`, `Paused` is reported once, after the last attempt.
- A workflow with step `retry`, dispatched against the server harness, creates a run, and the fake
  agent's assignment carries the retry and names `step_retry`.

**Review.** no.

### 9. Control of running jobs

**Goal.** Implement the commands that act on a running job, sent to its agent as control messages:

- `run.pause` and `job.pause` on jobs `in_progress`, and `run.resume` and `job.resume` of paused
  ones;
- `step.skip` and `step.breakpoint` on pending steps of running jobs;
- `step.abort` with `as`;
- `run.force-cancel`.

A control message carries its arguments; `control_message` gains them as an additive column. When
the agent answers:

- `Paused` moves the job and its execution to `paused`, and the step before which it stopped to
  `paused` as well;
- `ControlAck` is recorded on the row and as `control.acknowledged`, so a command that did not take
  effect says so in the journal.

The rest of this step:

- **Assignment.** It carries `pause`, and the breakpoints and skips that step 6 stored, naming the
  features `pause` and `breakpoints`; `pause` leaves the pending list.
- **Expiry.** The task of step 5 now also cancels paused running jobs, through their agent.
- **Slots.** The agents and queue resources say which slots a paused job occupies.
- **Fake agent.** The scripted fake agent of `bosun-testkit` learns `Paused`.
- **CLI.** It gains `bosun step abort [--as skipped]` and `bosun run cancel --force`.

**Touches.** `crates/server/`, `crates/db/`, `crates/testkit/`, `crates/workflow/src/surface.rs`,
`bin/cli/`, `contracts/openapi.json`.

**Depends on.** 2, 6, 7.

**Out of scope.** Holds (step 11), job-level retry (step 12).

**Acceptance.**

- A `job.pause` given through replica A reaches a fake agent connected to replica B. Its `Paused`
  moves the job, the execution and the step to `paused`, with the events in that order, and
  `job.resume` brings them back to `in_progress`.
- A breakpoint that the agent acknowledges with `took_effect` false leaves the job running, and the
  journal shows both the command and that it did not take effect.
- A control message given while the agent has no stream arrives once when the agent reconnects.
- `run.pause` of a run with one running and one queued job pauses the first through its agent and
  holds the second back, and `run.resume` continues both.
- A breakpoint and a skip set before the start reach the fake agent in the assignment.
- `step.abort` with `as: skipped` needs `run.override`, and `run.force-cancel` is valid only after
  `run.cancel`.
- One replica cancels a running job paused longer than a shortened `max_pause`, with the reason
  `pause_expired`.
- A job token stays valid across a pause longer than the job's timeout.
- The property test drives pauses, resumes and aborts through the fake agent, and the projected
  statuses equal a replay of the journal.
- `GET /api/v1/agents` shows an agent holding a paused job, and the queue counts that slot as
  occupied.

**Review.** yes: delivering control messages across replicas and reconnects, and the state machine
of jobs, executions and steps.

### 10. Holds on the agent

**Goal.** Add hold-on-failure to the engine.

A job enters a hold when four things are true: a step fails, the step's retries are spent,
`continue-on-error` does not apply, and the job carries a hold duration. The engine then:

- starts no further step, neither `if: always()` steps nor post steps;
- reports `Held` with the failed step;
- leaves the workspace or the job container as the step left it, including the files behind
  `GITHUB_ENV` and `GITHUB_OUTPUT`.

It handles these controls:

- `retry_step`: the failed step runs again in that state. A success continues the job as if the step
  had passed; a failure holds the job again.
- `continue` with `as`: the step counts as `skipped` or `success`, and the next step starts.
- `release`: the steps that run after a failure and the post steps run, then the job ends as
  `failure`.
- `extend`: moves the deadline.

The job's timeout stops counting while it is held, and the agent adds `hold` to the features it
implements. Two questions are open, and this step settles both and records them in
[design/05-execution.md](design/05-execution.md): which steps run when a held job is torn down for
`job.mark-success`, and whether the agent ends a hold by itself once its stream has been down past
the deadline.

**Touches.** `crates/runner/`, `bin/agent/`, `docs/design/05-execution.md`.

**Depends on.** 8.

**Out of scope.** The terminal (step 19), the server side of holds (step 11).

**Acceptance.**

- A failing step under hold reports `Held`. A following `if: always()` step and a post step have not
  run, and a line the step wrote to `GITHUB_ENV` before it failed is still in its file.
- A step with `retry` under hold is held only after its last attempt.
- After the test creates by hand the file the failed step checks for, `retry_step` makes the step
  pass and the job succeed.
- `continue` with `as: success` makes `success()` true for the next step, and with `as: skipped` the
  step counts as skipped.
- `release` runs the `if: always()` step and the post step, and the job ends as `failure`.
- A job with a shortened timeout, held for longer than that timeout, does not time out.
- The teardown for `job.mark-success` runs the steps this step settles, and no process of the job is
  left.

**Review.** yes: a held job keeps a workspace with the job's secrets alive, and the controls race
with the failed step and its cleanup.

### 11. Holds on the server

**Goal.** Handle holds on the server.

`Held` moves the job and its execution to `held` with a deadline: the time of the hold plus the
job's `debug.hold-on-failure`, capped by `limits.max_hold`. `hold.started` records it. The
assignment carries the capped duration with the feature `hold`, and `debug.hold-on-failure` leaves
the pending list.

The hold commands go to the agent as control messages:

- `hold.retry-step`: the step goes from `completed` back to `in_progress` when the agent starts it
  again.
- `hold.continue` with `as`: the step's conclusion is replaced with source `operator`, recorded as
  `step.conclusion`.
- `hold.release`.
- `hold.extend`: the design states its limit only as "up to the installation cap". This step settles
  whether the cap bounds the whole hold or each extension, and the default extension, and records
  both in [design/04-runs.md](design/04-runs.md).

`job.mark-success` of a held job tears the job down first, then completes it as `success` with the
supplied outputs. A periodic task releases holds past their deadline, with the reason
`hold_expired`. The agents and queue resources say which slots a held job occupies, the fake agent
learns `Held`, and the CLI gains
`bosun hold retry-step | continue [--as success] | release | extend`.

**Touches.** `crates/server/`, `crates/db/`, `crates/testkit/`, `crates/workflow/src/surface.rs`,
`bin/cli/`, `contracts/openapi.json`, `docs/design/04-runs.md`.

**Depends on.** 9, 10.

**Out of scope.** The terminal (step 20), job-level retry (step 12), debug re-runs (step 13).

**Acceptance.**

- Each hold command is accepted only while the job is held, and refused otherwise.
- `hold.retry-step`, `hold.release` and `hold.extend` need `job.debug`; `hold.continue` and
  `job.mark-success` need `run.override`.
- After `hold.retry-step` and the fake agent's report of a passing step, the step's events go
  `completed` → `in_progress` → `completed`, and the job continues.
- `hold.continue` with `as: success` records `step.conclusion` with source `operator`, and the job's
  conclusion follows from it.
- A `debug.hold-on-failure` above `max_hold` reaches the assignment capped.
- One replica releases a hold past a shortened deadline, and the job completes as `failure` with the
  reason `hold_expired`.
- `hold.extend` past the cap behaves as this step settles.
- `job.mark-success` of a held job sends the agent its teardown and completes the job as `success`,
  with source `operator` and outputs that a dependent then receives.
- A job token stays valid across a hold longer than the job's timeout.
- The property test includes holds and their commands.

**Review.** yes: the run state machine and the journal, including the one transition out of
`completed`.

### 12. Job-level retry

**Goal.** Make `retry` on jobs work. The job goes from `in_progress` back to `queued`, with the
reason `job_retry`, when:

- an execution ends with a conclusion listed in the job's `when` (`failure`, `timeout`); or
- the agent is lost, past the grace period or through a `Hello` that no longer lists the execution,
  and `when` lists `agent-lost`.

Each retry is a new execution within the same attempt, possibly on another agent, until `attempts`
are spent. Earlier executions stay listed with the job, the job keeps one check run on the forge,
and job `retry` leaves the pending list. How job retry combines with hold-on-failure is open; this
step settles it and records it in [design/04-runs.md](design/04-runs.md).

**Touches.** `crates/server/`, `crates/db/`, `crates/workflow/src/surface.rs`,
`docs/design/04-runs.md`.

**Depends on.** 11.

**Out of scope.** Step retry (step 8).

**Acceptance.**

- A job with `retry: { attempts: 2, when: [failure] }` whose first execution fails gets a second
  execution in the same attempt, on another fake agent. Both executions are listed, and
  `github.run_attempt` does not change.
- An agent lost past the grace period re-queues a job whose `when` is the default `[agent-lost]`,
  and fails a job with `when: [failure]`; a `Hello` that does not list the execution does the same.
- A job that has spent its attempts completes as `failure`.
- The fake forge receives one check run for the job across both executions.
- A job with both `retry` and `debug.hold-on-failure` behaves as this step settles.
- The property test includes job retries.

**Review.** yes: new executions within an attempt are part of the run state machine, and a lost
agent races with its reconnect.

### 13. Debug re-runs

**Goal.** Make `debug` on `run.rerun`, `run.rerun-failed` and `job.rerun` work. It needs `job.debug`
and is recorded on the attempt (`debug` of `attempt.created`). The assignments of the re-run jobs
carry `RUNNER_DEBUG=1`, `ACTIONS_STEP_DEBUG=true` and `ACTIONS_RUNNER_DEBUG=true`, and a hold
duration. Carried jobs are untouched, and a re-run requested from the forge is never a debug re-run.
The design does not name the hold duration of a debug re-run; this step settles it and records it in
[design/04-runs.md](design/04-runs.md).

The CLI gains `--debug` on `bosun run rerun` and `bosun job rerun`.

**Touches.** `crates/server/`, `crates/db/` (the debug flag of attempts), `bin/cli/`,
`contracts/openapi.json`, `docs/design/04-runs.md`.

**Depends on.** 11.

**Out of scope.** Run again with changes (step 14).

**Acceptance.**

- `run.rerun` with `debug` is `403 forbidden` without `job.debug`. With it, the assignments of the
  re-run jobs carry the three variables and the hold duration, and carried jobs are untouched.
- `job.rerun` with `debug` does the same for the job and its dependents.
- A failing step of a debug re-run is held, although the workflow sets no `debug.hold-on-failure`.
- `bosun run rerun --debug` and `bosun job rerun --debug`, run against the server harness, create
  attempts with `debug`.

**Review.** yes: `job.debug` guards a re-run that opens holds and terminals.

### 14. Run again with changes

**Goal.** Serve `POST /api/v1/runs/{run}/derive`, which "Run again with changes" calls:

- it takes edited `inputs` and a `ref`, checked like a dispatch;
- it needs `run.dispatch` and accepts an `Idempotency-Key`;
- it creates a run linked to the original by `derived_from`, which `run.created` carries.

Which revision, event name and payload a derived run has is not decided by the design; this step
settles it in [design/04-runs.md](design/04-runs.md). Whatever it settles, a run derived from an
untrusted run is untrusted as well.

**Touches.** `crates/server/`, `crates/db/` (`derived_from` of runs), `contracts/openapi.json`,
`docs/design/04-runs.md`.

**Depends on.** 1.

**Out of scope.** Deriving from a source draft (M4, with drafts), the dialog (step 28).

**Acceptance.**

- `derive` with a changed input creates a run with `derived_from`, the new input and the original's
  other inputs. An input of the wrong type is `422 invalid_input` naming `inputs.<name>`.
- `derive` leaves the original run unchanged, and a repeated `Idempotency-Key` returns the same run.
- A caller without `run.dispatch` gets `403 forbidden`.
- A run derived from a run of a fork pull request is untrusted: its jobs receive no secrets and
  never reach a trusted pool.

**Review.** yes: a derived run must not shed the trust level of its original, and creating it is a
permission check.

### 15. Gates and the approval inbox

**Goal.** Implement the gates of [design/02-workflows.md](design/02-workflows.md) and the approval
inbox; `gate` leaves the pending list.

A gate that becomes ready skips the strategy, `runs-on` and the queue. Once its admission rules
pass, it waits with the reason `approval`, and the server renders its `message` and records an
approval request.

**Approving.** `approval.approve` counts one approval, with a comment:

- the approver must be listed, as a user or as a member of a listed team, and must hold
  `project.view`;
- the inputs are checked against the gate's input schema, and defaults are applied;
- the run's actor is refused unless the gate allows self-approval;
- one reviewer counts once.

**Ending a gate.**

- `approval.reject` completes the gate as `failure` with the reason `approval_rejected`.
- A periodic task completes gates past their `timeout` with `gate_timed_out`.
- With `min-approvals` reached, the gate completes as `success`; its outputs are the inputs and
  `approved-by`.
- While the run is paused, approvals are recorded but nothing is released until the run resumes.

The design names the table `approval` but not its shape; this step settles what a request and a
decision are in [design/07-storage.md](design/07-storage.md), behind the API shape of step 1.

**Elsewhere.**

- `GET /api/v1/approvals` lists the requests the caller may decide.
- Approval events go out on the event stream, in its sequence for events that are not run events.
- The check run of a waiting gate is `in_progress`, with "waiting for approval" in its title and a
  details address that opens the approval.
- The `commands` of a waiting gate list the approval commands for its eligible approvers.
- The CLI gains `bosun approval list | approve | reject`.

**Touches.** `crates/server/` (the planner, approvals, commands, the outbox), `crates/db/`,
`crates/workflow/src/surface.rs`, `bin/cli/`, `contracts/openapi.json`, `docs/design/07-storage.md`.

**Depends on.** 3, 5.

**Out of scope.** Environment approvals and `approval.bypass` (step 17), notifier plugins (M5).

**Acceptance.**

- A workflow with a gate, dispatched against the server harness, creates a run, and the gate never
  reaches an agent.
- The assignment of a job that needs a gate carries in `needs.<gate>.outputs` the value the approver
  gave, and `approved-by` as a JSON array of logins.
- An input of the wrong type is `422 invalid_input` naming `args.inputs.<name>`, and an omitted
  input takes its default.
- The run's actor is refused without `allow-self-approval` and accepted with it.
- A user who is neither listed nor in a listed team gets `403 forbidden`, and so does a listed user
  without `project.view`.
- With `min-approvals: 2`, two approvals by one reviewer leave the gate waiting.
- Two approvals given at the same moment by two reviewers, through two replicas, complete the gate
  exactly once.
- A rejection completes the gate as `failure`, and its dependents are skipped.
- A gate past a shortened timeout completes as `failure` with `gate_timed_out`.
- An approval given during `run.pause` is in the journal, and the gate stays waiting until
  `run.resume`.
- The inbox lists the request for each eligible reviewer and for nobody else.
- A subscriber of the event stream receives the request and the decision, and resumes with
  `Last-Event-ID`.
- The fake forge receives the gate's check run as `in_progress`, with "waiting for approval" in its
  title.
- Each CLI subcommand of this step, run against the server harness, produces the journal event of
  its command.

**Review.** yes: approvers are a permission boundary, and concurrent decisions race across replicas.

### 16. Environments and their rules

**Goal.** Store the environments of [design/04-runs.md](design/04-runs.md), per organisation or per
project, in `environment` and `environment_rule`.

**Rules.**

- required reviewers (users and teams), with a minimum number of approvals;
- prevention of self-review;
- a wait timer;
- patterns of the branches and tags allowed to deploy;
- whether administrators may bypass.

**API.** `/api/v1/environments` takes `GET`, `POST`, `PUT` and `DELETE`, scoped by `?organisation=`
or `?project=`. Updates require `If-Match`, and writes require `environment.manage`.

**Secrets and variables.** Environments get their own, encrypted and bound as
[design/07-storage.md](design/07-storage.md) says, with the environment in the associated data. The
design leaves open, and this step settles:

- how `/secrets` and `/variables` name an environment scope, recorded in 11-api.md;
- how a name resolves when both a project and its organisation define an environment of that name,
  recorded in 04-runs.md.

**Audit.** Environments, their rules and their secrets are recorded in the audit trail of step 4.

**CLI.** It gains `--environment` on `bosun secret set`, and the environment scope of
`bosun var set`.

**Touches.** `crates/db/`, `crates/server/`, `bin/cli/`, `contracts/openapi.json`,
`docs/design/04-runs.md`, `docs/design/11-api.md`.

**Depends on.** 1, 4.

**Out of scope.** What environments do to jobs (step 17), the settings pages (step 27).

**Acceptance.**

- An update with a stale `If-Match` is `412 precondition_failed`, and a write without
  `environment.manage` is `403 forbidden`.
- A rule that names an unknown user or team is `422 invalid_input` naming the field.
- The API never returns an environment secret.
- An environment secret's ciphertext copied onto a project secret of the same name fails to decrypt.
- Each write records one audit event, without the secret's value.
- `bosun secret set --environment` and `bosun var set` with an environment scope write what `GET`
  then lists.

**Review.** yes: secrets and their scoping.

### 17. Environment admission

**Goal.** Admit jobs that name an `environment`; `environment` without `url` leaves the pending
list.

When such a job becomes ready, the server:

1. evaluates the environment's name and resolves the environment;
2. checks the branch and tag patterns first, and completes the job as `failure` with the reason
   `branch_policy` when the ref is excluded;
3. then waits for the reviewers and the wait timer in parallel.

Reviewers decide through the approval requests of step 15, and self-review is prevented when the
rule says so. `approval.bypass` admits the job for holders of `environment.bypass`, where the rules
allow it. The admission is recorded as `job.environment`.

The admitted job's assignment carries the environment's secrets and variables over those of the
project and of the organisation, and an untrusted run gets no environment secret. This step commits
to `compat/` the case of that precedence with the result the target documents; recording it on
GitHub's hosted runners is an operator task, checked in the exit checklist. The check run of a job
waiting for approval is reported as it is for gates.

The design leaves these questions open, and this step settles them in 04-runs.md:

- what happens to a job that names an environment nobody defined, which the target creates on first
  use;
- whether one approval covers every job of the run waiting on the same environment, as the target's
  review does;
- which waiting reason a job shows while both reviewers and the timer are outstanding;
- the syntax of the patterns;
- how a repository-less project, which has no ref, meets a pattern.

A `workflow_dispatch` input of type `environment` must now name an environment the project can use.
The CLI gains `bosun approval bypass`.

**Touches.** `crates/server/` (the planner, the assignment, approvals), `crates/db/`,
`crates/workflow/src/surface.rs`, `bin/cli/`, `compat/` (the precedence case),
`contracts/openapi.json`, `docs/design/04-runs.md`.

**Depends on.** 15, 16.

**Out of scope.** `environment.url` and deployments on the forge (step 18).

**Acceptance.**

- A job on a ref the patterns exclude completes as `failure` with `branch_policy`, and no approval
  is requested.
- With both reviewers and a wait timer, the job is queued only when both are satisfied, whichever
  comes first.
- With self-review prevented, the run's actor cannot approve.
- `approval.bypass`:
  - is `403 forbidden` without `environment.bypass`;
  - is `409 invalid_state` where the rules forbid it;
  - admits the job otherwise, with the bypass in the journal.
- An environment secret overrides a project secret of the same name in the assignment, and an
  environment variable an organisation and a project variable, as the case committed to `compat/`
  pins; a job of a fork run receives no environment secret.
- A dispatch whose `environment` input names no environment the project can use is
  `422 invalid_input` naming `inputs.<name>`.
- `bosun approval bypass`, run against the server harness, produces the journal event of its
  command.

**Review.** yes: environment secrets are released only through the rules, and bypass and self-review
are permission checks.

### 18. Environment URLs and forge deployments

**Goal.** Report the environment URL, and create deployments on the forge when a project asks for
them.

The server carries the template of `environment.url` in the assignment with the feature
`environment_url`. The agent evaluates it at the end of the job, with the contexts
[design/03-expressions.md](design/03-expressions.md) allows there, drops a URL that contains a
masked value with a warning as it does for job outputs, reports it in `JobFinished`, and adds
`environment_url` to its features. The server stores the URL with the job, records it as
`job.environment`, and the API returns it; `environment.url` leaves the pending list.

With the new project setting `forge_deployments` on (off by default), a job admitted to an
environment creates a deployment on the forge for the run's commit and environment. Its statuses
then follow the job: in progress, then success or failure, with the environment URL and the address
of the run page. They go through the outbox with one collapse key per job. With the setting off,
nothing reaches the forge's deployments. `bosun-forge` gains the calls, and the fake forge of the
test harness gains their endpoints.

**Touches.** `crates/runner/`, `bin/agent/`, `crates/server/`, `crates/forge/`, `crates/testkit/`
(the fake forge), `crates/db/` (the project setting), `crates/workflow/src/surface.rs`.

**Depends on.** 2, 17.

**Out of scope.** Deployment protection on the forge's side, which is a non-goal of
[design/01-overview.md](design/01-overview.md).

**Acceptance.**

- A URL template that reads a step's output yields, on the job in the API, the value that step
  wrote.
- A URL that contains a secret is dropped with a warning and reaches neither the API nor the forge.
- With `forge_deployments` on, the fake forge receives one deployment and the job's statuses in
  order, the last one carrying the URL and the run page's address.
- While the fake forge fails, pending statuses collapse into the latest one, which it receives once
  it answers again.
- With the setting off, the fake forge receives no deployment request.

**Review.** yes: the URL is computed by job code and leaves the agent for the API and the forge, so
it must pass the masking rule of job outputs.

### 19. The debug terminal on the agent

**Goal.** Add the debug terminal of [design/05-execution.md](design/05-execution.md) to the agent:

- on `TerminalOpen` for a held execution, the agent starts a pseudo-terminal;
- on `podman`, that is `podman exec -it` into the job container with the job's environment;
- on `shell`, and only with `allow_terminal = true` in the agent's configuration, it is a login
  shell in the workspace, as the agent's user, with the job's environment;
- input and resizes come from the stream, and output passes through the job's masker before it
  leaves the agent;
- the session ends after 30 minutes without input, on `TerminalClose`, or when the job is released,
  and the agent reports the end with its reason.

The pseudo-terminal library is chosen in this step and recorded in 05-execution.md.

**Touches.** `crates/runner/`, `bin/agent/` (the configuration key `allow_terminal`, the messages),
`docs/design/05-execution.md`.

**Depends on.** 10.

**Out of scope.** The server side of terminals (step 20), the drawer in the interface (step 28).

**Acceptance.**

- On `shell` without `allow_terminal`, `TerminalOpen` is refused. With it:
  - a session that runs `echo $GITHUB_JOB` returns the job key;
  - a secret of the job printed in the session arrives masked.
- `TerminalOpen` for an execution that is not held is refused.
- A session without input closes after a shortened idle time, the agent reports why, and no process
  the session started survives the teardown.
- Releasing the hold closes the session.
- In CI, a session in a `podman` job container runs in that container with the job's environment.

**Review.** yes: a terminal is access to the host or to the job container, and its output must pass
the masker.

### 20. The debug terminal on the server

**Goal.** Serve the debug terminal: `GET /api/v1/executions/{execution}/terminal`, as a WebSocket
with the frames of step 1, for holders of `job.debug`, on held executions only.

**Writer and watchers.** One person types and others watch. The design leaves open how a session and
its writer are chosen. This step settles it: one session per execution, held for writing by the
first viewer who asks until that viewer leaves.

**Relay across replicas.** The browser and the agent's stream may end on different replicas, and no
replica may keep state another one needs. This step settles the relay, for instance rows of input
and output with sequence numbers and a `NOTIFY` channel, the way logs travel, and records it in
[design/07-storage.md](design/07-storage.md) and [design/05-execution.md](design/05-execution.md).

**Recording.** `terminal_session` records each session. When a session closes:

- its output is written in the asciicast format to `terminals/<session>.cast` in the object store,
  without keystrokes;
- `terminal.closed` and the audit trail link the recording.

Recordings expire after 30 days. A session closes on release, after 30 minutes without input, or on
request. The fake agent of `bosun-testkit` learns the terminal messages, and the CLI gains
`bosun job terminal <job>`.

**Touches.** `crates/server/`, `crates/db/`, `crates/testkit/` (the fake agent's terminal),
`bin/cli/`, `contracts/openapi.json`, `docs/design/05-execution.md`, `docs/design/07-storage.md`.

**Depends on.** 4, 11.

**Out of scope.** The agent's side (step 19), the drawer in the interface (step 28).

**Acceptance.**

- A client connected to replica A and a fake agent connected to replica B exchange input and output
  in order. Nothing is lost when replica A re-establishes its listening connection.
- A second viewer is read-only, and its input is refused; it can write once the writer leaves.
- Without `job.debug` the upgrade is `403 forbidden`, and on an execution that is not held it is
  `409 invalid_state`.
- After a session closes, `terminals/<session>.cast` is a valid asciicast recording of the output
  with no input, and `terminal.closed` and an audit event link it.
- Releasing the hold closes the session for every viewer.
- A recording older than a shortened retention is deleted.
- `bosun job terminal`, run against the server harness with a fake agent that echoes, shows what was
  typed.

**Review.** yes: traffic between replicas, and a terminal is access to a job's environment that must
be permission-checked and recorded.

### 21. TOTP for local accounts

**Goal.** Give local accounts the second factor that [design/08-access.md](design/08-access.md)
promises: TOTP after RFC 6238, with SHA-1, six digits, 30 seconds and one step of drift.

**Enrolment.** The secret is encrypted under the KEK and bound to the user. Enrolment is confirmed
with a first code, and the factor can be removed.

**Sign-in.**

- A sign-in with a password to an enrolled account opens no session until `POST /api/v1/auth/totp`
  completes it.
- A code is accepted once.
- Failed codes count toward the lockout of passwords.

The design leaves open the enrolment endpoints, the shape of a sign-in that awaits its second
factor, recovery codes, and how an owner restores access to an account whose device is lost. This
step settles them in 08-access.md and 11-api.md, and gives the shapes fixtures that step 25 tests
against.

**Audit.** Enrolment, removal and failed codes are recorded in the audit trail.

OIDC accounts and personal tokens are unaffected.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `bin/server/` (the owner's restore
command, if settled so), `contracts/fixtures/api/`, `contracts/openapi.json`,
`docs/design/08-access.md`, `docs/design/11-api.md`.

**Depends on.** 4.

**Out of scope.** The interface (step 25); second factors other than TOTP; OIDC accounts, which
their provider protects.

**Acceptance.**

- The SHA-1 test vectors of RFC 6238 pass.
- A correct password without the code yields no session cookie, and with a valid code it does.
- The same code again within its window is refused, and so is a code two steps away.
- Failed codes lock the account as failed passwords do.
- The stored secret is not the secret in clear, and its ciphertext moved to another user fails to
  decrypt.
- A recovery code signs in once and never again.
- Enrolment, removal and a failed code each record one audit event.

**Review.** yes: authentication.

### 22. The MCP endpoint

**Goal.** Serve the Model Context Protocol at `/mcp` over streamable HTTP, authenticated with a
`bst_` token; a request without one gets `401` with `WWW-Authenticate: Bearer`. It offers the tools
of [design/11-api.md](design/11-api.md):

- `list_projects`, `list_workflows`, and `lint_workflow` over `POST /api/v1/lint` of M1;
- `dispatch_workflow`, `list_runs`, `get_run`;
- `get_job_log`, with `tail` and `grep`, over one step or the whole job;
- `run_command`;
- `list_approvals`, `decide_approval`;
- `get_queue`, `list_agents`, `get_annotations`.

Every tool calls the same functions and permission checks as its API endpoint, and tools that change
something say so in their description and annotations.

Replicas share nothing, so the endpoint must not need a session that one replica holds. This step
settles how, for instance with stateless requests, and which library to use, and records both in
11-api.md. `bosun-testkit` gains an MCP client for this step's tests and for step 30.

**Touches.** `crates/server/`, `crates/testkit/`, `Cargo.toml`, `docs/design/11-api.md`.

**Depends on.** 5, 15.

**Out of scope.** MCP resources and prompts; tools beyond the table.

**Acceptance.**

- The client lists the thirteen tools. `dispatch_workflow`, `run_command` and `decide_approval` are
  marked as changing something, and the others as read-only.
- Against the server harness with a fake agent, the client:
  - dispatches a run;
  - reads the failing step's log with `grep`, and receives only the matching lines with their
    numbers, and with `tail` only the last lines;
  - pauses a queued job with `run_command` and resumes it, and the journal names the token as the
    actor of both;
  - lists a waiting gate with `list_approvals` and approves it with `decide_approval`.
- `lint_workflow` returns the diagnostics that `POST /api/v1/lint` returns for the same source.
- A token without `run.control` receives the API's `forbidden` error through `run_command`, and a
  request without a token is `401`.
- Requests of one client that alternate between two replicas all succeed.

**Review.** yes: token authentication and a permission check on every tool, and behaviour across
replicas.

### 23. The audit page

**Goal.** Add the audit page to Settings: the trail of step 4 as a table with each event's actor,
action, target, data, address and time, filters by actor, action, organisation, project, target and
time, and cursor pagination. The page is offered only to holders of `audit.view`.

**Touches.** `web/`.

**Depends on.** 4.

**Out of scope.** The API (step 4).

**Acceptance.**

- Vitest tests against the fixtures of step 4:
  - each filter sends its query parameter;
  - the next page follows `next`, and the last page offers none;
  - a user whose permissions lack `audit.view` sees no audit entry in Settings.
- `pnpm lint && pnpm test && pnpm build` is green, with every new key in OwnLate.

**Review.** no.

### 24. Teams, roles and tokens in the interface

**Goal.** Build the Settings pages for identity and access that
[design/09-frontend.md](design/09-frontend.md) lists and M1 defers to M3, over the API of M1:

- **Teams** of an organisation with their members. A team synchronised from a groups claim shows its
  source and cannot be edited by hand.
- **Roles**: the built-in roles read-only, and custom roles edited as a matrix of permission keys.
- **Role bindings**: a user or a team, a role, and an organisation or a project.
- **Personal tokens**, in the personal settings: a name, permission keys, an optional restriction to
  organisations or projects, and a required expiry of at most one year. A new token is shown once,
  and tokens can be revoked.

**Touches.** `web/`.

**Depends on.** nothing in M3.

**Out of scope.** TOTP in the personal settings (step 25), plugins (M5).

**Acceptance.**

- Vitest tests against the fixtures of M1:
  - the permission matrix round-trips the custom role of its fixture, and a built-in role cannot be
    edited;
  - the binding form sends the request of its fixture;
  - the token form cannot be sent without an expiry or with one beyond a year, and a created token
    is shown once and is gone after leaving the page;
  - a synchronised team offers no editing of its members.
- `pnpm lint && pnpm test && pnpm build` is green, with every new key in OwnLate.

**Review.** no.

### 25. TOTP in the interface

**Goal.** Add the second factor to the interface:

- enrolment in the personal settings: the QR code of the secret, the confirmation with a first code,
  the recovery codes shown once, and removal;
- the second step of sign-in after a password, which takes a code or a recovery code.

The QR code library is chosen in this step.

**Touches.** `web/`.

**Depends on.** 21, 24.

**Out of scope.** The server side (step 21).

**Acceptance.**

- Vitest tests against the fixtures of step 21:
  - enrolment renders the QR code of the fixture's secret and sends the confirmation code;
  - the recovery codes are shown once;
  - a sign-in answered with the second-factor shape asks for a code and sends it to `/auth/totp`;
  - a wrong code shows the error and keeps the form.
- `pnpm lint && pnpm test && pnpm build` is green, with every new key in OwnLate.

**Review.** no.

### 26. Run page controls and history

**Goal.** Add the controls of [design/09-frontend.md](design/09-frontend.md) to the run page of M1:

- every command that the `commands` of the run, a job or a step lists, as a button on what it acts
  on;
- a reason, asked for by every command that needs one and sent with it;
- the outputs form of `job.mark-success`, built from the job's declared outputs;
- the choice of `as` for `step.abort` and `hold.continue`;
- priority, and pin through a picker of the online agents whose labels and pool fit;
- `debug` on re-runs;
- breakpoints before or after a step, set by clicking the gutter.

The graph draws gates distinctly, paused and held jobs show their deadlines, conclusions show their
source, and a job shows its environment and URL.

The history panel shows the journal as a timeline:

- filtered to interventions, or showing everything;
- with the actor, the command and the reason;
- with approvals and their inputs;
- with links to terminal recordings;
- kept live by the event stream.

**Touches.** `web/`.

**Depends on.** 1.

**Out of scope.** The terminal drawer and "Run again with changes" (step 28), approvals (step 27).

**Acceptance.**

- Vitest tests against the fixtures of step 1:
  - a job shows exactly the buttons its `commands` list;
  - a command that needs a reason cannot be sent without one, and the request carries it;
  - a gutter click sends `step.breakpoint` with the position;
  - every journal kind renders in the timeline with its actor, and the interventions filter keeps
    only the events of people, tokens and plugins.
- A Vitest test with user-event reaches every control of a job with the Tab key alone and activates
  it with Enter or Space.
- `pnpm lint && pnpm test && pnpm build` is green, with every new key in OwnLate.

**Review.** no.

### 27. Approvals and environments in the interface

**Goal.** Build the Approvals page: the inbox of gates and environments waiting for the user. Each
request shows its message, and the form for the gate's inputs is generated from their schema
(`string`, `boolean`, `choice`, `number`, `environment`). The user approves or rejects with a
comment, and bypasses where that is offered. The home page lists the approvals waiting for the user,
and both stay live through the event stream.

Environments appear in the settings of projects and of organisations:

- the list of environments;
- the rule editor: reviewers from users and teams, minimum approvals, self-review, wait timer,
  branch and tag patterns, administrator bypass;
- environment secrets (write-only) and variables;
- a conflict on a stale `If-Match`, shown together with the newer version.

**Touches.** `web/`.

**Depends on.** 1.

**Out of scope.** The run page (steps 26 and 28), the server side (steps 15 to 17).

**Acceptance.**

- Vitest tests against the fixtures of step 1:
  - the input form renders each input type with its default and refuses a value that does not fit
    before sending;
  - approve sends the inputs and the comment;
  - reject cannot be sent without a comment;
  - an approval event removes the request from the inbox.
- The rule editor round-trips every rule of the environment fixture.
- A `412` shows the newer version without losing the edit.
- `pnpm lint && pnpm test && pnpm build` is green, with every new key in OwnLate.

**Review.** no.

### 28. Terminal drawer, run again with changes, occupied slots

**Goal.** Three parts of the interface.

**Terminal drawer.** A held job opens its terminal in a drawer, with xterm.js over the WebSocket of
step 1:

- writable for the writer, and read-only for watchers, with a notice saying so;
- resized with the drawer, and reconnected when the connection drops;
- recordings linked from the history play back in the same component.

**"Run again with changes".** In the run header, a dialog takes the run's inputs, prefilled and
editable, and the ref, and sends them to `derive`. The new run's page links back to the original.

**Occupied slots.** The queue and agents pages show the agents holding a paused or held job, and the
slots occupied that way.

**Touches.** `web/`.

**Depends on.** 1.

**Out of scope.** The terminal relay and recording on the server (step 20), `derive` itself (step
14).

**Acceptance.**

- Vitest tests with a mocked WebSocket:
  - output frames reach the terminal;
  - keystrokes leave only in the writer's role;
  - a resize sends its frame;
  - a closed session says so.
- A recording fixture plays back to the same screen as the live session produced.
- The derive dialog sends the edited inputs and the ref, and navigates to the run in the response.
- An agent fixture holding a held job is shown as such, and its slot is counted as occupied.
- `pnpm lint && pnpm test && pnpm build` is green, with every new key in OwnLate.

**Review.** no.

### 29. Commands end to end

**Goal.** Extend `hack/e2e.sh` with a scenario for every command of the command table of
[design/04-runs.md](design/04-runs.md), run on the real binaries: two server replicas, two `shell`
agents, one of them with `allow_terminal = true`, and, in CI, a `podman` agent.

- Each command is given through the CLI and checked through the journal.
- The commands of earlier milestones are included: `run.cancel`, `job.cancel`, `run.admit` with the
  fake forge of M1's scenarios, and the re-runs.
- The debug re-run scenario checks that a `::debug::` line of a step appears in its log.

A test reads the command table and fails, naming the command, when a command has no scenario.

**Touches.** `hack/e2e.sh`, `hack/e2e/`, `.github/workflows/ci.yml`.

**Depends on.** 6, 9, 11, 13, 17.

**Out of scope.** The scenarios of the exit checklist and the Playwright cases (step 30); load and
soak tests.

**Acceptance.**

- Every scenario passes in CI.
- Removing the scenario of a command makes the coverage test fail with the command's name.
- The log of a step in a debug re-run shows the `::debug::` line that the log of the original run
  does not.

**Review.** no.

### 30. The exit scenarios end to end

**Goal.** Add the scenarios of the exit checklist to `hack/e2e.sh`, and Playwright cases on the run
page against the servers it starts:

- an approver's gate input, printed by the dependent job's step;
- a held job whose cause is fixed through `bosun job terminal`, followed by `hold.retry-step`, the
  job succeeding, and its recording stored in the object store and linked from the journal and the
  audit trail; in CI also in a `podman` job container;
- the MCP client of `bosun-testkit` (step 22), which dispatches a run, greps a failing step's log,
  and pauses and resumes a running job on a real agent;
- a Playwright case in which the run page's history shows who gave each command of a scenario;
- a Playwright case in which a held job opens its terminal in the drawer, typed input reaches the
  shell, and the recording plays back from the history.

**Touches.** `hack/e2e.sh`, `hack/e2e/`, `web/e2e/`, `.github/workflows/ci.yml`.

**Depends on.** 19, 20, 22, 26, 28, 29.

**Out of scope.** TOTP and the settings pages, which their own steps cover.

**Acceptance.**

- Every scenario passes in CI.
- The Playwright cases pass against the servers that `hack/e2e.sh` starts.

**Review.** no.

### 31. Agent credential rotation and pool address ranges

**Goal.** The two protections of agent credentials that [design/08-access.md](design/08-access.md)
lists under "What a stolen credential gets" and M1 leaves to M3:

- **Rotation.** The server asks an agent to rotate its credential in `Welcome`, as
  [design/05-execution.md](design/05-execution.md) describes, and the old credential stays valid
  until the agent has confirmed the new one. The agent writes the new credential into its
  configuration file, readable only by its user, before it confirms. A holder of `agent.manage` asks
  for the rotation of one agent or of every agent of a pool, and a pool may set a rotation period.
  The design leaves open how the new credential reaches the agent and how the agent confirms it;
  this step settles both in 05-execution.md, and the endpoints in
  [design/11-api.md](design/11-api.md).
- **Address ranges.** A pool gains the address ranges its agents may connect from, set with
  `agent.manage`. Registration and the agent stream from an address outside them are refused with
  `403 forbidden`. The address is taken from `X-Forwarded-For` only behind `server.trusted_proxies`,
  as sign-in takes it.

Rotations and changes of the ranges are recorded in the audit trail of step 4. The CLI gains
`bosun agent rotate`, and the pool commands of M1 take the address ranges.

**Touches.** `proto/` and `crates/proto/` (the confirmation, if it needs a message of its own),
`crates/server/`, `crates/db/`, `bin/agent/`, `bin/cli/`, `crates/testkit/` (the fake agent
rotates), `contracts/openapi.json`, `docs/design/05-execution.md`, `docs/design/11-api.md`.

**Depends on.** 4.

**Out of scope.** Executor plugins, which provide agents of their own (M5).

**Acceptance.**

- An agent asked to rotate connects with its new credential afterwards, and its old credential is
  refused once the server has the confirmation.
- An agent whose stream drops after it received the new credential and before it confirmed it still
  connects with the old one, and is asked again.
- With the clock injected, a pool's rotation period rotates each of its agents once per period
  without cancelling a running job.
- Registration and a stream from an address outside the pool's ranges are refused, directly and
  through a trusted proxy, and an untrusted proxy's `X-Forwarded-For` lets no address in.
- Without `agent.manage`, a rotation request and a change of the ranges are `403 forbidden`.
- Each rotation and each change of the ranges records one audit event, and no credential appears in
  `audit_event`.

**Review.** yes: agent credentials, and the addresses they are accepted from, decide who receives
jobs and their secrets.

### 32. Dead outbox rows

**Goal.** The administration page of dead outbox rows that
[design/07-storage.md](design/07-storage.md) names, which M1 leaves to M3 after logging the rows at
error level and counting them in `/metrics`. Installation owners list the dead rows with their kind,
target, attempts and last error, and retry a row, which puts it back with its attempts reset, or
discard it. The design does not name the endpoint; this step settles it and records it in
[design/11-api.md](design/11-api.md). Settings gain the page, offered only to owners, and retries
and discards are recorded in the audit trail of step 4.

**Touches.** `crates/server/`, `crates/db/`, `crates/types/`, `contracts/fixtures/api/`,
`contracts/openapi.json`, `web/`, `docs/design/07-storage.md`, `docs/design/11-api.md`.

**Depends on.** 4.

**Out of scope.** Outgoing webhooks and the list of their deliveries (M5).

**Acceptance.**

- With the clock injected, a row the fake forge refuses for 24 hours is listed as dead with its last
  error, and a retry after the fake recovers delivers it once.
- A discarded row is no longer listed and is never delivered.
- A caller who is not an owner gets `403 forbidden`.
- Each retry and each discard records one audit event.
- Vitest: the page renders the rows of its fixture and sends retry and discard.
- `pnpm lint && pnpm test && pnpm build` is green, with every new key in OwnLate.

**Review.** no.

## How we work

- The code lives in `GlobalArtInc/bosun-ci` on `main`. Push straight to `main`; one task is one change
  and is not split across branches.
- Verify with the commands CI runs before pushing: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` against Postgres,
  the WebAssembly build of the pure crates, `hack/check-deps.sh`, and from M0 step 14
  `hack/e2e.sh`. Once `web/` exists, `pnpm lint && pnpm test && pnpm build` join the list.
- CI runs on the self-hosted runners only; hosted runners are used only by the public
  compatibility repository, whose minutes are free.
- English everywhere in the repository: code, documentation, tests, CI, commit messages.
- Commit titles follow Conventional Commits (`feat(server): claim jobs with skip locked`); commit
  messages end with their last line of content, without attribution trailers.
- No comments in code. Explanations go into the commit message or into `docs/`; the only
  exception is a link to a specification next to the code that implements it.
- When code and design disagree, fix one of them in the same change.
- Postgres is the only database; tests get a database per test from a template, and there is no
  in-memory substitute.

## M0 exit checklist

- [ ] A two-job workflow runs end to end through `bosun workflow dispatch --watch` with live logs.
- [ ] A failing step fails its job, skips the dependent job and fails the run.
- [ ] `bosun run cancel` stops a running `sleep 600` within 10 seconds.
- [ ] A server restart in the middle of a job loses and duplicates no log line.
- [ ] Agents receive only jobs their labels satisfy.
- [ ] A cancel through one replica reaches an agent connected to the other.
- [ ] Projected statuses equal the replay of the journal in the property test.
- [ ] `cargo fmt --all --check` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `cargo test --workspace` is green against Postgres.
- [ ] The WebAssembly build of `bosun-types` and `bosun-workflow` is green.
- [ ] `hack/check-deps.sh` and `hack/e2e.sh` are green.
- [ ] The image starts on a clean host with only a Postgres address and runs the two-job workflow on
  an agent installed with the command the server prints.

## M1 exit checklist

- [ ] `hack/e2e.sh`: a push through the fake forge runs a workflow with `actions/checkout`, a matrix
  of two and job outputs. The fake records one check run per job, both matrix members included, each
  with conclusion `success`.
- [ ] (operator) A push to the test repository runs that workflow on the runner host's Bosun agent,
  and its pull request shows one check run per job with the right conclusions.
- [ ] The expression cases pass and match the committed recordings.
- [ ] (operator) The committed recordings come from a run of the public compatibility repository on
  GitHub's hosted runners, for the cases of this commit.
- [ ] A secret printed raw, as JSON and as base64 appears in none of these forms in the stored log,
  which shows `***` in their place.
- [ ] A fork pull request in a project with the default policy creates no run, and the fake forge
  records a status saying so.
- [ ] Through the CLI, a viewer cannot dispatch and a developer cannot write a secret.
- [ ] Every workflow under `.bosun/workflows/` validates without diagnostics, and `hack/e2e.sh` runs
  each of them on the shell agent against the fake forge with conclusion `success`.
- [ ] (operator) Bosun's simple workflows run on Bosun next to GitHub Actions on
  `GlobalArtInc/bosun-ci`, on the runner host's agent, with the same results.
- [ ] (operator) The corpus inventory covers every repository of GlobalArtInc and OwnLate, and none
  of their workflows has an `unknown_key`.
- [ ] `contracts/openapi.json` equals the document generated from the handlers.
- [ ] `hack/compat-recordings.py generate --check` finds the recording workflow up to date.
- [ ] `cargo fmt --all --check` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `cargo test --workspace` is green against Postgres and the local S3 server, including the S3
  conformance suite.
- [ ] The WebAssembly build of `bosun-types`, `bosun-expr` and `bosun-workflow` is green.
- [ ] `hack/check-deps.sh` and `hack/test-check-deps.sh` are green.
- [ ] `hack/e2e.sh` is green, including the Playwright scenarios.
- [ ] `pnpm lint && pnpm test && pnpm build` in `web/` is green.

## M2 exit checklist

- [ ] The corpus case `keel`, which runs Keel's four workflows byte for byte, passes end to end
  against the fake forge with real podman in CI, for push, pull request, tag and dispatch events.
- [ ] (operator) The CI of `GlobalArtInc/keel` runs unchanged from `.bosun/workflows/` on Bosun,
  with the same results as GitHub Actions for a week of pushes.
- [ ] `actions/upload-artifact@v4` and `actions/download-artifact@v4` move a file between jobs (case
  `artifacts`).
- [ ] `actions/cache@v4` restores what it saved over the current protocol (case `cache-v4`).
- [ ] `actions/cache@v3.3.3` restores what it saved over the earlier protocol (case `cache-v3`).
- [ ] A job with a Postgres service container passes its tests against it, on the host and in a job
  container (case `postgres-service`, real podman in CI).
- [ ] The corpus case `bosun-ci` passes, and every file of `.bosun/workflows/` equals its
  counterpart in `.github/workflows/`.
- [ ] (operator) Bosun's own CI runs on Bosun from `.bosun/workflows/` on the runner host's agents
  and agrees with GitHub Actions.
- [ ] A `check_run.rerequested` from the fake forge by a linked user creates the next attempt with
  that user as actor (step 20).
- [ ] (operator) A re-run requested from a check of a pull request on GitHub creates the next
  attempt on Bosun, with the linked user as actor.
- [ ] The cases `composite`, `composite-post`, `container-action`, `container-action-phases` and
  `reusable` pass in CI.
- [ ] (operator) The M2 cases of the public compatibility repository are recorded on GitHub's hosted
  runners and match Bosun's results.
- [ ] `docs/load-test.md` records the numbers of a full load test with its host and Postgres sizing,
  and `hack/load-test.sh --smoke` is green in CI.
- [ ] `cargo fmt --all --check` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `cargo test --workspace` is green against Postgres, with `BOSUN_TEST_PODMAN=1` in CI.
- [ ] The WebAssembly build of `bosun-types`, `bosun-expr` and `bosun-workflow` is green.
- [ ] `hack/check-deps.sh` and `hack/test-check-deps.sh` are green.
- [ ] `hack/e2e.sh` is green with every case under `compat/cases/`, including the podman agent.
- [ ] `hack/check-bosun-workflows.sh` is green.
- [ ] The generated `contracts/openapi.json` equals the committed one.
- [ ] `pnpm lint && pnpm test && pnpm build` in `web/` is green with the dictionaries pulled from
  OwnLate.

## M3 exit checklist

- [ ] `hack/e2e.sh` has a scenario for every command of the command table of design/04-runs.md,
  every scenario passes, and the coverage test fails for a command that has none.
- [ ] An approver's gate input reaches the step of the dependent job that prints it, in
  `hack/e2e.sh`.
- [ ] A held job's terminal session is recorded to the object store and linked from the journal and
  the audit trail, `hold.retry-step` after a fix made through `bosun job terminal` lets the job
  succeed, and the run page's history shows who gave each command (end-to-end scenarios and
  Playwright cases against `hack/e2e.sh`).
- [ ] (operator) On prod1, a held job on the runner host's agent with `allow_terminal = true` opens
  its terminal from the run page, `hold.retry-step` after a fix lets it succeed, and its recording
  plays back from the history.
- [ ] An MCP client dispatches a run, reads a failing step's log with `grep`, and pauses and resumes
  a running job, against the binaries of `hack/e2e.sh` with a real agent.
- [ ] (operator) The team's MCP client, connected to prod1 with a personal token, does the same.
- [ ] With `forge_deployments` on, a job with an environment creates a deployment and its statuses
  on the fake forge.
- [ ] (operator) The same on a test repository on GitHub, once the App holds the Deployments
  permission.
- [ ] Environment secrets and variables take precedence over project and organisation ones as the
  case in `compat/` pins.
- [ ] (operator) The environment cases recorded on GitHub's hosted runners in the public
  compatibility repository match Bosun's results.
- [ ] Projected statuses equal the replay of the journal in the property test, with M3's commands,
  holds and job retries interleaved.
- [ ] `cargo fmt --all --check` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `cargo test --workspace` is green against Postgres.
- [ ] The WebAssembly build of `bosun-types`, `bosun-expr` and `bosun-workflow` is green.
- [ ] `hack/check-deps.sh` and `hack/test-check-deps.sh` are green.
- [ ] `hack/e2e.sh` is green with the M3 scenarios.
- [ ] `pnpm lint && pnpm test && pnpm build` in `web/` is green.
- [ ] The generated OpenAPI document equals `contracts/openapi.json`.
