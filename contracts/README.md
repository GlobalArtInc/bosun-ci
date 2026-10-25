# contracts

Shapes that code on both sides of a boundary, and programs outside this repository, rely on.

- `fixtures/`: golden fixtures that tests load, from step 2 of M0.
- `fixtures/workflows/`: workflow sources, from step 3 of M0. Each one lowers to the IR of the same
  name in `fixtures/ir/`, or produces exactly the diagnostic of the same name in
  `fixtures/diagnostics/`. From step 4 of M1, a source that lowers with a warning has both: its IR,
  and the warning as its diagnostic. From step 6 of M1, the diagnostic is what
  `bosun_workflow::lint` returns for the source at the path in the diagnostic's `file`, so a case of
  a warning that needs a file under `.github/workflows/` names such a path, and every other case
  `.bosun/workflows/ci.yml`.
- `fixtures/actions/`: the metadata of actions, from step 6 of M1. Each `<case>.yml` is an
  `action.yml`, and `<case>.json` holds the `metadata` it lowers to, or `null`, and its
  `diagnostics`: its warnings, or every diagnostic when one is an error. The cases named after an
  owner, a repository and a major version are that action's `action.yml` at that version, copied
  without its comments; the others pin one rule each.
- `fixtures/steps/`: jobs of `run` steps on the host, from step 12 of M0, each with the conclusion,
  the outcome of every step that starts and the output lines the execution engine must produce.
  They pin the shell rules of the target: the default and the named shells, custom templates,
  `working-directory`, `env` and the order of the output. From step 21 of M1, a step may write
  expressions in `if`, `run`, `env`, `working-directory`, `continue-on-error` and `timeout-minutes`,
  and expects its `outcome`, its `conclusion` when that differs, and the `outputs` it reports; a step
  whose `if` does not hold expects `skipped`, and one without an `outcome` must not be reached. The
  job may have `defaults` for `run`, a `timeout-minutes`, `outputs` with the outputs it must have
  `reported`, and a line of output on which the test cancels it (`cancel-on`). A minute lasts one
  second in these cases. They pin conditions and the status functions, `continue-on-error`, the two
  timeouts and a cancel.
- `fixtures/file-commands/`: cases in the format of `fixtures/steps/`, from step 21 of M1. They pin
  `GITHUB_ENV`, `GITHUB_OUTPUT`, `GITHUB_PATH` and `GITHUB_STATE`, with the multi-line form, and the
  outputs of steps and jobs.
- `fixtures/masking/`: cases in the format of `fixtures/steps/`, from step 22 of M1, whose job may
  have `secrets`, the `secrets` context of its assignment. They pin what the agent masks: a value
  raw, in each encoded form the target masks, in the parts around its first `&` that Bosun adds and
  line by line, the merging of occurrences, a step output reported masked and a job output dropped
  with a line in the log. The URL-encoded and XML-escaped forms and the sections PowerShell prints
  are written out in the cases; the others are produced by the steps.
- `fixtures/hash-files/`: `cases.json`, from step 21 of M1, a list of cases each with a
  `description`, the `patterns` given to `hashFiles`, and either the `files` of `tree/` it hashes, in
  their order, or `fails`. The test copies `tree/` into a workspace and compares the result of each
  case with a hash it computes over those files with `sha256sum`.
- `fixtures/expr/`: expression cases and the availability table of contexts and functions per key,
  from step 2 of M1, and the evaluation and rendering cases with their contexts and results, from
  step 3 of M1, in the format of `docs/design/03-expressions.md`. From step 45 of M1, a case
  supplies arbitrary values as `matrix` or `env`, which a hosted runner can be given, and a case
  that a hosted runner cannot reproduce says so with `"hosted": false`; the others are recorded on
  hosted runners into `compat/recordings/`.
- `fixtures/github/`: the `github` context and the variables of a step, from step 7 of M1. Each case
  holds the event's name, the `payload`, the facts of the run, the runner, the step and Bosun
  (`facts`, `runner`, `step`, `bosun`), the expected `context` without `event`, which is the payload
  itself, and the expected `environment` of the step. A payload Bosun produces also has `produce`,
  what the producer is given, and the produced payload equals `payload`. Payloads are the forge's
  documented webhook examples, the `payload-examples` of `octokit/webhooks`: `push-branch` is the
  example of a push that creates a branch turned into an ordinary push, `push-tag` the example of a
  tag deletion turned into a tag push, and the produced payloads keep the documented objects.
- `fixtures/triggers/`: event matching, from step 5 of M1. Each file is a list of cases, each with a
  `description`, an `on` block written as JSON, an `event` (`name`, `action`, `ref`, `base_ref` and
  `changed_files`, each optional but the name) and whether it `matches`. `bosun-workflow` runs them.
