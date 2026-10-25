# API, CLI and MCP

This document specifies the REST API conventions and its resources, the event stream, outgoing
webhooks, the CLI, and the MCP endpoint. One rule holds across all of them: the web interface,
the CLI and MCP are clients of the same API, and none of them can do anything the API cannot.

## Conventions

- Base path `/api/v1`, JSON bodies, UTF-8.
- Authentication by the session cookie with `X-Bosun-CSRF`, or by `Authorization: Bearer bst_…`
  ([08-access.md](08-access.md)). A request to a served path under `/api/v1` without valid
  credentials gets `401 unauthenticated` with `WWW-Authenticate: Bearer`, whether the token or the
  session is missing, malformed, unknown, revoked, ended or expired; the message says which. The
  scheme is matched without regard to case. A request with an `Authorization` header is
  authenticated by it alone, and its cookie is not read. A path the server does not serve, and a method a served path does not
  accept, are `not_found` with or without credentials.
- Resources are addressed by id; organisations and projects also by slug
  (`/api/v1/projects/globalart/keel`). `{organisation}` in a path is the organisation's id or slug,
  and `{project}` is the project's id or its two slugs, so `/projects/globalart/keel/workflows` and
  `/projects/<id>/workflows` name the same list. A single segment after `/projects/` is always an
  id, and `/projects/<x>/workflows` is the list of project `<x>` when `<x>` is an id and the project
  `workflows` of organisation `<x>` otherwise, which is unambiguous because an organisation's slug
  never reads as an id ([08-access.md](08-access.md)); `/projects/<x>/settings` is read the same
  way.
- A request body that is not JSON of the shape the endpoint takes is `422 invalid_input`, with the
  parser's message saying where it stopped.
- Lists use cursor pagination: `?limit=` (default 50, at most 500) and `?cursor=` from the previous
  page's `next`, which is `null` on the last page. Filters are query parameters named after fields.
  A limit outside 1 to 500, a malformed cursor, and a query parameter the list does not take are
  `invalid_input`. A list of the items of one organisation (teams, roles, role bindings) requires
  the filter `organisation`, the organisation's id or slug, and is `invalid_input` naming
  `organisation` without it.
- Writes that create something accept an `Idempotency-Key` header; a repeated request with the same
  key and body returns the first response. The four whose answer holds a secret that is shown
  once, `POST /registration-tokens`, `POST /agents/register`, `POST /auth/login` and
  `POST /tokens`, take no key, since keeping their answer would show it again, and neither do
  `POST /label-aliases` and `POST /teams/{team}/members`, which set an alias or a membership rather
  than add one, so that repeating them changes nothing. A key is 1 to 255 visible ASCII characters and belongs to the user who sent
  it. The request it stands for is its method, its path with the query and the bytes of its body;
  the same key with another request within 24 hours is `409 conflict`. The response is kept in the
  transaction of the write, and only when the write created something, so a request that failed can
  be repeated with its key, and requests that race with one key create one resource and all get its
  response.
- Updates of documents that people edit concurrently (drafts, settings, environments) require
  `If-Match` with the version from `ETag`, and fail with `412` when it moved. Step 25 of M1
  ([../plan.md](../plan.md)) settled the rules with the first such document, the project settings:
  - The `ETag` of a document is its version, a number that starts at 1 and grows with each change,
    written as a strong entity tag, `"3"`. `GET` and every update answer with it.
  - An update matches `If-Match` against the current version with the strong comparison of
    RFC 9110: one entity tag of its list must be the current `ETag`, and a weak tag (`W/"3"`)
    matches nothing. `If-Match: *` names no version and is refused like a stale tag, since a change
    must say which version it was made from.
  - A missing or stale `If-Match` is `412 precondition_failed`; the message of a stale one names
    the tag it got and the current one. The precondition is evaluated after the permission and before the
    body, as RFC 9110 orders it, so a stale change learns that it is stale before anything about
    its body, and nothing is stored.
  - The check and the write are one conditional update of the row, so of two changes made from one
    version exactly one is stored and the other is `412`.
  - An update whose values are the current ones stores nothing and answers with the current
    version, so it does not make the copies of other editors stale.
  `If-Match` with the version from `ETag`, and fail with `412` when it moved.
