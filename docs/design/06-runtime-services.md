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
workflow from triggering itself.

The forge sits behind a trait in `bosun-forge`: read a file or a directory at a commit, compare two
commits, list the files of a pull request, mint a token, report a status, create a commit and a
pull request. A second forge implements the same trait ([13-decisions.md](13-decisions.md)).

## The `github` context and the environment

The `github` context of a run triggered by the forge is built from the webhook: `github.event` is
the payload as delivered, and the scalar properties are derived from it exactly as the target does
(`ref`, `ref_name`, `ref_type`, `sha`, `head_ref`, `base_ref`, `actor`, `actor_id`,
`triggering_actor`, `repository`, `repository_id`, `repository_owner`, `repository_owner_id`,
`event_name`, `workflow`, `workflow_ref`, `workflow_sha`, `job`, `run_id`, `run_number`,
`run_attempt`, `retention_days`, `server_url`, `api_url`, `graphql_url`, `token`, `workspace`,
`action`, `action_path`, `action_ref`, `action_repository`, `action_status`, and `env` and
`path`, which name the file command files). For a run started in Bosun
(dispatch, schedule, upstream, plugin), the payload is the one the target would produce for that
event, with the actor taken from the linked forge account of the Bosun user when there is one.

Every step receives the environment variables of the target: `CI=true`, `GITHUB_ACTIONS=true`, the
`GITHUB_*` variables that mirror the context (`GITHUB_WORKSPACE`, `GITHUB_REPOSITORY`,
`GITHUB_SHA`, `GITHUB_REF`, `GITHUB_REF_NAME`, `GITHUB_EVENT_NAME`, `GITHUB_EVENT_PATH`,
`GITHUB_RUN_ID`, `GITHUB_RUN_NUMBER`, `GITHUB_RUN_ATTEMPT`, `GITHUB_JOB`, `GITHUB_ACTOR`,
`GITHUB_WORKFLOW`, `GITHUB_WORKFLOW_REF`, `GITHUB_SERVER_URL`, `GITHUB_API_URL`,
`GITHUB_GRAPHQL_URL` and the rest), the file command paths, `RUNNER_OS`, `RUNNER_ARCH`,
`RUNNER_NAME`, `RUNNER_TEMP`, `RUNNER_TOOL_CACHE`, `RUNNER_WORKSPACE`,
`RUNNER_ENVIRONMENT=self-hosted`, `RUNNER_DEBUG` on a debug run, and the runtime service variables
below. Bosun adds `BOSUN_URL`, `BOSUN_RUN_ID`, `BOSUN_RUN_URL`, `BOSUN_JOB_ID` and `BOSUN_DRAFT`.

**Why `GITHUB_SERVER_URL` is the forge and never Bosun.** The toolkit that actions are built on
decides which protocol to speak from the host of `GITHUB_SERVER_URL`: for any host other than
`github.com`, a `*.ghe.com` host or a `*.localhost` host it assumes an enterprise server, and the
artifact client then refuses to work at all, while the cache client falls back to its older
protocol. `actions/checkout` also clones from that
address. So the variable stays the forge's address, and the runtime services are found through
their own variables, which point at Bosun. For a repository-less project the value is
`https://github.com` as well, for the same reason.

## The forge token

For each execution the server mints an installation token of the App, restricted to the run's
repository and to the permissions derived from the workflow's and the job's `permissions`:

- without `permissions`, the project's default set applies (default: `contents: read`,
  `metadata: read`);
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

The step summary (`GITHUB_STEP_SUMMARY`) is uploaded by the agent at the end of each step and
stored with the step. The run page renders the summaries of all steps as sanitised Markdown: no
raw HTML except the tags the target allows, no scripts, images only from `https` addresses.

Annotations from `::notice`, `::warning` and `::error` are stored with the step, shown on the run
page and in the job log at the line that produced them, and forwarded to the forge.

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

When the project switches checks off, the same information goes out as commit statuses with the
context `bosun/<workflow>/<job>`.

Runs started from a draft are not reported unless the person who started them asks for it, so an
experiment does not decorate a pull request. All reports go through the outbox
([07-storage.md](07-storage.md)), are retried with backoff when the forge fails, and collapse into
the latest state when several updates of one job are pending.

A `check_run` webhook with action `rerequested`, and `check_suite` with `rerequested`, are mapped to
re-runs ([04-runs.md](04-runs.md)).
