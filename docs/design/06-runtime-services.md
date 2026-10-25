# Runtime services

This document specifies what a running job sees and calls: the `github` context and the
environment variables, the forge token, the job token, the artifact service, the cache service,
step summaries and annotations, and how results are reported back to the forge. These services
are what makes the public actions work unchanged. The forge integration itself (webhooks, the
application, the clients) is part of this document too, because the forge token and the status
reports are its main uses.

## The forge

The first forge is GitHub, through a GitHub App that the installation owns. The App is installed
on the organisations whose repositories Bosun builds; each installation maps to a Bosun
organisation, and each repository the App can see may be bound to a project.

| App permission | Why |
|----------------|-----|
| Contents: read and write | read workflow files and actions; commits from the editor |
| Pull requests: read and write | changed files for path filters; pull requests from the editor |
| Checks: read and write | one check run per job |
| Commit statuses: read and write | the fallback when checks are switched off |
| Metadata: read | required |
| Deployments: read and write | `forge_deployments` |
| every other repository permission a workflow's `permissions` can name (issues, discussions, actions, attestations, pages, security events, and the rest), read and write | so that the job token can carry what the workflow asks for |

The App holds these permissions only so that a job token can be narrowed down to them; Bosun itself
uses the first group. Commits and pull requests from the editor are made with the user's own
authorisation of the App, never with the installation's, so the forge's own rules about who may
push where still apply ([09-frontend.md](09-frontend.md)).

Subscribed webhook events: `push`, `pull_request`, `pull_request_review`, `create`, `delete`,
`release`, `repository_dispatch`, `merge_group`, `check_run`, `check_suite`, `installation`,
`installation_repositories`, `repository`; further events can be added to the App without a change
in Bosun, since any delivered event can trigger a workflow ([02-workflows.md](02-workflows.md)).

Webhooks arrive at `/hooks/github`. The server verifies `X-Hub-Signature-256`, records the
delivery id so that a redelivery is not processed twice, stores the payload, and acknowledges
before planning, so a slow plan never makes the forge retry. An event whose sender is the App's own
bot account starts no run unless it is `workflow_dispatch` or `repository_dispatch`: those are the
events a job causes with its forge token, and the target ignores them for the same reason, to keep a
workflow from triggering itself. The server compares the id of the payload's `sender` with the id of
the bot, which it reads from the forge once per process.

