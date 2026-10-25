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
`bosun workflow lint` and on the run page. `bosun_workflow::parse` returns the IR together with the
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

#### Lowering `on`

`on` is an event name, a list of event names, or a mapping from event names to their filters, where
an empty value or `{}` means no filters. `schedule` takes a list of entries and so exists only in
the mapping form. Filters are literal text; no key under `on` takes an expression.

- `branches`, `tags` and `paths` take a pattern or a list of patterns, and each has an `-ignore`
  form. A key together with its `-ignore` form is `invalid_value` at the `-ignore` value, as are an
  empty list, an empty pattern and a pattern the syntax below refuses.
- `types` takes an activity type or a list of them. The names are not checked against the types the
  forge defines: a name the forge never sends never matches. Every event without a row of its own
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
  `actions/checkout` without a ref is refused with a hint. `./path` and `docker://image` are
  `not_implemented` and name M2.

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
  notes the substitution.
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
- An output has `description`. Its `value` is read by composite actions and arrives with them in
  M2.
- `runs` requires `using`. `node12`, `node16`, `node20` and `node24` are compared ignoring case, as
  the runner compares them. `composite` and `docker` are `not_implemented` and name M2, and the
  other keys of `runs` are not checked then; any other value is `invalid_value`. A Node action
  requires `main` and has `pre` and `post`, which are paths in the action's directory, non-empty
  and without expressions. `pre-if` and `post-if` are non-empty conditions in the forms of an `if`;
  without one, the condition is `always()`. As in a step's `if`, a condition that calls no status
  function behaves as `success() && (...)`, and the status functions look at the job, not at the
  action.
- Expressions are checked against the keys `inputs.<input_id>.default`, `runs.pre-if` and
  `runs.post-if` of the availability table ([03-expressions.md](03-expressions.md)). The agent
  evaluates them: the defaults when the step that uses the action is reached, and each condition
  before its pre or post step.

**Unknown keys.** In a workflow an unknown key is an error. The runner of the target reads the
root of an action's metadata and its inputs loosely and ignores keys it does not know, and public
actions rely on it: `marocchino/sticky-pull-request-comment` declares a `github-token` at its root,
`softprops/action-gh-release` an `env`, and `oven-sh/setup-bun` gives an input a `type`. Refusing
them would refuse actions that run on the target, so at the root, in an input and in `branding`,
which the runner does not read either, an unknown key is the warning `ignored_key`, with a
suggestion when a known key is close. In an output and in `runs`, which the runner reads strictly,
an unknown key is `unknown_key`, as in a workflow. The three actions are cases in
`contracts/fixtures/actions/`.

The keys of action metadata arrive by the rule of [Where each key arrives](#where-each-key-arrives),
and the keys of `using: composite` and `using: docker` with those kinds in M2:

| In the metadata | Keys | From |
|-----------------|------|------|
| root | `name`, `description`, `author`, `branding`, `inputs`, `outputs`, `runs` | M1 |
| `branding` | `icon`, `color` | M1 |
| `inputs.<input_id>` | `description`, `required`, `default`, `deprecationMessage` | M1 |
| `outputs.<output_id>` | `description` | M1 |
| `outputs.<output_id>` | `value` | M2 |
| `runs` | `using`, `main`, `pre`, `pre-if`, `post`, `post-if` | M1 |

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
the steps that implement them. A test reads this table, the extensions table and the table of
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
Workflow   { name, run_name: Template?, triggers: [Trigger], permissions: Permissions?,
             env: Map<Template>, defaults: Defaults, concurrency: Concurrency?, cache_mode, priority,
             notify: [Route], jobs: [JobTemplate] }
JobTemplate{ key, name: Template?, needs: [JobKey], if: Condition?, runs_on: Evaluable<RunsOn>,
             permissions: Permissions?, concurrency: Concurrency?, strategy: Evaluable<Strategy>?,
             env: Map<Template>, defaults: Defaults, timeout_minutes: Evaluable<u32>,
             continue_on_error: Evaluable<bool>, outputs: Map<Template>,
             secrets: SecretReferences,
             kind: Steps | Call | Gate, environment: Evaluable<Environment>?, container, services,
             cache_mode, retry, hold_on_failure, cache_volumes, priority, span }
Step       { key: StepKey?, name: Template?, if: Condition?,
             action: Run { script } | Uses(ActionReference),
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
           | WorkflowDispatch { inputs: [Input] } | RepositoryDispatch { types: [String]? }
           | Generic { name, types: [String]? }
Filter     = Include [pattern] | Exclude [pattern]
Cron       { expression, minutes, hours, days_of_month, months, days_of_week: [u8], days: Both | Either }
Input      { name, description: String?, required: bool,
             kind: String | Boolean | Choice { options } | Number | Environment, each with its default? }
ActionMetadata { name: String?, description: String?, author: String?, branding: Branding?,
             inputs: [ActionInput], outputs: [ActionOutput], runs: Runs }
Branding   { icon: String?, color: String? }
ActionInput{ name, description: String?, required: bool, default: Template?,
             deprecation_message: String? }
ActionOutput { name, description: String? }
Runs       = Node { using: Node12 | Node16 | Node20 | Node24, main, pre: String?,
             pre_if: Condition?, post: String?, post_if: Condition? }
```

The fields of later milestones (`cache_mode`, `priority`, `notify`, `environment`, `container`,
`services`, `retry`, `pause`, the hold and the cache volumes, the kinds `Call` and `Gate`, and the
composite and container actions) join
the IR with the milestone that implements them; until then deserialising the IR refuses them like
any unknown field.

`JobKey` and `StepKey` are the ids a workflow gives its jobs and steps (`jobs.<key>`,
`steps.<key>`); the identifiers of the database rows are a separate kind. Jobs and steps keep the
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
  and `runs-on`;
- `uses` is not a valid reference;
- a `workflow_dispatch` or `workflow_call` input has a default that does not fit its type;
- a reusable workflow call passes an input or a secret the callee does not declare.

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
| `limit_exceeded` | error | a limit of the parser, of the installation or of an expression is exceeded |
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
