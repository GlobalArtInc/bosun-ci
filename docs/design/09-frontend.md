# Frontend

This document specifies the web interface: its stack and delivery, live updates, the pages, and
the workflow editor in detail. The interface uses only the public API ([11-api.md](11-api.md)); it
has no endpoint of its own.

## Stack and delivery

| Concern | Choice |
|---------|--------|
| Language and build | TypeScript in strict mode, React 19, Vite |
| Components | Radix Themes and Radix primitives, lucide icons, Geist |
| Data | TanStack Query over the REST API; the event stream patches and invalidates its cache |
| Routing | React Router 7 |
| Graphs | React Flow with ELK for layered layout |
| YAML | Monaco with schema-driven completion; the `yaml` library's document model for edits that keep comments |
| Terminal | xterm.js |
| Parser in the browser | `bosun-workflow` and `bosun-expr` compiled to WebAssembly as `@globalart/bosun-wasm` |

The built interface is embedded in the `bosun-server` binary. Hashed assets are served as
immutable; `index.html` is served with `no-cache`, so a deployment is picked up on the next
navigation and never mixes old and new chunks.

Strings are translated into English and Russian through OwnLate, in a project of the interface's
own, `bosun-web`; keys exist in OwnLate before the change that uses them is pushed, because the build pulls the
dictionaries. The interface has a light and a dark theme.

### Serving

`web/` builds into `web/dist/interface/`, and the build script of `bosun-server` embeds every file
of it in the binary. A file under `/assets/`, whose name Vite derives from its content, is answered
with `Cache-Control: public, max-age=31536000, immutable`; `index.html` and the other files at the
root with `no-cache`. A `GET` or `HEAD` of any other path is answered with `index.html`, so a route
of the client such as `/runs/<id>` loads the interface. Paths under `/api`, `/hooks`, `/_runtime`,
`/twirp`, `/mcp` and `/assets` are never the page: there a path the server does not serve keeps the
JSON `not_found` of the API ([11-api.md](11-api.md)), and gRPC is told apart before any route.
Every file carries `X-Content-Type-Options: nosniff`, and the page also
`Content-Security-Policy: frame-ancestors 'none'`, so no other site can frame it.

### A build without the interface

`web/dist/` is in git with nothing but its `.gitignore`, so the build script always has a directory
to watch: Cargo reruns it when `pnpm build` writes `web/dist/interface/`, and never because the
interface was not built. Without `web/dist/interface/index.html` the build succeeds and embeds
nothing. Such a server logs a warning when it starts and answers the paths of the interface with
`404` and a line of text saying how to build it; the API, the CLI and the agents work as before. So
`cargo build`, `cargo test` and `cargo clippy` need neither Node nor the network. The image, CI and
`hack/e2e.sh` build the interface before the server, so everything they ship or test carries it.

### Strings, languages and themes

The build pulls the latest release of the interface's OwnLate project, as the over-the-air bundle
its access key serves without credentials (`web/scripts/dictionaries.mjs`), into
`web/generated/dictionaries.json`, which is not in git; `pnpm test` and `pnpm build` both pull it
first. A key the code uses, as the argument of `t('…')` or as a quoted key of a namespace it uses,
that a pulled dictionary lacks fails `pnpm test`, and so does a translation whose placeholders
differ from the English text. The type of `t` refuses a key the English dictionary lacks, so
`pnpm build` fails on it as well. The names of the languages in the language switch are written in
their own language and are not translated, and neither is the name Bosun.

The language and the theme a user picks are kept in the browser's local storage. Until a user picks
them, the interface follows the browser's languages and colour scheme.

### The API client and sign-in

The client's types are generated from `contracts/openapi.json` by `openapi-typescript` at every
`pnpm build`, into `web/generated/api.ts`, and the client is `openapi-fetch` over them, so a change
of the document that breaks the interface fails the type check of the build. The client keeps the
`csrf_token` of the last `Me` it received and sends it as `X-Bosun-CSRF` with every request other
than `GET`, `HEAD` and `OPTIONS`. Every answer that is not a success becomes an error with the
envelope's `code`, `message`, `fields` and `diagnostics` and the seconds of `Retry-After`; an answer
without the envelope becomes `internal`.

The sign-in page offers the password form when `GET /auth/methods` says `password: true`, and a
link to `/api/v1/auth/oidc/{name}/start` for each provider it lists. A session whose password must
change is led to `/password`, which asks for the current password and the new one twice and checks
the rule of 12 characters before it sends them. Every other page sends a visitor without a session
to `/sign-in?next=<path>`, and sign-in returns there; `next` is followed only when it is a path on
the same origin. The shell has the navigation, the switches of language and theme, and the user
menu with sign-out.

