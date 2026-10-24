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
  (`/api/v1/projects/globalart/keel`).
- Lists use cursor pagination: `?limit=` (default 50, at most 500) and `?cursor=` from the previous
  page's `next`. Filters are query parameters named after fields.
- Writes that create something accept an `Idempotency-Key` header; a repeated request with the same
  key and body returns the first response.
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
