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

A workflow can be disabled per project. A disabled workflow is still parsed and shown, but no
trigger creates a run of it; a manual dispatch says why it was refused.

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
`bosun workflow lint` and on the run page. A workflow file with errors produces a run that ends at once with
conclusion `startup_failure` when a trigger matches it, so a broken file is visible instead of
silent. Whether it matched is decided from the `on` block alone, which is parsed first; when `on`
itself cannot be parsed, the file produces such a run for every push that changes it.

Default limits, configurable per installation: 1 MiB of source per file, 256 job templates per
workflow, 256 jobs per matrix, 10 levels of nested reusable workflows, 50 distinct reusable
workflows per run, 10 levels of nested composite actions.

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
| `workflow_run` | completion of another workflow of the same project | `workflows`, `types`, `branches`, `branches-ignore` |
| `repository_dispatch` | forge webhook, or `POST /api/v1/projects/{id}/events` | `types` |
| any other webhook event the forge delivers (`release`, `create`, `delete`, `issue_comment`, ...) | forge webhook | `types`, matched against the payload's `action` |

As in the target, events caused by Bosun's own forge token (a push from a job, a comment it posts)
start no runs, except `workflow_dispatch` and `repository_dispatch`, so a workflow cannot trigger
itself in a loop ([06-runtime-services.md](06-runtime-services.md)).

Path filters are evaluated against the list of changed files: for a push, the comparison of
`before` and `after` through the forge API, since the webhook payload truncates its file lists;
for a pull request, the files of the pull request. Glob patterns follow the target syntax,
including negation with `!` and the order-sensitive evaluation of mixed lists.

Pull requests from forks are subject to the project's fork policy before any run is created
([08-access.md](08-access.md)).

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

### Steps

`id`, `name`, `if`, `uses`, `run`, `shell`, `working-directory`, `with` (including `args` and
`entrypoint` for container actions), `env`, `continue-on-error`, `timeout-minutes`.

`uses` accepts `owner/repo@ref`, `owner/repo/path@ref`, `./path` relative to the workspace, and
`docker://image`. The `run`, `shell` and `working-directory` semantics, the default shell and the
shells `bash`, `sh`, `pwsh`, `python` and custom templates with `{0}` are specified in
[05-execution.md](05-execution.md).

### Actions

`action.yml` and `action.yaml` with `name`, `description`, `inputs` (`description`, `required`,
`default`, `deprecationMessage`), `outputs` and `runs`:

- `using: node20` and `using: node24` with `main`, `pre`, `post`, `pre-if`, `post-if`. The
  installation's runtime map decides which Node runs which `using`; by default, as on current
  hosted runners, `node12`, `node16` and `node20` all run on Node 24, and the run page notes the
  substitution.
- `using: composite` with `steps`, where `run` steps require `shell`, and with `outputs` mapped
  from step outputs.
- `using: docker` with `image` as a Dockerfile path or `docker://`, `args`, `env`, `entrypoint`,
  `pre-entrypoint`, `post-entrypoint`.

### Where each key arrives

The surface is implemented milestone by milestone ([../plan.md](../plan.md)). Until its milestone,
a key of the surface is known to the parser and refused with `not_implemented`, whose message names
the milestone, rather than reported as unknown. Values are literals until M1, which brings
expressions. A step's `uses` arrives in M1 for `owner/repo@ref` and `owner/repo/path@ref`, and in M2
for `./path` and `docker://`. The extension keys arrive in the milestone of the From column of
their table below.

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

## Extensions

Bosun adds a closed list of keys. They are part of the published JSON Schema in
`contracts/schema/workflow.json`; anything else unknown is still an error. Extension keys in a file
under `.github/workflows/` produce a warning, because the file becomes invalid for GitHub Actions.

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

`project` (an `org/project` path; the current project when omitted), `workflows` (names),
`branches`, `conclusions` (default `[success]`). It fires when a run of a named workflow completes
with one of the conclusions; `github.event` carries the upstream run's summary. Unlike
`workflow_run`, it crosses projects, which requires the triggering user of the upstream run to have
`run.dispatch` on this project ([08-access.md](08-access.md)).

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
Workflow   { name, run_name: Template?, triggers: [Trigger], permissions, env: Map<Template>,
             defaults, concurrency: Concurrency?, cache_mode, priority, notify: [Route],
             jobs: [JobTemplate] }