The forge sits behind the trait `Forge` in `bosun-forge`. In M1 it reads a file and a directory at
a commit with their blob hashes, resolves a ref to a commit, compares two commits and says whether
the forge truncated the list of changed files, lists the files of a pull request, lists the
repositories an installation can see with their default branches, reads the App's own identity
(its slug and the account of its bot), mints an installation token for given repositories and
permissions and revokes a token, creates and updates a check run, creates a commit status, and
downloads a repository's tarball at a commit. It also resolves a ref and downloads a tarball
without credentials, which reaches public repositories only and serves the organisations that have
no installation ([05-execution.md](05-execution.md#actions-and-tools)). Creating a commit and a pull request join
it with the editor in M4. A second forge implements the same trait
([13-decisions.md](13-decisions.md)).

### Deliveries

Step 26 of M1 ([../plan.md](../plan.md)) settled how a delivery is taken and processed.

`POST /hooks/github` answers in this order, and stores nothing unless it answers `2xx`:

1. Without a GitHub App in the configuration ([12-operations.md](12-operations.md#configuration))
   it is `404 not_found`, since there is no secret to check a delivery against.
2. It reads the body, of at most 25 MiB, the forge's own limit, and checks `X-Hub-Signature-256`
   before it looks at anything else of the request: the header must be `sha256=` and 64
   hexadecimal digits, and the digits must be the HMAC-SHA256 of the body under the webhook
   secret, compared in constant time. A delivery without the header, or whose header is not that
   HMAC, is `401 unauthenticated` with a message saying which, and the server logs a warning,
   which is what the alert on webhook verification failures counts
   ([12-operations.md](12-operations.md#observability)).
3. `X-GitHub-Delivery` must be 1 to 64 letters, digits and `-`, `X-GitHub-Event` 1 to 64
   lowercase letters and `_`, and the body a JSON object; otherwise the answer is
   `422 invalid_input`.
4. It inserts the delivery into `webhook_delivery` with its id, its event and its payload, unless
   a row of that delivery id exists, and in the same transaction notifies `bosun_webhook`. A new
   delivery is answered `202` with `{"delivery": "<id>", "stored": true}`, a redelivery `200` with
   `"stored": false`; either way the forge sees it acknowledged, and a redelivery is not processed
   again.

Nothing else happens before the answer, so a slow forge or a slow plan never delays it.

Every replica with a GitHub App runs one worker, woken by `bosun_webhook` and otherwise every
5 seconds. It takes the oldest delivery that is not processed and is due, with
`FOR UPDATE SKIP LOCKED`, in a transaction that holds it until its processing is done, and
processes it inside a savepoint of that transaction:

- when the processing succeeds, its changes and `processed_at` commit together, so a delivery is
  processed exactly once whichever replicas stored and took it, and a replica that dies while it
  processes leaves the delivery to the next worker;
- when it fails for a reason that may pass (the database, the forge unreachable, a `5xx` or a rate
  limit), the savepoint is rolled back and the delivery waits 1 second after its first attempt,
  twice as long after each further one, up to an hour, and at least until a rate limit resets;
  `attempts` and `last_error` record it;
- when it cannot succeed (a payload without the fields its event has, a forge that refuses with
  `4xx`), the savepoint is rolled back and the delivery is marked processed with its
  `last_error`, and a warning names it.

A worker holds one database connection while it processes. Each processed delivery is logged as
`processed a webhook delivery` with its `delivery`, `event` and `duration_ms`.

The worker handles `installation`, `installation_repositories` and `repository` itself, and plans
the runs of every other event as [02-workflows.md](02-workflows.md#repository-workflows) says; the
pull request events and merge groups are processed as nothing until step 29 of M1 plans them.

### Installations and repositories

An installation of the App is a row of `forge_installation`, keyed by the forge's installation id,
with its account's id and login and the organisation it maps to. An installation maps to at most
one organisation and an organisation has at most one installation:

- an `installation` or `installation_repositories` event records the installation with the
  account its payload names. When the installation maps to no organisation, the worker creates the
  organisation whose slug is the account's login in lower case and whose name is the login, and
  maps the installation to it. When an organisation with that slug exists, or the login does not
  make a slug, the installation stays unmapped and a warning says so: an organisation the team made
  by hand is never taken over by whoever installs the App on an account of that name. An owner
  then maps it with `PATCH /organisations/{organisation}` ([11-api.md](11-api.md));
- `installation` with `deleted` removes the row, which unmaps the organisation;
- a mapping, once made, is kept until the installation is removed or an owner changes it.

The repositories an installation can see are rows of `forge_repository`, keyed by the forge's
repository id, with their owner, name and default branch and the installation that sees them,
which is empty when none does. They are not taken from the payloads, which name no default branch
and arrive in no particular order, but read from the forge: `installation` (except `deleted` and
`suspend`), `installation_repositories` and `repository` events list every repository the
installation can see, write each with its current owner, name and default branch, and mark the
installation's other repositories as seen by none. The worker lists them while it holds the
installation's row lock, so the reads of one installation run one after another and the last to
commit is the last to have read the forge; an event that arrives late changes nothing a later one
did. A `repository` event of an installation Bosun does not know changes nothing.

A project names its repository by the repository's id, so it follows a rename or a transfer, and
keeps it when the installation stops seeing it or is removed; it then receives nothing until the
repository is seen again. A repository is bound to at most one project. Rows of `forge_repository`
are never deleted, so a project's repository keeps its last known name.

### The GitHub App client

`GitHubApp` speaks the forge's REST API, version `2022-11-28`, at the configured API address:
`https://api.github.com` for GitHub, an address with a path such as `/api/v3` for an enterprise
server or the fake below.

- **The App's key.** The private key is read as PEM, in the PKCS#1 form the forge hands out or in
  PKCS#8; any other key is refused when the client is built. It stays in the memory of the process
  and only signs. A request that authenticates as the App carries a fresh JWT signed with RS256:
  `iat` 60 seconds in the past against clock drift, `exp` 9 minutes ahead (the forge refuses more
  than 10), and `iss` the App's id.
- **Installation tokens for Bosun's own requests.** A request about a repository carries a token of
  the repository's installation, minted for the whole installation and kept in memory per
  installation. A request that finds less than 5 minutes left on it mints a new one, and concurrent
  requests of one installation wait for the same mint.
- **Tokens for jobs.** `mint_token` always names the repositories and the permissions, and an empty
  list of either is refused before any request, because the forge reads an empty list as everything
  the installation holds. These tokens are not cached; whoever holds one revokes it.
- **The bot.** The App's bot account is `<slug>[bot]`, read without credentials from
  `/users/<login>`, since the forge accepts an App's JWT only on the App's own endpoints.
- **Conditional requests.** Every `GET` remembers the `ETag` of a successful answer, keyed by who
  asked (the App, an installation, or nobody), the media type and the address, and sends it back as
  `If-None-Match`. A `304` serves the remembered body, and the forge does not count it against the
  rate limit. The memory holds the 1024 answers stored last, of at most 1 MiB each; the oldest goes
  first.
- **Rate limits.** A `403` or `429` that the forge marks as a rate limit becomes
  `Error::RateLimited` with the time to wait until: `retry-after` seconds from now when the forge
  sends it, `x-ratelimit-reset` when `x-ratelimit-remaining` is 0, and one minute for a `429` that
  says neither. The client does not wait or retry by itself; its callers do, the outbox among them.
- **Pagination** follows the `next` link of the `Link` header, and only to an address under the
  API's own origin and path, so a token is never sent anywhere else. A pull request's files and the
  repositories of an installation (`/installation/repositories`, with the installation token) are
  read in pages of 100.
- **Files and directories.** The contents endpoint inlines a file up to 1 MiB as base64; above that
  it names the blob without its content, and the client reads the blob by its hash. A directory
  lists each entry's name, kind (file, directory, symlink or submodule) and hash; the forge lists at
  most 1000 entries of a directory, far more than a workflow directory holds. A path where nothing
  is, or where the other kind is, reads as absent.
- **References** resolve through `commits/<ref>` with the media type `application/vnd.github.sha`,
  which takes branch and tag names, full refs and commits and peels tags; `404` and `422` mean that
  no commit has that name.
- **Comparisons.** The forge lists at most 300 changed files and does not say whether it stopped
  there, so a comparison with 300 files counts as truncated, one of exactly 300 changes included.
  What a truncated comparison means for path filters is settled with the planning of pushes
  ([02-workflows.md](02-workflows.md)).
- **Tarballs** are requested with the installation token, or with no token for a public
  repository, and streamed from wherever the forge redirects, without the `Authorization` header
  when that is another origin. Transport errors of a download leave out its address, which may
  carry a short-lived download token.
- **Reports.** A check run is created with its name, head commit, details address and status, and
  updated with the same fields. Bosun's conclusions map one to one onto the forge's: `success`,
  `failure`, `cancelled`, `skipped` and `timed_out`.

### The fake forge

`bosun-testkit` holds `FakeForge`, a local HTTP server on a free port of `127.0.0.1` that tests use
in place of the forge, since no test reaches the forge or the internet. Its REST API is under
`/api/v3`, which keeps the root of its address free for serving git and exercises an API address
with a path. It holds, in memory:

- installations with their account and permissions; repositories of an installation's account,
  each with an id, branches, tags, commits and pull requests between two branches, and private
  unless a test publishes it. A public repository is read like the forge's: without a token, or
  with the live token of any installation, whose rate limit the request then counts against; a
  request without a token for a private repository is answered `404`, as the forge answers it. A commit is the
  full tree of paths and blob hashes, the hashes being git's own, and a commit to a new branch
  starts from the default branch. A test renames a repository, which keeps its id;
- the token endpoints. `/app` and the access token endpoint accept only a JWT that verifies against
  the public key of the App's fixture key and whose claims keep the forge's limits. A minted token
  is recorded with its installation, repositories and permissions; a request for a repository or a
  permission the installation lacks is refused with `422`, as the forge does; `DELETE
  /installation/token` revokes. A token minted without repositories has the selection `all` and
  covers every repository of its installation, those created or renamed after it included, as the
  forge's does. Tokens live an hour unless a test shortens it;
- `/installation/repositories`, the repositories a token covers, in pages of 30 by default and 100
  at most with `Link` headers;
- the repository endpoints the client uses. Each checks that the token is live, covers the
  repository and holds the permission the endpoint needs (`contents`, `pull_requests`, `checks` or
  `statuses`), and refuses with the forge's `401`, `404` and `403`;
- weak `ETag`s and `304` for a matching `If-None-Match`, and a rate limit per installation that a
  test sets, which counts every answer but a `304` and, once spent, refuses with the forge's `403`
  and its headers;
- check runs and commit statuses, recorded for assertions;
- comparisons cut at 300 files, pull request files in pages of 30 by default and 100 at most with
  `Link` headers, and tarballs that redirect to a download path and hold the tree under
  `<owner>-<repo>-<short commit>/`, each download recorded with its repository and commit for
  assertions;
- a sender of webhooks, signed with `X-Hub-Signature-256` under a secret drawn for each fake, with
  `X-GitHub-Event` and a fresh `X-GitHub-Delivery`. Sending the same webhook twice is a
  redelivery. The payloads of `installation`, `installation_repositories` and `repository` events
  are built from the fake's state in the forge's shape, and so are the `repository`,
  `installation` and `sender` of any other event a test sends: a push between two commits, with
  `created` and `deleted` set when either is the commit of zeros, or an event of any name with the
  fields the test gives it. A sender is a login and an id; `BOT` is the App's bot;
- a delay a test sets, by which every answer of the API is held back, so that tests make the
  forge slow;
- a failure a test switches on and off, under which every request is answered `502`, and a log of
  every request it answered, with its method, path, JSON body and status, by which tests show how
  often a report went out.

The App's keys for tests are fixtures in `crates/testkit/keys/`: `app.pem`, in PKCS#1, is the
fake App's key, and `foreign.pem`, in PKCS#8, is a key the fake does not know.

## The `github` context and the environment

The `github` context and the variables of a step are derived by pure functions in the `github`
module of `bosun-workflow`, so the server, the agent and the corpus share one derivation. The cases
in `contracts/fixtures/github/` pin it, one or more per kind of event.

**The context of a job** is built from the event's name, its payload and the facts of the run: the
repository (owner, name and their forge ids), the forge's addresses, the ref and the commit where
the payload does not give them, whether the ref is protected, the workflow (its `name` and its file,
which is the path in the repository or the name of a server-side workflow), the run's id, number
and attempt and the days its logs are kept, the actor (login and forge id) and the triggering actor,
the job's key, and the workspace. `github.event` is the payload as delivered. As in the target,
every other property is a string, numbers included, except `ref_protected`, which is a boolean:

| Property | Value |
|----------|-------|
| `actor`, `actor_id` | the actor's login and forge id; the id is empty for an actor without a forge account |
| `triggering_actor` | the login of whoever started the attempt, which differs from `actor` on a re-run |
| `event_name` | the event's name |
| `ref`, `sha` | by event, in the table below |
| `ref_name` | `ref` without `refs/heads/`, `refs/tags/` or `refs/pull/`, so a pull request's is `<number>/merge` |
| `ref_type` | `branch` for `refs/heads/` and `refs/pull/`, `tag` for `refs/tags/`, otherwise empty |
| `ref_protected` | whether a branch protection or a ruleset covers the ref |
| `head_ref`, `base_ref` | the head and base branches of the pull request for `pull_request` and `pull_request_target`; empty for every other event, a push included even when its payload has a `base_ref` |
| `repository`, `repository_id` | `<owner>/<name>` and the repository's forge id |
| `repository_owner`, `repository_owner_id` | the owner's login and forge id |
| `repositoryUrl` | `git://<host of server_url>/<owner>/<name>.git` |
| `workflow` | the workflow's `name`, or its file when it has none |
| `workflow_ref`, `workflow_sha` | `<owner>/<name>/<file>@<ref>`, and `sha` |
| `run_id`, `run_number`, `run_attempt` | the run's global number, its number among the runs of its workflow, the attempt's number |
| `retention_days` | how many days the run's logs are kept |
| `job` | the job's key |
| `workspace` | the job's working directory on the agent, `GITHUB_WORKSPACE`; empty in the expressions the server evaluates |
| `server_url`, `api_url`, `graphql_url` | the forge's addresses |
| `token` | the forge token, below |

The ref and the commit follow the target's table of events:

| Event | `ref` | `sha` |
|-------|-------|-------|
| `push` | `ref` | `after`; when `deleted` is true, both are the default branch and its last commit, from the facts |
| `create` | `ref`, under `refs/heads/` or `refs/tags/` by `ref_type` | the created ref's last commit, from the facts |
| `pull_request`, `pull_request_review`, `pull_request_review_comment` | `refs/pull/<number>/merge`; `refs/heads/<base>` once merged | the commit of the merge ref, from the facts; `merge_commit_sha` once merged |
| `pull_request_target` | `refs/heads/<base>` | the base branch's last commit, from the facts |
| `merge_group` | `merge_group.head_ref` | `merge_group.head_sha` |
| `workflow_dispatch` | `ref` | the commit of that ref, from the facts |
| `release` | `refs/tags/<release.tag_name>` | the tag's commit, from the facts |
| `deployment`, `deployment_status` | the deployed branch or tag, from the facts; empty for a commit | `deployment.sha` |
| every other event, `schedule` and `repository_dispatch` among them | the default branch, from the facts | its last commit, from the facts |

A value its row takes from the payload comes from the facts when the payload lacks it. For a
deletion, the row follows the target's statement that the commit "and its associated refs" revert to
the default branch.

**The context of a step** adds, on the agent, the properties of one step:

- `action`: the step's `id`, or for a step without one `__run` for a script and `__<owner>_<repo>`
  for an action, with every character other than a letter, a digit, `_` and `-` replaced by `_`,
  and `_2`, `_3` and so on appended to a name already taken in the job, as the target names them;
- `action_repository` and `action_ref`: empty for a script, the action's repository and ref for an
  action, which also gets `action_path`, the directory the action was fetched into;
- `event_path`: `<RUNNER_TEMP>/_github_workflow/event.json`, the file that holds the payload;
- the file command files, fresh for each step under `<RUNNER_TEMP>/_runner_file_commands/`: `env`
  (`set_env_<step>`), `path` (`add_path_<step>`), `output` (`set_output_<step>`), `state`
  (`save_state_<step>`) and `step_summary` (`step_summary_<step>`), where `<step>` is a version 7
  UUID the agent draws for each step, in place of the target's random GUID, since the server creates
  its own rows for the steps only when the agent accepts the job.

Two properties of the target are absent: `secret_source`, which says which of the target's secret
stores a run reads, while Bosun has one; and `action_status`, which exists inside composite actions
and arrives with them in M2.

**The variables of a step** are:

- `CI=true` and `GITHUB_ACTIONS=true`;
- `GITHUB_<PROPERTY>`, the name in upper case, for each property of the step's context in the
  target's list whose value is a string or a boolean (`true` or `false`): `action`, `action_path`,
  `action_ref`, `action_repository`, `actor`, `actor_id`, `api_url`, `base_ref`, `env`,
  `event_name`, `event_path`, `graphql_url`, `head_ref`, `job`, `output`, `path`, `ref`,
  `ref_name`, `ref_protected`, `ref_type`, `repository`, `repository_id`, `repository_owner`,
  `repository_owner_id`, `retention_days`, `run_attempt`, `run_id`, `run_number`, `server_url`,
  `sha`, `state`, `step_summary`, `triggering_actor`, `workflow`, `workflow_ref`, `workflow_sha` and
  `workspace`. An empty property gives an empty variable; `event`, `repositoryUrl` and `token` give
  none;
- `RUNNER_OS`, `RUNNER_ARCH`, `RUNNER_NAME`, `RUNNER_TEMP`, `RUNNER_TOOL_CACHE` and
  `RUNNER_WORKSPACE` from the facts of the runner, `RUNNER_ENVIRONMENT=self-hosted`, and
  `RUNNER_DEBUG=1` on a debug run;
- `BOSUN_URL`, `BOSUN_RUN_ID`, `BOSUN_RUN_URL`, `BOSUN_JOB_ID` and `BOSUN_DRAFT` (`true` or
  `false`);
- the runtime service variables below.

**Payloads Bosun produces.** For a run started in Bosun, the payload has the shape the target gives
the event, with the actor taken from the linked forge account of the Bosun user when there is one:

- `workflow_dispatch`: `inputs`, every value as a string (`true` or `false` for a boolean, a number
  as written), or `null` when the dispatch has no inputs; `ref`; `workflow`, the file; and
  `repository`, `organization` and `sender` as the forge describes them, when Bosun has them. The
  `inputs` context keeps the typed values.
- `schedule`: `{ "schedule": "<cron>" }` and nothing else.
- `repository_dispatch` sent through `POST /api/v1/projects/{project}/events`: `action`, the event
  type; `branch`, the default branch; `client_payload` as sent; and `repository`, `organization`,
  `sender` and `installation` as the forge describes them, when Bosun has them. Bosun keeps no
  description of them from the forge in M1, so they are absent; the `github` context takes the
  repository from the facts.

`upstream` (M2) and trigger plugins (M5) define the payloads of their runs with their triggers.

**A project without a repository** has no repository, ref or commit. Its `repository`,
`repository_id`, `repository_owner`, `repository_owner_id`, `repositoryUrl`, `ref`, `ref_name`,
`ref_type`, `sha`, `workflow_ref` and `workflow_sha` are empty and `ref_protected` is false. Its
dispatch payload has `ref: null`, its repository dispatch payload `branch: null`, and neither has a
`repository`. The values are empty rather than an invented `<org>/<project>`: `actions/checkout`
clones `GITHUB_REPOSITORY` by default and the toolkit's `context.repo` reads it, so an invented name
would clone or call whatever repository of that name exists on the forge, while an empty one fails
at once with the toolkit's own error. The project's path is `bosun.project`
([03-expressions.md](03-expressions.md)). Its forge addresses are the public forge's,
`https://github.com`, `https://api.github.com` and `https://api.github.com/graphql`, for the reason
that follows.

**Why `GITHUB_SERVER_URL` is the forge and never Bosun.** The toolkit that actions are built on
decides which protocol to speak from the host of `GITHUB_SERVER_URL`: for any host other than
`github.com`, a `*.ghe.com` host or a `*.localhost` host it assumes an enterprise server, and the
artifact client then refuses to work at all, while the cache client falls back to its older
protocol. `actions/checkout` also clones from that
address. So the variable stays the forge's address, and the runtime services are found through
their own variables, which point at Bosun. A project without a repository gets
`https://github.com` for the same reason.

## The forge token

For each execution the server mints an installation token of the App, restricted to the run's
repository and to the permissions derived from the workflow's and the job's `permissions`:

- without `permissions`, the project's setting `token_permissions` applies (default: `contents:
  read`, `metadata: read`, [11-api.md](11-api.md#project-settings));
- `permissions: read-all` and `write-all`, and the per-scope form, map to the App permissions of
  the same names, intersected with what the App holds; a scope the App does not hold is named in a
  warning on the run page instead of being dropped silently;
- for a pull request from a fork, the token is read-only whatever the workflow asks.

The token is `secrets.GITHUB_TOKEN` and `github.token`, is masked, and is revoked when the
execution ends. An installation token lives at most one hour, so for an execution that is still
running when the token is about to expire, the agent asks the server for a fresh one and rewrites
the credentials it configured; actions that copied the token earlier see it expire, which is
recorded as a known limitation in the compatibility corpus.

## The job token

The runtime services authenticate the job with a job token: a JWT signed by the server with
Ed25519, given to steps as `ACTIONS_RUNTIME_TOKEN`. Its `scp` claim has the form the toolkit
parses:

```
Actions.GenericRead:<run> Actions.UploadArtifacts:<run>:<job> Actions.Results:<run>:<job>
```

where `<run>` and `<job>` are the identifiers of the attempt and the execution. Because pauses and
holds stretch a job beyond its timeout, the token's expiry is generous (the job's timeout plus
`max_pause`, the hold cap and one hour), and the services also refuse a token whose execution has
completed. The token grants nothing outside the runtime services.

| Variable | Value |
|----------|-------|
| `ACTIONS_RUNTIME_URL` | `<bosun>/_runtime/` |
| `ACTIONS_RESULTS_URL` | `<bosun>/` |
| `ACTIONS_CACHE_URL` | `<bosun>/_runtime/cache/` |
| `ACTIONS_CACHE_SERVICE_V2` | `true` |
| `ACTIONS_RUNTIME_TOKEN` | the job token |

The toolkit's Twirp clients keep only the origin of `ACTIONS_RESULTS_URL` and call
`/twirp/<service>/<method>` on it, so the Twirp services are served at the root of Bosun's public
address, under `/twirp/`.

## Artifacts

The artifact service implements the protocol of the current artifact actions: the Twirp service
`github.actions.results.api.v1.ArtifactService` with `CreateArtifact`, `FinalizeArtifact`,
`ListArtifacts`, `GetSignedArtifactURL` and `DeleteArtifact`.

`CreateArtifact` returns a signed upload address under `/_runtime/blob/`. The client uploads to it
with the block blob protocol of Azure Storage, which is what the toolkit's upload library speaks:
artifacts are always uploaded as staged blocks, `PUT` with `?comp=block&blockid=<id>` for each,
then `PUT ?comp=blocklist` with the list; caches may also use a single `PUT` with
`x-ms-blob-type: BlockBlob`. The endpoint implements exactly these three requests, streams blocks
into the object store, and assembles the object on the block list. `FinalizeArtifact` records the
size and the SHA-256 the client reports. `GetSignedArtifactURL` returns a signed download address
that serves the stored object, with range requests. An artifact is a zip archive unless it was
uploaded with `archive: false`, in which case it is the single file with a content type taken from
its name.

Signed addresses are HMAC-signed by the server, carry an expiry of one hour and the artifact they
are for, and need no other credentials, because the client sends none.

Artifacts belong to the attempt and the job that uploaded them and are visible to every job of the
same attempt, including jobs carried into a later attempt ([04-runs.md](04-runs.md)). Their
retention is `retention-days` when the step gives it, capped by the project's maximum, and the
project's default otherwise ([07-storage.md](07-storage.md)). The run page lists them for download.
Downloading an artifact of another run by its run id goes through the forge's API in the current
actions and is therefore not supported.

## Caches

The cache service implements both protocols the toolkit speaks, so old and new versions of the
cache actions work:

- **The current protocol**: the Twirp service `github.actions.results.api.v1.CacheService` with
  `CreateCacheEntry`, `FinalizeCacheEntryUpload` and `GetCacheEntryDownloadURL`, uploads through
  the same block blob endpoint as artifacts.
- **The earlier protocol** under `ACTIONS_CACHE_URL`: `GET _apis/artifactcache/cache?keys=…&version=…`,
  `POST _apis/artifactcache/caches` to reserve, `PATCH _apis/artifactcache/caches/<id>` with
  `Content-Range` to upload, `POST _apis/artifactcache/caches/<id>` to commit.

Cache semantics follow the target: an entry is identified by key and version; lookup tries the
primary key exactly, then the primary key as a prefix, then each restore key as a prefix, newest
first; an entry is immutable once committed; a branch sees entries of its own ref, then of the
default branch, and a pull request also those of its base branch. Entries are scoped per project.
An entry not read for 7 days is evicted, and when a project's caches exceed 10 GiB the least
recently used entries are evicted first; both limits are project settings.

`cache-mode` (`read`, `write`, `write-only`, `none`) is enforced by the service, not by the client:
a reserve or upload that the job's mode forbids is refused, and a lookup that it forbids finds
nothing. The defaults per trigger, including read-only for low-trust triggers such as
`pull_request_target`, `issue_comment` and `workflow_run`, and the rule about which triggers may
write into the default branch's scope, follow the target's table, held as a fixture. Without this,
a low-trust run could poison the cache a release build restores.

## Step summaries and annotations

The step summary (`GITHUB_STEP_SUMMARY`) travels in the agent's `StepFinished` at the end of each
step, masked and cut at 1 MiB ([05-execution.md](05-execution.md#file-commands-and-workflow-commands)),
and is stored with the step. Step 23 of [../plan.md](../plan.md) put it inline in the report where
the design had an upload that the report referred to: until `bosun-services` arrives in M2 an agent
has no endpoint to upload to, a report that refers to an upload needs the upload to land first and
leaves a dangling object when the report is lost, and at 1 MiB the summary is small beside the log
the same stream already carries. The run page renders the summaries of all steps as sanitised
Markdown: no raw HTML except the tags the target allows, no scripts, images only from `https`
addresses.

Annotations from `::notice`, `::warning` and `::error` and from problem matchers travel in
`StepFinished` too, within the target's limits
([05-execution.md](05-execution.md#file-commands-and-workflow-commands)). They are stored with the
step, shown on the run page and in the job log at the line that produced them, which the agent
writes after `##[notice]`, `##[warning]` or `##[error]`, and forwarded to the forge.

Step 35 of [../plan.md](../plan.md) settled how the server keeps them:

- The replica that holds the agent's stream applies a `StepFinished` as it applies the other
  reports. A non-empty summary goes first to the object store, as `summaries/<execution>/<step>.md`,
  and only for a step of that execution that the agent holds, that is not completed and that has
  no summary yet, so a report the agent sends again stores nothing a second time. The transaction
  that finishes the step then records the object in `step_summary` and the annotations in
  `annotation` ([07-storage.md](07-storage.md#tables)); a report the transaction ignores, because
  the step was completed meanwhile, removes the object it wrote. A summary the store refuses is
  logged at warning level and leaves the step without one, since the agent sends a report once and
  does not send it again on a new stream.
- An annotation of no kind is a breach of the protocol and ends the stream, as a conclusion of
  none does. The server keeps every annotation an agent reports, beyond the 10 of each kind per
  step and 50 per job that the agent keeps, so what the API shows is what the agent sent.
- The API serves them on the steps of an execution and the summary of a step byte for byte
  ([11-api.md](11-api.md#step-results)).

## Reporting to the forge

Every job of a run of a forge-bound project is reported as one check run named as the target names
it (the job's `name`, or its key, with the matrix values in parentheses), so that required checks
of branch protection keep matching by name; only their binding to an App has to change when a
repository moves to Bosun ([12-operations.md](12-operations.md)). The check run carries the
details address on Bosun, its status and conclusion, a summary
built from the step summaries, and its annotations, in batches of 50 because the forge accepts no
more per request. A job group is reported per member. A job completed by an operator is reported
with its conclusion and a summary line naming the operator and the command. A gate or an
environment waiting for approval is reported as `in_progress` with "waiting for approval" in its
title, and its details address opens the approval.

When the project switches checks off, its setting `report` being `statuses`
([11-api.md](11-api.md#project-settings)), the same information goes out as commit statuses with
the context `bosun/<workflow>/<job>`.

Runs started from a draft are not reported unless the person who started them asks for it, so an
experiment does not decorate a pull request. All reports go through the outbox
([07-storage.md](07-storage.md)), are retried with backoff when the forge fails, and collapse into
the latest state when several updates of one job are pending.

Step 28 of M1 ([../plan.md](../plan.md)) settled the reports of M1; summaries and annotations
joined them with step 35, and the summary line of an operator's conclusion and the approvals join
them with M3.

- A run is reported when it has a commit and its project is bound to a repository that an
  installation of the App sees; a run of a server-side workflow planned without a commit is not.
- A job is reported from the moment it leaves `pending`: `waiting`, `queued` and `assigned` as
  `queued`, `in_progress`, `paused` and `held` as `in_progress`, and `completed` with its
  conclusion. A change of state writes a report only when what the forge shows changes, so
  `queued` to `assigned` writes none. The job of a job group, which its members stand for, is not
  reported; a job whose strategy cannot be evaluated, or that is cancelled before it expands, is
  reported under its own name.
- The name is the job's name when the report is written: its `name`, rendered once the job is
  ready when it holds expressions, or its key when it has none. A member of a job group whose
  `name` holds no expression takes that name, or the key, with its matrix values in parentheses
  ([04-runs.md](04-runs.md#matrices)), so `Build` with the values `linux` and `18` is
  `Build (linux, 18)`.
- The first report of a job creates its check run and records the forge's id of it in
  `forge_check_run` ([07-storage.md](07-storage.md#the-outbox)); later reports update that check
  run with the same fields.
- The details address of a job is `<public_url>/runs/<run>/jobs/<job>`, which the interface opens
  as the run page with the job's panel ([09-frontend.md](09-frontend.md#pages)); a server without
  `server.public_url` sends none.
- With `report: statuses`, `<workflow>` in the context `bosun/<workflow>/<job>` is the workflow's
  `name`, or the name of its file without the directory and `.yml` or `.yaml` when it has none, and
  `<job>` is the job's name as above, so a job `build` without a name of the workflow `ci` is
  `bosun/ci/build`. A `queued` or `in_progress` job is `pending`, a conclusion `success` or
  `skipped` is `success`, `failure` and `timed_out` are `failure`, and `cancelled` is `error`, since
  a commit status has no state for it. The description says the same in a word or two (`Queued`,
  `In progress`, `Succeeded`, `Failed`, `Cancelled`, `Skipped`, `Timed out`), and the target address
  is the details address.
- The report of a completed job carries the check run's `output`, built when it is delivered from
  the steps of the job's latest execution, when they wrote a summary or an annotation; the
  reports before it carry none. Its `title` is the check run's name. Its `summary` is the step
  summaries in the order of the steps, each followed by a line break unless it ends with one, or
  `The steps of this job wrote no summary.` when only annotations were written. The forge holds at
  most 65,535 characters there; the server counts bytes, which are never fewer, and cuts a longer
  text at a character so that, with the notice `The summaries of this job are longer than a check
  run holds, so they end here; the job's page on Bosun shows them whole.` after a rule, it holds
  at most that many. It reads no more of the summaries than the cut keeps, and leaves out one
  that is no longer in the store.
- The annotations go in batches of 50, the forge's limit per request: the first with the status
  and the conclusion, each further one in another update of the same check run with the same
  title and summary, since the forge appends the annotations of every request. After each request
  the delivery records in `forge_check_run` how many the check run holds, so a delivery that
  failed halfway and is tried again sends only the rest
  ([07-storage.md](07-storage.md#the-outbox)).
- An annotation goes to the forge in the form its check runs require: `path`, `start_line`,
  `end_line`, `start_column`, `end_column`, `annotation_level`, `message` and `title`. One without
  a file names `.github`, the path the target gives such annotations; a missing or zero line is
  line 1 and a missing end line is the start line; columns go only on an annotation of one line,
  from at least 1; the kinds `notice`, `warning` and `error` are the levels `notice`, `warning`
  and `failure`; and the message is cut at 65,535 bytes and the title at 255 characters, the
  forge's limits, at a character. The client of the forge applies these rules, and the fake forge
  refuses a request that breaks them, as the forge does.
- A run that ends as `startup_failure` has no jobs, so it is reported once, as settled: a check run
  named after its workflow as `<workflow>` above, completed with the conclusion `failure`, whose
  details address is the run page, `<public_url>/runs/<run>`, where its notices say why it could
  not start; with `report: statuses`, the context `bosun/<workflow>` with the state `failure` and
  the description `The workflow could not start`. The forge has no conclusion of a check run for a
  startup failure, and `failure` keeps a required check of that name from passing.

A `check_run` webhook with action `rerequested`, and `check_suite` with `rerequested`, are mapped to
re-runs ([04-runs.md](04-runs.md)).
