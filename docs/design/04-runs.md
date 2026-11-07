# Runs

This document specifies what a run is made of, the states of runs, jobs and steps, how a run is
planned and its jobs are dispatched, the journal that records everything that happens to a run,
the commands an operator can give, re-runs and attempts, gates and environments, and holding a
failed job for debugging. How the agent executes a job is in [05-execution.md](05-execution.md);
who may give which command is in [08-access.md](08-access.md).

## Parts of a run

- A **run** is created by one trigger for one workflow revision. It records the revision, the IR
  snapshot, the trigger (event name, payload, actor), the ref and the commit, the dispatch inputs,
  a number that counts runs of the workflow, and the global number exposed as `github.run_id`.
- An **attempt** is one pass of the run. Attempt 1 is created with the run; every re-run adds the
  next one ([Re-runs and attempts](#re-runs-and-attempts)).
- A **job** belongs to one attempt. It is created from a job template; a template with a matrix
  becomes a **job group** whose member jobs are created when the matrix is expanded. The job of the
  template is the group: it stays `pending` while its members run, completes when all of them have,
  and is what `needs` and `needs.<job>` of other jobs name. Each member has the template's key, the
  group in `group_id` and its combination of the matrix in `matrix`. A call of a reusable workflow
  becomes a group whose members are the callee's jobs, keyed `caller/callee` and named
  `<caller name> / <callee job name>` ([Calls of reusable workflows](#calls-of-reusable-workflows)).
  A matrix on the call gives one such group per member of the matrix.
- An **execution** is one placement of a job on an agent. A job has more than one execution only
  when job-level `retry` re-queued it.
- A **step** belongs to an execution. Steps of composite actions are nested under the step that
  used the action; pre and post steps of actions are steps of their own. The steps of an execution
  are created `pending` when its agent starts the job, one per stage of the job
  ([05-execution.md](05-execution.md#node-actions)): the pre steps of its actions, one step per step
  of the job template, then the post steps of its actions. A step of the template is named by its
  `name`, or else by `Run ` and the first line of its script, or by `Run ` and the `uses` of a step
  that uses an action, such as `Run actions/checkout@v4`, as in the target; its pre and post steps
  by `Pre ` and `Post ` before that name, such as `Post Run actions/checkout@v4`. The stages are
  those of the assignment: the replica that sends it stores them on the execution when an action
  adds a pre or a post step ([07-storage.md](07-storage.md#tables)), and an execution without
  stored stages has one step per step of the template. Until the agent
  renders names (step 21 of [../plan.md](../plan.md)), these names are taken before anything is
  evaluated: an expression in the `name` or in that line is left out, and a `name` that holds
  nothing else counts as none, so `name: Build ${{ matrix.os }}` names the step `Build` and
  `run: echo ${{ github.sha }}` names it `Run echo`.

## States

| Entity | Status | Meaning |
|--------|--------|---------|
| run | `queued` | created, no job has started |
| | `in_progress` | at least one job has started and not all are complete |
| | `waiting` | no job is queued or on an agent, and at least one waits, for admission, an approval, a timer, a policy, the resolution of its actions or a concurrency group; the others are pending behind it |
| | `paused` | an operator paused the run |
| | `completed` | every job is complete; the conclusion is set |
| job | `pending` | its `needs` are not complete |
| | `waiting` | ready, but held back: see [waiting reasons](#gates-and-environments) |
| | `queued` | may be dispatched to an agent |
| | `assigned` | offered to an agent, which has not confirmed the start yet |
| | `in_progress` | running on an agent |
| | `paused` | stopped at a step boundary by an operator or a breakpoint |
| | `held` | a step failed and the job is kept alive for debugging |
| | `completed` | the conclusion is set |
| execution | `assigned`, `in_progress`, `paused`, `held`, `completed` | the job's status while this execution is the current one; an earlier execution of a retried job stays `completed` |
| step | `pending`, `in_progress`, `paused`, `completed` | as for jobs |

Conclusions are `success`, `failure`, `cancelled`, `skipped` and `timed_out`; a run can also end
with `startup_failure` when its workflow is invalid or its workflow-level `concurrency` cannot be
evaluated ([Concurrency groups](#concurrency-groups)). A step has an `outcome` (the result before
`continue-on-error`) and a `conclusion` (after it), as in the target syntax. Every conclusion has a
**source**: `natural`, or `operator` when a command chose it (`job.skip`, `job.mark-success`,
`step.skip`, `hold.continue`); the source is shown everywhere the conclusion is, and is part of the
API. A cancellation is not a chosen result, so a job or a step that a cancel command ends is
`cancelled` with the source `natural`, and the reason `command` names its cause.

```
pending ──needs complete, if true──▶ waiting ──rules satisfied──▶ queued ──▶ assigned ──▶ in_progress
   │                                    │                           ▲           │            │  ▲
   └──if false──▶ completed(skipped)    └──rejected, timed out──▶ completed      └─no start──┘  │
                                                                                 paused ◀────┤  │
                                                                                 held  ◀─────┘  │
                                                                                   └──retry─────┘
```

The transitions of a job, exhaustively:

| From | To | Cause |
|------|----|-------|
| `pending` | `waiting` | `needs` complete, `if` true, an admission rule holds the job back |
| `pending` | `queued` | `needs` complete, `if` true, nothing holds the job back |
| `pending` | `completed` | `if` false (`skipped`), `job.skip`, `job.mark-success`, cancellation, the members of its group completed |
| `waiting` | `queued` | every admission rule is satisfied |
| `waiting` | `waiting` | the admission rule that held the job back is satisfied and a later one holds it, such as `max_parallel` giving way to `concurrency`, or `concurrency` giving way to `resolving` |
| `waiting` | `completed` | rejection, gate timeout, branch policy, policy denial, a forbidden or unresolvable action, `job.skip`, `job.mark-success`, cancellation |
| `queued` | `waiting` | `job.pause`, `run.pause` |
| `queued` | `assigned` | claimed by an agent |
| `queued` | `completed` | cancellation, `no_agent`, `job.skip`, `job.mark-success` |
| `assigned` | `queued` | the lease expired or the agent rejected the job |
| `assigned` | `in_progress` | the agent started the job |
| `assigned` | `completed` | cancellation, the agent removed, an evaluation error at the assignment |
| `in_progress` | `paused` | a pause request or a breakpoint at a step boundary |
| `paused` | `in_progress` | `job.resume`, `run.resume` |
| `in_progress` | `held` | a failing step under hold-on-failure |
| `held` | `in_progress` | `hold.retry-step`, `hold.continue` |
| `in_progress` | `queued` | job-level `retry`, which starts a new execution |
| `in_progress`, `paused`, `held` | `completed` | the job ends, cancellation, timeout, `hold.release`, `job.mark-success` from `held`, lost agent |

The status of a run is derived from its jobs, so its transitions follow from the definitions in the
state table. The transitions of a run, exhaustively:

| From | To | Cause |
|------|----|-------|
| `queued` | `in_progress` | a job started |
| `queued` | `waiting` | every job that is not complete waits, or is pending behind one that waits |
| `queued`, `in_progress`, `waiting` | `paused` | `run.pause` |
| `queued`, `in_progress`, `waiting`, `paused` | `completed` | every job is complete, `startup_failure`, cancellation |
| `in_progress` | `waiting` | every job that is not complete waits, or is pending behind one that waits |
| `waiting` | `queued` | a job left `waiting` and no job has started yet |
| `waiting` | `in_progress` | a job left `waiting` after another job had started |
| `paused` | `queued`, `in_progress`, `waiting` | `run.resume`, to whatever the jobs imply |

The transitions of an execution, exhaustively:

| From | To | Cause |
|------|----|-------|
| `assigned` | `in_progress` | the agent started the job |
| `assigned` | `completed` | the lease expired, the agent rejected the job, cancellation, the agent removed, an evaluation error at the assignment |
| `in_progress` | `paused` | a pause request or a breakpoint at a step boundary |
| `in_progress` | `held` | a failing step under hold-on-failure |
| `paused`, `held` | `in_progress` | `job.resume`, `run.resume`, `hold.retry-step`, `hold.continue` |
| `in_progress`, `paused`, `held` | `completed` | the job ends, cancellation, timeout, `hold.release`, lost agent |

The transitions of a step, exhaustively:

| From | To | Cause |
|------|----|-------|
| `pending` | `in_progress` | the agent starts the step |
| `pending` | `paused` | a breakpoint before the step |
| `pending`, `paused` | `completed` | `if` false, `step.skip`, the job ends before the step |
| `paused` | `in_progress` | `job.resume`, `run.resume` |
| `in_progress` | `completed` | the step ends, `step.abort`, cancellation |
| `completed` | `in_progress` | `hold.retry-step`, and nothing else |

When an execution completes, its steps that have not completed complete with it: a `pending` one as
`skipped`, since the job ended before it, and one still `in_progress` as `cancelled`.

`bosun-types` holds these four tables as code and refuses every other transition; a test reads the
tables from this document and compares them with the code.

## Planning

When a trigger is accepted, the server finds the workflows whose `on` matches it and, in one
transaction per workflow, creates the run, attempt 1, and one job per template in status
`pending`; the runs of an event the forge delivered share the transaction of its delivery
([02-workflows.md](02-workflows.md#repository-workflows)). It evaluates `run-name` and the
workflow-level `concurrency` at that moment, the `concurrency` before the jobs are created, so a
group that cannot be evaluated ends the run with `startup_failure` and no jobs, and the run then
enters its group ([Concurrency groups](#concurrency-groups)). A reusable workflow named by a job
is fetched and validated before the run is created, so a missing, invalid or forbidden callee ends
the run with `startup_failure` and no jobs, before anything executes
([Calls of reusable workflows](#calls-of-reusable-workflows)). The planner then runs in the same
transaction, so
the jobs that need no other job leave it `queued`.

The warnings of the revision become the run's first notices ([Run notices](#run-notices)). The
run's **display name** is `run-name` rendered with the contexts of its key
([03-expressions.md](03-expressions.md)); without `run-name`, or when it renders to nothing but
whitespace, it is the workflow's `name`, or else the workflow's file, which is the name of a
server-side workflow. An evaluation error in `run-name` does not stop the run: the run keeps that
default name and gains an error notice at the expression, and its jobs are planned as usual, since
the name decides nothing about what runs. A job is created with its literal `name`, or with its
key when the `name` has an expression or there is none; the rendered `name` replaces it when the
job becomes ready.

The planner runs in the transaction of every change that can make a job ready or end a run: the
dispatch, an agent starting or finishing a job, a cancel command and the loss of an agent. It holds
the run's lock ([07-storage.md](07-storage.md)), so two jobs that complete at once are planned one
after the other and their dependent sees both. After deciding the jobs it settles the run's status
from them, as the state table defines it: `in_progress` once a job has started, `completed` when
every job is.

A pending job becomes ready when every job in its `needs` is complete. The server then evaluates,
in this order:

1. `if`, with the status functions over the job's ancestors, the jobs it needs directly or through
   other jobs. False completes the job as `skipped` with the reason `if_false`, or, while the run is
   being cancelled, as `cancelled` with the reason `command`. Without an `if` the job behaves as
   `if: success()`, and an `if` that calls no status function as `success() && (<if>)`, which are
   false when any of its ancestors did not succeed ([03-expressions.md](03-expressions.md)), so a
   job after a failed, skipped or cancelled one is skipped unless its `if` says otherwise. A job
   that failed or timed out under `continue-on-error` counts as succeeded here, in the status
   functions and in `needs.<job>.result` of its dependents, while its own conclusion stays. An
   ancestor that is not complete yet, which happens when `job.cancel` completed a pending job
   between them, counts as neither succeeded nor failed.
2. `strategy`. A matrix expands into member jobs here, in status `pending` with their `needs`
   already met, and each member continues from step 3 ([Matrices](#matrices)). An empty vector
   or matrix completes the group as `failure` with an error that names it, as the target does. A
   strategy without a matrix makes no group: the job continues from step 3 with its `fail-fast`
   and `max-parallel` in the `strategy` context.
3. `name`, `runs-on`, `environment`, `concurrency`, `timeout-minutes`, `continue-on-error`, the
   job's `env` and `defaults`, `container` and `services` (image and credentials). `runs-on` gives
   the job its labels, stored lowercased, and the pools it may run in
   ([07-storage.md](07-storage.md)). The rendered `name` becomes the job's display name; the other
   values are stored on the job for its assignment ([07-storage.md](07-storage.md)). A key that is
   one `${{ }}` takes the type of the expression's value, any other text is a string: `runs-on` is
   a label, a list of labels or a mapping of `group` and `labels`, none of them empty;
   `timeout-minutes` a whole number of at least 0; `continue-on-error` a boolean; and a shell in
   `defaults.run` one of the shells of [02-workflows.md](02-workflows.md).
4. The admission rules: the environment's protection rules, the admission policies of plugins,
   the concurrency groups, the resolution of the job's remote actions, and a pause of the run or the
   job. While any of them holds the job back, it is `waiting` with the reason recorded. A branch
   policy that excludes the ref, or a policy that denies, completes the job as `failure` with the
   reason. The rules of M1 apply in this order: the `max-parallel` of the job's group
   ([Matrices](#matrices)), the admission of a run from a fork
   ([08-access.md](08-access.md#code-from-outside-the-team)), the run's own concurrency group, which
   holds every job of a run that waits in it, the job's concurrency group
   ([Concurrency groups](#concurrency-groups)), and the resolution of the job's remote actions
   ([Resolving actions](#resolving-actions)). A job enters its group before its actions resolve, so
   that it arrives in the order in which it became ready, however long the forge takes for each job.
   When a rule gives way, the job goes on with the rules after it, so a member that `max-parallel`
   releases may go on to wait for its concurrency group, and one that its group admits may go on to
   wait for its actions to resolve, which a `job.status` event from `waiting` to `waiting` with the
   new reason records.
5. Otherwise the job becomes `queued`.

A gate skips steps 2, 3 and the queue: once its admission rules pass, it waits for approvals.

An expression of steps 1 to 3 that fails to evaluate, or whose value the key cannot use, completes
the job as `failure` with the reason `evaluation_failed`, whatever its `continue-on-error`, after an
error notice at the expression's span that names the job. Its dependents then follow their `if`.
The keys are evaluated in the order of the list, and the first error ends the evaluation. The
workflow's `env` is evaluated later, when an agent claims the job
([05-execution.md](05-execution.md#the-job-assignment)), and so are the job's `env` and
`defaults.run` when they read secrets ([03-expressions.md](03-expressions.md#the-contexts-the-server-supplies));
an error there, or a secret that does not decrypt, completes the assigned execution and the job the
same way, before the agent is sent anything.

A job group completes when all its members are complete. Its conclusion is `failure` when a member
failed or timed out without `continue-on-error`, `cancelled` when a member was cancelled, and
`success` otherwise. `fail-fast`, true by default, cancels the remaining members of a group when one
fails. `max-parallel` keeps the members beyond the limit in `waiting` with reason `max_parallel`.

### Matrices

A matrix is expanded as the target expands it ([02-workflows.md](02-workflows.md)), with the
contexts of `jobs.<job_id>.strategy`: `github`, `needs`, `vars`, `inputs` and `bosun`. A key or a
value that is one `${{ }}` takes the type of its value and any other text is a string, as in
step 3, so `matrix: ${{ fromJSON(needs.setup.outputs.matrix) }}` and a vector computed the same way
expand like written ones. The cases of `contracts/fixtures/matrix/` pin the rules:

- The vectors combine in their order, the first varying slowest, so `os: [linux, macos]` and
  `node: [20, 22]` give `(linux, 20)`, `(linux, 22)`, `(macos, 20)` and `(macos, 22)`.
- An `exclude` entry removes every combination it matches, and it matches when each of its values
  equals the combination's value of that vector, so an entry may name only some of the vectors.
  An entry that names something that is no vector fails the group.
- `include` applies after `exclude`, entry by entry. An entry extends every combination left by
  `exclude` whose values it matches in the vectors it names, adding its other values and
  overwriting only values an earlier entry added; an entry that matches none of them is a
  combination of its own, which later entries do not extend. A matrix of `include` alone has one
  combination per entry.
- Values compare as JSON values: strings exactly, mappings regardless of the order of their keys.
- The group fails when a vector has no value, when the matrix has neither a vector nor an
  `include`, when `exclude` leaves no combination and nothing is included, and when the matrix
  expands to more than 256 jobs, the installation's limit of [02-workflows.md](02-workflows.md),
  which the message names. Vectors whose combinations exceed 65 536 before `exclude` fail the
  group with the same code without being combined.

Each combination becomes a member, created in the order of the combinations. A member is named
like its template, its `name` or else its key, followed by its values in parentheses, joined by
`, ` in the order of the combination, the values of a mapping or a list in their order, so
`test (linux, 20)`; a `name` with an expression is rendered at step 3 with the member's contexts
and gets no parentheses, so `Build on ${{ matrix.os }}` names the member `Build on linux`. A member
sees its combination as `matrix` and `strategy` as `{"fail-fast", "job-index", "job-total",
"max-parallel"}`, where `job-index` counts from 0 in the order of creation and `max-parallel` is the
number of members when the strategy sets none. A job without a strategy sees `strategy` as
`{"fail-fast": true, "job-index": 0, "job-total": 1, "max-parallel": 1}` and `matrix` as `null`.
Each member's `continue-on-error` is its own, so `continue-on-error: ${{ matrix.experimental }}`
lets the experimental members fail without failing the group.

The planner keeps the group going in every transaction it runs in:

- When `fail-fast` holds and a member failed or timed out without `continue-on-error`, including a
  member whose evaluation failed, every member that is not complete is cancelled with the reason
  `fail_fast`, as a cancel command cancels it ([Operator commands](#operator-commands)): at once
  when it is not on an agent, through a `cancel` control message when it is. Waiting members are
  not released after that.
- Otherwise, when fewer members than `max-parallel` are `queued`, `assigned`, `in_progress`,
  `paused` or `held`, or wait with the reason `concurrency` or `resolving`, the members waiting
  with the reason `max_parallel` are released in the order of creation until the limit is reached
  again, each to the admission rules after `max-parallel`, which queue it unless a concurrency
  group holds it or its remote actions are still to resolve. A member that becomes ready while the
  limit is reached goes to `waiting` with the reason `max_parallel` after step 3. A member that
  waits for a concurrency group or for its actions counts against the limit, so admitting it never
  exceeds `max-parallel`.
- When every member is complete, the group completes with the conclusion above, from `pending`,
  and with the outputs of its members: the members are taken in the order they completed, and a
  value that is not empty replaces what an earlier member gave for that name, so the last member
  that set an output wins, as in the target.

The run completes when every job of the attempt is complete. Its conclusion is the first of these
that holds: `failure` when a job failed or timed out without `continue-on-error`, `cancelled` when
the run or one of its jobs was cancelled, `skipped` when every job was skipped, and `success`
otherwise. A run that accepted `run.cancel` ends `cancelled` unless a job failed, even when its last
running job then finished well before the cancel reached it. A job failed by an evaluation error
has no `continue-on-error`, so it fails the run, and so does a job that an action policy fails with
`action_forbidden` ([Resolving actions](#resolving-actions)).

### Calls of reusable workflows

A job that calls a workflow is a template like any other until it is ready. Its `if` and its
`strategy` are decided as step 1 and 2 above say, and a matrix gives one **call** per member. Then,
instead of a placement, the call is **expanded**: its `with` and `secrets` are evaluated, its row
keeps the result in `call` ([07-storage.md](07-storage.md#tables)), and a member is created in
`pending` for each job of the callee, in the order of the file, keyed `<call>/<callee job id>` with
`kind` `steps` or `call`, its `group_id` the call and `needs` the callee's, each with the call's key
before it. A call stays `pending` while its members run, as the template of a matrix does, and the
callee's jobs continue from step 1 with the contexts of the callee. The cases of
`contracts/fixtures/calls/` pin what the planning refuses, and `compat/cases/reusable/` the inputs,
the secrets and the outputs.

- **Scope.** `needs`, `needs.<job>` and the status functions of a callee's job name jobs of the
  same call, by their ids in the callee, so a callee's `needs.build.outputs.x` reads the job `build`
  of the callee even when the caller has a job `build` too. The jobs of different calls never see
  each other. `inputs` is the call's evaluated `with`, with the defaults; a job of the run's own
  workflow sees the run's inputs. `github`, `vars` and `bosun` are the run's, and `secrets` is what
  the call passes ([02-workflows.md](02-workflows.md#reusable-workflows)).
- **Names.** The call's display name is its `name` rendered, else its key, and a member is named
  `<call name> / <member name>`, so a matrix member `deploy (eu)` that calls `build` has the member
  `deploy (eu) / build`. That is the name of the check run of a member and of the member the
  interface shows. A call is a group and is never reported.
- **Secrets.** A member is assigned with the secrets of its call, resolved when it is assigned:
  the call keeps, for each secret it passes, a text that was evaluated when the call was ready or
  the name of the caller's secret, and the assignment follows that chain outward until the project's
  own secrets, reading only the names the member's expressions name. An untrusted run reads none, as
  for any job. A secret that does not decrypt fails the member with `secret_unreadable`, as for any
  job. No value of a secret is written to the database by the call.
- **Completion.** When every member of a call has completed, the call completes from `pending` with
  the conclusion of a group ([Matrices](#matrices)) and with the outputs that `on.workflow_call`
  defines, evaluated then ([02-workflows.md](02-workflows.md#reusable-workflows)), which
  `needs.<call>.outputs` of its dependents reads. A call whose `if` is false completes `skipped`
  and has no members. A call whose members all completed but whose outputs fail to evaluate
  completes `failure` with an error notice. The members of a matrix of calls complete when their
  own members do, and the matrix's group completes as any group does, with the outputs of its
  members merged.
- **Matrix controls.** `fail-fast` of a matrix of calls cancels the members that are not complete,
  and the members of their calls with them; `max-parallel` counts a call that has members and has
  not completed as running, and a member of the matrix that would exceed it stays `pending`
  without being expanded until a call completes.
- **Cancelling.** A cancellation of the run or of a job reaches the members of a call, and the call
  follows them. `job.cancel` of a call cancels every member below it, however deep, and cancels a
  call that has not been expanded as any pending job.
- **Reports.** The `permissions` of a call limit the token of every job below it, as
  [02-workflows.md](02-workflows.md#reusable-workflows) says. The `concurrency` of a call and of the
  workflow of a callee is not applied in M2, and the run says so in a `not_implemented` warning for
  each call and each distinct callee that has it, among the notices of the run from the moment it is
  planned, whether or not the call is ever expanded.
- **Reading the callees.** The server reads every callee of a run in the transaction that creates
  the run, and the whole reading has 30 seconds. A webhook delivery that exceeds them fails as a
  forge failure and is tried again with backoff; the firing of a schedule is rolled back and
  tried at the next minute; a dispatch answers with an internal error. The forge answers 403 and 451
  are not told from the other refusals of the forge: for a webhook delivery they close the delivery
  with the error and start no run, and for a schedule they are tried again at every minute, until the
  installation can read the callee or the call is removed.
- **Limit on jobs.** An attempt of a run holds at most 500 jobs that come from calls
  ([02-workflows.md](02-workflows.md#reusable-workflows)). The planning counts the jobs of the callees
  without any matrix before the run is created and refuses a run over the limit with
  `startup_failure`; at run time a call, or a matrix on a call or inside a callee, that would pass
  it fails with `limit_exceeded` and fails its group and, with it, the run. Planning rereads the jobs
  of the attempt after every expansion, so it costs time quadratic in their number, and this limit
  is what keeps that cost bounded.

The codes of the notices about reusable workflows, all with the file that holds the call, its
`uses` or the entry of `with` or `secrets` at fault, and no job:

| Code | Severity | Meaning |
|------|----------|---------|
| `callee_missing` | error | a reusable workflow is not at the commit it is read at, or its ref resolves to no commit in the installation or among public repositories, or the server has no GitHub App to read it with, or the workflow has no repository and the callee is local |
| `callee_invalid` | error | a reusable workflow does not lower, is larger than a file may be, or is not a file `.yml` or `.yaml` directly in `.github/workflows/` or `.bosun/workflows/`; the diagnostics of the file that does not lower follow in that file |
| `callee_forbidden` | error | the organisation's action policy forbids a reusable workflow of another repository, or the call passes `secrets: inherit` to a callee of another account than the one of the calling workflow's repository |
| `limit_exceeded` | error | the call is at the 11th level of nesting, would be the 51st distinct callee, or brings the jobs that come from calls over 500; at run time, a call or a matrix that would pass 500 |
| `invalid_value`, `invalid_type`, `unknown_key`, `missing_key` | error | `check_call`: the callee has no `workflow_call`, or the call passes an undeclared or ill-typed input or secret or leaves out a required one |
| `invalid_type`, `invalid_value`, `function_failed` | error | with the job of the call as the job of the notice: an input of `with` or a passed secret fails to evaluate when the call is ready, or an output fails when the call ends |
| `not_implemented` | warning | a call or the workflow of a callee has a `concurrency`, which M2 does not apply |

### Resolving actions

Step 4 of M2 ([../plan.md](../plan.md)) moved the resolution of remote `uses` from the assignment to
the moment a job is admitted, so that the organisation's action policy
([05-execution.md](05-execution.md#actions-and-tools)) fails a job before it can be queued, and the
commits an agent receives are fixed before any agent claims the job.

- **What waits.** A job whose steps use remote actions, `owner/repo@ref` or `owner/repo/path@ref`,
  that are not resolved yet is `waiting` with the reason `resolving` from the planner's transaction
  on, which calls no forge. The rule comes last: after `max-parallel`, the admission of a run from a
  fork, the run's own concurrency group and the job's own concurrency group
  ([Planning](#planning)). A job resolves only once its own group has admitted it, so the forge is
  never asked for a run nobody has admitted, for a run that waits for its group, or for a job that
  waits for its own group, and a member that a newer arrival displaces from a group is cancelled
  without a request to the forge. Because the job entered its group before it began to resolve, the
  order of arrival in the group is the order in which the jobs became ready, and does not depend on
  which answer of the forge came first. A member in `resolving` is the running member of its group:
  an arrival with `cancel-in-progress` cancels it at once, and the answer of the resolver that comes
  later is dropped. A member in `resolving` counts against the `max-parallel` of its group, as one
  that waits for its concurrency group does. A job without remote actions, and a job whose commits
  are stored, does not stop here.
- **The resolver.** Every replica runs one. It claims the jobs that are `waiting` with the reason
  `resolving` and due with `FOR UPDATE SKIP LOCKED`, in a transaction that only stamps the job with
  a claim token and a lease and commits, so two replicas never take the same job and a replica that
  dies leaves a job another takes when the lease runs out. It asks the forge for the commit of each
  distinct `owner/repo@ref` of the job, in the order of the steps, **with no transaction open**: a
  forge that hangs holds the resolver's task for that job, and a replica runs at most 16 such tasks
  at once, so a hung forge can occupy all 16 until their requests time out and the jobs beyond them
  wait for a free task. The planning of other runs, which never calls the forge, does not wait for
  it. `limits.resolve_request_timeout` bounds each request, not the pass over the job's
  references: a request that lasts longer counts as a forge that did not answer, and the message
  names the reference that was being asked. The pass is bounded by `limits.resolve_give_up`: the
  renewal of the lease before each further request also reports that the job has been `resolving`
  for that long, and the pass then stops as one the forge did not answer, naming the reference it
  was about to ask and saying that the forge did not finish within the limit. The lease of the claim is twice that timeout, and the
  resolver renews it before each further request, so a pass over many references cannot outlive its
  lease. A row of a job that does not read, such as an IR that does not parse, is logged and
  postponed with the backoff, and the other jobs of the same claim go on; once the job has been
  `resolving` for `limits.resolve_give_up`, the next claim completes it as `failure` with the
  reason `action_unresolved` and a notice that says only that the server cannot read the record of
  the job and names nothing from it. If the run cannot be planned after that, because the same
  record is what the planner would read, the job is still completed in a transaction of its own and
  the failure is logged at `error`: the jobs of that run that still wait for it stay pending, the run does
  not conclude and keeps the concurrency groups it holds, and cancelling the run, by hand or by a run
  that arrives in the same group with `cancel-in-progress`, fails on the same record; `job.cancel`
  on each pending job is the recovery. The policy is checked
  before the forge is asked, so the forge is never asked about an action the policy forbids and
  cannot be used to find out whether a repository exists.
- **The decision.** One transaction takes the run's lock, as every change to a run does, and checks
  that the job is still `waiting` with the reason `resolving` and still carries this claim: a
  cancellation, or another replica after an expired lease, may have decided it, and a result that
  arrives late is dropped. It reads the policy again, so a policy changed while the forge answered
  applies, and then:
  - **an action is forbidden.** Each forbidden action adds an error notice `action_forbidden` at
    the first step that uses it, naming the action and the policy. The job completes as `failure`
    with the reason `action_forbidden`, the planner takes the run on, and the job never reaches
    `queued`. The refusal is the server's, not the job's, so it ignores the job's
    `continue-on-error`: the stored value is cleared in the same transaction, the job fails its run,
    and a job that `needs` it follows its `if` as it follows any failed job. A job that fails with
    `action_unresolved` is an ordinary failed job and keeps its `continue-on-error`.
  - **the forge does not know a ref**, or the server has no GitHub App to ask. An error notice
    `action_unresolved` names the action and what the forge said; the job completes as `failure`
    with the reason `action_unresolved` at once. The ref is looked up for the job's organisation as
    [05-execution.md](05-execution.md#actions-and-tools) describes, and a repository the
    organisation cannot see is a ref the forge does not know.
  - **every action resolves and the policy allows them.** The commits are stored with the job, as
    its `actions`, in the same transaction, and the job becomes `queued`. No rule follows
    `resolving`: a job with a concurrency group entered it before it began to resolve and holds it
    as the running member, and the transaction does not enter the group again.
  - **the forge did not answer** (it refused, was rate limited, could not be reached or took longer
    than the request limit). Nothing changes in the job's state: the claim is released and the job
    is due again after a backoff that starts at `limits.resolve_backoff_min` (1 second), doubles
    with each failure and stops at `limits.resolve_backoff_max` (1 minute). A job that has been
    `resolving` for `limits.resolve_give_up` (10 minutes) when a request fails, or when a pass
    over its references is still going on, completes as `failure` with the reason
    `action_unresolved`, and its notice names the forge's error or the reference the pass had
    reached. `limits.resolve_give_up` is thus the limit of the whole time in `resolving`, whatever
    keeps the job there, give or take one request: a pass checks it before each request after the
    first, and a retry waits for a backoff that was already set, so a job can pass it by one request
    timeout and one backoff. The
    give-up clock starts when the job enters `resolving`, so the time it waited for its group
    does not count. Until then the error of the forge is in the server's log: a permanent refusal,
    such as a `401` or `403` of an installation that lost its access, retries with the backoff for
    the whole limit before the job's notice names it.
- **The journal.** Every change of state above is a `job.status` event written in the transaction
  that changes the columns: `pending`, or `waiting` with the reason `max_parallel`, `untrusted` or
  `concurrency`, to `waiting` with `waiting_reason` `resolving`; `waiting` to `queued`; `waiting` to
  `completed` with the reason. A retry changes no status, so it writes none; the claim, the lease
  and the time of the next attempt are bookkeeping of the resolver, not state of the run.
- **Cancelling.** `run.cancel` and `job.cancel` complete a job in `resolving` at once, as they
  complete any waiting job; the resolver that holds it finds that its result belongs to nobody.
- **The assignment.** It takes the stored commits and resolves nothing, so moving a tag after the
  job was admitted changes nothing, and another job resolves the tag again.

## Dispatch

The scheduler matches queued jobs to idle agents. A job may go to an agent when the agent's pool
allows the job's project and trust level ([08-access.md](08-access.md)), when the agent's labels
include every label of `runs-on` (case-insensitively) that no label alias replaced, and when the
agent has the executor the job needs (`podman` for `container`, `services` and container actions,
and the capability its label aliases name, [05-execution.md](05-execution.md#pools-and-label-aliases)).

Among the jobs an idle agent may take, it gets the first in this order: higher `priority` first;
then the project with the lowest ratio of running jobs to its weight (default weight 1), so one
busy repository cannot starve the others; then the oldest. A project may also have a cap on
running jobs; jobs beyond it stay `queued`.

The queue lives in Postgres ([07-storage.md](07-storage.md)). An assignment is a lease: when the
agent does not confirm the start within 30 seconds (`agents.assignment_lease`), the job goes back to
`queued`, and so it does at once when the agent rejects it. The execution that was offered
completes as `cancelled` with the reason `lease_expired` or `rejected_by_agent`, and the job's
`job.status` event from `assigned` to `queued` carries the same reason; the job keeps its
`queued_at`, so it keeps its place in the queue. An agent whose lease expired is also sent a
`cancel` for that execution, in case it starts the job after all. A job that stays
`queued` longer than the installation's `max_queue_time` (default 24 hours) is cancelled with the
reason `no_agent`, so a typo in `runs-on` does not wait forever; the time counts from `queued_at`, so
returns to the queue do not restart it. The job completes `cancelled` with the source `natural`, as
in the example of [The journal](#the-journal), its dependents are skipped, and the run ends
`cancelled` unless a job failed. The run page shows which labels no
online agent offers from the moment the job is queued.

Concurrency groups follow the target syntax: a group has at most one running member and, with
`queue: single` (the default), one pending member, so a new arrival cancels the pending one, and
with `cancel-in-progress` the running one too. With `queue: max` up to 100 members wait in order of
arrival and none is cancelled; it cannot be combined with `cancel-in-progress`. The workflow-level
group applies to runs, the job-level one to jobs.

### Concurrency groups

The `concurrency` of the workflow makes its run a member of a group, and the `concurrency` of a job
makes the job one. The workflow's group is evaluated when the run is created, with the contexts of
its key (`github`, `inputs`, `vars` and `bosun`, [03-expressions.md](03-expressions.md)), and a
job's at step 3 of [Planning](#planning), with the contexts of a job; `cancel-in-progress` is
evaluated together with its group, and a key that is one `${{ }}` takes the type of its value, as in
step 3. The cases of `contracts/fixtures/concurrency/` pin the rules:

- A group is named by its rendered `group`, compared without regard to case within the project, and
  runs and jobs share the names, as in the target: a job whose group is the workflow-level group of
  another run waits for that run. A job whose group is the workflow-level group of its own run
  would wait for itself; it completes `cancelled` with the reason `concurrency` and an error notice
  `concurrency_deadlock`. Only that job is cancelled: the other jobs of the run go on, and the run
  ends `cancelled` because one of its jobs did, where the target cancels the whole run.
- A group has at most one running member and admits members in their order of arrival. A run is
  running from its admission until it completes, and a job from its admission to the group until it
  completes, whether its admission queues it or leaves it `waiting` with the reason `resolving`
  while its remote actions resolve ([Resolving actions](#resolving-actions)): a job in `resolving`
  is the running member of its group, and an arrival with `cancel-in-progress` cancels it at once.
  A member that arrives while the group has a running member or waiting members waits: a run keeps
  every job it makes ready `waiting` with the reason `concurrency`, so the run itself is `waiting`,
  and a job waits with the reason `concurrency`.
- With `queue: single`, the default, an arrival cancels every member that waits in the group, so the
  newest waits alone. With `queue: max`, members wait in their order of arrival and none is
  cancelled; when 100 members wait already, the arrival itself is cancelled, a run with every job
  and a job alone, with the reason `concurrency` and an error notice `concurrency_queue_full`, so
  a group holds at most 101 members and the ones that wait keep their places.
- With `cancel-in-progress` evaluated to `true`, an arrival also cancels the running member, and
  waits until that member has completed. An expression that gives `true` there together with
  `queue: max` is `invalid_value`, as the literal is when the file is validated.
- An arrival cancels a member with the reason `concurrency`, after a `concurrency.cancelled` event in
  the member's run that names the group and the arrival. A job is cancelled as `job.cancel` cancels
  it. A running run is cancelled as `run.cancel` cancels it: its cancellation is requested, so
  `cancelled()` holds for its remaining jobs and a pending job whose `if` admits cancellation still
  runs, as in the target. When such a job's `if` gives false once it is ready, it completes
  `cancelled` with the reason `concurrency`, since the first event that requested the run's
  cancellation is the `concurrency.cancelled`; the reason is `command` when a `run.cancel` requested
  it first. A waiting
  run has started nothing, so every job of it that is not complete is cancelled at once, such
  pending jobs included.
- A `run.cancel` of a run that waits in its group has the command's usual effect and leaves the run
  in its place in the group: its jobs are cancelled but for a pending job whose `if` admits
  cancellation, which waits with the reason `concurrency` once it is ready and runs when the group
  admits the run. A run with such a job therefore completes only after its admission; one without
  completes `cancelled` at once and leaves the group.
- When the running member completes, the group admits its first waiting member in the same
  transaction. An admitted run releases the jobs it kept waiting, each to the admission rules after
  its run's group; an admitted job is queued, or waits with the reason `resolving` when its remote
  actions are not resolved yet.
- A run from a fork that waits for admission ([08-access.md](08-access.md#code-from-outside-the-team))
  is a member of no group, so that a pull request nobody has reviewed cancels nothing and holds no
  place. Its workflow's group is evaluated when the run is created, to refuse a faulty one at once,
  and again when `run.admit` admits the run, which then enters the group like a new arrival; a group
  that fails to evaluate by then cancels the run's jobs with the reason `evaluation_failed` and an
  error notice. Its jobs wait with the reason `untrusted` until then, and `run.admit` releases each
  one to the admission rules of its run's group and of its own.
- An expression in the workflow's `concurrency` that fails to evaluate, gives an empty group or a
  `cancel-in-progress` that is not a boolean ends the run with `startup_failure` before it has
  jobs, with an error notice at the expression, since the run cannot know whether it may start. In a
  job's `concurrency` it fails the job with the reason `evaluation_failed`, as any key of step 3
  does.

Where the members are stored and how two replicas never admit two of them is in
[07-storage.md](07-storage.md#concurrency-groups).

Timeouts: `timeout-minutes` of a job (default 360) counts only the time spent `in_progress`, so
pauses and holds do not consume it. The agent enforces it, as it does a step's `timeout-minutes`: a
job that runs out of time concludes `timed_out`, and a step that does concludes `timed_out` and
fails its job unless the step may fail under `continue-on-error`
([05-execution.md](05-execution.md#main-steps)). A job with `timeout-minutes: 0` or with more than
seven days outlives its job token, whose longest life is 8 days and 3 hours
([06-runtime-services.md](06-runtime-services.md#the-job-token)); the runtime services refuse the
token after that. Paused and held time has its own limit,
`max_pause` (default 24 hours) and the hold duration ([02-workflows.md](02-workflows.md)); when it
runs out, the job is cancelled or released as failed. A paused or held job keeps its agent slot,
and the queue page says which slots are occupied this way.

## The journal

Everything that happens to a run is an event in its journal, the table `run_event`: an
ever-increasing sequence number, the run, the attempt, the job, the execution and the step it
concerns where applicable, the kind, the actor (`user`, `token`, `agent`, `forge`, `plugin` or
`system`, with its identity; `forge` is the account of the forge whose event started the run, with
its forge id and login), a JSON payload, and the time. The status columns of runs, jobs, executions
and steps are a projection of the journal, written in the same transaction as the event, so reading
a status never requires replaying events and the journal never disagrees with it; a run's
`cancel_requested_at` belongs to the projection too, set by the `command.accepted` of a
`run.cancel` or by a `concurrency.cancelled` without a job. A run's `admitted_at` does not, any more
than its `trusted`: both are facts of the run's creation under the project's fork policy, which
`run.created` does not record, so a replay cannot tell a run admitted at its creation under
`automatic` from a trusted one. An accepted `run.admit` sets `admitted_at` in the transaction of its
`command.accepted`, so the change is in the journal all the same, and the projection holds its
effect: the jobs that wait with the reason `untrusted` and the `job.status` events that release
them. A property test drives random job graphs with random step results, commands, lost agents,
expired leases, rejected assignments and queue timeouts, and checks after every transaction that the
columns equal a replay of the journal.

An event has this shape, the same in the database, in the API and in the event stream:

```json
{
  "seq": 42,
  "run_id": "0192f3a4-5b6c-7d8e-9f01-23456789abcd",
  "attempt_id": "0192f3a4-5b6c-7d8e-9f01-23456789abce",
  "job_id": "0192f3a4-5b6c-7d8e-9f01-23456789abcf",
  "execution_id": null,
  "step_id": null,
  "actor": { "kind": "system" },
  "at": "2026-10-24T18:34:50.078Z",
  "kind": "job.status",
  "data": {
    "from": "queued",
    "to": "completed",
    "waiting_reason": null,
    "conclusion": "cancelled",
    "conclusion_source": "natural",
    "reason": "no_agent"
  }
}
```

The kinds of M0, and those M1 adds:

| Kind | Data |
|------|------|
| `run.created` | workflow, revision, run number, global number, event name |
| `attempt.created` | attempt number |
| `job.created` | job key, display name, `needs`, and for a member of a job group the group's job as `group_id` and its combination as `matrix`, both `null` otherwise |
| `execution.created` | execution number, agent |
| `step.created` | step index, display name |
| `run.status` | `from`, `to`, `conclusion` |
| `job.status` | `from`, `to`, `waiting_reason`, `conclusion`, `conclusion_source`, `reason` |
| `execution.status` | `from`, `to`, `conclusion`, `reason` |
| `step.status` | `from`, `to`, `outcome`, `conclusion`, `conclusion_source` |
| `command.accepted` | the command as it was given |
| `command.refused` | the command, the error code and message |
| `run.notice` | from M1: a diagnostic of [02-workflows.md](02-workflows.md) with the severity `error`, `warning` or `notice`; the event's `job_id` names the job it concerns, or is `null` ([Run notices](#run-notices)) |
| `concurrency.cancelled` | from M1: an arrival cancelled a member of a concurrency group: `group`, as the arrival rendered it, and `arrival`, its `run_id` and its `job_id` or `null`; the event's `job_id` names the cancelled job, or is `null` for a run, whose cancellation it requests as the `command.accepted` of a `run.cancel` does ([Concurrency groups](#concurrency-groups)) |
| `trigger.skipped` | from M2: a run that finished or started would have triggered workflows of a project through `on.workflow_run` or `on.upstream`, and the server did not create the runs: the `project_id` of the project whose workflows would have run, which is the run's own project or a project that follows it, and the `reason`: `no_user` (the run has no Bosun user, so it fires no cross-project `upstream`), `no_permission` (its user, or its token, does not hold `run.dispatch` on that project) or `chain_too_long` (the run is the third level of a chain of these triggers). It is written once for the project and the attempt, however many workflows of the project match, and names no workflow. The event's `attempt_id` names the attempt ([Triggers between runs](#triggers-between-runs)) |

Later milestones add kinds for approvals, holds, terminals and the rest of the commands' effects.
A status event always names both ends of the transition, so a replay can check that each event
starts where the previous one ended. The `reason` of a status event says what caused a transition
that is not self-explanatory:

| Reason | Meaning |
|--------|---------|
| `if_false` | the `if` evaluated to false, including the implicit `success()` after a failed dependency |
| `evaluation_failed` | an expression the server evaluated for the job failed, or gave a value its key cannot use, such as a matrix that cannot be expanded, or a secret the job reads could not be decrypted at its assignment; the run's notices hold the error |
| `command` | an operator command caused it; the preceding `command.accepted` says which |
| `fail_fast` | another member of the job group failed under `fail-fast` ([Matrices](#matrices)) |
| `lease_expired` | the agent did not confirm the start within the lease |
| `rejected_by_agent` | the agent answered the assignment with a rejection |
| `no_agent` | the job stayed queued longer than `max_queue_time` |
| `agent_lost` | the agent did not come back within the grace period |
| `agent_removed` | the agent was deleted while it held the job: a job it ran fails, and one it had not accepted yet returns to the queue ([05-execution.md](05-execution.md#draining-and-deleting-agents)) |
| `action_unavailable` | when the job is assigned, an action a step uses has no pinned commit or a tarball that cannot be downloaded or read (the notice has the code `action_unresolved`), has metadata that is missing or invalid (`action_invalid`), or is of a kind the server does not run yet (`not_implemented`); the run's notices name the action ([05-execution.md](05-execution.md#actions-and-tools)). A reference that does not resolve to a commit never gets this far: it fails at readiness with the reason `action_unresolved` |
| `action_forbidden` | the organisation's action policy forbids an action a step of the job uses; the run's notices name the action ([05-execution.md](05-execution.md#actions-and-tools)) |
| `action_unresolved` | the forge does not know a ref of an action a step of the job uses, or did not answer within the limit; the run's notices name the action and the forge's answer ([Resolving actions](#resolving-actions)) |
| `concurrency` | its concurrency group cancelled it: a newer member arrived under `queue: single` or with `cancel-in-progress`, which a `concurrency.cancelled` event names, or the group refused it, which the run's notices say ([Concurrency groups](#concurrency-groups)) |
| `token_unavailable` | the forge did not mint the forge token of the job's execution at its assignment: the server has no GitHub App, or the forge refused, was rate limited or could not be reached; the run's notices hold the forge's error ([06-runtime-services.md](06-runtime-services.md#the-forge-token)) |

### Run notices

A run notice tells what the run page shows beside the graph: a problem of the workflow or of the run
that is not a status. Notices are `run.notice` events, so they keep the order of the journal, and
`GET /runs/{run}` lists them ([11-api.md](11-api.md)). A run gains them

- when it is created, from the warnings of its revision, with their codes from
  [02-workflows.md](02-workflows.md) and without a job;
- when the server evaluates an expression for the run or one of its jobs and the evaluation fails,
  as an error at the expression's span;
- when a concurrency group refuses a run or a job, as an error at its `concurrency`
  ([Concurrency groups](#concurrency-groups));
- when a file with errors yields a run that ends with `startup_failure`, from every diagnostic of
  the file, and from a key the planner does not apply yet as `not_implemented`, with their codes
  from [02-workflows.md](02-workflows.md#repository-workflows) and without a job;
- when the server resolves the actions of a job as it becomes ready and the policy forbids one or
  the forge does not resolve one, as an error at the first step that uses it
  ([Resolving actions](#resolving-actions));
- when the server prepares the actions of a job at its assignment and one of them cannot be used,
  as an error at the step that uses it, or when it runs one on another Node than its `using`
  names, as a warning there ([05-execution.md](05-execution.md#actions-and-tools));
- when the server mints the forge token of a job at its first assignment and the installation of
  the GitHub App does not hold a permission the job asks for, as a warning per permission, or when
  the forge does not mint the token, as an error that fails the job
  ([06-runtime-services.md](06-runtime-services.md#the-forge-token)).

The codes of runtime notices:

| Code | Severity | Meaning |
|------|----------|---------|
| `function_failed` | error | a function failed while the server evaluated an expression, as [03-expressions.md](03-expressions.md) lists |
| `invalid_type` | error | the value of an expression has a type its key cannot use, such as a string for `timeout-minutes` |
| `invalid_value` | error | the value has the right type but the key cannot use it, such as `timeout-minutes: 1.5`, an empty label, a shell that does not exist or an empty vector of a matrix |
| `limit_exceeded` | error | a matrix expands to more jobs than the installation's limit ([Matrices](#matrices)) |
| `concurrency_queue_full` | error | the member arrived at a group where 100 members wait under `queue: max`, so it was cancelled |
| `concurrency_deadlock` | error | a job named the workflow-level group of its own run, so it was cancelled |
| `secret_unreadable` | error | a secret the job reads does not decrypt when the job is assigned: the configuration lacks the KEK of its data key, or its ciphertext or data key is not the one written for its scope and name ([07-storage.md](07-storage.md#secrets-at-rest)) |

The codes of the notices about actions:

| Code | Severity | Meaning |
|------|----------|---------|
| `action_unresolved` | error | the forge does not resolve the `uses` to a commit for the job's organisation, refuses the request or cannot be reached within the limit, when the job becomes ready; or the tarball of the commit cannot be downloaded or is not a tarball, when the job is assigned |
| `action_forbidden` | error | the organisation's action policy forbids the `uses` |
| `action_invalid` | error | the action has no `action.yml` or `action.yaml` in its directory, or its metadata has errors; the message quotes the first one with its line |
| `not_implemented` | error | the action declares `using: composite` or `using: docker`, which arrive in M2 |
| `node_substituted` | warning | the installation's runtime map runs the action on another major version of Node than its `using` names |

The codes of the notices about the forge token:

| Code | Severity | Meaning |
|------|----------|---------|
| `permission_unavailable` | warning | the job's `permissions`, the workflow's or the project's `token_permissions` ask the forge token for a permission that the installation of the GitHub App does not hold, or holds only for reading; the token is minted without it, or with the level the installation holds |
| `token_unavailable` | error | the forge did not mint the job's forge token: the server has no GitHub App, or the forge refused, was rate limited or could not be reached |

The journal is the source for:

- the history panel of the run page: who paused what, who approved with which inputs, which
  conclusion was set by an operator and why;
- the event stream of the API ([11-api.md](11-api.md));
- the outbox that updates statuses on the forge, sends notifications and delivers outgoing
  webhooks, so a side effect is never lost when a replica dies after a commit.

## Triggers between runs

Step 16 of M2 ([../plan.md](../plan.md)) settled how a run that finishes, or starts, creates runs of
workflows that follow it through `on.workflow_run` and `on.upstream`. What they mean, their
payloads and the rules about users and levels are in
[02-workflows.md](02-workflows.md#runs-that-follow-runs); this is how the server does it, so that
no replica creates a run twice and none is lost.

- **The outbox row.** The journal events that a run offers write an `outbox` row of kind
  `run_trigger`, in the transaction of the event, as `journal::append` writes the reports to the
  forge: `attempt.created` offers `requested`, a `run.status` to `in_progress` offers
  `in_progress` and one to `completed` offers `completed` with the run's conclusion. The payload
  is the run, the attempt, the transition and the conclusion, and the collapse key is
  `run_trigger/<run>/<attempt>/<transition>`, so a run that goes `in_progress` again after a
  pause is offered once. A row is written whether or not anything follows the run, so that a
  subscription saved a moment before the run completes is not missed.
- **The worker.** Every replica runs one worker, whether or not it has a GitHub App, woken by
  `bosun_outbox` and otherwise every second. It claims `run_trigger` rows with `SKIP LOCKED` as
  the worker that reports to the forge claims its own kinds, and the two never claim each
  other's. It reads the run. A `requested` row carries `awaiting_admission`, whether the run
  waited for its admission when the row was written, in the transaction of the journal event, and
  such a row is done without a trace however late it is processed; the other transitions are done
  the same way while the run still waits when they are processed. It reads the rows of
  `upstream_subscription` that watch the run's project, matches them in memory, and groups the
  matches by the project of the subscriber. For each project other than the run's own it first
  asks whether the project is in the followers of the run's project, one indexed query, and a
  project that is not costs nothing more and leaves no trace, not even a log line. Only for the
  rest does it read the workflows, log a row it cannot read once for each process, and decide once
  for the project: fire or skip, in a transaction of its own. A row that fails is tried again with the
  backoff of the outbox, and the transactions that committed are not repeated.
- **Exactly once.** Each decision starts by inserting a row into `triggered_run`, whose primary
  key is the run, the attempt, the workflow and the transition. A decision that finds the row
  there already does nothing; one that inserts it goes on, in the same transaction, to create the
  run, or to write the `trigger.skipped` event, and commits both. Two replicas that process one
  row at once therefore meet at the key: the second waits for the first to commit and then finds
  the key. A worker that dies after its commit leaves the outbox row, which another claim processes
  after the lease, and finds every key taken. A run created this way is recorded in its key, with
  the number of levels it is above the first run of its chain.
- **Journal events.** `trigger.skipped` is written to the followed run, once for each project and
  attempt, under the run's lock, which orders the check that none is there yet, and in a
  transaction of its own, never in the transaction that creates a run, so that no transaction
  holds the concurrency lock of one project and waits for that of another.
- **The user.** A run's triggering user is read from its actor when the row is processed, with
  the keys and restrictions of its token; a token that is revoked or has expired by then carries no
  authority, and its run is skipped with `no_permission`. A forge account maps to a Bosun user through
  `linked_user`, which step 20 of M2 fills in; until then it finds none, so a run started by a
  forge event follows no `upstream` of another project.

## Operator commands

A command is a request to change a run: `{ type, target, args, reason }`, where `target` names the
run and, as needed, the attempt, the job and the step. Commands arrive through
`POST /api/v1/runs/{run}/commands` with an `Idempotency-Key`, from the web interface, the CLI, MCP
or a plugin; all four use the same endpoint. The server checks the permission and the state of the
target, applies the effect in one transaction with the event that records it, and returns the
event. A command that is not valid in the current state is refused with the reason and is recorded
as well. Effects on a running job are delivered to its agent as control messages
([05-execution.md](05-execution.md)); the agent confirms them, and the confirmation is recorded on
the control message and becomes a journal event with the kinds later milestones add.

An accepted command answers `201` with its `command.accepted` event, which precedes in the journal
every effect it causes; the effects carry the command's actor, and what the planner does after them
the actor `system`. A refused one answers `409 invalid_state` with the reason, and its
`command.refused` event is committed all the same. A target the command does not take is
`422 invalid_input` naming the field, and is not recorded: a job or a step for `run.cancel`, no job
or a job of another run for `job.cancel`, a step for `job.cancel`, and an attempt other than the
current one. M0 serves `run.cancel` and `job.cancel`, step 29 of M1 ([../plan.md](../plan.md))
`run.admit`; the other commands arrive with the milestones that give them their effects.

`run.admit` is valid for a run from a fork that waits for admission
([08-access.md](08-access.md#code-from-outside-the-team)): untrusted, not admitted and not
completed. It takes no job. Any other run refuses it with ``the run does not wait for admission``,
or ``the run is completed``, followed by ``; `run.admit` needs a run from a fork that waits for
it``. An accepted admission sets the run's `admitted_at` and releases every job that waits with the
reason `untrusted`: it is queued, or waits on with the reason `concurrency` when a
[concurrency group](#concurrency-groups) holds it, each `job.status` event carrying the command's
actor. Until then a member of a
job group that `max-parallel` would hold back waits with `max_parallel` rather than `untrusted`,
and no member is released for room, so the members that wait with `untrusted` are at most
`max-parallel` and the admission queues them all; the others follow as room frees.

A cancellation completes a job that is not on an agent at once, and an assigned one together with
its execution, as `cancelled` with the reason `command`; a job running on an agent is cancelled
there through a `cancel` control message, also sent for an assigned one in case its agent starts it,
and completes when the agent reports it or is lost. An execution is sent one `cancel`, however many
causes ask for it. A job group is cancelled through its members, and completes when they do.
`run.cancel` records the request on the run and cancels every job that is not complete, except a
pending job whose `if` calls a status function, since only such an `if` can admit a cancelled run.
That job is decided when its `needs` are complete, with `cancelled()` true and `success()` and
`failure()` false: it runs when its `if` holds, as `if: always()` and `if: cancelled()` do, and is
cancelled otherwise. Nothing else of the run is queued or started after the cancel.

| Command | Valid when | Effect | Permission |
|---------|-----------|--------|------------|
| `run.cancel` | run not completed | pending, waiting, queued and assigned jobs are cancelled at once, except pending jobs whose `if` admits cancellation, which run when their `needs` are complete; running jobs are cancelled on the agent, where steps whose `if` admits cancellation and post steps still run | `run.cancel` |
| `run.force-cancel` | after `run.cancel` | running jobs are killed without the remaining steps | `run.cancel` |
| `run.pause` | run not completed | no job of the run starts or continues past a step boundary; gates still record approvals but release nothing | `run.control` |
| `run.resume` | run paused | lifts the pause | `run.control` |
| `run.admit` | run from a fork waiting for admission | lets its jobs proceed ([08-access.md](08-access.md)) | `run.override` |
| `run.rerun` | run completed | new attempt re-running every job; `debug` switches on debug logging and hold-on-failure | `run.rerun`, and `job.debug` for `debug` |
| `run.rerun-failed` | run completed | new attempt re-running failed and cancelled jobs and their dependents; `debug` as above | `run.rerun`, and `job.debug` for `debug` |
| `job.cancel` | job not completed | cancels this job only, or every member of a job group; dependents see `cancelled` | `run.cancel` |
| `job.pause` | job not completed | before start: held back from dispatch; running: stops at the next step boundary | `run.control` |
| `job.resume` | job `paused`, or `waiting` with reason `paused` | continues | `run.control` |
| `job.skip` | job not started | completes it as `skipped`, source `operator`; dependents follow their `if` | `run.control` |
| `job.mark-success` | job not started, or held | completes it as `success`, source `operator`, with `outputs` supplied in the command; a held job is torn down first | `run.override` |
| `job.rerun` | run completed | new attempt re-running this job and, unless `dependents: false`, every job that depends on it | `run.rerun` |
| `job.prioritise` | job queued or waiting | changes its priority | `run.control` |
| `job.pin` | job queued | restricts it to one agent that satisfies its labels and pool | `agent.manage` |
| `step.skip` | step pending | the step completes as `skipped`, source `operator` | `run.control` |
| `step.breakpoint` | step pending | sets or clears a pause before or after the step | `run.control` |
| `step.abort` | step running | kills the step's processes; `as` is `failure` (default) or `skipped` | `run.control`; `run.override` for `as: skipped` |
| `hold.retry-step` | job held | re-executes the failed step in the same environment | `job.debug` |
| `hold.continue` | job held | treats the failed step as `skipped` or `success` and continues with the next step | `run.override` |
| `hold.release` | job held | ends the job as failed after its remaining cleanup | `job.debug` |
| `hold.extend` | job held | extends the hold up to the installation cap | `job.debug` |
| `approval.approve` | gate or environment waiting for approval | counts one approval; for a gate, `inputs` supplies its input values | listed approver |
| `approval.reject` | same | completes the job as `failure` | listed approver |
| `approval.bypass` | environment waiting, and its rules allow administrators to bypass | admits the job without approvals | `environment.bypass` |

`job.mark-success` validates the supplied outputs against the job's declared output names. Outputs
of a job marked succeeded are what dependents see in `needs.<job>.outputs`, so an operator can
replace the result of a step that cannot run today (for example, give a deploy job the version an
earlier release already published) without editing the workflow.

The inputs of a run never change after it is created. Values chosen mid-run flow through gate
inputs and through the outputs of `job.mark-success`; anything else is a new run. "Run again with
changes" in the interface creates a new run from an existing one with edited inputs, ref or source
draft, linked to the original by `derived_from`.

A pause takes effect at a step boundary: the agent checks for a pause request and for a breakpoint
before it starts each step and after it finishes each step. A step that is running is never
interrupted by a pause; `step.abort` is the way to interrupt it.

## Re-runs and attempts

A re-run is allowed once the run is completed, and adds attempt N+1. The jobs chosen for re-running
start again from `pending`; every other job of the previous attempt is carried into the new
attempt with its conclusion, outputs, logs and artifacts, by reference. `github.run_attempt` is the
new number; `github.run_number` and `github.run_id` stay. A re-run uses the revision and the IR
snapshot of the run, never the current workflow file, as the target syntax does.

Artifacts of carried jobs belong to the new attempt as well, so a re-run job can download what a
carried job uploaded. A debug re-run sets `RUNNER_DEBUG=1`, `ACTIONS_STEP_DEBUG=true` and
`ACTIONS_RUNNER_DEBUG=true` and switches on hold-on-failure for every job it re-runs.

A re-run requested from the forge (the "re-run" button of a check) is mapped to `job.rerun` or
`run.rerun-failed`, with the forge user as the actor, when that user is linked to a Bosun user with
`run.rerun` on the project.

## Gates and environments

A job is `waiting` for one of these reasons, which the run page shows:

| Reason | Ends when |
|--------|-----------|
| `untrusted` | a maintainer admits the run from a fork with `run.admit` |
| `approval` | the gate or the environment has enough approvals, or a rejection |
| `wait_timer` | the environment's wait timer has run out |
| `policy` | an admission policy plugin admits the job, or denies it |
| `resolving` | the job's remote actions are resolved to commits and allowed by the organisation's action policy, or one is forbidden or does not resolve |
| `concurrency` | the concurrency group has room |
| `max_parallel` | the job group has room |
| `paused` | the job or the run is resumed |

Environments are defined in Bosun, per organisation or per project, and carry protection rules,
secrets and variables. Protection rules: required reviewers (users and teams) with a minimum number
of approvals, prevention of self-review (the actor of the run cannot approve it), a wait timer,
patterns of branches and tags allowed to deploy, and whether administrators may bypass. The branch
pattern is checked first, because it fails at once; the reviewers and the wait timer then run in
parallel.

An approval request is listed in the approval inbox of every eligible reviewer, in-app, and sent
through notifier plugins when they exist ([10-plugins.md](10-plugins.md)). Each approval and
rejection carries the reviewer, an optional comment and, for gates, the inputs; all of it is in the
journal and visible on the run page.

## Holding a failed job

When a step fails, the failure would end the job, and hold-on-failure applies, the agent does not
continue: steps that would run after a failure, including `if: always()` steps and post steps, are
not started yet. The job becomes `held`. The container or the workspace stays as it is, with the
state the failed step left, including the files behind `GITHUB_ENV` and `GITHUB_OUTPUT`.

An operator then may:

- open a terminal inside the job's environment: a shell in the job container for `podman`, or a
  shell in the workspace as the agent's user for `shell`, which the agent allows only when its
  configuration says `allow_terminal = true` ([08-access.md](08-access.md));
- `hold.retry-step`: run the failed step again, for example after fixing something by hand in the
  terminal; a success continues the job as if the step had passed;
- `hold.continue`: treat the failed step as skipped or succeeded and go on;
- `hold.release`: end the job; the remaining steps that run after a failure run now, then the job
  completes as `failure`;
- `hold.extend`: buy time.

The terminal is a pseudo-terminal on the agent, carried over the agent's connection to the server
and over a WebSocket to the browser. One person types; others with `job.debug` can watch. The
output of the session passes through the job's masker before it leaves the agent, and is recorded
in the asciicast format to the object store and linked from the journal; keystrokes are not
recorded, since a person may type a secret.