JobTemplate{ key, name: Template?, needs: [JobKey], if: Expr?, kind: Steps | Call | Gate,
             runs_on: Evaluable<RunsOn>, environment: Evaluable<Environment>?,
             concurrency, strategy: Evaluable<Strategy>?, container, services, env, defaults,
             timeout_minutes: Evaluable<u32>, continue_on_error: Evaluable<bool>,
             outputs: Map<Template>, cache_mode, retry, hold_on_failure, cache_volumes, priority,
             span }
Step       { key: StepKey?, name: Template?, if: Expr?, action: Run | Uses, shell, working_directory,
             with: Map<Template>, env: Map<Template>, continue_on_error: Evaluable<bool>,
             timeout_minutes: Evaluable<u32>?, retry, pause, span }
```

`JobKey` and `StepKey` are the ids a workflow gives its jobs and steps (`jobs.<key>`,
`steps.<key>`); the identifiers of the database rows are a separate kind. Jobs and steps keep the
order of the source. `Template` is a string with embedded expressions, already parsed; `Expr` is a
bare expression as in `if`; `Evaluable<T>` is a literal value or a template that must evaluate to
`T`. The expression syntax tree is plain data in `bosun-types`, so that the IR can hold it; parsing
and evaluating it is the business of `bosun-expr`. Every node keeps its source span, so an error
found at run time points at the line that caused it.

The IR is serialised as JSON for storage in the run snapshot and for the job assignment sent to an
agent. Its shape is pinned by golden fixtures in `contracts/fixtures/ir/`. Deserialising the IR
refuses unknown fields: an agent that does not know a field must refuse the job rather than run it
as if the field were absent.

## Validation

Validation runs after lowering and before any run is created. Errors:

- the job graph has a cycle, or `needs` names a job that does not exist;
- a job id or step id does not match `^[A-Za-z_][A-Za-z0-9_-]*$`, or a step id repeats;
- an expression does not parse, calls an unknown function or a function with the wrong arity, or
  uses a context that is not available at that key ([03-expressions.md](03-expressions.md));
- mutually exclusive keys appear together: `run` and `uses`, `gate` and `steps`, a reusable call
  and `runs-on`;
- `uses` is not a valid reference;
- a `workflow_dispatch` or `workflow_call` input has a default that does not fit its type;
- a reusable workflow call passes an input or a secret the callee does not declare.

Warnings:

- extension keys in `.github/workflows/`;
- deprecated workflow commands or Node runtimes;
- secrets accessed by a computed name (`secrets[format(...)]`), which forces the job to receive
  every secret in scope instead of only the referenced ones ([08-access.md](08-access.md));
- `pull_request_target` combined with a checkout of the pull request's head, which runs untrusted
  code with secrets.

`bosun workflow lint`, the editor and the server run the same validation from `bosun-workflow`.

### Diagnostic codes

Every diagnostic carries one of these codes, and every code is pinned by a case in
`contracts/fixtures/workflows/`:

| Code | Meaning |
|------|---------|
| `yaml_syntax` | the source is not well-formed YAML |
| `unsupported_yaml` | a merge key, a tag, or more than one document |
| `limit_exceeded` | a limit of the parser or of the installation is exceeded |
| `duplicate_key` | a key appears twice in one mapping, or an event twice in the list form of `on` |
| `invalid_type` | a value of the wrong type, such as a mapping where a string or a list is expected |
| `invalid_value` | a value of the right type that the key does not accept, such as an empty `steps` or a custom shell without `{0}` |
| `missing_key` | a required key is absent; not reported for a mapping in which another key is already at fault |
| `unknown_key` | a key or an event that is neither in the compatibility surface nor an extension; the message suggests the closest known one |
| `not_implemented` | a key, an event or a form of the compatibility surface that a later milestone implements; the message names the milestone |
| `invalid_job_key`, `invalid_step_key` | a job id or a step id that does not match `^[A-Za-z_][A-Za-z0-9_-]*$` |
| `repeated_step_key` | two steps of one job with the same id |
| `unknown_job` | `needs` names a job that does not exist |
| `needs_cycle` | `needs` forms a cycle |
