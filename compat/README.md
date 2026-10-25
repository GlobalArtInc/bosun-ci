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
  imported from it. `fixtures/actions/` holds Bosun's own fixture actions, `first`, `second` and
  `failure`, built with the toolkit: their sources are in `src/<action>/`, and
  `corepack pnpm install && corepack pnpm build` in that directory bundles each into
  `<action>/dist/`, which is committed so that tests need no package registry. The tests of the
  runner run them from this directory, the agent's tests from a tarball of it, and the server's
  tests through the fake forge.
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

## Known limitations

What the corpus records as differing from GitHub Actions on purpose, with the reason.

- **A copied forge token expires.** The forge token of a job is an installation token, which lives
  an hour, where the target's lives as long as the job, up to a day. The agent asks for a fresh one
  before a step when less than 45 minutes are left, and the later steps see it in `github.token`,
  `secrets.GITHUB_TOKEN` and the job's `env`
  ([docs/design/06-runtime-services.md](../docs/design/06-runtime-services.md#the-forge-token)).
  A copy the job made elsewhere keeps the earlier token and fails once it expires: the credentials
  `actions/checkout` writes into the repository's `.git/config` (`persist-credentials`), so a
  `git push` of a later step is refused once the token it wrote has expired; a value a step wrote
  to `GITHUB_ENV`; and a single step that runs for longer than the token has left. A workflow that
  pushes late in a long job runs `actions/checkout` again, or passes `secrets.GITHUB_TOKEN` to the
  step that pushes.
