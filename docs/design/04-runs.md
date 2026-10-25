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
  becomes a group whose members are the callee's jobs, keyed `caller/callee`.
- An **execution** is one placement of a job on an agent. A job has more than one execution only
  when job-level `retry` re-queued it.
- A **step** belongs to an execution. Steps of composite actions are nested under the step that
  used the action; pre and post steps of actions are steps of their own. The steps of an execution
  are created `pending` when its agent starts the job, one per step of the job template, each named
  by its `name`, or else by `Run ` and the first line of its script, or by `Run ` and the `uses` of
  a step that uses an action, such as `Run actions/checkout@v4`, as in the target. Until the agent
  renders names (step 21 of [../plan.md](../plan.md)), these names are taken before anything is
  evaluated: an expression in the `name` or in that line is left out, and a `name` that holds
  nothing else counts as none, so `name: Build ${{ matrix.os }}` names the step `Build` and
  `run: echo ${{ github.sha }}` names it `Run echo`.

## States

| Entity | Status | Meaning |
|--------|--------|---------|
| run | `queued` | created, no job has started |
| | `in_progress` | at least one job has started and not all are complete |
| | `waiting` | every remaining job waits for an approval, a timer or a policy |
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
with `startup_failure` when its workflow is invalid. A step has an `outcome` (the result before
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
| `waiting` | `completed` | rejection, gate timeout, branch policy, policy denial, `job.skip`, `job.mark-success`, cancellation |
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
| `queued` | `waiting` | every job that is not complete waits |
| `queued`, `in_progress`, `waiting` | `paused` | `run.pause` |
| `queued`, `in_progress`, `waiting`, `paused` | `completed` | every job is complete, `startup_failure`, cancellation |
| `in_progress` | `waiting` | every job that is not complete waits |
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
`pending`. It evaluates `run-name` and the workflow-level `concurrency` at that moment. A reusable
workflow named by a job is fetched and validated now, so a missing or invalid callee ends the run
with `startup_failure` before anything executes. The planner then runs in the same transaction, so
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
   the concurrency group, and a pause of the run or the job. While any of them holds the job back,
   it is `waiting` with the reason recorded. A branch policy that excludes the ref, or a policy that
   denies, completes the job as `failure` with the reason.
5. Otherwise the job becomes `queued`.

A gate skips steps 2, 3 and the queue: once its admission rules pass, it waits for approvals.

An expression of steps 1 to 3 that fails to evaluate, or whose value the key cannot use, completes
the job as `failure` with the reason `evaluation_failed`, whatever its `continue-on-error`, after an
error notice at the expression's span that names the job. Its dependents then follow their `if`.
The keys are evaluated in the order of the list, and the first error ends the evaluation. The
workflow's `env` is evaluated later, when an agent claims the job
([05-execution.md](05-execution.md#the-job-assignment)); an error there completes the assigned
execution and the job the same way, before the agent is sent anything.

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
  `paused` or `held`, the waiting members are released to `queued` in the order of creation until
  the limit is reached again. A member that becomes ready while the limit is reached goes to
  `waiting` with the reason `max_parallel` after step 3.
- When every member is complete, the group completes with the conclusion above, from `pending`,
  and with the outputs of its members: the members are taken in the order they completed, and a
  value that is not empty replaces what an earlier member gave for that name, so the last member
  that set an output wins, as in the target.

The run completes when every job of the attempt is complete. Its conclusion is the first of these
that holds: `failure` when a job failed or timed out without `continue-on-error`, `cancelled` when
the run or one of its jobs was cancelled, `skipped` when every job was skipped, and `success`
otherwise. A run that accepted `run.cancel` ends `cancelled` unless a job failed, even when its last
running job then finished well before the cancel reached it. A job failed by an evaluation error
has no `continue-on-error`, so it fails the run.

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

Timeouts: `timeout-minutes` of a job (default 360) counts only the time spent `in_progress`, so
pauses and holds do not consume it. The agent enforces it, as it does a step's `timeout-minutes`: a
job that runs out of time concludes `timed_out`, and a step that does concludes `timed_out` and
fails its job unless the step may fail under `continue-on-error`
([05-execution.md](05-execution.md#main-steps)). Paused and held time has its own limit,
`max_pause` (default 24 hours) and the hold duration ([02-workflows.md](02-workflows.md)); when it
runs out, the job is cancelled or released as failed. A paused or held job keeps its agent slot,
and the queue page says which slots are occupied this way.

## The journal

Everything that happens to a run is an event in its journal, the table `run_event`: an
ever-increasing sequence number, the run, the attempt, the job, the execution and the step it
concerns where applicable, the kind, the actor (`user`, `token`, `agent`, `plugin` or `system`,
with its identity), a JSON payload, and the time. The status columns of runs, jobs, executions and
steps are a projection of the journal, written in the same transaction as the event, so reading a
status never requires replaying events and the journal never disagrees with it; a run's
`cancel_requested_at` belongs to the projection too, set by the `command.accepted` of a
`run.cancel`. A property test drives random job graphs with random step results, commands, lost
agents, expired leases, rejected assignments and queue timeouts, and checks after every transaction
that the columns equal a replay of the journal.

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

Later milestones add kinds for approvals, holds, terminals and the rest of the commands' effects.
A status event always names both ends of the transition, so a replay can check that each event
starts where the previous one ended. The `reason` of a status event says what caused a transition
that is not self-explanatory:

| Reason | Meaning |
|--------|---------|
| `if_false` | the `if` evaluated to false, including the implicit `success()` after a failed dependency |
| `evaluation_failed` | an expression the server evaluated for the job failed, or gave a value its key cannot use, such as a matrix that cannot be expanded; the run's notices hold the error |
| `command` | an operator command caused it; the preceding `command.accepted` says which |
| `fail_fast` | another member of the job group failed under `fail-fast` ([Matrices](#matrices)) |
| `lease_expired` | the agent did not confirm the start within the lease |
| `rejected_by_agent` | the agent answered the assignment with a rejection |
| `no_agent` | the job stayed queued longer than `max_queue_time` |
| `agent_lost` | the agent did not come back within the grace period |
| `agent_removed` | the agent was deleted while it held the job: a job it ran fails, and one it had not accepted yet returns to the queue ([05-execution.md](05-execution.md#draining-and-deleting-agents)) |

### Run notices

A run notice tells what the run page shows beside the graph: a problem of the workflow or of the run
that is not a status. Notices are `run.notice` events, so they keep the order of the journal, and
`GET /runs/{run}` lists them ([11-api.md](11-api.md)). A run gains them

- when it is created, from the warnings of its revision, with their codes from
  [02-workflows.md](02-workflows.md) and without a job;
- when the server evaluates an expression for the run or one of its jobs and the evaluation fails,
  as an error at the expression's span;
- from later milestones: the diagnostics of a `startup_failure` (step 27 of [../plan.md](../plan.md)),
  a Node runtime substituted for an action (step 32), and a scope of the forge token that the App
  lacks (step 34).

The codes of runtime notices:

| Code | Severity | Meaning |
|------|----------|---------|
| `function_failed` | error | a function failed while the server evaluated an expression, as [03-expressions.md](03-expressions.md) lists |
| `invalid_type` | error | the value of an expression has a type its key cannot use, such as a string for `timeout-minutes` |
| `invalid_value` | error | the value has the right type but the key cannot use it, such as `timeout-minutes: 1.5`, an empty label, a shell that does not exist or an empty vector of a matrix |
| `limit_exceeded` | error | a matrix expands to more jobs than the installation's limit ([Matrices](#matrices)) |

The journal is the source for:

- the history panel of the run page: who paused what, who approved with which inputs, which
  conclusion was set by an operator and why;
- the event stream of the API ([11-api.md](11-api.md));
- the outbox that updates statuses on the forge, sends notifications and delivers outgoing
  webhooks, so a side effect is never lost when a replica dies after a commit.

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
current one. M0 serves `run.cancel` and `job.cancel`; the other commands arrive with the milestones
that give them their effects.

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