## Live updates

Each tab keeps one connection to `GET /api/v1/events` with the filters of the page it shows, and
reconnects with `Last-Event-ID` when the page changes or the connection drops, so nothing is missed
in between. An event updates the cached object it concerns, or invalidates the query that lists
it. A followed log is a separate stream per open step
(`GET /api/v1/executions/{id}/steps/{step}/log?follow=true`).

Step 41 of M1 ([../plan.md](../plan.md)) settled how the stream and the cache of TanStack Query
meet:

- **One connection.** The shell owns the connection and each page names its filters: the home page
  `kind=run.created`, `run.status` and `queue.job`; a project's page `project=<id>` with
  `run.created` and `run.status`; a run's page `run=<id>` with every kind. The stream is read
  through `fetch`, not `EventSource`, so that a new connection of the same tab can carry
  `Last-Event-ID` when the page changes, and so that Vitest feeds it fixture streams through the
  API client. A connection that ends, or fails on the network or with a `5xx`, is opened again
  after the `retry` the server sent with `Last-Event-ID`; a `401` makes the shell read `/me`
  again, which leads to the sign-in page, and a `403`, `404` or `422` ends the following of that
  page.
- **The stream before the reads.** The queries the stream keeps current (runs, jobs, executions,
  workflows and the queue) wait until the first connection of the tab has received its first
  message, the position it begins at, or has failed. Every read they make therefore happens after
  the stream began, as [11-api.md](11-api.md#the-event-stream) asks of a client, and a page loaded
  later reads after a position the stream resumes from.
- **Patching.** Every status event names both ends of its transition, so an event is applied to a
  cached run, job, execution or step only when the cached status is the event's `from`. When the
  cached status is already its `to` the event was seen before, and nothing changes; any other
  status means the cache missed something, and the query that holds the object is invalidated,
  which reads it again. A status event sets the times of the transition from its `at`: `started_at`
  when the object first reaches `in_progress`, `completed_at` when it completes, and a job's
  `queued_at` when it enters the queue from `pending` or `waiting`. `job.created`,
  `execution.created` and `step.created` add the object they describe to the cached list or
  execution, unless it is there already; `execution.created` also sets the job's `execution_id`.
  `run.notice` adds the notice to every cached copy of the run, unless an equal one is there.
  `run.created` and `attempt.created` invalidate the lists they change, since their data does not
  hold the objects. `queue.job` replaces the entry of its job in the cached queue, or removes it
  when the job left the queue; since agents that come and go change the missing labels without an
  event, the queue is also read again every 30 seconds while a page shows it. What no event carries,
  such as a job's labels and outputs, stays as it was last read.
- **The fixture.** `web/src/test/fixtures/stream.ts`, typed against the generated client so that a
  change of the API document breaks its build, pins the outcome: the run, job, execution, step and
  queue events of a run with a job group patch the cached run, jobs, executions, queue, run list
  and workflow list to the state the fixture expects without invalidating anything, also when the
  connection is resumed and an event arrives twice.

## Pages

**Home.** Recent runs of the projects the user can see, the approvals waiting for them, failing
workflows, and the state of the queue.

**Project.** Workflows with the status of their latest run on the default branch; the run list
with filters by workflow, branch, event, status and actor; the project's settings, environments,
secrets and variables for those who may see them.

Step 41 of M1 ([../plan.md](../plan.md)) settled the pages of M1 where runs are found and watched:

- **Routes.** The home page is `/`, a project's page `/projects/<organisation>/<project>` with the
  path the API gives a project, and a run's page `/runs/<id>`, whose `?job=<id>` opens the panel of
  one job. The filters of a project's run list are the query of its address (`workflow`, `branch`,
  `event`, `status` and `actor`, named as `GET /api/v1/runs` names them), so a filtered list can be
  linked to.
- **Home.** The fifteen newest runs of every project the user can see, with their project; the
  failing workflows, those whose latest run ([11-api.md](11-api.md)) concluded `failure`,
  `timed_out` or `startup_failure`, read from the workflows of each project; the queue in short:
  how many jobs are queued and how many are held back, the longest wait so far, and the labels that
  no online agent offers; and the projects themselves, the way to a project's page. The approvals
  join the page with the inbox in M3.
