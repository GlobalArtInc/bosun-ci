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
  imported from it. `fixtures/actions/` holds Bosun's own fixture actions, `first`, `second`,
  `failure` and `runtime` (which prints the runtime variables and the masked token), built with the
  toolkit: their sources are in `src/<action>/`, and
  `corepack pnpm install && corepack pnpm build` in that directory bundles each into
  `<action>/dist/`, which is committed so that tests need no package registry. The tests of the
  runner run them from this directory, the agent's tests from a tarball of it, and the server's
  tests through the fake forge.
- `actions/<owner>/<repo>/<ref>/`: upstream actions vendored for the end-to-end scenarios, from M1
  step 46. Each is `tarball.tar.gz`, the tarball the forge serves for the upstream commit that
  `<ref>` names, and `manifest.json` with that `commit` and the tarball's `sha256`. The fake forge
  serves each as the public repository `<owner>/<repo>` holding the tarball's files at the
  upstream commit, with the tag `<ref>` at it, so the server resolves `<owner>/<repo>@<ref>` to
  the same commit as on GitHub. It checks the tarball's hash against the manifest before it
  serves the action, and refuses to start, naming the action, when they differ.
- `images.lock`: the images the tests use, pinned by digest; see [Images](#images).
- `cases/<name>/`: the end-to-end cases; see [Cases](#cases).

## Vendored actions

`actions/checkout/v4` is `actions/checkout@v4` at its release v4.4.0, the commit
`11d5960a326750d5838078e36cf38b85af677262`. The other vendored actions are
`actions/upload-artifact@v4`, `actions/download-artifact@v4`, `actions/cache@v4`,
`actions/cache@v3.3.3` and `actions/setup-node@v4`, each at the commit its tag points to. Vendoring an
action, or moving one to the commit its ref names now, takes the tarball from the forge and records
its hash:

```sh
commit=$(git ls-remote https://github.com/<owner>/<repo> refs/tags/<ref> | cut -f1)
curl -fsSL -o tarball.tar.gz https://api.github.com/repos/<owner>/<repo>/tarball/$commit
shasum -a 256 tarball.tar.gz
```

For an annotated tag, `git ls-remote` also lists `refs/tags/<ref>^{}`, and the commit is that line.
None of the vendored tags is annotated.

`actions/bosun-fixtures/actions/v1/` is Bosun's own fixture action `runtime`, packed from
`fixtures/actions/runtime/` (its `dist/` is built as above first). The tarball is the pack of that
directory, the same bytes each time (GNU tar headers, mode 0644, owner 0, time 0, no directory
entries, gzip level 9 with time 0), and nobody writes it by hand:

```sh
hack/pack-fixture-action.sh compat/fixtures/actions/runtime compat/actions/bosun-fixtures/actions/v1
```

rewrites `tarball.tar.gz` and the `sha256` of `manifest.json`, whose `commit` it keeps. With
`--check` before the two directories, the script writes nothing and fails when the manifest's hash is
not the tarball's or when the tarball, once decompressed, is not the pack of the directory (the
compressed bytes may differ between zlib builds, the tar inside does not). `hack/e2e.sh` runs the
check before it builds anything, and so does CI.

## Cases

`cases/<name>/` is one end-to-end case. `hack/e2e.sh` runs every case, or one with `--case <name>`,
on the fake forge through the agents of the harness:

- `repository.json`: the commits of the case's repository on `main`, in order, as
  `{"commits": [{"message": "...", "tags": [{"name": "v1", "annotated": true}]}]}`. The tags of a
  commit are annotated when `annotated` is true and lightweight otherwise.
- `commits/NN/`: the files of commit `NN` (counted from `01`), at their paths in the repository,
  `.bosun/workflows/` included. Each is copied over the tree of the commit before it, so a later
  commit may change a file; a commit without a directory is empty.
- `events.json`: the events to deliver, in order. The harness delivers `{"event": "push", "commit": N}`,
  where `N` counts the commits of `repository.json` from 1, and refuses any other event.
- `secrets.json` and `variables.json`, optional: objects of the project's secrets and variables.
- `home/`, optional: a tree copied into the directory of each agent before the case runs.
- `expected.json`: `description`, optionally `podman`, true for a case whose jobs run in containers,
  and `runs`, each with the `name` of its workflow (the run's display
  name), its `conclusion`, `checks`, the names of the check runs of its head commit, which must all be
  completed, and `jobs`. A job has its display `name`, its `conclusion`, the `outputs` it must hold,
  and `logs`, extended regular expressions that one of its log lines must match.

A case whose workflows use an action runs only on Linux, where the server hands Node to agents. A
case with `"podman": true` runs only when `BOSUN_TEST_PODMAN=1`, with the pool `default` allowing
privileged options while it runs, so that its containers can use `--network=host` and reach the
servers on the loopback address of the host; when it has run, the harness checks that no container,
network or volume labelled `bosun.execution` is left and that the agents' directories hold no file
that the agent's user does not own. On other systems, and without `BOSUN_TEST_PODMAN`, the harness
prints that it skipped the case, and a run in which every case is skipped ends with
`scenario cases: skipped`. After each case, and when the harness exits, it removes what a
case wrote under the runner cache, `/opt/actions-runner/cache/*/e2e-*` (a workflow such as Keel's
writes `/opt/actions-runner/cache/<project>/$RUNNER_NAME`, and the harness's agents are named `e2e-*`);
`BOSUN_E2E_RUNNER_CACHE` overrides the cache root. When a case fails, the harness prints, for every
run of the case, `bosun run show` with the notices of the run, and for every job its status,
conclusion and the source of the conclusion, the annotations of its steps and the last 200 lines
of its log, then the last 50 lines of the log of each agent.

## Images

`images.lock` lists one image per line as `name:tag@sha256:<digest>`. The digest is the one of the
image index, which `docker buildx imagetools inspect <name>:<tag> --format '{{.Manifest.Digest}}'`
prints. `hack/warm-images.sh` checks every line before it pulls anything, and refuses a line that is
not pinned by digest. It pulls each image with podman. From `node:24-alpine` it also packs the Node
of the image as `node-v<version>-linux-x64-musl` into `target/node-upstream/v<version>/`, with its
line in `SHASUMS256.txt`. CI runs the script before the tests; the tests never pull.

The two builds meet in `hack/e2e.sh`. When `target/node-upstream/` holds one `v<version>/` with a
musl archive, the harness takes that version for both settings, `BOSUN_TOOLS_NODE20` and
`BOSUN_TOOLS_NODE24`, packs the host's Node as the glibc build under that label (the binary is the
host's, only the version is the pinned one), copies the musl archive next to it and writes both lines
into one `SHASUMS256.txt`. Both builds then come from `file://<the harness's Node directory>` through
`BOSUN_TOOLS_NODE_UPSTREAM` and `BOSUN_TOOLS_NODE_MUSL_UPSTREAM`, so a job on a musl image gets the
version the glibc build has. Two such directories make the harness fail, naming them. Without the
directory, as on a machine without podman, the harness uses the host's version and sets no musl
upstream.

The images of the job containers are two of the lock. `alpine` is the plain Alpine image of the
scenario `job-containers`; its line is the digest of the index of `alpine:3.24` that the registry
answers for the tag, read without pulling (`HEAD /v2/library/alpine/manifests/3.24` with an
`Accept` of `application/vnd.oci.image.index.v1+json` and a pull token), the base that
`node:24-alpine` is built on. The case `job-container` runs in `node:24-alpine` itself instead:
Node of the musl build needs `libstdc++` and `libatomic`, which that image installs and plain Alpine
does not. The harness's agents run with `pull_policy = "never"` when `BOSUN_TEST_PODMAN=1`, so a
reference that the lock does not list fails its job, naming the image, rather than pulling it.

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
