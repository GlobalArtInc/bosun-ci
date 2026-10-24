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
own; keys exist in OwnLate before the change that uses them is pushed, because the build pulls the
dictionaries. The interface has a light and a dark theme.

## Live updates

Each tab keeps one connection to `GET /api/v1/events` with the filters of the page it shows, and
reconnects with `Last-Event-ID` when the page changes or the connection drops, so nothing is missed
in between. An event updates the cached object it concerns, or invalidates the query that lists
it. A followed log is a separate stream per open step
(`GET /api/v1/executions/{id}/steps/{step}/log?follow=true`).

## Pages

**Home.** Recent runs of the projects the user can see, the approvals waiting for them, failing
workflows, and the state of the queue.

**Project.** Workflows with the status of their latest run on the default branch; the run list
with filters by workflow, branch, event, status and actor; the project's settings, environments,
secrets and variables for those who may see them.

**Run.** The page an operator lives on.

- The header: status and conclusion with their source, trigger, commit, actor, attempt selector,
  duration, and "Run again with changes".
- The graph of jobs: job groups collapsed with member counts and expandable; gates drawn
  distinctly; colours by status; waiting jobs show their reason; live.
- The job panel: steps with durations and conclusions, nested composite steps, the log viewer
  (virtualised, ANSI colours, collapsible groups from `::group::`, optional timestamps, links to
  lines, search inside a step, raw download), annotations at their lines, the rendered step
  summaries, artifacts.
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
with their differences and can restore one as a new revision; "export to repository" commits the
workflow to a path in a bound project.

## Quality

The interface is usable with the keyboard alone, keeps visible focus, and meets contrast for text
in both themes. Unit tests run with Vitest; end-to-end tests run with Playwright against a server
started by `hack/e2e.sh` with a real agent.
