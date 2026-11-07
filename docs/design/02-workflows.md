# Workflows

This document specifies where workflow definitions come from, how they are parsed and validated,
which part of the GitHub Actions workflow syntax Bosun implements, which keys Bosun adds, and the
intermediate representation (IR) that the planner and the agent work from. Expressions inside
workflows are specified in [03-expressions.md](03-expressions.md); what happens to a run once it
exists is in [04-runs.md](04-runs.md).

## Sources

A workflow has exactly one source.

- **Repository file.** Every `*.yml` and `*.yaml` file directly under `.bosun/workflows/` at the
  commit a trigger refers to. When the project setting `github_workflows` is on, the files under
  `.github/workflows/` are read as well; a file with the same name in both directories is taken
  from `.bosun/workflows/` only. The server reads the directories through the forge API at the
  exact commit and caches parsed files by the forge's blob hash, so a push that does not touch a
  workflow file costs no parsing.
- **Server-side record.** A workflow created in the web interface, the API or the CLI and stored
  in the database, with its own history of revisions. It belongs to a project, which may be bound
  to a repository or be repository-less. This is the home of operational jobs that have no natural
  repository: backups, runbooks, scheduled maintenance. Its name follows the rule of slugs
  ([08-access.md](08-access.md)), is unique among the server-side workflows of its project, and is
  the `file` of its diagnostics, since it has no path.

A **revision** is an immutable source text identified by the SHA-256 of its bytes and its path.
A repository file yields a new revision whenever its blob changes; a server-side workflow yields
one on every save of a source it has not had before, and a save of a source it had makes that
revision current again, so saving the same source twice yields one revision. Every save validates
the source first and stores nothing when it has errors. A run always refers to the revision it was
planned from and keeps a snapshot of the IR it was lowered to, so a later change of the parser never
changes the meaning of a past run.

A **draft** is an unsaved edit kept on the server for one workflow and one author. A draft can be
run ([04-runs.md](04-runs.md)) and turned into a commit or a pull request
([09-frontend.md](09-frontend.md)). Drafts are never picked up by triggers.

