# API, CLI and MCP

This document specifies the REST API conventions and its resources, the event stream, outgoing
webhooks, the CLI, and the MCP endpoint. One rule holds across all of them: the web interface,
the CLI and MCP are clients of the same API, and none of them can do anything the API cannot.

## Conventions

- Base path `/api/v1`, JSON bodies, UTF-8.
- Authentication by the session cookie with `X-Bosun-CSRF`, or by `Authorization: Bearer bst_…`
  ([08-access.md](08-access.md)). A request to a served path under `/api/v1` without valid
  credentials gets `401 unauthenticated` with `WWW-Authenticate: Bearer`, whether the token is
  missing, malformed, unknown, revoked or expired; the message says which. The scheme is matched
  without regard to case. A path the server does not serve, and a method a served path does not
  accept, are `not_found` with or without credentials.
- Resources are addressed by id; organisations and projects also by slug
  (`/api/v1/projects/globalart/keel`). `{organisation}` in a path is the organisation's id or slug,
  and `{project}` is the project's id or its two slugs, so `/projects/globalart/keel/workflows` and
  `/projects/<id>/workflows` name the same list. A single segment after `/projects/` is always an
  id, and `/projects/<x>/workflows` is the list of project `<x>` when `<x>` is an id and the project
  `workflows` of organisation `<x>` otherwise, which is unambiguous because an organisation's slug
  never reads as an id ([08-access.md](08-access.md)).
- A request body that is not JSON of the shape the endpoint takes is `422 invalid_input`, with the
  parser's message saying where it stopped.
- Lists use cursor pagination: `?limit=` (default 50, at most 500) and `?cursor=` from the previous
  page's `next`, which is `null` on the last page. Filters are query parameters named after fields.
  A limit outside 1 to 500, a malformed cursor, and a query parameter the list does not take are
  `invalid_input`.
- Writes that create something accept an `Idempotency-Key` header; a repeated request with the same
  key and body returns the first response. A key is 1 to 255 visible ASCII characters and belongs
  to the user who sent it. The request it stands for is its method, its path with the query and the
  bytes of its body; the same key with another request within 24 hours is `409 conflict`. The
  response is kept in the transaction of the write, and only when the write created something, so
  a request that failed can be repeated with its key, and requests that race with one key create
  one resource and all get its response.
- Updates of documents that people edit concurrently (drafts, settings, environments) require
  `If-Match` with the version from `ETag`, and fail with `412` when it moved.
- Errors share one shape, with a stable `code`, the fields at fault for invalid input, and the
  diagnostics ([02-workflows.md](02-workflows.md)) when the input was a workflow. Both lists are
  always present, empty when they do not apply:

```json
{
  "error": {
    "code": "invalid_state",
    "message": "job `deploy` is completed; `job.pause` needs a job that is not completed",
    "fields": [],
    "diagnostics": []
  }
}
```

  An entry of `fields` is `{ "field": "inputs.env", "message": "…" }`, where `field` is the dotted
  path of the field in the request body.

  A path the server does not serve, and a method a served path does not accept, are both
  `not_found` with the message `there is no <METHOD> <path>`: the table below has no code for
  `405`, and to a client both mean that the endpoint it asked for does not exist.

- Optional fields of responses are present with `null` rather than left out, so a client sees the
  whole shape of a resource in every response.

| Code | Status |
|------|--------|
| `unauthenticated` | 401 |
| `forbidden` | 403 |
| `not_found` | 404 |
| `conflict`, `invalid_state` | 409 |
| `precondition_failed` | 412 |
| `invalid_input` | 422 |
| `rate_limited` | 429 |
| `internal` | 500 |

The OpenAPI document is generated from the handlers and committed as `contracts/openapi.json`; CI
fails when the generated document differs from the committed one.

## Resources

