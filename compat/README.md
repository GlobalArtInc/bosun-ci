# compat

The compatibility corpus of [docs/design/13-decisions.md](../docs/design/13-decisions.md): what the
workflows of the organisations use, and results recorded on GitHub's hosted runners, against which
Bosun is checked.

- `workflows/`: the workflow files of the repositories of the organisations, by repository, as
  `<owner>/<repository>/.github/workflows/<file>`. `hack/compat-sync.sh` writes them and the
  operator commits them.
- `inventory/`: what those workflows use, written by `bosun workflow inventory`.
- `recordings/`: the workflow that records the expression cases on hosted runners, `expr.yml`, and
  what they recorded, `expr.json`.
- `fixtures/`: the fixtures of the tools of this directory: `inventory/`, a tree of workflows with
  its inventory, and `recordings/`, cases with their workflow, a log of its run and the recordings
  imported from it. From M1 step 33, `fixtures/actions/` holds Bosun's own fixture actions, built
  with the toolkit.
- `actions/<owner>/<repo>/<ref>/`: upstream actions vendored for the end-to-end scenarios, each with
  its upstream commit and the SHA-256 of its tarball, from M1 step 46.

## Workflows and their inventory

`hack/compat-sync.sh GlobalArtInc OwnLate` fetches, with the GitHub CLI signed in, the `*.yml` and
`*.yaml` files directly under `.github/workflows/` of every repository of the organisations that is
not archived, at its default branch. It replaces `workflows/<owner>/` as a whole, so a workflow that
was removed upstream leaves the corpus too, and it touches nothing when a request fails.

`bosun workflow inventory compat/workflows` then rewrites `inventory/`:

- `uses.json` lists every `uses:` of a job or a step, what it names and its count, with the count of
  each version, most used first. A version is the ref after `@`, the tag or digest of a
  `docker://` image, or `null` for a local action or workflow.
- `keys.json` lists every key path, named as the availability table of
  [docs/design/03-expressions.md](../docs/design/03-expressions.md) names keys
  (`jobs.<job_id>.steps.uses`), with its count, `from`, the milestone it arrives in according to the
  parser, `null` for a key the parser does not know, and `extension`, which is true for a key that
  is an extension of Bosun and would collide with the target syntax.

The operator commits both together. A test of `bosun-cli` fails when `inventory/` is not the
inventory of `workflows/`, and a test of `bosun-workflow` fails, naming the file and the key, when a
workflow of the corpus has an `unknown_key`.

## Expression recordings

The cases of [contracts/fixtures/expr/](../contracts/fixtures/expr/) that a hosted runner can
reproduce, by the rule of [docs/design/03-expressions.md](../docs/design/03-expressions.md), are
recorded in a public repository whose hosted minutes cost nothing:

1. `hack/compat-recordings.py generate` writes `recordings/expr.yml`. CI runs it with `--check`, so
   the committed workflow is always the one of the committed cases.
2. The operator copies it into `.github/workflows/` of the public repository and runs it there.
3. `gh run view <run> --log > expr.log` saves the log of the whole run, and
   `hack/compat-recordings.py import expr.log` turns it into `recordings/expr.json`. The importer
   refuses a log that lacks a case of this commit or that recorded a case this commit no longer
   holds.
4. The operator commits `recordings/expr.json`. A test of `bosun-expr` evaluates every recording and
   fails, naming the case, when Bosun prints something other than the hosted runner did.

Until the first recorded run, `recordings/expr.json` is an empty list. `hack/test-compat-recordings.sh`
runs the tools on `fixtures/recordings/`, and a test of `bosun-workflow` checks that both recording
workflows validate without diagnostics; CI runs both.