- A `PATCH` takes the fields it changes. A field the body leaves out keeps its value, `null`
  clears a field that may be empty, and a field the resource does not have is `invalid_input`; an
  empty body changes nothing and answers with the resource. Its request schema lists no field as
  required ([The OpenAPI document](#the-openapi-document)).
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
  `405`, and to a client both mean that the endpoint it asked for does not exist. The one exception
  is a `GET` or `HEAD` outside `/api`, `/hooks`, `/_runtime`, `/twirp`, `/mcp` and `/assets`, which
  is a route of the web interface and is answered with its page
  ([09-frontend.md](09-frontend.md#serving)).

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
fails when the generated document differs from the committed one ([below](#the-openapi-document)).

## Resources

| Path | Methods | Purpose |
|------|---------|---------|
| `/me` | GET | the caller as `{"user", "csrf_token", "password_change_required", "permissions"}` |
| `/auth/login`, `/auth/logout`, `/auth/password`, `/auth/totp` | POST | local sign-in, sign-out, change of one's own password, second factor |
| `/auth/methods` | GET | the sign-in methods the sign-in page offers |
| `/auth/oidc/{provider}/start`, `/auth/oidc/{provider}/callback` | GET | OpenID Connect sign-in |
| `/auth/forge/link`, `/auth/forge/callback` | GET | linking the user's forge account |
| `/organisations`, `/organisations/{org}` | GET, POST, PATCH | organisations and their settings, among them the action policy |
| `/label-aliases` | GET, POST, PATCH, DELETE | label aliases, per installation or organisation |
| `/projects`, `/projects/{project}` | GET, POST, PATCH, DELETE | projects |
| `/projects/{project}/settings` | GET, PATCH | the project's settings, one document with a version |
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
| `/events` | GET | the event stream ([below](#the-event-stream)) |
| `/pools`, `/agents`, `/registration-tokens` | GET, POST, PATCH, DELETE | pools, agents (drain, labels), registration tokens |
| `/agents/register` | POST | agent registration with a `brt_` token |
| `/environments`, `/secrets`, `/variables` | GET, POST, PUT, DELETE | scoped by `?organisation=` or `?project=`; secrets are write-only |
| `/plugins`, `/plugin-instances` | GET, POST, PATCH, DELETE | plugins and instances |
| `/users`, `/teams`, `/teams/{team}/members`, `/roles`, `/role-bindings`, `/tokens` | GET, POST, PATCH, DELETE | identity and access ([below](#identity-and-access)) |
| `/webhook-subscriptions` | GET, POST, PATCH, DELETE | outgoing webhooks |
| `/audit` | GET | the audit trail |

Outside `/api/v1`: `/hooks/github` ([06-runtime-services.md](06-runtime-services.md#deliveries))
and `/hooks/plugins/{instance}` for incoming webhooks,
`/twirp/` and `/_runtime/` for the services jobs call ([06-runtime-services.md](06-runtime-services.md)), `/mcp`,
`/healthz`, `/readyz`, `/metrics`, `/install-agent.sh` and the agent binaries at
`/_runtime/agent/linux-<arch>` without a token ([12-operations.md](12-operations.md#agents)), and
the gRPC services `bosun.agent.v1.Agent` and `bosun.plugin.v1.*` on the same listener. Every other
path belongs to the web interface ([09-frontend.md](09-frontend.md#serving)).

### Sign-in and sessions

The mechanisms behind these endpoints are in [08-access.md](08-access.md#local-sign-in).

- `GET /me` answers `{"user", "csrf_token", "password_change_required", "permissions"}`: the
  caller, the CSRF token of its session or `null` for a bearer token, whether its password must
  change before its sessions may do anything else, and the permissions it holds
  ([Permissions](#permissions)).
- `POST /auth/login` takes `{"login", "password"}` with the content type `application/json`, and
  needs no credentials. It answers `200` with the caller as `GET /me` answers it for the new
  session, and sets the session cookie. A wrong login or password is `401 unauthenticated`, with
  the message ``the login or the password is not right``, and a sign-in during a lockout is `429
  rate_limited` with `Retry-After`. A body of another content type is `422 invalid_input`. It takes
  no `Idempotency-Key`, since its answer holds a credential that is shown once.
- `POST /auth/logout` ends the session of the request and answers `204` with the cookie emptied. A
  request with a personal token is `409 invalid_state`, since a token has nothing to sign out.
- `POST /auth/password` takes `{"current_password", "new_password"}`, with the content type
  `application/json`, and answers `200` as `GET /me` does. A new password that breaks the rule or
  equals the current one, and a wrong current password, are `422 invalid_input` naming
  `new_password` or `current_password`; a wrong current password also counts as a failed sign-in,
  and so may be `429 rate_limited`. A user without a password is `409 invalid_state`.
- `GET /auth/methods` needs no credentials and answers `{"password": true, "providers": […]}`:
  whether password sign-in is offered, and the configured identity providers, each as `{"name"}`.
  The list stays empty until OpenID Connect arrives with step 11 of M1.
- While the caller's password must change, a session reaches only `GET /me`, `POST /auth/logout`
  and `POST /auth/password`; every other operation is `403 forbidden`.

### Permissions

Step 9 of M1 ([../plan.md](../plan.md)) settled how the API applies the permissions and roles of
[08-access.md](08-access.md#roles-and-bindings).

- **`permissions` in `/me`** is `{"installation", "organisations", "projects"}`. `installation` lists
  the permissions held on the whole installation, which are every permission for an owner and none
  for anyone else. `organisations` maps the id of each organisation on which a binding gives the
  caller a permission to the permissions those bindings give, and `projects` does the same for the
  bindings on a project. Each list is ordered as the permission table of
  [08-access.md](08-access.md#permissions-and-roles) and holds no key twice. The permissions on an
  organisation are those of `installation` together with its entry; the permissions on a project are
  those of its organisation together with its own entry, so a client computes them from `/me` and
  the project's `organisation_id` without asking again. `contracts/fixtures/api/me.json` pins the
  shape: a developer of an organisation, a maintainer of one of its projects through a team, and the
  holder of a custom role on another.
- **`403` or `404`.** A caller who holds no permission at all where an operation is checked gets the
  answer the operation gives for a resource that does not exist: `404 not_found` with the same
  message, or for the organisation named in `POST /projects` the same `invalid_input`, so ids and
  slugs of other organisations and projects tell nothing; for an organisation named in the query
  of a list, or in the body of `POST /teams`, `POST /roles` and `POST /role-bindings`, that is
  `invalid_input` naming `organisation`. A caller who holds some permission there
  but not the one the operation needs gets `403 forbidden`, whose message names the login, the
  permission and the organisation or project, such as `` `dana` does not hold `run.dispatch` on
  project `keel` ``. An operation checked on the installation answers `403` to everyone but owners.
- **Order.** An operation checks the permission as soon as it has found the resource it is checked
  on, before it validates the rest of the request, so a caller who may not act learns nothing about
  what the request would need. `POST /projects` reads its body first, because the body names the
  organisation; `POST /runs/{run}/commands` checks that the run is visible before it looks at the
  body, and the permission of the command after. The lists of teams, roles and role bindings read
  their query first for the same reason, and so do `POST /teams`, `POST /roles` and
  `POST /role-bindings` their body.
- **Lists** declared on `items` answer `200` to every caller and hold only the items on whose
  project the caller holds the permission the list declares, `project.view` for every such list
  served so far. A list checked on the installation or on an organisation is refused there like any
  other operation.

Every operation declares what it needs, and the declaration is its `x-bosun-permission` in
`contracts/openapi.json` ([The OpenAPI document](#the-openapi-document)), an object whose `on` says
where it is checked:

| `x-bosun-permission` | Means |
|----------------------|-------|
| `{"on": "anyone"}` | the operation takes no credentials |
| `{"on": "authenticated"}` | any valid credentials; the operation acts on the caller alone |
| `{"on": "installation", "key": K}` | `K` on the installation, which only owners hold |
| `{"on": "organisation", "key": K}` | `K` on the organisation the path, the query or the body names, or on the organisation of the team, role or role binding the path names |
| `{"on": "project", "key": K}` | `K` on the project the path names, or the project of the workflow, run, job or execution it names |
| `{"on": "items", "key": K}` | a list of the items on whose project the caller holds `K` |
| `{"on": "items", "key": K, "installation": I}` | a stream of the items on whose project the caller holds `K`, and of the items that belong to no project when the caller holds `I` on the installation ([The event stream](#the-event-stream)) |
| `{"on": "project", "by_command": {…}}` | the permission of the command in the body, on the run's project; the map gives it for each command the endpoint takes, as the command table of [04-runs.md](04-runs.md#operator-commands) does |

The endpoints and what they need, which a test compares with `contracts/openapi.json`, and
another, together with the role table of [08-access.md](08-access.md#permissions-and-roles),
with the answers each built-in role gets:

| Operation | Permission | On |
|-----------|------------|----|
| `GET /me` | — | `authenticated` |
| `POST /auth/login` | — | `anyone` |
| `POST /auth/logout` | — | `authenticated` |
| `POST /auth/password` | — | `authenticated` |
| `GET /auth/methods` | — | `anyone` |
| `POST /organisations` | `organisation.manage` | `installation` |
| `GET /organisations/{organisation}` | `project.view` | `organisation` |
| `PATCH /organisations/{organisation}` | `organisation.manage` | `installation` |
| `GET /projects` | `project.view` | `items` |
| `POST /projects` | `project.manage` | `organisation` |
| `GET /projects/{project}` | `project.view` | `project` |
| `PATCH /projects/{project}` | `project.manage` | `project` |
| `GET /projects/{project}/workflows` | `project.view` | `project` |
| `POST /projects/{project}/workflows` | `workflow.edit` | `project` |
| `GET /projects/{project}/settings` | `project.view` | `project` |
| `PATCH /projects/{project}/settings` | `project.manage` | `project` |
| `GET /workflows/{workflow}` | `project.view` | `project` |
| `PUT /workflows/{workflow}/source` | `workflow.edit` | `project` |
| `GET /workflows/{workflow}/revisions` | `project.view` | `project` |
| `POST /workflows/{workflow}/dispatch` | `run.dispatch` | `project` |
| `GET /runs` | `project.view` | `items` |
| `GET /runs/{run}` | `project.view` | `project` |
| `GET /runs/{run}/attempts/{attempt}/jobs` | `project.view` | `project` |
| `POST /runs/{run}/commands` | the command's ([04-runs.md](04-runs.md#operator-commands)) | `project` |
| `GET /jobs/{job}` | `project.view` | `project` |
| `GET /executions/{execution}` | `project.view` | `project` |
| `GET /executions/{execution}/steps/{step}/log` | `project.view` | `project` |
| `GET /events` | `project.view`, and `agent.manage` on the installation for the events of no project | `items` |
| `GET /queue` | `project.view` | `items` |
| `GET /agents` | `agent.manage` | `installation` |
| `PATCH /agents/{agent}` | `agent.manage` | `installation` |
| `DELETE /agents/{agent}` | `agent.manage` | `installation` |
| `POST /registration-tokens` | `agent.manage` | `installation` |
| `POST /agents/register` | — | `anyone` |
| `GET /pools` | `agent.manage` | `installation` |
| `POST /pools` | `agent.manage` | `installation` |
| `GET /label-aliases` | `agent.manage` | `installation` |
| `POST /label-aliases` | `agent.manage` | `installation` |
| `GET /users` | `organisation.manage` | `installation` |
| `POST /users` | `organisation.manage` | `installation` |
| `DELETE /users/{user}` | `organisation.manage` | `installation` |
| `GET /teams` | `organisation.manage` | `organisation` |
| `POST /teams` | `organisation.manage` | `organisation` |
| `DELETE /teams/{team}` | `organisation.manage` | `organisation` |
| `GET /teams/{team}/members` | `organisation.manage` | `organisation` |
| `POST /teams/{team}/members` | `organisation.manage` | `organisation` |
| `DELETE /teams/{team}/members/{user}` | `organisation.manage` | `organisation` |
| `GET /roles` | `organisation.manage` | `organisation` |
| `POST /roles` | `organisation.manage` | `organisation` |
| `PATCH /roles/{role}` | `organisation.manage` | `organisation` |
| `DELETE /roles/{role}` | `organisation.manage` | `organisation` |
| `GET /role-bindings` | `organisation.manage` | `organisation` |
| `POST /role-bindings` | `organisation.manage` | `organisation` |
| `DELETE /role-bindings/{binding}` | `organisation.manage` | `organisation` |
| `GET /tokens` | — | `authenticated` |
| `POST /tokens` | — | `authenticated` |
| `DELETE /tokens/{token}` | — | `authenticated` |

A step that adds an endpoint adds its row here.

### Projects and server-side workflows

- `GET /projects` lists the projects the caller may view, oldest first, as a cursor page without
  filters.
- `POST /organisations` takes `slug` and `name`; `POST /projects` takes `organisation` (an id or a
  slug), `slug`, `name` and `repository`. Both answer `201` with the resource, a slug that is taken
  is `409 conflict`, and a slug or a name that breaks its rule ([08-access.md](08-access.md)) is
  `invalid_input` naming the field.
- An organisation carries `installation`, the installation of the GitHub App it maps to as
  `{"id", "account"}`, or `null`
  ([06-runtime-services.md](06-runtime-services.md#installations-and-repositories)).
  `PATCH /organisations/{organisation}` takes `installation`, the forge's id of an installation
  Bosun learned of from its webhooks, or `null`, and maps the organisation to it in place of the
  one it had, or to none; it needs `organisation.manage` on the installation, so only owners map
  installations. An installation Bosun does not know is `invalid_input` naming `installation`, and
  one that another organisation holds is `409 conflict`.
- A project carries `repository`, the repository it is bound to as
  `{"id", "owner", "name", "default_branch"}` with the forge's id and the names the forge last
  gave, or `null`. `repository` in `POST /projects`, and in `PATCH /projects/{project}`, which
  needs `project.manage`, is the repository's full name `<owner>/<name>`, matched without regard
  to case, or `null`, which leaves the project without one or unbinds it. A name that is not of
  that form, an organisation without an installation, and a repository its installation cannot see
  are `invalid_input` naming `repository`; a repository bound to another project is
  `409 conflict`.
- `POST /projects/{project}/workflows` takes `name` and `source` and creates a server-side workflow
  with its first revision; `GET` lists the project's workflows oldest first and filters by `name`.
  `PUT /workflows/{workflow}/source` takes `source` and answers with the revision: `201` when the
  source is new to the workflow, `200` when it is the source of a revision the workflow has, which
  becomes current again. A source with errors is `422 invalid_input` with the field `source`, the
  message `the workflow is not valid` and the diagnostics, and stores nothing
  ([02-workflows.md](02-workflows.md)). `GET /workflows/{workflow}/revisions` lists newest first.
- `POST /workflows/{workflow}/dispatch` takes `ref` and `inputs` and answers `201` with the run as
  planning left it, whose actor is the token of the request. In the same transaction it creates
  attempt 1, one `pending` job per job template and their journal events, stores the IR of the
  current revision as lowered by this server, records the revision's warnings as notices, renders
  the display name, and plans the run, so the jobs that need no other job are `queued`, or
  completed when their `if` or an evaluation decided so, when the response leaves
  ([04-runs.md](04-runs.md)). A project without a repository has no refs, so `ref` must be `null`;
  until `workflow_dispatch` inputs arrive in M1, every input is an `invalid_input` naming
  `inputs.<name>`. A disabled workflow, and a current revision that this server's parser refuses,
  are `409 invalid_state`, the latter with its diagnostics. So is, until the planner applies it, a
  revision that uses a key the planner would otherwise ignore, so that a run never quietly differs
  from its workflow: `concurrency` until step 17 of [../plan.md](../plan.md) and a step that uses an
  action until step 33, because the assignment carries no such step before. The message names the
  key and the step that brings it.

### Project settings

Step 25 of M1 ([../plan.md](../plan.md)) settled that a project's settings are a sub-resource of the
project, `/projects/{project}/settings`, rather than fields of the project. The reasons: they are
the part of a project that people edit concurrently, so they carry a version of their own (`ETag`,
[Conventions](#conventions)), which `PATCH /projects/{project}` (the repository binding of step 26)
neither needs nor makes stale; and they grow with every milestone (retention, the dispatch weight
and the running cap, `workspace`) without changing the shape of `Project`, which every list of
projects returns, or the requests that create one.

- `GET /projects/{project}/settings` answers `200` with the document and its `ETag`.
  `contracts/fixtures/api/project-settings.json` is the document of a new project, its defaults:

  | Setting | Values | Default |
  |---------|--------|---------|
  | `github_workflows` | `true` or `false`: whether the files under `.github/workflows/` are read too ([02-workflows.md](02-workflows.md)) | `false` |
  | `fork_policy` | `disabled`, `approval` or `automatic` ([08-access.md](08-access.md#code-from-outside-the-team)) | `disabled` |
  | `token_permissions` | the permissions of the forge token for workflows that declare no `permissions`, a map of App permissions (`actions`, `attestations`, `checks`, `contents`, `deployments`, `discussions`, `issues`, `metadata`, `models`, `packages`, `pages`, `pull_requests`, `repository_projects`, `security_events`, `statuses`) to `read` or `write` ([06-runtime-services.md](06-runtime-services.md#the-forge-token)) | `{"contents": "read", "metadata": "read"}` |
  | `report` | `checks` or `statuses`: how runs are reported to the forge ([06-runtime-services.md](06-runtime-services.md)) | `checks` |

  The names of `token_permissions` are the forge's names of App permissions, with `_`, not the
  scopes of a workflow's `permissions`, with `-`, because `metadata` is not a scope.
- `PATCH /projects/{project}/settings` takes `If-Match` and a JSON object that names the settings
  to change, each with its whole new value: a `token_permissions` replaces the map rather than
  merging into it, and `null` is not a value of any setting. A full document is a valid change.
  It answers `200` with the new document and its new `ETag`. An object that names a setting the
  document does not have, or a value outside the setting's values, is `422 invalid_input` with
  `the settings are not valid` and one entry of `fields` for each setting at fault, named as in the
  document; an empty object is `invalid_input` as well, since it changes nothing. A refused change
  stores nothing of what it names.
- Each later step that adds a setting adds it to the document with a default, which a project that
  existed before takes. Applying the settings belongs to the steps that use them: `github_workflows`
  to step 27, `report` to step 28, `fork_policy` to step 29 and `token_permissions` to step 34.

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

### Runs, jobs and executions

- `GET /runs` lists the runs of the projects the caller may view, newest first, as a cursor page of
  runs in the shape of `GET /runs/{run}`; its filters arrive with step 38 of
  [../plan.md](../plan.md).
- `GET /runs/{run}` answers with the run, in the shape `POST /workflows/{workflow}/dispatch`
  returns; its `attempt` is the number of its latest attempt, and the list of its attempts is not
  served yet. `display_name` is the run's display name and `notices` its run notices in the order
  of the journal, each a diagnostic with the `job_id` it concerns or `null`
  ([04-runs.md](04-runs.md#run-notices)).
- `GET /runs/{run}/attempts/{n}/jobs` lists the jobs of attempt `n` in the order they were
  created, as a cursor page without filters. A job carries `execution_id`, the id of its latest
  execution, or `null` while no agent has claimed it, and `outputs`, the outputs its agent reported
  for the names the job declares, `{}` until then. A member of a job group carries the group's id
  in `group_id` and its combination of the matrix in `matrix`; both are `null` for every other job,
  and a group is the job that members name ([04-runs.md](04-runs.md#parts-of-a-run)), listed before
  them.
- `GET /jobs/{job}` answers with one job in the same shape, and `GET /executions/{execution}` with
  one execution and its `steps`, ordered by `index`, empty until the agent starts the job.
- `{run}`, `{job}` and `{execution}` are ids. Any other segment, and an id of nothing, are
  `404 not_found` with the message ``there is no run `<run>` `` (or `job`, `execution`); an attempt
  the run does not have is ``there is no attempt `<n>` of run `<run>` ``.

### Pools and label aliases

Step 18 of M1 ([../plan.md](../plan.md)) settled these endpoints; what pools and aliases do is in
[05-execution.md](05-execution.md#pools-and-label-aliases).

- `GET /pools` lists the pools oldest first, as a cursor page without filters. A pool is
  `{"id", "name", "trust", "access", "created_at"}`, and its `access` is
  `{"all_projects", "organisations", "projects"}`: whether every project of the installation may use
  it, the organisations whose projects may, each `{"id", "slug"}`, and the single projects that may,
  each `{"id", "organisation_id", "path"}` with `path` written `<organisation>/<project>`.
- `POST /pools` takes `name`, `trust` (`trusted` or `untrusted`) and `access` in the same shape,
  with each organisation named by its id or slug and each project by its id or
  `<organisation>/<project>`, and answers `201` with the pool. A name follows the rule of slugs
  ([08-access.md](08-access.md#organisations-projects-and-teams)) and is unique, so a name that is
  taken is `409 conflict`. An organisation or a project that does not exist is `invalid_input`
  naming `access.organisations.<n>` or `access.projects.<n>`, and an access list that opens the pool
  to no project is `invalid_input` naming `access`, since no endpoint changes an access list yet and
  such a pool could never take a job.
- `GET /label-aliases` lists the aliases oldest first, as a cursor page without filters. An alias is
  `{"id", "label", "organisation_id", "pool_id", "capability", "image", "created_at",
  "updated_at"}`, whose `organisation_id` is `null` for an alias of the installation.
- `POST /label-aliases` takes `label`, `organisation` (an id or a slug, or `null` for the
  installation), `pool` (a pool's name, or `null` for any pool open to the project), `capability`
  (`shell`, `podman`, or `null` for either) and `image` (or `null`), and sets the alias of the label
  in that scope. It answers `201` with the alias when the scope had none for the label, and `200`
  when it replaced the one the scope had. The label follows the rule of labels
  ([05-execution.md](05-execution.md#registration-and-identity)) and is stored lowercased. An image
  is 1 to 255 characters without white space or control characters; it makes a missing capability
  `podman`, and next to `shell` it is `invalid_input` naming `capability`, since a job with an image
  runs in a container. An organisation or a pool that does not exist is `invalid_input` naming the
  field.
- All four are checked on the installation, an organisation's aliases too: an alias sends jobs to a
  pool, and pools belong to the installation ([08-access.md](08-access.md#roles-and-bindings)).

### Identity and access

Step 10 of M1 ([../plan.md](../plan.md)) settled these endpoints; who manages what, and what a
token holds, is in [08-access.md](08-access.md#users-teams-roles-and-tokens).

- **Users.** `GET /users` lists the users oldest first, as a cursor page without filters, each in the
  shape of `user` in `GET /me`. `POST /users` takes `login`, `name`, `email` and `password`, and
  answers `201` with the user, who is not an owner and must change the password at the first
  sign-in. A login, a name or a password that breaks its rule ([08-access.md](08-access.md)), and an
  email address that is not one (up to 254 characters without white space or control characters,
  with one `@` between a local part and a domain), are `invalid_input` naming the field; a login
  that is taken is `409 conflict`. `DELETE /users/{user}` deletes the user and answers `204`; the
  caller's own id is `409 invalid_state`. All three are checked on the installation.
- **Teams.** A team is `{"id", "organisation_id", "slug", "name", "created_at"}`. `GET /teams`
  lists the teams of the organisation `organisation` names, oldest first. `POST /teams` takes
  `organisation`, `slug` and `name`, and answers `201` with the team; a slug that breaks the rule of
  slugs or reads as an id, and a name that breaks the rule of names, are `invalid_input` naming the
  field, and a slug that the organisation has already is `409 conflict`. `DELETE /teams/{team}`
  deletes the team with its memberships and bindings, and answers `204`.
- **Members.** `GET /teams/{team}/members` lists the members of the team as users, in the order the
  users were created.
  `POST /teams/{team}/members` takes `user`, an id or a login, and answers `201` with the user when
  it added them and `200` when they were a member already; a user who does not exist is
  `invalid_input` naming `user`. `DELETE /teams/{team}/members/{user}` takes the user's id and
  answers `204`, or `404 not_found` with ``there is no member `<user>` in team `<team>` `` when the
  user is not a member.
- **Roles.** A role is `{"id", "organisation_id", "name", "permissions", "created_at"}`, where
  `organisation_id` is `null` for a built-in role and `permissions` is ordered as the permission
  table of [08-access.md](08-access.md#permissions-and-roles). `GET /roles` lists the built-in roles
  and the custom roles of the organisation `organisation` names, oldest first, so the built-in roles
  come first. `POST /roles` takes `organisation`, `name` and `permissions`, and answers `201` with
  the role. `PATCH /roles/{role}` takes `name`, `permissions` or both, in the way of a change of the
  project settings ([Project settings](#project-settings)) but without a version, each replacing
  what the role had, and answers `200` with the role; the change reaches the role's bindings at
  their next request. `DELETE /roles/{role}` deletes the role with its bindings and answers `204`.
  A key that is not a permission key is `invalid_input` naming `permissions.<n>`, a name that
  breaks the rule of slugs or reads as an id is `invalid_input` naming `name`, and a name that the
  organisation has already, or that a built-in role has, is `409 conflict`. A built-in role answers
  `PATCH` and `DELETE` with `409 invalid_state` to every caller, since the built-in roles are the
  same in every installation.
- **Role bindings.** A binding is `{"id", "role", "user", "team", "organisation_id", "project",
  "created_at"}`: the role as `{"id", "name"}`; the user as `{"id", "login"}` or the team as
  `{"id", "slug"}`, the other being `null`; and the project as `{"id", "organisation_id", "path"}`,
  or `null` for a binding on the organisation. `GET /role-bindings` lists the bindings of the
  organisation `organisation` names, on it and on its projects, oldest first.
  `POST /role-bindings` takes `role` (an id or a name), `user` (an id or a login) or `team` (an id
  or a slug), `organisation` (an id or a slug) and `project` (an id, `<organisation>/<project>`, or
  `null`), and answers `201` with the binding. A role that is neither built in nor a custom role of
  the organisation, a team or a project of another organisation, a user who does not exist, and a
  request that names both a user and a team or neither, are `invalid_input` naming the field; the
  same binding again is `409 conflict`. `DELETE /role-bindings/{binding}` deletes the binding and
  answers `204`.
- A team, a role or a binding named by its id in the path is checked on its organisation: one that
  does not exist, or belongs to an organisation where the caller holds nothing, is `404 not_found`
  with ``there is no team `<id>` `` (or `role`, `role binding`).
- **Personal tokens.** A token is `{"id", "name", "permissions", "restricted", "organisations",
  "projects", "created_at", "expires_at", "revoked_at"}`: the keys it carries, ordered as the
  permission table, whether it is restricted, and the organisations (each `{"id", "slug"}`) and
  projects (each `{"id", "organisation_id", "path"}`) it is restricted to. `restricted` stays
  `true` when those were all deleted, and a token that the server made for an owner lists every
  key. `GET /tokens` lists the caller's own tokens oldest first, revoked and expired ones included.
  `POST /tokens` takes `name`, `permissions`, `organisations` (ids or slugs), `projects` (ids or
  `<organisation>/<project>`) and `expires_at`, and answers `201` with the token and, in `token`,
  its `bst_` text, which is shown only this once; empty `organisations` and `projects` make a token
  that is not restricted. Each of these is `invalid_input` naming the field: a name that breaks the
  rule of names; no permission (`permissions`); a key that is not a permission key, or one that the
  caller does not hold anywhere the token would reach (`permissions.<n>`); an organisation or a
  project that does not exist or where the caller holds nothing (`organisations.<n>`,
  `projects.<n>`); an expiry that has passed or lies more than 365 days ahead (`expires_at`). A
  request that a token authenticates is also refused for a token that would not be restricted
  while the requesting token is (`organisations`), and for an expiry after the requesting token's
  (`expires_at`). `DELETE /tokens/{token}` revokes one of the caller's tokens and answers `204`,
  also when it was revoked already; a token of another user is `404 not_found`. All three act on
  the caller alone, so they are declared `authenticated`.

### Agents

Step 19 of M1 ([../plan.md](../plan.md)) settled draining and deleting agents; what each does is in
[05-execution.md](05-execution.md#draining-and-deleting-agents).

- `GET /agents` lists the agents oldest first, as a cursor page without filters. An agent is
  `{"id", "name", "pool_id", "labels", "capabilities", "version", "online", "drain",
  "last_seen_at", "execution_id", "created_at"}`, whose `drain` says whether it is drained and whose
  `execution_id` is its execution that is not completed, or `null`. A deleted agent is not listed.
- `PATCH /agents/{agent}` takes `drain`: `true` drains the agent and `false` lifts its drain. It
  answers `200` with the agent. Draining a drained agent, and lifting the drain of one that is not
  drained, change nothing; an empty body changes nothing either, and `null` is `invalid_input`,
  since an agent is drained or not.
- `DELETE /agents/{agent}` deletes the agent, revokes its credential, ends its stream and ends its
  executions, and answers `204` without a body.
- `{agent}` is an agent's id. Any other segment, an agent that does not exist and one that was
  deleted are `404 not_found` with the message ``there is no agent `<agent>` ``, so a second
  `DELETE` of an agent is `404`.
- All three are checked on the installation, since agents belong to it
  ([08-access.md](08-access.md#roles-and-bindings)).

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

### The queue

- `GET /queue` lists the jobs that are `queued` or `waiting`, of the projects on which the caller
  holds `project.view`, oldest job first, as a cursor page without filters. Step 37 of M1
  ([../plan.md](../plan.md)) settled the entry, `QueuedJob`:
  `{"job", "project_id", "since", "missing_labels"}`.
  - `job` is the job in the shape of `GET /jobs/{job}`, whose `status` and `waiting_reason` are the
    reason it waits, and whose `labels` are those its `runs-on` evaluated to.
  - `since` is when the job took its place: the `queued_at` of a queued job, which a return to the
    queue keeps ([04-runs.md](04-runs.md#dispatch)), and the time of the `job.status` event that
    made a waiting job wait. Its waiting time is the time since then, which a client counts itself.
  - `missing_labels` are the labels of the job that no online agent offers, in the order of the
    job's labels, `[]` when each of them is offered by some online agent. Labels are compared as
    they are stored, lowercased.
- An entry that a `queue.job` event carries for a job that left the queue holds the job with its new
  status, `since` `null` and `missing_labels` `[]` ([The event stream](#the-event-stream)).

## The OpenAPI document

`contracts/openapi.json` describes, in OpenAPI 3.1, every operation served under `/api/v1`. Paths
outside it are not part of the document: `/hooks`, `/_runtime`, `/twirp`, the health endpoints and
gRPC. `bosun-server openapi` prints the document without reading a configuration, and
`hack/openapi.sh` writes it into `contracts/openapi.json`. A test fails when the committed file
differs from the document the handlers describe, and its message names the script, so a change of
an endpoint regenerates the file in the same commit; the file is never edited or merged by hand.

The generator is Bosun's own, `crates/server/src/openapi.rs`, over the JSON Schemas that the API
shapes of `bosun-types` derive with schemars. Step 1 of M1 ([../plan.md](../plan.md)) chose it by
two criteria: the document is derived from the request and response types the handlers use, so it
cannot drift from them, and the shared parts are described once. Of the libraries that generate
OpenAPI for axum, utoipa derives the schemas, but its operations, their bodies and responses among
them, are written by hand in an attribute beside each handler, and nothing checks them against the
handler, so they drift. aide derives operations from the handlers' signatures, but documents each
handler under the route string it is registered at: the projects' wildcard route, which tells an id
from two slugs, would be one operation answering two shapes, and describing the operations it
dispatches to would mean writing them by hand. Its released version is also tied to schemars 0.9,
with the version on schemars 1 still an alpha. Bosun's own generator is a few hundred lines that
cover exactly the extractors and responses its handlers use.

How the document follows the handlers:

- Routes under `/api/v1` are registered through `Api` (`get`, `post`, `put`, `patch`,
  `delete`), which routes the
  handler and records its operation in one call. The operation is derived from the handler's
  signature, at compile time, through two traits: each argument contributes what it reads, and the
  return type the responses. `Body<T>` is a JSON body of `T`; `Idempotent<T>` is that body and the
  `Idempotency-Key` header; `Listing<F>` is `limit`, `cursor` and the filters of `F`; the query of
  a log and the agent list have their own extractors. `Json<T>` answers `200` with `T`,
  `Created<T>` `201`, `Saved<T>` `201` or `200`, `Tagged<T>` `200` with `ETag`, `Removed` `204`
  without a body, and `ApiError` the error envelope. A filter that `F` requires is a required query
  parameter. `Changes<T>` is a body that changes a document of `T`: its schema has the
  properties of `T`, inline, none of them required, no others and at least one. A handler that
  reads a raw query, header map or body, or returns an untyped response, has no operation, and so
  cannot be registered.
- A handler behind credentials takes the caller as `Caller<R>`, whose rule `R` names what it needs:
  `Authenticated`, `OnInstallation<K>`, `OnOrganisation<K>`, `OnProject<K>`, `Listed<K>` or
  `ByCommand`, with `K` a permission key. The rule contributes the operation's
  `x-bosun-permission` ([Permissions](#permissions)), and the caller offers only the check of that
  rule, so the declaration and the check cannot differ. An operation without credentials declares
  `{"on": "anyone"}`, and an operation behind credentials whose handler takes no `Caller` fails the
  generation of the document.
- The path parameters come from the path template. Each name has its description and schema in
  `openapi.rs`, and a name without one fails the tests.
- The routes of projects are one wildcard route that tells an id from two slugs
  ([Conventions](#conventions)) and hands the request to the handler of `/projects/{project}` or
  `/projects/{project}/workflows`; those handlers are registered as the operations, with
  `{project}` described as an id or `<organisation>/<project>`. A test sends a request with the
  owner's token to every operation of the document, with each `{project}` as an id and as
  `globalart%2Fops`, and fails on the `not_found` of an unserved path.
- The document's root requires the scheme `bearer` or the scheme `session`, the cookie
  `__Host-bosun_session`. An operation behind credentials whose method is not `GET` requires
  `bearer`, or `session` together with `csrf`, the header `X-Bosun-CSRF`, in its own `security`.
  `POST /agents/register`, which the registration token in its body authenticates, and
  `POST /auth/login` and `GET /auth/methods`, which need no credentials, have an empty `security`.
- Routes are registered through `Api::new()` behind credentials, `Api::interim()` behind
  credentials that a session whose password must change may also use (`/me`, `/auth/logout`,
  `/auth/password`), and `Api::public()` without credentials. The sign-in answers with its own
  response type, which documents the `Set-Cookie` header of the `200` of `POST /auth/login` and of
  the `204` of `POST /auth/logout`.

The shared parts are described once:

- the error envelope is the schema `ErrorEnvelope`, and the response `Error` holds it; every
  operation's `default` response is `Error`, since every operation answers its failures, and those
  behind credentials their `401`, with the envelope;
- the cursor page is the schema `Page`, with `items` and `next`; a list answers with `allOf` of
  `Page` and the schema of its items, and a test checks that an operation answers with a page
  exactly when it takes a cursor;
- `limit`, `cursor` and `Idempotency-Key` are the parameters `Limit`, `Cursor` and
  `IdempotencyKey`;
- `ETag` is the header `ETag`, which the response type `Tagged<T>` contributes to its `200`, and
  `If-Match` the required parameter `IfMatch`, which the extractor `IfMatch` contributes.

The schemas describe the shapes as the server writes them (schemars' serialize contract): every
field is present, with `null` when it has no value, as the conventions require of responses.
Request bodies are described by the same schemas, so a field that may be `null` is listed as
required in them too; the server also takes a request that leaves such a field out, and reads it
as `null`. The bodies of `PATCH` are the exception: they have schemas of their own, whose fields
are all optional, since a field left out keeps its value ([Conventions](#conventions)). Ids are
strings of the format `uuid`, times strings of the format `date-time`, and job
keys strings with their pattern.

A followed log is `text/event-stream`, and OpenAPI 3.1 has no schema for the events of a stream.
The media type carries the extension `x-bosun-events`, which maps each event name to the schema of
its data: `message` to `LogLine` and `end` to `LogEnd`. OpenAPI 3.2's `itemSchema` can replace it
once the tools that read the document take 3.2.

`info.version` is `v1`, the version plane of the REST API, not the version of the binaries. Keys
are written sorted, with two spaces of indentation and a final newline, so that the file does not
depend on the order in which routes are registered.

Every fixture in `contracts/fixtures/api/` belongs to a request or a response of an operation, or
to a schema that one of them refers to, and validates against that schema; a fixture of a shape no
operation serves fails the suite, so such a fixture arrives with its endpoint.

## The event stream

`GET /api/v1/events` is a server-sent event stream filtered by `?project=`, `?run=`, `?kind=`.
Each event has an id that is the journal's sequence number for run events and a separate
sequence for the others (agents, queue, approvals), encoded so that `Last-Event-ID` resumes both.
Event payloads are the same objects the REST resources return, so a client can apply them without
a second request.

Step 37 of M1 ([../plan.md](../plan.md)) settled the stream as follows; how its events are kept
and read in order is in [07-storage.md](07-storage.md#the-journal).

- **Events.** The answer is `200` with `text/event-stream`. Every event is a message without a
  name, whose data is one JSON object: a journal event of a run in the shape of
  [04-runs.md](04-runs.md#the-journal), `RunEvent`, or an event that belongs to no run's journal, a
  stream event, `StreamEvent`: `{"seq", "project_id", "run_id", "at", "kind", "data"}`. `kind` tells
  the two apart, since the kinds of the journal and these differ. The extension `x-bosun-events` of
  the operation maps `message` to both schemas.

| Stream kind | Data | Written |
|-------------|------|---------|
| `agent.status` | the agent as `GET /agents` lists it | when an agent registers, when it connects, when a heartbeat brings an agent that was marked offline back online, when the sweep marks it offline, when it is drained or its drain is lifted, and when it is deleted, which marks it offline; `project_id` and `run_id` are `null` |
| `queue.job` | the job's entry as `GET /queue` lists it ([The queue](#the-queue)) | when a job becomes `queued` or `waiting`, when it moves between the two, and when it leaves them; `project_id` and `run_id` are those of the job |

A heartbeat of an online agent changes its `last_seen_at` without an event, and an agent's
`execution_id` follows its executions, whose `execution.created` and `execution.status` are run
events. The missing labels of an entry are those of the moment its event was written; agents that
come or go change them without a `queue.job` event, and `GET /queue` reads them afresh.

- **Who receives what.** An event of a project, every run event and every `queue.job`, reaches a
  caller who holds `project.view` on that project. An event of no project, every `agent.status`,
  reaches a caller who holds `agent.manage` on the installation, as `GET /agents` does. The
  operation declares both, `{"on": "items", "key": "project.view", "installation":
  "agent.manage"}`. The permissions are read once, when the request arrives, and the server ends a
  stream 60 seconds after it began; the client reconnects with `Last-Event-ID`, and the new request
  checks its credentials and reads its permissions again, so a revoked token, an ended session or a
  removed binding stops the events within a minute. The first message of a stream sets the
  reconnection time of an `EventSource` to one second (`retry: 1000`) and carries the position the
  stream begins at (**Ids** below), and a comment every 15 seconds keeps a quiet stream open.
- **Filters.** `project` takes a project's id or `<organisation>/<project>`, `run` a run's id, and
  `kind` a kind of the journal or of the table above. Each may be given more than once: the values
  of one parameter are alternatives, and the parameters together must all hold. An event of no
  project matches no `project` and no `run`. A project or a run the caller may not view is `422
  invalid_input` with the message of one that does not exist, ``there is no project `<p>` `` or
  ``there is no run `<r>` ``, so a filter tells nothing about what the caller cannot see. An unknown
  kind, any other query parameter and a malformed `Last-Event-ID` are `422 invalid_input` too.
- **Ids.** The journal and the stream events are two sequences, and an event's place in its
  sequence is its position, two numbers ([07-storage.md](07-storage.md#the-journal)). The id of a
  message is the position the stream had reached in both sequences once it was sent:
  `<journal>.<journal>.<stream>.<stream>`, four decimal numbers, such as `1028.42.1031.7`.
  `Last-Event-ID` resumes each sequence after its own position, so a client that reconnects receives
  exactly the events of both that it missed, each once. Without it, a stream begins at the moment of
  the request: it delivers every event committed after the request arrived, and possibly some
  committed shortly before. A client therefore opens the stream before it reads the resources it
  shows, and so misses nothing between the two. The first message of every stream has no data and,
  as its id, the position the stream begins at, which sets the last event id of an `EventSource`
  without dispatching an event. A stream that the server ends before it delivered an event is
  therefore resumed where it began, and its client misses nothing across that reconnect either.
- **Order.** The events of one run arrive in the order of its journal, and the stream events of one
  agent, or of one job's entry, in the order they were written. Events of different runs, agents and
  jobs may arrive in another order than they were committed.
- **Across replicas.** The transaction that writes an event announces it with
  `NOTIFY bosun_events` ([07-storage.md](07-storage.md#notifications-between-replicas)), and every
  replica wakes the streams the announcement may concern: a stream filtered by `run` wakes for those
  runs only. A woken stream reads what it may deliver. When it reaches events that are committed
  but not yet readable in order ([07-storage.md](07-storage.md#the-journal)), it reads again after
  50 milliseconds, doubling the pause up to a second until they are, and announcements do not cut
  these pauses short. A stream that hears nothing reads again every 5 seconds, and every stream
  reads again when the replica's listening connection is re-established. When the server shuts
  down, its streams end, so that their clients reconnect to another replica.

## Outgoing webhooks

A subscription has an address, a secret, a filter (projects, event kinds) and an active flag. Each
delivery is a `POST` with the event as JSON, `X-Bosun-Event`, `X-Bosun-Delivery` and
`X-Bosun-Signature-256` (HMAC-SHA256 of the body with the secret). Deliveries go through the
outbox ([07-storage.md](07-storage.md)), with its retries and dead-lettering; the subscription page
lists the latest deliveries with their responses.

## The CLI

`bosun` keeps profiles in `~/.config/bosun/config.toml`, or `$XDG_CONFIG_HOME/bosun/config.toml`
when that variable is set: a table `[profiles.<name>]` per server with its `server` address and
`token`. The file and its directory are readable by their owner only (modes `0600` and `0700`), and
the token stays in the file rather than in a system keyring, which servers, agents and CI hosts
mostly do not have. Every command takes `--profile <name>` (default `default`) and prints text by
default and JSON with `--json`. Failures are printed to standard error as `error: <message>`, with
the fields at fault and the diagnostics of the error envelope on the lines below it, and exit with
status 1; a usage error exits with status 2.

```
bosun login [--server <url>]
bosun project list | show <org/project> | settings <org/project> [--set key=value]...
bosun workflow list <project> | show <workflow> | enable | disable
bosun workflow push <project> <file> [--name <name>]
bosun workflow lint [<file>...]
bosun workflow inventory <dir> [--output <dir>]
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
bosun agent list | drain <agent> [--lift] | delete <agent>
bosun pool list | token create <pool> [--expires <d>] [--reusable]
bosun pool create <name> [--untrusted] [--all-projects] [--organisation <o>]... [--project <p>]...
bosun alias set <label> [--pool <p>] [--capability <c>] [--image <i>] [--organisation <o>]
bosun secret set <name> [--project <p> | --organisation <o> | --environment <e>] < value
bosun var set <name> <value> [scope]
bosun plugin install <oci-ref> | list | instance create <plugin> --name <n> --config <file>
bosun token create --name <n> --scope <perm>... --expires <d> [--organisation <o>]... [--project <p>]...
```

What M0 settles of these commands:

- `bosun login` reads a personal token from the first line of standard input, without echoing it on
  a terminal, checks it with `GET /me` and stores it in the profile. `--server` may be left out when
  the profile has a server already.
- `bosun workflow push <project> <file>` creates the server-side workflow, or saves the file as its
  new source when the project has a workflow of that name. The name defaults to the file name
  without its extension, and `<project>` is `<organisation>/<project>` or an id.
- `bosun workflow dispatch <workflow>` takes the workflow's id or `<organisation>/<project>/<name>`.
  With `--watch` it prints the run, a line `<job>: <status>` whenever a job's status or conclusion
  changes (the first one with the job's id), every log line of every job as `<job> | <text>`, and
  `run <id>: completed <conclusion>` at the end; a job's completion is printed after its last log
  line. It exits with 0 when the run succeeds and with 1 otherwise, until step 38 of M1 settles a
  status per conclusion for `bosun run watch`.
- `bosun job logs <job>` prints each step's log under a heading `==> step <n>: <name>`, its lines as
  the API serves them, with their time. `--step <n>` prints only step `n`, counted from 1, without a
  heading; `--follow` waits for the job to reach an agent and prints the lines as they arrive until
  every step has ended.
- `bosun pool token create <pool>` prints the token alone on standard output, and on standard
  error its pool and expiry followed by the one-line command that installs an agent with it,
  `curl -fsSL <server>/install-agent.sh | sudo sh -s -- --token <token>`, where `<server>` is the
  server of the profile. `--expires` takes a whole number of minutes, hours or days (`30m`,
  `12h`, `7d`); without it the server's default of 24 hours applies. `--reusable` lets the token
  register any number of agents rather than one.
- `--watch` and `--follow` outlive a restart of the replica they talk to: a request that gets no
  answer, or `502`, `503` or `504`, is tried again for up to 60 seconds, and a followed log resumes
  after its last line with `Last-Event-ID`. Other commands fail at once.

What M1 settles of these commands:

- `bosun project settings <org/project>` prints the settings of the project, one line per setting
  with its value, a value that is not text as JSON; `--json` prints the document. Each `--set
  key=value` changes one setting: the command reads the document and its `ETag`, sends the changes
  with `If-Match`, and prints the document the server answers. A value that reads as JSON is sent
  as that JSON (`--set github_workflows=true`, `--set 'token_permissions={"contents": "write"}'`),
  and any other value as a string (`--set fork_policy=approval`). A key given twice fails before
  any request; a change that raced with another fails with the server's `412`.
- `bosun pool list` prints one line per pool with its name, its trust level, what it is open to
  (`all projects`, or the slugs of its organisations and the paths of its projects) and its id.
- `bosun pool create <name>` creates a pool, trusted unless `--untrusted` is given, open to every
  project with `--all-projects` and to the organisations and projects that `--organisation` and
  `--project` name, each as often as needed, and prints the pool. A pool needs at least one of the
  three.
- `bosun alias set <label>` sets the alias of the label for the installation, or for the
  organisation `--organisation` names, to the pool, the capability (`shell` or `podman`) and the
  image its options give, replacing what the label had in that scope, and prints the alias.
- `bosun token create` creates a personal token of the profile's user that carries the permission
  keys `--scope` names, each as often as needed, restricted to the organisations and projects that
  `--organisation` and `--project` name, if any, and that expires after `--expires`, a whole number
  of minutes, hours or days as for `bosun pool token create`, which the command requires. It prints
  the token alone on standard output and, on standard error, a line that names its keys, where it
  reaches and its expiry, and says that it is shown only once; `--json` prints the created token.
- `bosun agent list` prints one line per agent with its name, its state (`online` or `offline`,
  followed by `, drained` for a drained agent), its labels, capabilities and version, the execution
  it runs and its id.
- `bosun agent drain <agent>` drains the agent and prints that it finishes the job it runs and is
  offered no other; `--lift` lifts the drain instead. `bosun agent delete <agent>` deletes the agent
  and prints that its credential is revoked. `<agent>` is an agent's id or the name of an agent,
  which no other agent may have: the commands read the list of agents first, and a name that
  several agents have fails, listing their ids. With `--json`, `drain` prints the agent as the
  server answers it and `delete` the agent it deleted, as it was.
- `bosun workflow inventory <dir>` reads no profile and talks to no server. It reads every `*.yml`
  and `*.yaml` file under `<dir>`, at any depth, with `bosun_workflow::inventory`, and writes
  `uses.json` and `keys.json` into the directory `--output`, by default `compat/inventory`, in the
  formats of [compat/README.md](../../compat/README.md). It prints how many files, uses and key
  paths it counted. A file that is not YAML fails the command, naming the file and the fault, and
  nothing is written.

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