| Path | Methods | Purpose |
|------|---------|---------|
| `/me` | GET | the current user as `{"user": …}`; its permissions and the CSRF token from M1 |
| `/auth/login`, `/auth/logout`, `/auth/totp` | POST | local sign-in, sign-out, second factor |
| `/auth/oidc/{provider}/start`, `/auth/oidc/{provider}/callback` | GET | OpenID Connect sign-in |
| `/auth/forge/link`, `/auth/forge/callback` | GET | linking the user's forge account |
| `/organisations`, `/organisations/{org}` | GET, POST, PATCH | organisations and their settings, among them the action policy |
| `/label-aliases` | GET, POST, PATCH, DELETE | label aliases, per installation or organisation |
| `/projects`, `/projects/{project}` | GET, POST, PATCH, DELETE | projects and their settings |
| `/projects/{project}/workflows` | GET, POST | workflows; POST creates a server-side one |
| `/workflows/{workflow}` | GET, PATCH | workflow details, enable and disable |
| `/workflows/{workflow}/source` | PUT | new source of a server-side workflow, stored as a revision when it validates |
| `/workflows/{workflow}/revisions` | GET | revisions with their source |
| `/workflows/{workflow}/drafts`, `/drafts/{draft}` | GET, POST, PUT, DELETE | drafts |
| `/drafts/{draft}/run`, `/drafts/{draft}/save` | POST | run a draft; commit it or open a pull request |
| `/workflows/{workflow}/dispatch` | POST | start a run with inputs and a ref |
| `/projects/{project}/events` | POST | a `repository_dispatch` style event |
| `/lint` | POST | validate a workflow source and return diagnostics |
| `/actions/metadata?uses=` | GET | inputs and outputs of an action |
| `/runs`, `/runs/{run}` | GET | runs with filters; one run with its attempts |
| `/runs/{run}/attempts/{n}/jobs` | GET | jobs of an attempt with their groups |
| `/runs/{run}/journal` | GET | the run's journal |
| `/runs/{run}/commands` | POST | operator commands ([04-runs.md](04-runs.md)) |
| `/runs/{run}/derive` | POST | "run again with changes" |
| `/jobs/{job}`, `/executions/{execution}` | GET | a job; one execution with its steps |
| `/executions/{execution}/steps/{step}/log` | GET | a step's log: range, search, or `?follow=true` as an event stream |
| `/executions/{execution}/terminal` | GET (WebSocket) | the debug terminal |
| `/runs/{run}/artifacts`, `/artifacts/{artifact}` | GET, DELETE | artifacts and their download |
| `/approvals` | GET | the caller's approval inbox |
| `/queue` | GET | queued and waiting jobs with their reasons |
| `/pools`, `/agents`, `/registration-tokens` | GET, POST, PATCH, DELETE | pools, agents (drain, labels), registration tokens |
| `/agents/register` | POST | agent registration with a `brt_` token |
| `/environments`, `/secrets`, `/variables` | GET, POST, PUT, DELETE | scoped by `?organisation=` or `?project=`; secrets are write-only |
| `/plugins`, `/plugin-instances` | GET, POST, PATCH, DELETE | plugins and instances |
| `/users`, `/teams`, `/roles`, `/role-bindings`, `/tokens` | GET, POST, PATCH, DELETE | identity and access |
| `/webhook-subscriptions` | GET, POST, PATCH, DELETE | outgoing webhooks |
| `/audit` | GET | the audit trail |

Outside `/api/v1`: `/hooks/github` and `/hooks/plugins/{instance}` for incoming webhooks,
`/twirp/` and `/_runtime/` for the services jobs call ([06-runtime-services.md](06-runtime-services.md)), `/mcp`,
`/healthz`, `/readyz`, `/metrics`, and the gRPC services `bosun.agent.v1.Agent` and
`bosun.plugin.v1.*` on the same listener.

### Projects and server-side workflows

- `POST /organisations` takes `slug` and `name`; `POST /projects` takes `organisation` (an id or a
  slug), `slug` and `name`. Both answer `201` with the resource, a slug that is taken is
  `409 conflict`, and a slug or a name that breaks its rule ([08-access.md](08-access.md)) is
  `invalid_input` naming the field.
