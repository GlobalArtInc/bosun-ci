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
  `working-directory`, `env` and the order of the output.
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
- `schema/workflow.json`: the JSON Schema of workflows with Bosun's extensions, from step 6 of M1.
  Every key of the tables of arrivals and of extensions in `docs/design/02-workflows.md` carries
  its milestone as `x-bosun-from`, and every extension `x-bosun-extension: true`.
- `fixtures/api/`: request and response bodies of the REST API. From step 1 of M1, each one belongs
  to a request or a response of an operation in `openapi.json`, or to a schema one of them refers
  to, and validates against that schema; a fixture of a shape no operation serves fails the suite.
- `openapi.json`: the description of the REST API generated from the handlers, from step 1 of M1.
  `hack/openapi.sh` writes it, a test fails when it differs from the generated document, and it is
  never edited or merged by hand
  ([../docs/design/11-api.md](../docs/design/11-api.md#the-openapi-document)).

A change to a file here is a change of a contract, and its commit message says so.