- **Status and conclusion.** A run, a job or a step shows its conclusion once it is completed and
  its status before. A conclusion an operator chose carries the mark "set by an operator"; a
  natural one carries none. A run has no source of its own in the API, so its conclusion is marked
  when a job of its attempt has a conclusion an operator chose. A waiting job shows the reason it
  waits.
- **Notices.** A run's notices are listed errors first, then warnings, then notices, each group in
  the order of the journal, with the severity, `file:line:column` of the span, the job it concerns
  and the message.
- **Pre and post steps.** The API lists the steps of an execution flat. The job panel nests a step
  named `Pre <name>` under the first later step named `<name>`, and a step named `Post <name>` under
  the last earlier one, as the target names the pre and post steps of an action, so step 33 of
  [../plan.md](../plan.md) names them that way; a step whose name matches no such step stays where
  it is.
- **Commands.** `run.cancel` is offered on a run that is not completed and `job.cancel` on a job
  that is not completed, to a user whose permissions on the run's project, computed from `/me` and
  the project's `organisation_id` ([11-api.md](11-api.md#permissions)), hold `run.cancel`. Each asks
  for a confirmation and is sent with an `Idempotency-Key` of its own; the page then follows the
  cancellation through the stream.
- **Keys.** The test of the dictionaries takes every quoted dotted string of a namespace the code
  uses as a key, so the interface's namespaces are ones no value of the API shares: `runPage.` and
  `projectPage.` rather than `run.` and `project.`, whose strings are command types and permission
  keys.

**Run.** The page an operator lives on.

- The header: status and conclusion with their source, trigger, commit, actor, attempt selector,
  duration, and "Run again with changes".
- The graph of jobs: job groups collapsed with member counts and expandable; gates drawn
  distinctly; colours by status; waiting jobs show their reason; live.