A workflow can be disabled per project, with `PATCH /workflows/{workflow}` and `workflow.manage`
([11-api.md](11-api.md#projects-and-server-side-workflows)). A disabled workflow is still parsed and
shown, but no trigger creates a run of it; a manual dispatch says why it was refused.

### Repository workflows

Step 27 of M1 ([../plan.md](../plan.md)) settled how the forge's events become runs of a
repository-bound project's files.

- **Which events.** Every delivery the worker of
  [06-runtime-services.md](06-runtime-services.md#deliveries) processes, other than the App's own
  `installation`, `installation_repositories` and `repository` events and `ping`, belongs to the
  project bound to the payload's `repository.id`, if any; a repository that no installation sees
  receives nothing. `pull_request`, `pull_request_target`, `pull_request_review`,
  `pull_request_review_comment` and `merge_group` are planned as [Pull requests and merge
  groups](#pull-requests-and-merge-groups) says, and a delivered `workflow_dispatch`, which
  the forge sends for a dispatch of one of its own workflows, starts no run: dispatches of Bosun's
  workflows come through its API.
- **Bosun's own events.** An event whose `sender` has the id of the App's bot starts no run, unless
  it is `repository_dispatch` (or `workflow_dispatch`, which the API starts). The server reads the
  App's identity from the forge once per process. A push of the bot to the default branch is still
  read, for its schedules and the current revisions, and plans no run
  ([Schedules](#schedules)).
- **Which commit.** The files are read at the commit the `github` context names for the event
  ([06-runtime-services.md](06-runtime-services.md#the-github-context-and-the-environment)): a
  push's `after`, or the default branch's last commit when it deletes a ref; for `create` and
  `release`, the commit of the created ref or of the release's tag; for `deployment` and
  `deployment_status`, `deployment.sha`; for every other event, the last commit of the default
  branch, which the server resolves through the forge. A ref that resolves to nothing, as in a
  repository without commits, starts no run. The run records that ref and that commit, which the
  planner gives the `github` context with the repository and the forge's addresses.
- **Which files.** Every file ending in `.yml` or `.yaml` directly under `.bosun/workflows/` at that
  commit, and with `github_workflows` on, under `.github/workflows/` the files whose names
  `.bosun/workflows/` does not have. Each file is a workflow of the project named by its path,
  created the first time an event reads it; its revisions are its blobs.
- **The blob cache.** A revision of a repository workflow carries the forge's hash of its blob, so
  the server reads a file's content from the forge only for a blob the workflow has no revision of;
  the directory listings, which carry the hashes, are all an event reads otherwise. What a file
  lowers to (its triggers, its IR when it has no error, and every diagnostic `lint` gives at its
  path) is kept in memory by each replica, keyed by the path and the blob's hash, for the 256 files
  parsed last; the oldest goes first. A new version of the server therefore parses afresh, so
  nothing a parser of another version lowered is ever reused. Each event is logged as `planned the runs of an
  event` with the counters of the cache: `files` read, `parsed` and `reused`, and `runs` planned.
- **Current revisions.** An event read at the last commit of the default branch makes the
  revisions it read the current ones of their workflows, so `revision_id` follows the default
  branch; a workflow seen only on another branch has none.
- **Matching.** Each file's `on` is matched with `matches` ([Matching events](#matching-events)):
  the event's name, its `action`, and for a push its `ref` and its changed files. A match plans a
  run in the transaction of the delivery, with the payload and the `sender` as the trigger, the
  actor being of the kind `forge`; the runs of one delivery commit together with its processing,
  so a delivery that is tried again plans them once. The warnings of the file become the run's
  first notices. A disabled workflow creates no run, not even a `startup_failure`.
- **Broken files.** A file with errors whose `on` matches yields a run that is completed at once
  with `startup_failure`: it has no jobs, its IR holds only the file's triggers, and its notices are
  every diagnostic of the file, errors and warnings, in their order. A file whose `on` itself
  cannot be parsed yields such a run for every push that changes it. A file the planner cannot run
  yet, because it uses a key whose step has not arrived
  ([11-api.md](11-api.md#projects-and-server-side-workflows)), yields one too, with an error notice
  `not_implemented` at that key.
- **Changed files of a push.** The cases in `contracts/fixtures/pushes/` pin, for each kind of push,
  where its workflows are read and what the server compares:
  - a push to an existing branch, forced or not, compares `before` with `after`;
  - a push that creates a branch compares the default branch with `after`, the changes of the new
    branch since it left the default branch; the push that creates the default branch itself, the
    first of a repository, compares nothing and changes files the server does not list;
  - a tag push compares nothing, since path filters do not apply to tags, and changes no workflow
    file for the rule of broken files;
  - a push that deletes a branch or a tag is read at the default branch and changes no file, so a
    workflow with path filters does not run for it;
  - a comparison the forge truncated, and one it cannot make because it does not know a commit,
    changes files the server does not list: path filters are then met, and every file counts as
    changed for the rule of broken files.

### Pull requests and merge groups

Step 29 of M1 ([../plan.md](../plan.md)) settled the events of pull requests and merge groups. Each
reads its files at the commit its `github` context names
([06-runtime-services.md](06-runtime-services.md#the-github-context-and-the-environment)), and the
run records that ref and that commit:

- `pull_request`, `pull_request_review` and `pull_request_review_comment` run at
  `refs/pull/<number>/merge`, at the commit the server resolves it to through the forge, which is
  the merge commit the forge made of the pull request's head and its base. The forge makes that
  commit again in the background once the head moves, so the event of a push can come while the
  merge ref still names the merge of the previous head, and a run there would test code other than
  the head its checks are reported on. The server therefore reads the parents of the commit, and
  while they do not include `pull_request.head.sha`, the delivery fails as one that may pass and is
  tried again ([06-runtime-services.md](06-runtime-services.md#deliveries)). At its tenth attempt,
  about eight and a half minutes after the first, the event starts no run at the merge ref, as for
  a pull request with conflicts, whose merge ref the forge no longer moves; a pull request whose
  merge ref resolves to nothing starts none either. Either way the `pull_request_target` runs of
  the delivery are planned as usual. A merged pull request (`pull_request.merged`) runs at
  `refs/heads/<base>` and its `merge_commit_sha`, the commit the merge left on the base branch.
- `pull_request_target` runs at `refs/heads/<base>` and the base branch's last commit, so the
  files are the base branch's, whatever the pull request changes in them. The forge delivers no
  event of that name: a `pull_request` delivery is planned twice, as `pull_request` and as
  `pull_request_target`, with the same payload and actor, and a delivery named
  `pull_request_target` is planned as nothing.
- `merge_group` runs at `merge_group.head_ref` and `merge_group.head_sha`.

`branches` match `refs/heads/<base>` of the pull request, and for a merge group its
`merge_group.base_ref`. `paths` of `pull_request` and `pull_request_target` match the files of the
pull request, which the server lists through the forge only for an event that a file read for it
filters by paths, so most events cost no such request. The forge lists at most 3000 files of a pull
request and does not say whether it stopped there, so a list of 3000 counts as truncated, and as
for a push whose comparison the forge truncated, the files are then not listed and every path
filter is met. These events are not pushes, so no file of theirs counts as changed for the rule of
broken files. A pull request from a fork meets the project's fork policy before any run is created
([08-access.md](08-access.md#code-from-outside-the-team)).

The case `contracts/fixtures/github/pull-request-merged.json` pins the `github` context of a
merged pull request, and the server's tests in `bin/server/tests/server/pull_requests.rs` pin
against the fake forge the ref and the commit each of these events runs at: a merged pull request,
one the forge made no merge commit of, one whose merge ref lags behind its head, which the fake
shows by holding its merge commits back, and one with more files than the forge lists.

### Schedules

Step 31 of M1 ([../plan.md](../plan.md)) settled how `on.schedule` becomes runs.

- **The table.** A `schedule` row is one entry of a workflow's `schedule`, identified by the
  workflow, the cron as written and its time zone (`UTC` when the entry has none), so two equal
  entries of one file are one schedule. It holds the revision it was read from, the cron as the IR
  lowers it, the ref and commit a run of it starts at, its actor and its next instant.
- **Rebuilt from the default branch.** Every push to the default branch rebuilds the rows of every
  repository workflow of the project from the files the push reads at its `after`
  ([Repository workflows](#repository-workflows)): a file whose `on` lowers gives its entries, its
  other errors included, so that a broken file's schedule yields `startup_failure` runs as its
  other triggers do; a file that is gone, whose `on` is broken or that no longer has the entry
  loses it. The rows take that commit as their commit and the default branch as their ref, which
  is the default branch's last commit as Bosun last saw it, and the revision the push read. A push
  the App's own bot sends to the default branch starts no run but rebuilds the rows all the same,
  so a file a job commits is scheduled as written. A push to any other branch, a tag push and every
  other event leave the rows as they are. A project bound to another repository, or unbound, loses
  the rows of its repository files until a push to the new repository's default branch.
- **Rebuilt on every save.** Creating a server-side workflow and every `PUT
  /workflows/{workflow}/source` rebuild its rows from the source in the same transaction, from a
  source saved again as much as from a new one. Its runs have no ref and no commit, as its
  dispatches have none.
- **The actor.** The actor of a scheduled run is whoever last changed its entry: the sender of the
  push to the default branch, or the user or token that saved the server-side workflow, when the
  entry first appeared with its cron and time zone. A rebuild that keeps an entry keeps its actor
  and its next instant, so a push that changes another line of the file does not take the schedule
  over. This follows the target, where the user who changes the cron of a schedule becomes its
  actor, and gives `github.actor` and `github.triggering_actor` an account to name, which actions
  that commit or comment read. The run's trigger, its attempt and its journal events carry that
  actor.
- **The scheduler.** Each replica wakes one second into every minute of its clock and takes the
  advisory lock `bosuncrn` ([07-storage.md](07-storage.md#periodic-tasks)); the replica that holds
  it finds every row whose next instant has come. Each one then fires in a transaction of its own,
  which locks the row with `SKIP LOCKED`, checks again that it is due, moves it to its first
  instant after the round's time and plans the run with step 7's payload, `{"schedule": "<cron>"}`
  with the cron as written ([06-runtime-services.md](06-runtime-services.md)). So two replicas
  never fire one instant twice, and a schedule whose instants passed while no replica ran fires
  once at the next round, not once for each instant it missed.
- **What fires.** A disabled workflow's schedule moves to its next instant and creates no run, so
  enabling the workflow again fires none of the instants it missed. So does the schedule of a
  repository file when no installation of the GitHub App sees the repository any more. Otherwise
  the run is planned from the row's revision as an event of the name `schedule` is
  ([Repository workflows](#repository-workflows)), with the revision's warnings as notices, or
  completed as `startup_failure` when the file has errors.
- **Instants.** A cron is evaluated in its time zone, at whole minutes, a day matching its day
  fields by the rule of `days`. Across a change of the time zone's offset, as the cases in
  `contracts/fixtures/schedules/cases.json` pin:
  - a local time that the change to daylight saving time skips advances to the first instant after
    the gap, as the target documents (`30 2 * * *` in `Europe/Belgrade` fires at 03:00 on the
    night the clocks go forward), and several skipped times of one cron fire once there;
  - a local time that the change from daylight saving time repeats fires at its first occurrence
    only, so a nightly job runs once; a cron that selects every hour of the day fires at both,
    since it counts hours rather than naming a time of day. The target documents only the first
    change. For the second, cron on Linux runs a job at both occurrences when its minute or hour
    field is written with `*`; Bosun decides by the hours the IR keeps, so `0 */2 * * *` fires
    once there.
- **Never.** A cron with no instant in the next eleven years, such as one for the 31st of
  February, never fires; its row keeps no next instant.

### Runs that follow runs

Step 16 of M2 ([../plan.md](../plan.md)) settled how `on.workflow_run` and `on.upstream` become runs;
the mechanism that creates them is in [04-runs.md](04-runs.md#triggers-between-runs) and the tables
in [07-storage.md](07-storage.md#tables).

- **Subscribers are read from the default branch.** An `upstream_subscription` row is one
  `workflow_run` or `upstream` key of a workflow: the workflow, the revision it was read from, the
  key as the IR lowers it, the project it watches as written (none for its own), and the ref and
  commit a run of it starts at. The rows are maintained at the occasions of the schedules
  ([Schedules](#schedules)): every push to the default branch rebuilds the rows of every repository
  workflow of the project from the files the push reads, so a file that is gone, whose `on` is
  broken or that no longer has the key loses its row; creating a server-side workflow and every
  save of its source rebuild its rows in the same transaction, and unbinding or rebinding the
  project's repository forgets the rows of its repository workflows. A workflow on any other
  branch, in a pull request or in a tag subscribes to nothing, and a change to a trigger takes
  effect when it reaches the default branch. A revision that a dispatch made current on the
  default branch (`POST /workflows/{workflow}/dispatch` on a ref that is the default branch) does
  not rebuild the rows; it is picked up at the next push to the default branch, save of a
  server-side workflow or rebinding, as the schedules are. The rows take the default branch and its last commit as Bosun last
  saw it as the ref and the commit of the runs they start, as a schedule does; a server-side
  workflow's runs have neither.
- **What a run offers.** A run of an attempt offers three transitions, `requested` when the
  attempt is created, `in_progress` when the run starts, and `completed`; `workflow_run` takes
  them as its `types`, and `upstream` takes `completed` only. A transition is matched against the
  rows that watch the run's project: `workflow_run` rows of the project itself, and `upstream`
  rows that name the project's path or name none and belong to it. `workflows` lists the names of
  workflows, compared exactly and with case: a workflow's `name`, or its file when it has none, or
  its name on the server, and no pattern is read in it, so `CI*` names a workflow with a star in
  its name and the lowering refuses it. `branches` and `branches-ignore` filter the head branch of
  the run: the branch of its ref, the head branch of a pull request, the head ref of a merge
  group's queue; a run on a tag, on a schedule or on no ref has none and meets neither filter. `conclusions`
  filters the conclusion of a run that completed, `startup_failure` among them. A disabled
  workflow follows nothing. A run that awaits its admission, a pull request run from a fork that no
  maintainer has approved ([08-access.md](08-access.md)), offers nothing while it waits. Its
  `requested` is never delivered: the row records, when it is written, that the run was waiting,
  and however late a worker processes it, even after `run.admit`, it fires nothing. Its
  `in_progress` and `completed` follow the state of the run when they are processed, so they fire
  once the run is admitted. A run that was cancelled while it waited can never be admitted,
  since `run.admit` refuses a completed run, and its `completed` fires nothing.
- **The run that follows.** It is created at the revision of the row and at its ref and commit,
  with the event name `workflow_run` or `upstream`, a payload below, the actor of the run it
  follows and the trust of a trusted run, since it runs a reviewed file of the default branch as
  `pull_request_target` does. It is created once for a run, an attempt, a workflow and a
  transition, however many of the workflow's keys match, and a workflow that is disabled, gone or
  broken in its `on` is skipped without a journal event (the server logs it); a file with errors in its jobs yields a run that
  ends with `startup_failure`, as its other triggers do.
- **The `upstream` payload.** `github.event` is the resource of the upstream run, as `GET
  /runs/{run}` shows it, with two more members, `project` (`id`, `organisation_id`, `path`) and
  `workflow` (`id`, and `name`, the name `workflows` is matched against). Its `status` and
  `conclusion` are those of the transition, its `attempt` that of the attempt, and `notices` is
  always empty, since a notice may quote the upstream project's files. The case is
  `contracts/fixtures/github/upstream.json`.
- **The `workflow_run` payload.** It is what the target produces for the event, as far as the run
  resource fills it: `action`; `workflow_run` with `id` (the `github.run_id` of the run), `name`,
  `path`, `display_title`, `head_branch`, `head_sha`, `event`, `status`, `conclusion`,
  `run_number`, `run_attempt`, `html_url` (`null` when the installation has no address),
  `created_at`, `updated_at`, `run_started_at`, `head_repository`, `actor` and
  `triggering_actor`; and `workflow` with `name` and `path`. `head_repository` has `full_name` and
  `fork`: for a pull request run the repository the pull request comes from, a fork when it is not
  the repository the pull request targets or is gone, and for every other run the project's own
  repository, `null` when it has none, with `fork` false, so that a guard on
  `github.event.workflow_run.head_repository.fork` holds for a fork run as it does in the target. An actor has a `login`, and an `id` when the forge's account started the
  run. Left out are the members that name objects Bosun does not keep or addresses of the forge's
  API: `node_id`, `workflow_id`, `check_suite_id`, `check_suite_node_id`, `check_suite_url`,
  `url`, `jobs_url`, `logs_url`, `artifacts_url`, `cancel_url`, `rerun_url`,
  `previous_attempt_url`, `workflow_url`, `pull_requests`, `head_commit`, `repository` and
  `referenced_workflows`, and the top-level `repository`, `organization`,
  `sender` and `installation`. The case is `contracts/fixtures/github/workflow-run.json`. In both
  payloads the `github` context takes `ref` and `sha` from the run, so they are the default
  branch and its last commit, which is what the target documents for `workflow_run`. Bosun
  differs from the target in two things. A run on a tag has a `head_branch` of `null`, where the
  target puts the tag's name, so a `branches` filter never meets it. And `workflows` matches
  exactly and with case, with no pattern, where the target's behaviour with patterns is not
  verified; a refusal can be relaxed later and an acceptance could not be taken back.
- **A cross-project `upstream` needs the consent of the project it follows.** The project U
  that is followed lists the projects S whose workflows may subscribe to its runs, its
  followers: `PUT /projects/{U}/followers/{S}` adds one and needs `project.manage` on U
  ([11-api.md](11-api.md#projects-and-server-side-workflows)). Without it, anyone who could
  write the default branch of S could subscribe to a private project and read the branch names,
  run names, commits and actors of its runs. A subscription that names no project, or the
  workflow's own, needs no list. The server decides in this order: the subscription is the
  workflow's own project's, which fires subject to the depth of its chain; or S is not in U's list,
  and the subscription is ignored without a trace, with no run and no journal event of any kind,
  so that a project that is not a follower cannot append to U's journal and does not learn that
  U's run was the third level of a chain; or the chain would pass its third level; or the user
  holds `run.dispatch` on S, as the next paragraph says. Removing S from the list stops the
  subscription at the next transition.
- **A run without a Bosun user fires no cross-project `upstream`.** The triggering user of a run
  is the user who started it, the user of the token that did, or, for a run a forge event started,
  the Bosun user linked to the event's sender; the forge account link arrives with step 20 of M2,
  and until then a run a forge event started has no user. A schedule, an agent and the system
  have none either. A token acts with the keys it carries and within the organisations and
  projects it is restricted to, so the user must hold `run.dispatch` on the subscriber through
  the token as well, and a token that is revoked or has expired when the transition is processed
  carries none. When the upstream run has no user, or its user does not hold `run.dispatch` on
  the follower project, no run is created and the upstream run's journal gets a
  `trigger.skipped` event, once for the follower project and the attempt however many of its
  workflows match, with the follower's `project_id` and a `reason` of `no_user` or
  `no_permission` ([04-runs.md](04-runs.md#the-journal)). The event names no workflow, since a
  reader of U may not be allowed to see S. An `upstream` that names its own project crosses
  nothing and asks nothing of the user, and `workflow_run` never does.
- **Chains stop after three levels.** A run that an event, a schedule or a person started is level
  0, and a run that a trigger created is one level above the run it followed. A trigger creates a
  run only up to level 3: if A is started by a push, B follows A, C follows B and D follows C, then
  D is created and the E that would follow D is not, as in the target, where `workflow_run` chains
  stop at the same depth. The run that would have been the fourth level gets no run, and the run it
  followed gets a `trigger.skipped` event with the reason `chain_too_long`, for the project of
  the workflow that would have followed it when that project is the run's own or one of its
  followers.

## Parsing

The parser reads YAML 1.2 with the core schema, so `on`, `yes` and `no` are strings, as the target
syntax expects. Every node keeps its source span. Anchors and aliases are resolved; merge keys
(`<<`) are rejected. The parser does not use serde for workflow files, because diagnostics need
the span of the exact node at fault and because unknown keys must be reported rather than
ignored.

The YAML library is `saphyr-parser`, which reports where every node starts and ends; the tree with
spans on top of it is Bosun's own (decision 20 in [13-decisions.md](13-decisions.md)). Plain scalars
resolve by the core schema: `null`, `Null`, `NULL`, `~` and an empty value are null; `true`,
`True`, `TRUE`, `false`, `False` and `FALSE` are booleans; decimal integers, `0o` octal and `0x`
hexadecimal integers, decimals with an optional exponent, `.inf` and `.nan` are numbers; every
other plain scalar and every quoted or block scalar is a string. Where a key expects a string, a
boolean or a number is converted the way templates convert values
([03-expressions.md](03-expressions.md)), so `VERSION: 3.10` is `3.1` and `COUNT: 0x10` is `16`, as
in the target; null, a list or a mapping there is an error. A file holds exactly one document, tags
such as `!!str` are rejected, and a byte order mark at the start is skipped.

Unknown keys are errors, as they are for the target syntax. A diagnostic has this shape:

```json
{
  "severity": "error",
  "code": "unknown_key",
  "message": "unknown key `runs_on` in job `build`; did you mean `runs-on`?",
  "file": ".bosun/workflows/ci.yml",
  "span": { "line": 7, "column": 5, "end_line": 7, "end_column": 12 },
  "path": ["jobs", "build", "runs_on"]
}
```

A span counts lines and columns from 1, columns in Unicode characters, and its end is exclusive.
`path` leads from the root to the node at fault, with mapping keys as strings and list positions as
numbers from 0. The span of a key that is unknown, repeated or not supported yet covers the key; a
missing key is reported at the key whose mapping lacks it, or at the whole mapping when no key owns
it, such as a step; a fault in the YAML itself has an empty path; every other span covers the value
at fault.

Severities are `error`, which prevents a run, and `warning`, which is shown in the editor, in
`bosun workflow lint` and on the run page. A run notice carries a diagnostic too, with a third
severity, `notice`, for information that is neither ([04-runs.md](04-runs.md#run-notices)); an
error notice reports a failure while the run goes on, and the parser gives no notices. `bosun_workflow::parse` returns the IR together with the
file's warnings when the file has no error, and every diagnostic of the file, warnings included, in
the order of their position when it has one. A workflow file with errors produces a run that ends
at once with conclusion `startup_failure` when a trigger matches it, so a broken file is visible
instead of silent. Whether it matched is decided from the `on` block alone, which is parsed first;
when `on` itself cannot be parsed, the file produces such a run for every push that changes it.

`bosun_workflow::lint` takes the path of the file as its `file` and returns every diagnostic of
the file, in the same order: those of `parse` and the warnings that need the location of the file
or the whole of it, `extension` and `untrusted_checkout` ([Validation](#validation)). The editor,
`POST /api/v1/lint` and `bosun workflow lint` call it. `parse` and `triggers` do not give these
warnings, since the planner calls them for server-side workflows too, which have no path.

`bosun_workflow::inventory` serves the compatibility corpus ([13-decisions.md](13-decisions.md)) and
`bosun workflow inventory`. It reads a file as `parse` does and returns every `uses:` of a job or a
step, split into what it names and its version, and every key the lowering meets, with the milestone
of its From column ([Where each key arrives](#where-each-key-arrives)), none for a key that is
`unknown_key`, and whether it is an extension. A key is named by its place as the availability table
names keys, `jobs.<job_id>.steps.run` or `on.workflow_dispatch.inputs.<input_id>.type`, and an event
of the list form of `on` counts as the key `on.<event>`. A permission scope and the `include` and
`exclude` of a matrix arrive with the key that holds them. Names a workflow chooses, such as the
variables of `env`, the inputs of `with` and the vectors of a matrix, are no keys, and the keys under
a key of a later milestone are not listed until the parser reads them in that milestone. A file
whose YAML does not load has no inventory, only its fault.

Default limits: 1 MiB of source per file, 256 job templates per workflow, 256 jobs per matrix,
10 levels of nested reusable workflows, 50 distinct reusable workflows per run, 500 jobs that come
from reusable workflows in one attempt of a run, 10 levels of nested composite actions. They are
meant to be configurable per installation, but no setting reads them yet: every one of them, those of
M1 as well as those of M2, is a constant in the code.

Two limits protect the parser itself and are fixed: collections nest at most 64 levels deep, and
expanding aliases adds at most 100 000 nodes to a file.

## The compatibility surface

### Top-level keys

`name`, `run-name`, `on`, `permissions`, `env`, `defaults`, `concurrency` (with `group`,
`cancel-in-progress` and `queue`), `cache-mode` and `jobs` are implemented with the meaning they
have in the target syntax.

### Triggers

| Event | Where it comes from | Filters |
|-------|---------------------|---------|
| `push` | forge webhook | `branches`, `branches-ignore`, `tags`, `tags-ignore`, `paths`, `paths-ignore` |
| `pull_request`, `pull_request_target` | forge webhook | `types`, `branches`, `branches-ignore`, `paths`, `paths-ignore` |
| `schedule` | the server's scheduler | `cron`, `timezone` |
| `merge_group` | forge webhook | `types`, `branches`, `branches-ignore` |
| `workflow_dispatch` | web interface, API, CLI, MCP | `inputs` of type `string`, `boolean`, `choice`, `number`, `environment` |
| `workflow_call` | another workflow's job | `inputs`, `secrets`, `outputs` |
| `workflow_run` | a run of another workflow of the same project, when it is requested, starts or completes ([Runs that follow runs](#runs-that-follow-runs)) | `workflows`, `types`, `branches`, `branches-ignore` |
| `repository_dispatch` | forge webhook, or `POST /api/v1/projects/{id}/events` | `types` |
| any other webhook event the forge delivers (`release`, `create`, `delete`, `issue_comment`, ...) | forge webhook | `types`, matched against the payload's `action` |

As in the target, events caused by Bosun's own forge token (a push from a job, a comment it posts)
start no runs, except `workflow_dispatch` and `repository_dispatch`, so a workflow cannot trigger
itself in a loop ([06-runtime-services.md](06-runtime-services.md)).

Path filters are evaluated against the list of changed files: for a push, the comparison of
`before` and `after` through the forge API, since the webhook payload truncates its file lists, as
[Repository workflows](#repository-workflows) details; for a pull request, the files of the pull
request. Glob patterns follow the target syntax,
including negation with `!` and the order-sensitive evaluation of mixed lists.

Pull requests from forks are subject to the project's fork policy before any run is created
([08-access.md](08-access.md)).

#### Lowering `on`

`on` is an event name, a list of event names, or a mapping from event names to their filters, where
an empty value or `{}` means no filters. `schedule` takes a list of entries and so exists only in
the mapping form. Filters are literal text; no key under `on` takes an expression.

- `branches`, `tags` and `paths` take a pattern or a list of patterns, and each has an `-ignore`
  form. A key together with its `-ignore` form is `invalid_value` at the `-ignore` value, as are an
  empty list, an empty pattern and a pattern the syntax below refuses.
- `types` takes an activity type or a list of them. Except for those of `workflow_run`, the names
  are not checked against the types the forge defines: a name the forge never sends never matches. Every event without a row of its own
  in the table accepts `types`, matched against the payload's `action`.
- A `schedule` entry has `cron`, which is required, and `timezone`. A cron has the five fields of
  the target: minute (0-59), hour (0-23), day of the month (1-31), month (1-12 or `JAN` to `DEC`)
  and day of the week (0-6 or `SUN` to `SAT`, with 7 for Sunday as well); names ignore case. A
  field is a list of items separated by `,`, each `*`, a value or a range `a-b`, with an optional
  step `/n`, where `a/n` runs from `a` to the end of the field. Shortcuts such as `@daily` and the
  extensions `?`, `L`, `W` and `#` are `invalid_value`. The IR keeps the expression as written,
  which `github.event.schedule` reports, the sorted values each field selects, and `days`: when
  either day field starts with `*`, a day must match both fields (`both`); otherwise it must match
  either (`either`). This is the rule of cron on Linux, and the scheduler evaluates the form without
  looking at the expression again.
- `timezone` is an exact name of the IANA time zone database that `bosun-workflow` compiles in
  through `chrono-tz` (database 2025b), so the browser's parser knows the same names as the
  server. A name in the wrong case is refused with a suggestion. Without `timezone` a schedule runs
  in UTC.
- `workflow_run` takes `workflows`, which is required and lists names, `types`, and `branches` or
  `branches-ignore`; `upstream` takes `project`, `workflows`, which is required, `branches` and
  `conclusions`. A name in `workflows` is not empty and holds none of `*`, `?` and `[`, since
  `workflows` takes exact names; a `types` entry of `workflow_run` is `completed`, `requested` or
  `in_progress`; `project` is two slugs joined by `/`, and a `conclusions` entry is one of the
  conclusions of a run (`success`, `failure`, `cancelled`, `skipped`, `timed_out`,
  `startup_failure`); each is `invalid_value` otherwise. An absent
  `conclusions` lowers to `[success]`, an absent `types` to none, which every type meets. Neither
  key has a form without a mapping: `on: workflow_run` is a `missing_key` for `workflows`.
- `workflow_dispatch` `inputs` keep the order of the source. An input has `description`,
  `required` (a boolean, `false` when absent), `type` (`string` when absent), `default`, and
  `options`, which `choice` requires, non-empty, and no other type accepts. An input's name follows
  the rule of job ids. A `boolean` default is a YAML boolean and a `number` default a finite YAML
  number, so a quoted `'true'` or `'5'` is `invalid_type`; a `string` or `environment` default is
  converted like every string value, and a `choice` default must be one of the options.

`bosun_workflow::triggers` lowers a file as `parse` does and returns its triggers together with
every diagnostic of the file. The triggers are present only when `on` itself has no diagnostic:
a file whose jobs are broken still has triggers to match for its `startup_failure` run, and a file
whose `on` is broken has none.

#### Matching events

`bosun_workflow::matches` decides whether an event matches a workflow's triggers. The event is its
name, its action, its ref and its base ref as full refs (`refs/heads/main`), and its changed files
or their absence. The cases are in `contracts/fixtures/triggers/`.

- **Patterns.** `*` matches any characters but `/`, `**` any characters, and `**/` any number of
  leading directories, none included, so `docs/**/*.md` matches `docs/README.md`. `?` makes the
  character or bracket expression before it optional, and `+` repeats it once or more; at the start
  of a pattern both are literal. `[...]` lists letters, digits and ranges within `a-z`, `A-Z` or
  `0-9`. `\` makes the next character literal. Every other character, `.` included, is literal, and
  matching is case-sensitive.
- **Lists.** A leading `!` negates a pattern. A name is selected when the last pattern of the list
  that matches it is not negated, so `['**', '!docs/**', 'docs/keep.md']` selects `docs/keep.md` and
  `src/a.rs`, and not `docs/guide.md`. An `-ignore` list is read the same way and ignores what it
  selects, so a negation there takes a name back out of the ignored ones.
- **Refs.** `branches` match the branch of a push's ref, and the base ref of a pull request or of a
  merge group's queue. `tags` match the tag of a push. A push with only branch filters does not run
  for tags, one with only tag filters does not run for branches, and one with neither runs for both.
- **Paths.** A path filter is met when at least one changed file is selected, or for `paths-ignore`
  not ignored; a change of no files meets neither. Path filters are not evaluated for a tag push,
  as in the target, nor for an event that carries no list of changed files, which the server passes
  when it has none to give.
- **Types.** `pull_request` and `pull_request_target` default to `opened`, `synchronize` and
  `reopened`, `merge_group` to `checks_requested`, and every other event to every action. An event
  without an action meets no `types`, written or default.

Every filter of a trigger must be met, and a workflow matches when one of its triggers does.

#### Dispatch inputs

Step 30 of M1 ([../plan.md](../plan.md)) settled how a dispatch's inputs are read; the request is
in [11-api.md](11-api.md#projects-and-server-side-workflows) and the cases are in
`contracts/fixtures/inputs/`. The declared inputs are those of the `workflow_dispatch` trigger of
the source the run takes, the repository file at the dispatched commit or the current revision of a
server-side workflow; a workflow without that trigger or without `inputs` declares none. A given
input is matched by its exact name.

- A `boolean` takes the JSON booleans and the texts `true` and `false`, in lower case.
- A `number` takes a JSON number, or a text that reads as one by the JSON grammar, so ` 3`, `+3`,
  `.5` and the empty text do not.
- A `string` takes any text, the empty text included, and a `choice` one of its options, compared
  exactly. An `environment` takes any text, as a `string` does, since environments arrive in M3,
  which checks it against the environments of the project; the target offers the repository's
  environments to choose from.
- None of these takes a JSON value of another kind, `null` included, where it takes text.
- An input that the dispatch does not give takes its default. A required input without a default
  that the dispatch does not give is refused; `required` decides nothing else, as in the target.
  Any other input that is not given is left out of both contexts below, so it reads as `null`: an
  empty text in a template and false in a condition.
- An input that the workflow does not declare is refused.

Each refused input is an entry of `fields` named `inputs.<name>`, in the order of the names, and
nothing is stored. Otherwise the run keeps the inputs with their types as the `inputs` context:
booleans as booleans, numbers as numbers, with a whole number stored as an integer, and the others
as text. `github.event.inputs`, in the payload of
[06-runtime-services.md](06-runtime-services.md#the-github-context-and-the-environment), holds each
of them as text: a value given as text as it was written, so `2.50` stays `2.50` there while
`inputs` holds `2.5`; a JSON number and a default as the shortest decimal of their value; a boolean
as `true` or `false`. It is `null` when no input has a value. Neither changes after the run is
created ([04-runs.md](04-runs.md)).

### Jobs

`name`, `needs`, `if`, `runs-on`, `environment`, `concurrency`, `outputs`, `env`, `defaults`,
`timeout-minutes`, `continue-on-error`, `strategy` (`matrix` with `include` and `exclude`,
`fail-fast`, `max-parallel`), `container`, `services`, `permissions`, `cache-mode`, `steps`, and calls to
reusable workflows through `uses` with `with` and `secrets` (including `secrets: inherit`).

`runs-on` accepts a string, a list, or an object with `group` and `labels`. `group` names a Bosun
pool ([05-execution.md](05-execution.md)). Labels match case-insensitively, as in the target
syntax. Every agent carries `self-hosted`, its operating system (`Linux`) and its architecture
(`X64`, `ARM64`) among its labels, so the common `runs-on: [self-hosted, Linux, X64]` selects a
Bosun agent without a change. Labels of hosted images such as `ubuntu-latest` resolve through the
installation's label aliases, which map a label to a pool, an executor and, for `podman`, a
default image.

### Reusable workflows

A job with `uses` calls a workflow whose `on` holds `workflow_call`, and takes the `with` and
`secrets` of the call. The server reads the callee when the run is planned, and a reusable workflow
is read, checked and run as follows. Three of these rules are Bosun's own and are decided here; the
rest follows the target.

- **Where a callee lives.** A local callee, `uses: ./.github/workflows/build.yml`, is a file
  directly in `.github/workflows/` or in `.bosun/workflows/` of the repository that holds the
  workflow with the `uses`, at that workflow's commit: the run's commit for the workflow the run
  starts from, and the callee's own commit for a callee that calls another local one. Either
  directory serves whatever directory the calling file is in and whatever the project's setting
  `github_workflows` says, since that setting decides which files start runs and a callee starts
  none. A file elsewhere, or without the extension `.yml` or `.yaml`, is refused. A workflow of a
  server-side project has no repository, so a local callee is refused there.
- **A callee of another repository.** `uses: owner/repo/.github/workflows/release.yml@v1` must
  belong to the installation of the GitHub App that serves the project's organisation, or be
  public: the server asks the forge for it with the installation's credentials, or without any when
  the organisation has no installation, so a private repository of another installation answers
  as a missing one. The path follows the rule of a local callee. The action policy of the
  organisation ([05-execution.md](05-execution.md#actions-and-tools)) applies to the reference as
  it is written, `owner/repo/path@ref`, with the same pure check that steps use.
- **A ref is read once.** A ref of a callee resolves to a commit through the forge when the run is
  planned, and the run keeps the commit with the callee in its IR snapshot. A tag moved later
  changes nothing in a run that was planned before; a callee that a job reaches only after its
  `needs` still comes from the snapshot, never from the forge.
- **Check runs.** The jobs of a callee are reported to the forge as check runs named
  `<caller name> / <callee job name>`: the display name of the calling job, a slash with a space
  on each side and the display name of the callee's job. A callee that calls another gives
  `<outer> / <inner> / <job>`, one more name for each level of nesting. A call is a group at every
  depth and is not reported.
- **What is refused.** A callee that is missing, that does not lower, that has no `workflow_call`,
  that the caller passes an undeclared input or secret to, that leaves out a required one
  ([Validation](#validation)), that the policy forbids, or to which the call passes `secrets: inherit`
  across accounts (see Secrets), ends the run with `startup_failure`
  before any job exists. Each diagnostic points at the `uses` of the call, or at the entry of
  `with` or `secrets` at fault, in the file that holds the call; a callee that does not lower adds
  its own diagnostics in its own file. The codes are those of `check_call`, `limit_exceeded` and the three
  that [04-runs.md](04-runs.md#calls-of-reusable-workflows) lists: `callee_missing`, `callee_invalid`
  and `callee_forbidden`.
- **Limits.** A run nests reusable workflows at most 10 levels deep and calls at most 50 distinct
  ones, and one attempt of a run holds at most 500 jobs that come from calls: the members of every
  call at every depth, the nested calls among them, and the members of a matrix on a call or of a
  matrix inside a callee. A callee is one file at one commit: the same file called twice counts once
  as a callee and twice in the 500. A call at the 11th level, which a recursion reaches, and the call
  that would add a 51st callee are refused with `limit_exceeded`, whose message names the limit, and
  so is the call of the run's own workflow after which the jobs that calls bring, counted without any
  matrix, pass 500. A matrix makes the count known only at run time, and then the call or the matrix
  that would pass 500 fails with `limit_exceeded` as a job fails, and the run ends `failure`. A job
  of the run's own workflow, with or without a matrix, is not counted, so a matrix of 256 jobs
  keeps working. The limits are constants in the code.
- **Inputs.** `with` is evaluated when the calling job is ready, with the contexts of its other
  keys (`github`, `needs`, `strategy`, `matrix`, `inputs`, `vars` and `bosun`). Each input is
  typed by its declaration: a string input takes a string, a number or a boolean as text, a
  boolean input a boolean or the text `true` or `false`, a number input a finite number or text
  that reads as one. An input that `with` leaves out takes its `default`, and without one an empty
  string, `false` or `0`. The callee sees the result as `inputs`, and its jobs see no other
  `inputs` than that.
- **Secrets.** Secrets cross to a callee only as the caller passes them. A named secret takes the
  value of the caller's `${{ secrets.NAME }}` or a text; passing a secret inside other text or an
  expression is `invalid_value`, since a secret is decrypted when a job is assigned and never held
  by the planner. `secrets: inherit` passes every secret the caller holds, and no more: a middle
  workflow that received `a` only hands `a` on. It is allowed only for a callee whose repository
  owner is the owner of the repository of the file that holds the call, compared without regard to
  case, and a call of a workflow of another account with `secrets: inherit` is refused with
  `callee_forbidden`, whatever the action policy says and without a request to the forge; the
  secrets are passed by name to such a callee. For a call in a nested callee the file is that
  callee, and a workflow without a repository inherits to no callee of another repository. A local
  callee is of the same repository, so it needs no check. A secret the callee declares and the
  caller leaves out is empty, and `GITHUB_TOKEN` is the token of the job that reads it in every
  case. The values of secrets are never written to a diagnostic, a log or the run's rows: the call
  keeps which name takes which source, and for a text the evaluated text, as the outputs of a job
  are kept; a text that comes from `needs.*.outputs` or `inputs` is therefore in the row in clear,
  and a secret is not.
- **Concurrency.** The `concurrency` of a call and the `concurrency` of the workflow of a callee are
  not applied yet: the jobs of the callee run without them. Each is a `not_implemented` warning,
  which the run's notices hold from the moment it is planned: one for each call that has the key, at
  the call, whether the call is skipped or has a matrix or not, and one for each distinct callee that
  has it, at the key in the callee's file. The `concurrency` of a job inside a callee applies as for
  any job.
- **Context and permissions.** A callee sees the `github` context of the run, the caller's
  `workflow`, and `github.job` is the callee's own id for the job. The `permissions` of a callee's
  workflow or job can only narrow what its caller's grant: the token of a callee job holds, per
  permission, the lowest access that the callee's job, the call, the workflow that holds the call
  and every call above it allow, and none of a permission one of them leaves out. The trust of the
  run, which a pull request from a fork lowers, applies to the jobs of a callee unchanged.
- **Outputs.** The `outputs` of `on.workflow_call` are evaluated when every job of the call has
  completed, with `github`, `inputs`, `vars`, `bosun` and `jobs`, where `jobs.<id>.result` and
  `jobs.<id>.outputs.<name>` describe the callee's jobs. They become the outputs of the calling
  job, so `needs.<caller>.outputs.<name>` reads them. An output that fails to evaluate fails the
  call, with an error notice at the output.

### Steps

`id`, `name`, `if`, `uses`, `run`, `shell`, `working-directory`, `with` (including `args` and
`entrypoint` for container actions), `env`, `continue-on-error`, `timeout-minutes`.

`uses` accepts `owner/repo@ref`, `owner/repo/path@ref`, `./path` relative to the workspace, and
`docker://image`. The `run`, `shell` and `working-directory` semantics, the default shell and the
shells `bash`, `sh`, `pwsh`, `python` and custom templates with `{0}` are specified in
[05-execution.md](05-execution.md).

### Lowering jobs and steps

Every value a key reads as text becomes a `Template`, and every `if` a `Condition`. Each expression
in them is checked against the availability of its key ([03-expressions.md](03-expressions.md)).
The keys that table does not list take no expressions: a `${{` in a step's `shell` or `uses`, or in
the workflow's `defaults.run`, is `invalid_value`. The workflow's `name` is never evaluated, as in
the target, and keeps a `${{` as text. `id`, `needs`, `permissions` and the keys under `on` hold
names and patterns, in which a `${{` is not valid anyway.

Expressions in the IR carry their place in the file. `bosun-workflow` maps every character of a
scalar's value back to the source, through quotes, escape sequences, indentation and folded line
breaks, so a diagnostic or a run-time error points at the context or the call at fault.

- **`if`**, of a job or a step: the three forms of [03-expressions.md](03-expressions.md). An empty
  value or null sets no condition. A template, an `if` with text around its `${{ }}` or with several
  of them, is lowered as written and produces the warning `template_condition`.
- **`runs-on`**: a string with an expression is one template, evaluated as a whole to a label, a
  list of labels or a mapping with `group` and `labels`. A list or a mapping keeps its structure,
  and each label and the group is a template of its own.
- **`permissions`**, of the workflow and of a job: `read-all`, `write-all`, or a mapping of scopes
  to `read`, `write` or `none`, where `{}` grants nothing. The scopes are the target's: `actions`,
  `attestations`, `checks`, `contents`, `deployments`, `discussions`, `id-token`, `issues`,
  `models`, `packages`, `pages`, `pull-requests`, `repository-projects`, `security-events` and
  `statuses`. `id-token` takes only `write` or `none`, and `models` only `read` or `none`, as in the
  target; another scope is `unknown_key` with a suggestion. Without `permissions` the IR holds
  none, and the project's default set applies ([06-runtime-services.md](06-runtime-services.md)).
- **`defaults.run`** has `shell` and `working-directory`. At the workflow they are literal, as in
  the target; at a job they take expressions (`jobs.<job_id>.defaults.run`).
- **`concurrency`**, of the workflow and of a job: a string is the group; a mapping has `group`,
  which is required, `cancel-in-progress`, a boolean or an expression that defaults to `false`, and
  `queue`, `single` (the default) or `max`. A literal `cancel-in-progress: true` together with
  `queue: max` is `invalid_value` at `cancel-in-progress`; an expression there is checked when the
  group is evaluated ([04-runs.md](04-runs.md)). An empty group is `invalid_value`.
- **`timeout-minutes`**, of a job and of a step, is a whole number of minutes or an expression; a
  job without it has 360, a step without it none. **`continue-on-error`**, of both, is a boolean or
  an expression and defaults to `false`. A string that holds no expression, such as `'true'` or
  `'10'`, is `invalid_type`, as in the target.
- **`strategy`** is a mapping or one expression. Its `matrix` is a mapping or one expression; in the
  mapping, every key but `include` and `exclude` is a vector, a list or one expression, and
  `include` and `exclude` are each a list of mappings or one expression. Values are any YAML value,
  with strings as templates and the order of mappings kept, since the order of vectors names the
  jobs a matrix expands to. `fail-fast` is a boolean or an expression and defaults to `true`;
  `max-parallel` is a whole number from 1 or an expression. An empty vector or matrix is refused when
  it is expanded ([04-runs.md](04-runs.md)), since a computed one can be empty too.
- **`outputs`** of a job and **`with`** of a step map names to templates; an empty name is
  `invalid_value`.
- **A step** has `run` or `uses`. Both together are `invalid_value` at `uses`, neither is
  `missing_key` at the step. `with` applies only to a step that uses an action, `shell` and
  `working-directory` only to one that runs a script; each is `invalid_value` at its value on the
  other kind of step, as in the target.
- **`uses`** is `owner/repo@ref` or `owner/repo/path@ref`: the owner holds letters, digits and `-`,
  the repository letters, digits, `-`, `_` and `.`, the path non-empty parts that are not `.` or
  `..`, and the ref is non-empty and holds no whitespace. Anything else is `invalid_value`, so
  `actions/checkout` without a ref is refused with a hint. `./path` and `docker://image` lower to
  the actions `Local` and `Docker`, and are `not_implemented` naming M2 while their forms are on
  the pending list.

Each job template also records the secrets its expressions read, together with those of the
workflow's `env`, which every job receives under its own. `secrets.NAME` and `secrets['NAME']` name
a secret; names compare ignoring case, as the target's secret names do, so the IR keeps them in
upper case. An index by another literal is converted to a string first, as the target's index is:
`secrets[true]` names `TRUE`, and `secrets[null]`, whose string is empty, names none. An index
computed by an expression, such as `secrets[format('TOKEN_{0}', matrix.env)]`, or the whole
context, such as `toJSON(secrets)` or `secrets.*`, makes the job read every secret in scope, and
produces the warning `all_secrets` at that expression. The assignment carries only the secrets the
job reads ([05-execution.md](05-execution.md)).

### Actions

`action.yml` and `action.yaml` with `name`, `description`, `author`, `branding`, `inputs`
(`description`, `required`, `default`, `deprecationMessage`), `outputs` and `runs`:

- `using: node12`, `node16`, `node20` and `node24` with `main`, `pre`, `post`, `pre-if` and
  `post-if`. The installation's runtime map decides which Node runs which `using`; by default, as
  on current hosted runners, `node12`, `node16` and `node20` all run on Node 24, and the run page
  notes the substitution ([05-execution.md](05-execution.md#actions-and-tools)).
- `using: composite` with `steps`, where `run` steps require `shell`, and with `outputs` mapped
  from step outputs.
- `using: docker` with `image` as a Dockerfile path or `docker://`, `args`, `env`, `entrypoint`,
  `pre-entrypoint`, `post-entrypoint`.

#### Lowering action metadata

`bosun_workflow::action` lowers the metadata of an action: the file `action.yml` of the action's
directory or, when there is none, `action.yaml`, as the runner of the target looks for them. It
reads YAML as workflows are read, keeps spans the same way and reports diagnostics of the same
shape, and returns the metadata with its warnings, or every diagnostic when one is an error. The
cases are in `contracts/fixtures/actions/`: the metadata of public actions, and cases that pin one
rule each.

- The root has `name`, `description` and `author`, which are text and never evaluated, so a `${{`
  in them stays as written; `branding`, with `icon` and `color`; `inputs`; `outputs`; and `runs`,
  which is required. Inputs and outputs keep the order of the source, and a name is any non-empty
  text.
- An input has `description` and `deprecationMessage`, text as well; `required`, a boolean that is
  `false` when absent; and `default`, a template. A boolean or a number there is converted the way
  templates convert values, so the `fetch-depth: 1` of `actions/checkout` is `1`, and a null
  default is none: as in the runner, an input without a value or a default is the empty string.
- An output has `description` and `value`. A composite action's outputs require `value`, a template
  over the steps of the action; in an action of another kind `value` is read and kept.
- `runs` requires `using`. `node12`, `node16`, `node20` and `node24` are compared ignoring case, as
  the runner compares them. `composite` and `docker` are lowered with the keys below, and while
  they are on the pending list ([Where each key arrives](#where-each-key-arrives)) `using` is
  `not_implemented` and names M2 beside the diagnostics of the other keys of `runs`; any other
  value is `invalid_value`. A Node action requires `main` and has `pre` and `post`, which are paths
  in the action's directory, non-empty and without expressions. `pre-if` and `post-if` are
  non-empty conditions in the forms of an `if`; without one, the condition is `always()`. As in a
  step's `if`, a condition that calls no status function behaves as `success() && (...)`, and the
  status functions look at the job, not at the action.
- A composite action requires `steps`, lowered as the steps of a job with the same rules, and
  every step that runs a script requires `shell`, which is `missing_key` at the step. Its
  expressions are checked against `runs.steps` and `runs.steps.if`, which see neither `secrets`
  nor `needs`, `matrix` and `strategy` ([03-expressions.md](03-expressions.md)).
- A docker action requires `image`, which is text without expressions, and has `args`, a list of
  templates, `env`, a mapping of templates, `entrypoint`, `pre-entrypoint` and `post-entrypoint`,
  non-empty text without expressions, and `pre-if` and `post-if` as a Node action does. The
  expressions of `args` and `env` are checked against `runs.args` and `runs.env`, which see
  `github`, `inputs`, `vars` and `bosun`.
- Expressions are checked against the keys `inputs.<input_id>.default`, `outputs.<output_id>.value`,
  `runs.pre-if`, `runs.post-if`, `runs.steps`, `runs.steps.if`, `runs.args` and `runs.env` of the
  availability table ([03-expressions.md](03-expressions.md)). The agent evaluates them: the
  defaults when the step that uses the action is reached, and each condition before its pre or
  post step.

**Unknown keys.** In a workflow an unknown key is an error. The runner of the target reads the
root of an action's metadata and its inputs loosely and ignores keys it does not know, and public
actions rely on it: `marocchino/sticky-pull-request-comment` declares a `github-token` at its root,
`softprops/action-gh-release` an `env`, and `oven-sh/setup-bun` gives an input a `type`. Refusing
them would refuse actions that run on the target, so at the root, in an input and in `branding`,
which the runner does not read either, an unknown key is the warning `ignored_key`, with a
suggestion when a known key is close. In an output and in `runs`, which the runner reads strictly,
an unknown key is `unknown_key`, as in a workflow. The three actions are cases in
`contracts/fixtures/actions/`.

The keys of action metadata arrive by the rule of [Where each key arrives](#where-each-key-arrives).
The keys of `runs` that only a composite or a docker action has are listed under `runs.composite`
and `runs.docker`, with `using` in `runs`:

| In the metadata | Keys | From |
|-----------------|------|------|
| root | `name`, `description`, `author`, `branding`, `inputs`, `outputs`, `runs` | M1 |
| `branding` | `icon`, `color` | M1 |
| `inputs.<input_id>` | `description`, `required`, `default`, `deprecationMessage` | M1 |
| `outputs.<output_id>` | `description` | M1 |
| `outputs.<output_id>` | `value` | M2 |
| `runs` | `using`, `main`, `pre`, `pre-if`, `post`, `post-if` | M1 |
| `runs.composite` | `steps` | M2 |
| `runs.docker` | `pre-if`, `post-if` | M1 |
| `runs.docker` | `image`, `args`, `env`, `entrypoint`, `pre-entrypoint`, `post-entrypoint` | M2 |

### Where each key arrives

The surface is implemented milestone by milestone ([../plan.md](../plan.md)). Until its milestone,
a key of the surface is known to the parser and refused with `not_implemented`, whose message names
the milestone, rather than reported as unknown. Values are literals until M1, which brings
expressions. A step's `uses` arrives in M1 for `owner/repo@ref` and `owner/repo/path@ref`, and in M2
for `./path` and `docker://`. The extension keys arrive in the milestone of the From column of
their table below.

`bosun-workflow` names the milestone it has reached, `MILESTONE`, and keeps in `PENDING`, in
`crates/workflow/src/surface.rs`, the keys whose milestone has been reached but which are not
implemented yet, each by its place, as the column Where names it but with `on` for a trigger, and
its name. A key is implemented exactly when its milestone is reached and it is not pending; every
other key is `not_implemented` and names its From milestone. M1 leaves the list empty. The first
step of a later milestone moves `MILESTONE` on and puts the keys of that milestone on the list until
the steps that implement them.

At M2 the list holds every key the milestone brings except the two triggers of step 16 and the keys
of reusable workflows, which step 15 implements (`on.workflow_call` and the `uses`, `with` and
`secrets` of a job), and the forms that sit inside a key that exists already: `uses: ./path` and
`uses: docker://` of a step, and `using: composite` and `using: docker` of `runs`. The parser lowers all of them, and the planner and the agent refuse what
they cannot run, but a file that uses one is `not_implemented` until its step removes the entry.
`on.workflow_run` and `on.upstream` are not on the list: step 16 lowers them and the server executes
them, so a file that uses them is accepted whatever else it holds. The
tests lower pending keys through `parse_with`, `lint_with` and `action_with`, which exist only
under the feature `admit-pending` of `bosun-workflow`; no build of the server, the agent, the CLI or
the WebAssembly packages enables it. A test reads this table, the extensions table and the table of
action keys, checks the rule against them, and checks that the JSON Schema lists exactly the keys of
the first two.

| Where | Keys | From |
|-------|------|------|
| workflow | `name`, `on`, `env`, `jobs` | M0 |
| workflow | `run-name`, `permissions`, `defaults`, `concurrency` | M1 |
| workflow | `cache-mode` | M2 |
| `on` | `workflow_dispatch` | M0 |
| `on` | `push`, `pull_request`, `pull_request_target`, `schedule`, `merge_group`, `repository_dispatch` | M1 |
| `on` | `branch_protection_rule`, `check_run`, `check_suite`, `create`, `delete`, `deployment`, `deployment_status`, `discussion`, `discussion_comment`, `fork`, `gollum`, `issue_comment`, `issues`, `label`, `milestone`, `page_build`, `project`, `project_card`, `project_column`, `public`, `pull_request_review`, `pull_request_review_comment`, `registry_package`, `release`, `status`, `watch` | M1 |
| `on` | `workflow_call`, `workflow_run` | M2 |
| `workflow_dispatch` | `inputs` | M1 |
| job | `name`, `needs`, `runs-on`, `env`, `steps` | M0 |
| job | `if`, `permissions`, `concurrency`, `outputs`, `defaults`, `timeout-minutes`, `continue-on-error`, `strategy` | M1 |
| job | `container`, `services`, `cache-mode`, `uses`, `with`, `secrets` | M2 |
| job | `environment` | M3 |
| step | `id`, `name`, `run`, `shell`, `working-directory`, `env` | M0 |
| step | `if`, `uses`, `with`, `continue-on-error`, `timeout-minutes` | M1 |

### Where Bosun differs

- `permissions` is turned into the permission set of the forge token minted for the job
  ([06-runtime-services.md](06-runtime-services.md)). `id-token: write` is accepted and has no
  effect until the identity token service exists ([13-decisions.md](13-decisions.md)).
- `github.run_id` is a number from a global sequence of the installation, because actions parse it
  as a number. `github.server_url` is the forge's address, never Bosun's
  ([06-runtime-services.md](06-runtime-services.md) explains why).
- Actions that ask the forge's API about the run itself (list runs, fetch artifacts of another run
  by id, read job logs) talk to the forge, which knows nothing about Bosun runs. This is
  documented per known action in the compatibility corpus rather than emulated.
- `jobs.<id>.environment` does not create a deployment object on the forge unless the project
  setting `forge_deployments` is on.
- `timeout-minutes` and `max-parallel` take whole numbers. The target truncates a fraction such as
  `0.5` to `0`, which for a step means no timeout at all; Bosun refuses it with `invalid_value`
  instead.
- A push whose comparison the forge truncated at 300 files meets every path filter. The target
  judges such a push by the 300 files it lists, and so skips a workflow whose files come later;
  Bosun would rather run a workflow too many than skip one silently. For the same reason the path
  filters of a pull request see every file the forge lists of it, up to 3000, and a pull request of
  3000 files or more meets them all, where the target sees the first 300 files of its diff.
- The `matrix` context lists the properties of a member in the order of their names, since the
  member's combination is stored as a JSON object ([07-storage.md](07-storage.md)), rather than in
  the order of the workflow; it shows only in `toJSON(matrix)` and `matrix.*`, and the member's
  name keeps the order of the workflow.
- Schedules follow the default branch through its pushes ([Schedules](#schedules)): a change of
  the repository's default branch moves them to the new branch with its first push, where the
  target moves them at once and makes whoever changed the branch their actor. A scheduled run
  starts at the commit of the last push to the default branch that Bosun processed.

## Extensions

Bosun adds a closed list of keys. They are part of the published JSON Schema in
`contracts/schema/workflow.json`; anything else unknown is still an error. Extension keys and the
`bosun` context in a file directly under a `.github/workflows/` directory, whose path may be
relative or absolute, produce the warning `extension` from `bosun_workflow::lint`, because the file
becomes invalid for GitHub Actions; under `.bosun/workflows/` they produce none. For an extension
whose milestone is not reached, the warning comes beside its `not_implemented`.

The schema follows JSON Schema 2020-12 and describes every key of the surface and every
extension, whatever its milestone, so it stays the same from milestone to milestone. Each key of the
table of arrivals and of the extensions table carries the milestone of its From column as
`x-bosun-from`, and each extension also `x-bosun-extension: true`, so an editor can say when a key
arrives and which keys GitHub Actions does not know. It describes the shapes of values and refuses
unknown keys, but it checks no expression, pattern, cron or job graph, which the parser checks. A
test validates every workflow fixture the parser accepts against it, with the `jsonschema` crate as
a dev-dependency of `bosun-workflow`, and refuses those with an unknown key. The forms of the keys
of M3 are left open, and the steps that implement them describe them.

| Key | Where | Meaning | From |
|-----|-------|---------|------|
| `retry` | step, job | repeat a failed step in place, or re-queue a failed job | M3 |
| `pause` | step | stop before or after the step until an operator resumes | M3 |
| `debug.hold-on-failure` | job | keep a failed job alive for inspection | M3 |
| `cache-volumes` | job | directories that persist between runs on an agent host | M2 |
| `gate` | job | a job without steps that waits for approval and yields outputs | M3 |
| `priority` | workflow, job | `low`, `normal`, `high` for dispatch order | M2 |
| `on.upstream` | trigger | run after a workflow of another project completes | M2 |
| `on.plugin` | trigger | an event from a trigger plugin instance | M5 |
| `notify` | workflow | route run and job events to notifier plugin instances | M5 |

```yaml
on:
  push:
    branches: [main]
  upstream:
    project: globalart/keel
    workflows: [release]
    conclusions: [success]
  schedule:
    - cron: "0 3 * * *"
      timezone: Europe/Belgrade

priority: high

notify:
  - when: [failure, fixed]
    to: telegram-ops

jobs:
  build:
    runs-on: [self-hosted, Linux, X64]
    cache-volumes:
      - name: cargo-target
        path: target
    retry:
      attempts: 2
      when: [agent-lost]
    debug:
      hold-on-failure: 30m
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --workspace
        retry:
          attempts: 3
          backoff: 10s

  approve:
    needs: build
    gate:
      approvers: [team:ops]
      min-approvals: 1
      timeout: 24h
      message: "Deploy ${{ github.sha }} to production?"
      inputs:
        canary:
          type: number
          default: 10

  deploy:
    needs: approve
    runs-on: [self-hosted, deploy]
    steps:
      - run: ./deploy.sh --canary "${{ needs.approve.outputs.canary }}"
        pause: before
```

### `retry`

On a step: `attempts` (2 to 10, counting the first), `backoff` (a duration, default `0s`),
`factor` (default `1`, multiplies the backoff after each attempt), `when` (default `[failure]`;
also `timeout`). A retry re-executes the step on the same agent in the same environment. Writes a
failed attempt made to `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH` and `GITHUB_STATE` are
discarded; only the attempt that ends the sequence counts. The log of each attempt is a separate
section of the step's log. A cancelled step is never retried.

On a job: `attempts` and `when`, where `when` is a subset of `failure`, `timeout` and
`agent-lost`, default `[agent-lost]`. A retried job goes back to the queue as a new execution of
the same job within the same attempt of the run, possibly to another agent; the earlier execution
stays visible on the run page.

### `pause`

`pause: before` stops the job before the step starts; `pause: after` stops it after the step ends
and before the next one begins. The job waits in status `paused` until an operator resumes it
([04-runs.md](04-runs.md)). Operators can also set the same breakpoints at run time without
editing the workflow.

### `debug.hold-on-failure`

A duration. When a step fails and the failure would end the job, the job enters status `held`
instead of tearing down: the container or the workspace stays as it is, an operator can open a
terminal inside it, retry the failed step, continue past it or release the job. When the duration
runs out, the job is released as failed. The installation caps the duration (default `2h`).
Re-running a job with debugging switched on sets this key for every job of the attempt.

### `cache-volumes`

A list of directories that survive between jobs on the same agent host. Each entry has `name`,
`path` (where the directory appears; relative paths are relative to the workspace), `scope`
(`project`, the default, or `organisation`), `key` (an optional template that partitions the
volume, for example by toolchain), and `mode` (`exclusive`, the default, takes a lock for the
duration of the job; `shared` does not). A string entry is a name with `path` equal to the name.

Lowering holds an entry to these rules, each `invalid_value` at the value at fault:

- `name` is required, literal text of letters, digits, `-`, `_` and `.` that does not start with a
  `.`, so it is one path component and never `..`; it is a directory name on the host.
- `path`, literal, defaults to the name; it may be absolute or relative and must be neither blank
  nor hold a NUL character or a `..` part.
- `scope` is `project` or `organisation`, and `mode` is `exclusive` or `shared`; a text that is
  not one of them is `invalid_value`, with a suggestion when one is close.
- Two entries of one job may not share a `name`, nor a `path`; the second is the diagnostic, and
  one that repeats both reports the name.
- `key` is a template over `github`, `vars`, `strategy`, `matrix`, `needs`, `inputs` and `bosun`.
  It has no `secrets`, since the key becomes a directory name that persists on the host, outside
  the masking of logs ([03-expressions.md](03-expressions.md#where-each-expression-is-evaluated)).
- A job that calls a workflow takes no `cache-volumes`.
On the `podman` executor the directory is bind-mounted; on `shell` it is a symbolic link created
before the first step and removed after the last one. These volumes are host-local and are not a
replacement for the `actions/cache` service, which is portable between agents.

### `gate`

A job with `gate` has neither `runs-on` nor `steps` nor `uses`. It never reaches an agent. It
waits in status `waiting` until it has `min-approvals` approvals (default 1) from the listed
`approvers` (users as `user:<login>`, teams as `team:<slug>`), or a rejection, or the end of
`timeout` (default `24h`). `message` is a template rendered for the approver. `inputs` uses the
schema of `workflow_dispatch` inputs; the approver fills them, and they become the gate's outputs,
together with `approved-by` (a JSON array of logins). A rejection or a timeout ends the gate with
conclusion `failure`. The actor of the run cannot approve its gates unless the gate sets
`allow-self-approval: true`.

A gate is a checkpoint, not a protection: whoever can change the workflow can change the gate's
approvers. Protecting a target is the job of an environment, whose required reviewers are
configured in Bosun and not in the workflow ([04-runs.md](04-runs.md)). A gate is for an approval
that is not tied to an environment, or that needs input.

### `priority`

The dispatch priority of the jobs of a run. A job-level value overrides the workflow-level one.
The scheduler's use of it is in [04-runs.md](04-runs.md).

### `on.upstream`

`project` (an `organisation/project` path; the current project when omitted), `workflows` (names,
required), `branches` and `conclusions` (default `[success]`). It fires when a run of a named
workflow completes with one of the conclusions; `github.event_name` is `upstream` and `github.event`
is the upstream run's resource with its project and workflow. Unlike `workflow_run`, it crosses
projects, which requires the triggering user of the upstream run to have `run.dispatch` on this
project ([08-access.md](08-access.md)). [Runs that follow runs](#runs-that-follow-runs) settles
the rest.

### `on.plugin` and `notify`

`on.plugin` lists trigger plugin instances by name with the filter object each plugin defines.
`notify` is a list of routes, each with `when` (a subset of `started`, `success`, `failure`,
`cancelled`, `fixed`, `waiting`) and `to` (notifier plugin instance names). Both are specified in
[10-plugins.md](10-plugins.md).

## The IR

Lowering turns the syntax tree into the IR defined in `bosun-types`. The planner works from the IR
on the server and the execution engine works from the job part of it on the agent; neither reads
YAML.

```
Workflow   { name, run_name: Template?, triggers: [Trigger], permissions: Permissions?,
             env: Map<Template>, defaults: Defaults, concurrency: Concurrency?,
             cache_mode: CacheMode?, priority: Priority?, notify: [Route], jobs: [JobTemplate],
             calls: Map<JobKey, usize>, callees: [Reusable] }
Reusable   { repository: "owner/name", path, commit, workflow: Workflow }
JobTemplate{ key, name: Template?, needs: [JobKey], if: Condition?, runs_on: Evaluable<RunsOn>?,
             permissions: Permissions?, concurrency: Concurrency?, strategy: Evaluable<Strategy>?,
             env: Map<Template>, defaults: Defaults, timeout_minutes: Evaluable<u32>,
             continue_on_error: Evaluable<bool>, outputs: Map<Template>,
             secrets: SecretReferences, container: Container?, services: [Service],
             cache_mode: CacheMode?, cache_volumes: [CacheVolume], priority: Priority?,
             kind: Steps [Step] | Call(Call) | Gate, environment: Evaluable<Environment>?, retry,
             hold_on_failure, span }
Container  { image: Template, credentials: Credentials?, env: Map<Template>, ports: [Template],
             volumes: [Template], options: Template? }
Credentials{ username: Template?, password: Template? }
Service    { name, container: Container }
CacheVolume{ name, path, scope: Project | Organisation, key: Template?, mode: Exclusive | Shared }
CacheMode  = Read | Write | WriteOnly | None
Priority   = Low | Normal | High
Call       { workflow: Callee, with: [Argument], secrets: CallSecrets, span }
Callee     = Local { path } | Remote(ActionReference)
Argument   { name, value: Boolean | Number | String(Template), span }
CallSecrets= Inherit | Named [PassedSecret { name, value: Template, span }]
Step       { key: StepKey?, name: Template?, if: Condition?,
             action: Run { script } | Uses(ActionReference) | Local { path } | Docker { image },
             shell: Shell?, working_directory: Template?, with: Map<Template>, env: Map<Template>,
             continue_on_error: Evaluable<bool>, timeout_minutes: Evaluable<u32>?, retry, pause, span }
RunsOn     { group: Template?, labels: [Template] }
Permissions= ReadAll | WriteAll | Scopes(Map<Scope, Read | Write | None>)
Defaults   { shell: Evaluable<Shell>?, working_directory: Template? }
Concurrency{ group: Template, cancel_in_progress: Evaluable<bool>, queue: Single | Max }
Strategy   { matrix: Evaluable<Matrix>?, fail_fast: Evaluable<bool>, max_parallel: Evaluable<u32>? }
Matrix     { vectors: [Vector], include: Evaluable<[[MatrixField]]>?,
             exclude: Evaluable<[[MatrixField]]>? }
Vector     { name, values: Evaluable<[MatrixValue]> }
MatrixField{ name, value: MatrixValue }
MatrixValue= Null | Boolean | Number | String(Template) | List [MatrixValue] | Object [MatrixField]
SecretReferences = Named [name] | All
ActionReference { owner, repository, path: String?, ref }
Trigger    = Push { branches: Filter?, tags: Filter?, paths: Filter? }
           | PullRequest | PullRequestTarget { types: [String]?, branches: Filter?, paths: Filter? }
           | MergeGroup { types: [String]?, branches: Filter? }
           | Schedule [{ cron: Cron, timezone: String? }]
           | WorkflowDispatch { inputs: [Input] } | WorkflowCall(WorkflowCall)
           | WorkflowRun { workflows: [String], types: [String]?, branches: Filter? }
           | Upstream { project: String?, workflows: [String], branches: Filter?,
             conclusions: [String] }
           | RepositoryDispatch { types: [String]? } | Generic { name, types: [String]? }
WorkflowCall { inputs: [CallInput], secrets: [CallSecret], outputs: [CallOutput] }
CallInput  { name, description: String?, required: bool,
             kind: String | Boolean | Number, each with its default? }
CallSecret { name, description: String?, required: bool }
CallOutput { name, description: String?, value: Template }
Filter     = Include [pattern] | Exclude [pattern]
Cron       { expression, minutes, hours, days_of_month, months, days_of_week: [u8], days: Both | Either }
Input      { name, description: String?, required: bool,
             kind: String | Boolean | Choice { options } | Number | Environment, each with its default? }
ActionMetadata { name: String?, description: String?, author: String?, branding: Branding?,
             inputs: [ActionInput], outputs: [ActionOutput], runs: Runs }
Branding   { icon: String?, color: String? }
ActionInput{ name, description: String?, required: bool, default: Template?,
             deprecation_message: String? }
ActionOutput { name, description: String?, value: Template? }
Runs       = Node { using: Node12 | Node16 | Node20 | Node24, main, pre: String?,
             pre_if: Condition?, post: String?, post_if: Condition? }
           | Composite { steps: [Step] }
           | Docker { image, args: [Template], env: Map<Template>, entrypoint: String?,
             pre_entrypoint: String?, pre_if: Condition?, post_entrypoint: String?,
             post_if: Condition? }
```

The fields of later milestones (`notify`, `environment`, `retry`, `pause`, the hold and the kind
`Gate`) join the IR with the milestone that implements them; until then deserialising the IR
refuses them like any unknown field. The fields of M2 are in it, and lowering produces them
whether or not their keys are still on the pending list. A job that calls a workflow has no
`runs_on`, and a field that is `None`, empty or absent is left out of the serialised IR where it
is new in M2 (`container`, `services`, `cache_mode`, `cache_volumes`, `priority`).

`calls` and `callees` are filled by the planner, never by the parser: a run's snapshot holds, in
`callees` of its workflow, every reusable workflow of the run once, lowered, with the repository,
path and commit it was read at, and each workflow, the run's own and every callee, maps in `calls`
the key of each of its jobs that calls a workflow to the position of its callee. A snapshot with
no call has neither field.

`JobKey` and `StepKey` are the ids a workflow gives its jobs and steps (`jobs.<key>`,
`steps.<key>`); the identifiers of the database rows are a separate kind. The job of a called
workflow has the key `<caller>/<callee job id>` in a run, and a key nests as the calls do, so
`release/build/test` is the job `test` of the workflow that `build` calls in the workflow that
`release` calls; a job id a file writes has none of these slashes. Jobs and steps keep the
order of the source. `Template` is a string with embedded expressions, already parsed; `Expr` is a
bare expression; `Condition` is the value of an `if`, an `Expr` or, when text surrounds its
`${{ }}`, a `Template` ([03-expressions.md](03-expressions.md)); `Evaluable<T>` is a literal value
or a template that must evaluate to `T`. The expression syntax tree is plain data in `bosun-types`,
so that the IR can hold it; parsing and evaluating it is the business of `bosun-expr`. Every node
keeps its source span, so an error found at run time points at the line that caused it.

The IR is serialised as JSON for storage in the run snapshot and for the job assignment sent to an
agent. Its shape is pinned by golden fixtures in `contracts/fixtures/ir/`. Deserialising the IR
refuses unknown fields: an agent that does not know a field must refuse the job rather than run it
as if the field were absent.

A run snapshot outlives the build that stored it, and a run is planned, re-run and shown from its
snapshot. From the first release on, the IR therefore changes only in ways that keep every stored
snapshot readable with the meaning it had: a new field has a default that means what its absence
meant, and a field keeps its type and its meaning. Before the first release the shape changes
freely, and a snapshot an earlier build stored may no longer deserialise.

## Validation

Validation runs after lowering and before any run is created. Errors:

- the job graph has a cycle, or `needs` names a job that does not exist;
- a job id or step id does not match `^[A-Za-z_][A-Za-z0-9_-]*$`, or a step id repeats;
- an expression does not parse, calls an unknown function or a function with the wrong arity, or
  uses a context that is not available at that key ([03-expressions.md](03-expressions.md));
- mutually exclusive keys appear together: `run` and `uses`, `gate` and `steps`, a reusable call
  and `runs-on`. A job with `uses` calls a workflow and takes only `name`, `needs`, `if`, `uses`,
  `with`, `secrets`, `permissions`, `concurrency` and `strategy`; `steps`, `runs-on`, `env`,
  `container`, `services`, `outputs`, `defaults`, `timeout-minutes`, `continue-on-error`,
  `cache-mode`, `cache-volumes`, `priority` and every other key is `invalid_value` at the key,
  with the message that it cannot be used together with `uses`. The extension keys of the list are
  left out because this document does not say they apply to a call, and a refusal can be relaxed
  later where an acceptance cannot be taken back. `with` and `secrets` without `uses` are
  `invalid_value` at the value;
- `uses` is not a valid reference. A job's `uses` is `./path` without a ref, or
  `owner/repo/path@ref` whose path ends in `.yml` or `.yaml`; a repository without a path, a
  directory and a container image are refused. A step's `uses` is as [Lowering jobs and
  steps](#lowering-jobs-and-steps) says;
- a `workflow_dispatch` or `workflow_call` input has a default that does not fit its type;
- two `cache-volumes` of a job share a `name` or a `path`.

`check_call(file, caller, callee)` is the pure check of one call against the lowered callee, for
the planner to run when it has read the callee ([04-runs.md](04-runs.md)). It returns nothing for a
job that does not call. For a call it reports, each at the entry at fault with the file it is
given:

- a callee without an `on.workflow_call` trigger, `invalid_value` at the `uses`, and nothing else,
  since the target refuses such a call;
- an input of `with` the callee does not declare, `unknown_key` with a suggestion;
- a value that cannot be of the input's type, `invalid_type`. A string input takes any value; a
  boolean or number input takes its own kind, and a template, which can only be judged when it is
  evaluated, as does a literal text that reads as a boolean or a finite number;
- a required input without a default that `with` leaves out, `missing_key` at the `uses`;
- a secret of `secrets` the callee does not declare, `unknown_key`, and a required secret it
  leaves out, `missing_key` at the `uses`. `secrets: inherit` passes whatever the callee declares
  and is not checked.

Names of inputs and secrets compare ignoring case.

Warnings:

- extension keys and the `bosun` context in `.github/workflows/`;
- deprecated workflow commands or Node runtimes;
- an `if` with text around its `${{ }}`, or with several of them, which is a string and so true
  unless it is empty ([03-expressions.md](03-expressions.md));
- secrets accessed by a computed name (`secrets[format(...)]`) or as a whole (`toJSON(secrets)`),
  which forces the job to receive every secret in scope instead of only the referenced ones
  ([08-access.md](08-access.md));
- `pull_request_target` combined with a checkout of the pull request's head, which runs untrusted
  code with secrets: a step of a workflow that `pull_request_target` triggers uses
  `actions/checkout`, its owner and repository compared ignoring case, and its input `ref` or
  `repository` reads the head of the pull request. The head is `github.head_ref`, anything under
  `github.event.pull_request.head`, `github.event.pull_request.merge_commit_sha`, and the numbers
  `refs/pull/<n>/merge` is built from, `github.event.number` and `github.event.pull_request.number`.
  The warning `untrusted_checkout` is at the value of the input. An indirection, such as a variable
  of `env` that holds the head, is not followed.

`bosun workflow lint`, the editor and the server run the same validation from `bosun-workflow`.

### Diagnostic codes

Every diagnostic carries one of these codes, and every code is pinned by a case in
`contracts/fixtures/workflows/` or, for the metadata of actions, `contracts/fixtures/actions/`. The
codes of the static checks of expressions are those of [03-expressions.md](03-expressions.md),
raised at the expression in the file; `limit_exceeded` and `not_implemented` serve both. A case of
a warning lowers to its IR and has the warning beside it.

| Code | Severity | Meaning |
|------|----------|---------|
| `yaml_syntax` | error | the source is not well-formed YAML |
| `unsupported_yaml` | error | a merge key, a tag, or more than one document |
| `limit_exceeded` | error | a limit of the parser, of the installation or of an expression is exceeded, or a run nests reusable workflows deeper than 10 levels or calls more than 50 distinct ones ([Reusable workflows](#reusable-workflows)) |
| `duplicate_key` | error | a key appears twice in one mapping, or an event twice in the list form of `on` |
| `invalid_type` | error | a value of the wrong type, such as a mapping where a string or a list is expected |
| `invalid_value` | error | a value of the right type that the key does not accept, such as an empty `steps`, a custom shell without `{0}`, a `uses` without a ref, or an expression in a key that takes none |
| `missing_key` | error | a required key is absent; not reported for a mapping in which another key is already at fault |
| `unknown_key` | error | a key or an event that is neither in the compatibility surface nor an extension; the message suggests the closest known one |
| `not_implemented` | error | a key, an event or a form of the compatibility surface that a later milestone implements, or a call of `plugin()`; the message names the milestone |
| `invalid_job_key`, `invalid_step_key` | error | a job id or a step id that does not match `^[A-Za-z_][A-Za-z0-9_-]*$` |
| `repeated_step_key` | error | two steps of one job with the same id |
| `unknown_job` | error | `needs` names a job that does not exist |
| `needs_cycle` | error | `needs` forms a cycle |
| `expression_syntax` | error | an expression or a template does not parse, or a `${{` is not closed |
| `unknown_context` | error | an expression names something that is no context |
| `unavailable_context` | error | an expression uses a context its key cannot see, such as `steps` in `runs-on` |
| `unknown_function` | error | an expression calls a function the language does not have |
| `unavailable_function` | error | a status function outside `if`, or `hashFiles` outside the keys of a step |
| `wrong_argument_count` | error | a call with a number of arguments the function does not take |
| `template_condition` | warning | an `if` that is a template, and therefore true unless it is empty |
| `all_secrets` | warning | a job reads secrets by a computed name or as a whole, and so receives every secret in scope |
| `ignored_key` | warning | an unknown key at the root of an action's metadata, in an input or in `branding`, which the runner of the target ignores; the message suggests the closest known one |
| `extension` | warning | an extension key or the `bosun` context in a file under `.github/workflows/`, which GitHub Actions then refuses |
| `untrusted_checkout` | warning | a workflow that `pull_request_target` triggers checks out the head of the pull request |
