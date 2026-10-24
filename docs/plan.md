# Plan

This document orders the work: the milestones M0 to M5 with their scope and exit criteria, a
breakdown of M0 into steps of one change each, the working rules of the repository, and the M0
exit checklist. The design lives in `design/`; this document only says in which order it is built
and how each step is shown to be done. Nothing here carries a date or a duration.

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
  `on: workflow_dispatch` without inputs, literal `env`, jobs with `runs-on`, `needs`, `env` and
  `steps`, steps with `id`, `name`, `run`, `shell`, `working-directory`, `env`.
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

Done when:

- a pushed workflow with two jobs, the second needing the first, runs on a registered agent after
  `bosun workflow dispatch --watch`, with its logs streamed live into the terminal;
- a failing step fails its job, the dependent job is skipped and the run fails;
- `bosun run cancel` stops a running `sleep 600` step within 10 seconds and the run ends cancelled;
- a restart of the server in the middle of a job loses no log line and duplicates none;
- two agents with different labels receive only the jobs their labels satisfy;
- with two server replicas on one database, a cancel issued through one replica reaches an agent
  connected to the other;
- the CI commands are green, including the WebAssembly build of the pure crates.

### M1 — GitHub and the job language

Scope:

- `bosun-expr` in full; `bosun-workflow` for the whole compatibility surface of
  [design/02-workflows.md](design/02-workflows.md), with execution of what is listed here and,
  for the rest, an error that names the milestone it arrives in.
- The GitHub App: webhooks, installations, projects bound to repositories, workflow files read at
  the commit, the triggers `push`, `pull_request`, `pull_request_target`, `workflow_dispatch` with
  inputs, `schedule` with `timezone`, `repository_dispatch`, `merge_group`, generic events, path
  filters, the fork policy, no runs from the App's own events, check runs and commit statuses
  through the outbox.
- Jobs: `if`, matrix expansion with `include`, `exclude`, `fail-fast` and `max-parallel`, outputs,
  `concurrency` with `queue`, `timeout-minutes`, `continue-on-error`, `defaults`, label aliases.
- Steps: every context, file commands, workflow commands, annotations, step summaries, masking.
- Node actions with their pre and post steps: the action store on the server, Node runtimes for
  agents, the forge token, so that `actions/checkout` and other Node actions work.
- Secrets and variables at organisation and project scope, encrypted under the KEK.
- Sign-in with local passwords and OpenID Connect, sessions, roles and bindings, scoped tokens.
- The S3 backend; the event stream.
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
  socket, cache volumes.
- Composite actions, `uses: ./path`, `uses: docker://`.
- `bosun-services`: the artifact service and both cache protocols with the block blob upload
  endpoint, and `cache-mode`; `bosun exec` on top of it.
- Reusable workflows, local and from other repositories.
- `priority`, fair share and project caps; `on.upstream`; the action policy; `run.rerun`,
  `run.rerun-failed` and `job.rerun`, also when requested from the forge.
- A load test of log ingestion and dispatch with many agents, its numbers recorded in
  `docs/`.

Done when:

- the CI of Keel runs unchanged from a copy under `.bosun/workflows/` with the same results as on
  GitHub Actions;
- `actions/upload-artifact@v4` and `actions/download-artifact@v4` move a file between jobs,
  `actions/cache@v4` restores what it saved, and so does `actions/cache@v3.3.3`, a release from
  before the current cache protocol;
- a job with a Postgres service container passes its tests against it;
- Bosun's own CI runs on Bosun.

### M3 — Run control

Scope:

- Every remaining operator command of [design/04-runs.md](design/04-runs.md), from the API, the
  CLI and the run page, with the journal timeline.
- Gates with inputs, environments with protection rules, the approval inbox.
- `retry`, `pause` and breakpoints, `debug.hold-on-failure`, the debug terminal and its recording,
  debug re-runs, "run again with changes".
- TOTP for local accounts; the audit trail in the interface.
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
  autosave; running a draft; saving as a commit or a pull request with a three-way merge;
  server-side workflows with revisions and export; the action metadata endpoint.

Done when:

- for every workflow of the corpus, loading it and saving it without changes produces the same
  bytes, and renaming a job in the graph rewrites its references and nothing else;
- a draft run does not appear on the forge unless asked to;
- saving a draft opens a pull request with exactly the edited file.

### M5 — Plugins

Scope:

- `bosun-plugin-host` and the WIT package `bosun:plugin@1.0.0`; the external plugin protocol;
  installation from OCI with signatures; instances with configuration forms; `notify` routes,
  `on.plugin` triggers, admission policies, `plugin()` functions, secret backends.
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

M0 is split into fourteen steps. Each step is one change to `main`, ordered by dependency; a step
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

**Touches.** `bin/server/`, `crates/server/`, `Cargo.toml` (adds `bin/*` to the members).

**Out of scope.** Authentication (step 7), the interface.

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

**Touches.** `crates/server/`, `crates/db/`.

**Out of scope.** Passwords, OpenID Connect, sessions, roles (M1).

**Acceptance.**

- A request without a token gets `401 unauthenticated`; with a revoked or expired token too.
- The token's checksum is verified before any database lookup.

### 8. Projects, server-side workflows and dispatch

**Goal.** Organisations and repository-less projects; server-side workflows created and updated
through the API, each save validated and stored as a revision; `POST /workflows/{id}/dispatch`
creating a run with its first attempt and its pending jobs, and the IR snapshot.

**Touches.** `crates/server/`, `crates/db/`.

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

**Touches.** `crates/server/`, `crates/db/`.

**Out of scope.** `if`, matrices, gates, environments, every other command.

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
tasks.

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
compaction into the object store after the execution completes, and the 128 MiB limit.

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

## How we work

- The code lives in `GlobalArtInc/bosun` on `main`. Push straight to `main`; one task is one change
  and is not split across branches.
- Verify with the commands CI runs before pushing: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` against Postgres,
  the WebAssembly build of the pure crates, `hack/check-deps.sh`, and from step 14 `hack/e2e.sh`.
  Once `web/` exists, `pnpm lint && pnpm test && pnpm build` join the list.
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