- The job panel: steps with durations and conclusions, nested composite steps, the log viewer
  (virtualised, ANSI colours, collapsible groups from `::group::`, optional timestamps, links to
  lines, search inside a step, raw download), annotations at their lines, the rendered step
  summaries, artifacts. `/runs/<run>/jobs/<job>` opens the run page with the panel of that job; it
  is the details address of the job's check run on the forge
  ([06-runtime-services.md](06-runtime-services.md#reporting-to-the-forge)).
- The controls: every command of [04-runs.md](04-runs.md) valid in the current state, as buttons
  on the run, the job or the step they act on. Commands that override a result ask for a reason,
  which goes into the journal. Breakpoints are set by clicking the gutter next to a step.
- The history: the journal of the run as a timeline, filtered to interventions or showing
  everything.
- A held job opens the terminal in a drawer, writable for one person and read-only for others.

**Approvals.** The inbox of gates and environments waiting for the user, each with its message, the
form for the gate's inputs generated from their schema, and approve or reject with a comment.

**Queue and agents.** The live queue with each job's waiting time and, when no online agent offers
its labels, which labels are missing; agents by pool with their state (idle, busy, holding a paused
or held job, offline), current job, version, labels and capabilities; drain, delete, registration
tokens.

**Settings.** Organisation and project settings, secrets (write-only), variables, environments with
their rule editor, teams, roles with a matrix of permission keys for custom roles, personal tokens,
plugins with their catalogue and instance configuration forms generated from the plugin's JSON
Schema, the audit trail.

Step 44 of M1 ([../plan.md](../plan.md)) settled the queue and agents page, the pools, and the
secrets and variables of the settings:

- **Routes.** `/queue` is the queue and agents page and `/pools/<name>` the page of a pool;
  `/settings` lists the organisations and the projects whose settings the user can see,
  `/settings/<organisation>` holds an organisation's settings and
  `/projects/<organisation>/<project>/settings` a project's, which the project's page links to. The
  agents, pools, aliases and registration tokens are checked on the installation
  ([11-api.md](11-api.md#agents)), so their sections and the pool pages appear only to a user whose
  `/me` holds `agent.manage` on the installation; the navigation names the page "Queue and agents"
  for them and "Queue" for everyone else, who sees the queue alone.
- **The queue** lists every entry of `GET /queue` with its job, linked to the run page with that job
  open, its project, its status, its reason, its waiting time counted from `since`, and its labels,
  the missing ones marked. A waiting job's reason is its waiting reason; a queued job's is the labels
  that no online agent offers, or, when none is missing, that it waits for a free agent.
- **Agents** are listed under the pools of `GET /pools`, each pool with its trust and a link to its
  page. An agent's state is derived from `GET /agents`: offline when it is not online, busy when it
  holds an execution, idle otherwise, and marked as draining when it is drained, whatever its state;
  an offline agent also says when it was last seen. Its current job is read from its execution and
  the job that names, and links to the run page with that job open. Drain and lift send
  `PATCH /agents/{agent}`, delete asks for a confirmation first, and the answers update the cached
  list.
- **Live agents.** The page follows `queue.job`, and for those who manage agents also
  `agent.status`, `execution.created` and `execution.status`. `agent.status` replaces the cached
  agent; an agent the list does not hold, and one the event shows offline, which is also what a
  deletion writes, make the list read again, so a registered agent appears and a deleted one leaves.
  `execution.created` sets the `execution_id` of the agent it names, and an `execution.status` to
  `completed` clears it from the agent that held it, as [11-api.md](11-api.md#the-event-stream)
  says the field follows them. Like the queue, the list is also read again every 30 seconds, since a
  heartbeat changes `last_seen_at` without an event.
- **Pools.** The list shows each pool's trust, the projects it is open to and how many of its agents
  are online, and a form creates a pool with a name, a trust level and its access list (every
  project, or organisations and projects named by slug and path) and then opens its page. A pool's
  page shows its trust, access list and creation, its agents and the registration tokens; the page
  of an untrusted pool says that Bosun cannot check the isolation of its network and that its agents
  get no `shell` ([08-access.md](08-access.md#code-from-outside-the-team)).
- **Registration tokens.** The pool page creates one with an expiry of 1 hour, 1 day (the default),
  7 days or 30 days and either single or repeated use, and shows the one-line install command of
  [12-operations.md](12-operations.md#agents) with the new token,
  `curl -fsSL <origin>/install-agent.sh | sudo sh -s -- --token brt_…`, built from the origin of the
  page as `bosun pool token create` builds it from the server of its profile, with what the token
  registers and until when. The token lives only in the state of that request on the page, which
  is dropped as soon as nothing shows it: it never enters the query cache or the browser's storage,
  so "Done", a reload and leaving the page remove it, and nothing shows it again.
- **Label aliases** are listed with their label, scope (the installation, or the slug of the
  organisation, read with `GET /organisations/{organisation}`), pool, capability, image and the
  time they last changed, and a form sets one, which replaces the alias the scope had for the label.
- **Secrets and variables** are those of the scope itself, the organisation or the project, as
  [11-api.md](11-api.md#secrets-and-variables) serves them; a project's page says that its jobs
  also receive its organisation's, which its own of the same name shadow. Secrets show their name
  and the times they were created and last set, never a value: a value is only ever sent, as it was
  typed, the form is emptied once it is stored, and the request that carried it is dropped as soon
  as its form is gone. Variables also show their values. The form that
  sets one, the dialog that replaces a secret or edits a variable and the confirmed deletion appear
  only when the permissions on the scope, computed from `/me` (the installation and the
  organisation, and for a project also the project), hold `secret.manage` or `variable.manage`.
- **Organisations.** No endpoint lists organisations in M1, so `/settings` lists those on which the
  user holds `project.view` through the installation or a binding on the organisation: the
  organisations of `/me`'s permissions, and for an owner also those of the projects they see, each
  read with `GET /organisations/{organisation}`. An organisation without projects is reached by its
  address alone until a list arrives.
- **Keys.** The namespaces of these pages are `queuePage.`, `agentsPage.`, `poolPage.`,
  `settingsPage.` and `form.`, since `queue.`, `agent.`, `secret.` and `variable.` begin values of
  the API, event kinds and permission keys.

## The workflow editor

The editor arrives in M4. It edits a draft of a repository workflow or a server-side workflow, and
shows three views of the same document: the **graph** of jobs, the **inspector** of the selected
job or step, and the **YAML**.

### One source of truth

The document is a `yaml` document model, which keeps comments, key order, quoting and blank lines.
After every change the document is printed and parsed by `@globalart/bosun-wasm`, which returns
the syntax tree with spans, the IR and the diagnostics, exactly as the server would compute them.
The graph and the inspector are derived from that result; the spans connect each node back to its
place in the document.

- In the **YAML** view, edits are text edits; diagnostics appear as markers; completion offers the
  keys of the JSON Schema, and inside `${{ }}` the contexts and functions available at that key.
- In the **graph**, adding a job, connecting two jobs (which adds to `needs`), deleting a job and
  renaming a job are mutations of the document at the job's path. A rename also rewrites the
  `needs` lists and the `needs.<key>` references in expressions that name it, found through the
  spans. Layout is automatic; positions are not stored in the YAML.
- In the **inspector**, every field is bound to a path in the document. Job fields: `runs-on` with
  suggestions from the labels of online agents and the label aliases; `needs`; `if`; environment;
  a table editor for the matrix; container and services; the extensions (retry, hold, cache
  volumes, gate settings). Steps are listed in order and can be reordered by dragging. A `run`
  step edits its script in an embedded editor. A `uses` step has an action picker that searches
  the actions the organisation already uses and accepts any reference; the server fetches the
  action's metadata, and the inspector renders its inputs as a form with descriptions, required
  markers and defaults, and lists its outputs for use in expressions.

Because every edit is a mutation of the document rather than a regeneration of it, a file written
by hand keeps its shape when it is edited in the graph.

### Drafts

The editor saves the draft automatically a few seconds after each change, with `If-Match` on the
draft's version, so two tabs never overwrite each other silently. There is one draft per workflow
and author; a new workflow starts as a draft with a path that does not exist yet.

**Run the draft** opens a dialog: the ref to take the code from, the event to simulate
(`workflow_dispatch` with its inputs, or `push`), and whether to report to the forge (off by
default). The run is marked as a draft run, links back to the draft, and uses the draft's content
with the code of the chosen ref; rules that look at the ref see the draft's own ref
([08-access.md](08-access.md)).

**Save** shows the difference against the revision the draft started from, then commits to a branch
or creates a branch and a pull request, with a message the user writes, through the user's own
authorisation of the App. The draft records the blob it was based on; when the file changed in the
meantime, saving shows a three-way merge first.

### Server-side workflows

Server-side workflows use the same editor. Saving creates a revision; the history lists revisions
with their differences and can restore one, which makes it the current revision again
([02-workflows.md](02-workflows.md)); "export to repository" commits the workflow to a path in a
bound project.

## Quality

The interface is usable with the keyboard alone, keeps visible focus, and meets contrast for text
in both themes. Unit tests run with Vitest; end-to-end tests run with Playwright against a server
started by `hack/e2e.sh` with a real agent. CI runs `pnpm lint` (Biome), `pnpm test` and
`pnpm build` in `web/` before the checks of the Rust code, with Node 24 and pnpm through corepack
at the version `packageManager` pins.

`hack/e2e.sh` runs every scenario under `web/e2e/` in two Playwright projects, `light` and `dark`,
which emulate the colour scheme the interface follows. It gives the scenarios three variables:

- `BOSUN_E2E_URL`, the first replica of its installation, with the agents the other scenarios use;
  it is the `baseURL` of both projects.
- `BOSUN_E2E_TOKEN`, the personal token of `e2e`, the owner the script creates for that
  installation before it creates the organisation `e2e`. Scenarios use it through the API to
  create what they need, a project, workflows and users with a role on the project, each under a
  name of its own, and sign in as those users. Step 41 of M1 added it, since a scenario that
  dispatches runs needs a user the installation knows.
- `BOSUN_E2E_FIRST_START`, a JSON object with a server per theme, each started for the first time
  on a database of its own, as `{"light": {"url", "password"}, "dark": {…}}`, where `password` is
  the content of its `admin.password`. Scenarios that need an installation nobody has signed in to,
  such as the sign-in scenario, which changes that password, use these.

On the installation only an owner holds a permission, and no endpoint makes a user an owner, so the
scenarios that need `agent.manage` there, those of the agents and pools, open the interface with
`BOSUN_E2E_TOKEN` as the bearer token of every request of their browser context; the server takes
it as it takes a session, and the interface sends no `X-Bosun-CSRF`, since `/me` gives a token no
`csrf_token`. Step 44 of M1 settled this. The agents of these scenarios are registered through
`POST /agents/register` in a pool of their own, open to a project of their own, and never connect,
so they take no job of the other scenarios.

The accessibility scenario runs axe with the rules of WCAG 2.1 A and AA on the sign-in page in each
theme, and walks its tab order, requiring a visible focus indicator at every stop.

On Linux with docker, which is how CI runs it, the scenarios run inside
`mcr.microsoft.com/playwright:v<version>-noble`, the image of the exact `@playwright/test` version
of the lockfile, with the host's network, the runner's own user and `web/` mounted at its own path.
The image carries the browser and every library it needs, so nothing is installed on the runner and
nothing needs root. Elsewhere, as on a developer's Mac, they run directly, after
`playwright install --only-shell chromium` has fetched the browser once.