- `POST /projects/{project}/workflows` takes `name` and `source` and creates a server-side workflow
  with its first revision; `GET` lists the project's workflows oldest first and filters by `name`.
  `PUT /workflows/{workflow}/source` takes `source` and answers with the revision: `201` when the
  source is new to the workflow, `200` when it is the source of a revision the workflow has, which
  becomes current again. A source with errors is `422 invalid_input` with the field `source`, the
  message `the workflow is not valid` and the diagnostics, and stores nothing
  ([02-workflows.md](02-workflows.md)). `GET /workflows/{workflow}/revisions` lists newest first.
- `POST /workflows/{workflow}/dispatch` takes `ref` and `inputs` and answers `201` with the run,
  `queued`, whose actor is the token of the request. In the same transaction it creates attempt 1,
  one `pending` job per job template and their journal events, stores the IR of the current
  revision as lowered by this server, and plans the run, so the jobs that need no other job are
  `queued` when the response leaves ([04-runs.md](04-runs.md)). A project without a repository has
  no refs, so `ref` must be `null`; until `workflow_dispatch` inputs arrive in M1, every input is an
  `invalid_input` naming `inputs.<name>`. A disabled workflow, and a current revision that this
  server's parser refuses, are `409 invalid_state`, the latter with its diagnostics. So is, until
  the planner applies it, a revision that uses a key the planner would otherwise ignore, so that a
  run never quietly differs from its workflow: a job's `if`, an expression in `runs-on` and a job's
  `continue-on-error` until step 15 of [../plan.md](../plan.md), `strategy` until step 16,
  `concurrency` until step 17 and a step that uses an action until step 33, because the assignment
  carries no such step before. The message names the key and the step that brings it.

### Commands