- `fixtures/pushes/`: `cases.json`, from step 27 of M1, what the server makes of a push. Each case
  has a `description`, the repository's `default_branch` and the fields of the `push` payload that
  decide it, and expects where the push's workflows are `read` (`after` or `default_branch`), the
  comparison the server asks the forge for as `compare` (`base` and `head`, or `null`), and, given
  the forge's `comparison` (its `files` and whether it is `truncated`, or `null` when the forge
  cannot compare), the `changes`: `{"listed": [...]}`, the files path filters see, `"unknown"`,
  which meets every path filter and counts every file as changed, or `"untouched"`, for a push that
  path filters do not apply to and that changes no workflow file. The server's tests run them, and
  its end-to-end tests push a tag and create a branch against the fake forge as the cases say.
- `fixtures/runs/`: `filters.json`, from step 38 of M1, the filters of `GET /api/v1/runs`. Its `runs`
  are planted in this order, each with its `name`, the `project` it belongs to as
  `<organisation>/<project>`, the name of its `workflow`, its `event`, `ref`, `status`, `conclusion`
  and `actor`; each of its `cases` has a `description`, a `query` of filters, where a `workflow`
  names a workflow of the runs by its name and the test sends its id, and the names of the `runs`
  the list answers with, newest first. The server's tests plant the runs and send every query.
- `fixtures/matrix/`: the expansion of matrices, from step 16 of M1. Each case has a
  `description`, the `strategy` of a job `test` written as JSON in the order of the workflow, and
  maybe the job's `name` and the `outputs` of a job `setup` that `test` then needs. It expects
  either the `jobs` the strategy expands to, in order, each with its display `name`, its `matrix`
  and its `strategy` context, or the `error` that fails the group: its `code`, `message` and
  `path`. A strategy without a matrix expects the job itself with `matrix` `null`. The server's
  tests dispatch each case and compare the stored jobs.
- `fixtures/names/`: `cases.json`, from step 24 of M1, the names of secrets and variables, each case
  with a `description`, the `name` given, and the name it is `stored` as, or `null` when the name
  breaks the target's rules. The server's tests send every case to `POST /api/v1/secrets` and
  `POST /api/v1/variables`.
- `schema/workflow.json`: the JSON Schema of workflows with Bosun's extensions, from step 6 of M1.
  Every key of the tables of arrivals and of extensions in `docs/design/02-workflows.md` carries
  its milestone as `x-bosun-from`, and every extension `x-bosun-extension: true`.
- `fixtures/api/`: request and response bodies of the REST API. From step 1 of M1, each one belongs
  to a request or a response of an operation in `openapi.json`, or to a schema one of them refers
  to, and validates against that schema; a fixture of a shape no operation serves fails the suite.
- `fixtures/api/run.json` and `fixtures/events/run-notice.json`: from step 15 of M1, the server's
  tests compare the `display_name` and `notices` of `run.json` with the run of
  `fixtures/workflows/template-condition.yml`, and the data of `run-notice.json` with the error
  notice of the run of `fixtures/workflows/timeout-minutes.yml`, both dispatched as the server-side
  workflow `ci`.
- `fixtures/api/run-startup-failure.json`: from step 27 of M1, the server's tests push
  `fixtures/workflows/runs-on-steps-context.yml` as `.bosun/workflows/ci.yml` to the fake forge and
  compare the `display_name`, `event`, `status`, `conclusion`, `actor` and `notices` of the run the
  push yields with this fixture.
- `fixtures/api/me.json`: from step 9 of M1, the server's tests compare its `permissions` with
  those `GET /api/v1/me` lists for a user bound as the fixture describes
  ([../docs/design/11-api.md](../docs/design/11-api.md#permissions)).
- `fixtures/api/` of identity and access: from step 10 of M1, the server's tests send
  `create-user-request.json`, `create-team-request.json`, `add-member-request.json`,
  `create-role-request.json` and `create-role-binding-request.json`, and compare the answers with
  `user.json`, `team.json`, `role.json` and `role-binding.json` but for ids and times; the answer to
  a restricted token's creation has the fields of `personal-token-created.json`, and a listed token
  those of `personal-token.json`.
- `fixtures/api/` of secrets and variables: from step 24 of M1, the server's tests send
  `set-secret-request.json` and `set-variable-request.json`, and compare the answers with
  `secret.json` and `variable.json` but for ids and times.
- `openapi.json`: the description of the REST API generated from the handlers, from step 1 of M1.
  `hack/openapi.sh` writes it, a test fails when it differs from the generated document, and it is
  never edited or merged by hand
  ([../docs/design/11-api.md](../docs/design/11-api.md#the-openapi-document)).

A change to a file here is a change of a contract, and its commit message says so.