- `POST /runs/{run}/commands` takes a command, `{ "type", "target", "reason" }` with
  `"target": { "attempt", "job", "step" }` (each `null` when it does not apply) and, for commands
  that take them, `"args"`; M0 takes `run.cancel` and `job.cancel`
  ([04-runs.md](04-runs.md#operator-commands)). An accepted command answers `201` with its
  `command.accepted` event. A command the state of its target refuses answers `409 invalid_state`
  with the reason, such as ``the run is completed; `run.cancel` needs a run that is not completed``,
  and is recorded as `command.refused`. A type the endpoint does not take, and a target the command
  does not take, are `422 invalid_input`, the latter naming `target.job`, `target.step` or
  `target.attempt`, and neither is recorded. `{run}` is a run's id; any other segment, and a run
  that does not exist, are `404 not_found`. A repeated `Idempotency-Key` returns the first
  `command.accepted`; a refusal is not kept for its key, so repeating it records another one.

### Logs

- `GET /executions/{execution}/steps/{step}/log` serves the log of one step
  ([07-storage.md](07-storage.md#logs)). `{step}` is the id of a step of that execution; any other
  segment, a step of another execution and an execution that does not exist are `404 not_found`
  with the message ``there is no step `<step>` in execution `<execution>` ``.
- Without `follow`, the answer is `text/plain; charset=utf-8` with the lines as the agent sent them,
  each its RFC 3339 time, a space, the text and a newline. `offset` skips that many lines (default
  0) and `limit` caps the number of lines (at least 1; without it the answer runs to the end). The
  log of a running step holds the lines stored so far; a completed step whose output passed the
  limit ends with its notice. A failure after the first line ends the body early.
- With `?follow=true` the answer is a server-sent event stream with one event per line, whose id
  is the line's number, counted from 1, and whose data is `{"number": 12, "line": "<time> <text>"}`,
  with bytes that are not UTF-8 replaced by U+FFFD. `Last-Event-ID` resumes after that line in place
  of `offset`, so a client that reconnects receives every line exactly once. Once the step is
  completed and every line was sent, an event `end` with `{"lines": <count>}` closes the stream. The
  server sends a comment every 15 seconds while the step is quiet, and ends the stream without
  `end` when it shuts down, so that the client reconnects to another replica. `limit` does not
  apply to a followed log.
- Any other query parameter, a value that is not a number of lines or a boolean, and a
  `Last-Event-ID` that is not a line number are `422 invalid_input`.

## The event stream

`GET /api/v1/events` is a server-sent event stream filtered by `?project=`, `?run=`, `?kind=`.
Each event has an id that is the journal's sequence number for run events and a separate
sequence for the others (agents, queue, approvals), encoded so that `Last-Event-ID` resumes both.
Event payloads are the same objects the REST resources return, so a client can apply them without
a second request.

## Outgoing webhooks

A subscription has an address, a secret, a filter (projects, event kinds) and an active flag. Each
delivery is a `POST` with the event as JSON, `X-Bosun-Event`, `X-Bosun-Delivery` and
`X-Bosun-Signature-256` (HMAC-SHA256 of the body with the secret). Deliveries go through the
outbox ([07-storage.md](07-storage.md)), with its retries and dead-lettering; the subscription page
lists the latest deliveries with their responses.

## The CLI

`bosun` keeps profiles in `~/.config/bosun/config.toml` (server, token in the system keyring, or
in the file when no keyring exists) and prints tables by default and JSON with `--json`.

```
bosun login [--server <url>]
bosun project list | show <org/project> | settings <org/project> [--set key=value]...
bosun workflow list <project> | show <workflow> | enable | disable
bosun workflow push <project> <file> [--name <name>]
bosun workflow lint [<file>...]
bosun workflow dispatch <workflow> [--ref <ref>] [--input k=v]... [--watch]
bosun exec [<workflow>] [--job <key>] [--event <name>] [--input k=v]... [--secrets <file>]
bosun run list [--project <p>] [--status <s>] | show <run> | watch <run> | journal <run>
bosun run cancel [--force] | pause | resume | admit | rerun [--failed] [--debug] <run>
bosun job logs <job> [--step <n>] [--follow] [--grep <pattern>]
bosun job cancel | pause | resume | skip | rerun [--no-dependents] <job>
bosun job mark-success <job> [--output k=v]... --reason <text>
bosun job prioritise <job> <low|normal|high> | pin <job> <agent>
bosun job terminal <job>
bosun step skip | breakpoint [--after] [--clear] | abort [--as skipped] <job> <step>
bosun hold retry-step | continue [--as success] | release | extend <job> [<duration>]
bosun approval list | approve <id> [--input k=v]... | reject <id> --comment <text> | bypass <id>
bosun agent list | drain <agent> | delete <agent>
bosun pool list | token create <pool> [--expires <d>] [--reusable]
bosun alias set <label> [--pool <p>] [--capability <c>] [--image <i>] [--organisation <o>]
bosun secret set <name> [--project <p> | --organisation <o> | --environment <e>] < value
bosun var set <name> <value> [scope]
bosun plugin install <oci-ref> | list | instance create <plugin> --name <n> --config <file>
bosun token create --name <n> --scope <perm>... --expires <d>
```

## MCP

`/mcp` serves the Model Context Protocol over streamable HTTP, authenticated with a `bst_` token;
every tool checks the same permissions as the API. Tools that change something say so in their
descriptions.

| Tool | Does |
|------|------|
| `list_projects`, `list_workflows` | discovery |
| `lint_workflow` | diagnostics for a source text |
| `dispatch_workflow` | start a run |
| `list_runs`, `get_run` | runs, their jobs and statuses |
| `get_job_log` | a step's or a job's log, with `tail` and `grep` so that a model reads only what it needs |
| `run_command` | any operator command, with the command's own validation |
| `list_approvals`, `decide_approval` | the approval inbox |
| `get_queue`, `list_agents` | why something is waiting, and on what |
| `get_annotations` | errors and warnings of a run, with file and line |
