# bosun

A CI/CD server for the workflows a team already has. Bosun runs the GitHub Actions workflow syntax
on agents the team owns, so existing `.github/workflows/` move without a rewrite and the public
actions keep working, including the cache and artifact ones. On top of that it gives operators
control over a run while it runs (pause, breakpoints, skip, mark as succeeded with chosen outputs,
re-run one job, hold a failed job and open a terminal in it, gates that take input), a workflow
editor in the browser that runs drafts before they are committed, plugins for triggers,
notifications, admission policies, secrets and executors, and an API, a CLI and an MCP endpoint
for everything the interface does.

Backend in Rust; frontend in React; Postgres and an S3-compatible object store; agents are one
static binary that runs jobs on the host or in rootless podman containers.

## Status

Designed, not yet built. The design is in `docs/design/`, the order of work in `docs/plan.md`.

| Milestone | Scope | State |
|---|---|---|
| M0 | Walking skeleton: dispatch, planner, journal, queue, agent stream, shell steps, logs, CLI | not started |
| M1 | GitHub App, the full job language, expressions, Node actions, secrets, sign-in, the web interface | |
| M2 | The CI of Keel: podman, services, composite and container actions, artifacts, caches, reusable workflows | |
| M3 | Run control: every operator command, gates, environments, retry, hold and terminal, MCP | |
| M4 | The workflow editor: graph, inspector and YAML on one document, drafts, commits and pull requests | |
| M5 | Plugins: WebAssembly and external plugins, notifiers, triggers, admission policies | |

## Layout

```
crates/      bosun-types, bosun-expr, bosun-workflow, bosun-db, bosun-blob, ... (from M0)
bin/         bosun-server, bosun-agent, bosun (from M0)
proto/       agent and plugin protocols (from M0)
web/         the web interface (from M1)
contracts/   golden fixtures, the workflow JSON Schema, openapi.json
compat/      the compatibility corpus (from M1)
deploy/      image, systemd units, install-agent.sh, sample configs
docs/        design and plan
hack/        scripts CI runs
```

## Documents

- `docs/design/01-overview.md`: what Bosun is, processes, names, crates, versions
- `docs/design/02-workflows.md`: workflow sources, the compatibility surface, extensions, the IR
- `docs/design/03-expressions.md`: the expression language and where each expression is evaluated
- `docs/design/04-runs.md`: runs, attempts, states, the journal, operator commands, gates, holds
- `docs/design/05-execution.md`: the agent, its protocol, the execution engine, `shell` and `podman`
- `docs/design/06-runtime-services.md`: the forge, tokens, artifacts, caches, reporting
- `docs/design/07-storage.md`: Postgres, the queue, the outbox, the object store, logs, secrets
- `docs/design/08-access.md`: users, tokens, roles, untrusted code, audit
- `docs/design/09-frontend.md`: the web interface and the workflow editor
- `docs/design/10-plugins.md`: extension points, WebAssembly and external plugins
- `docs/design/11-api.md`: REST, the event stream, outgoing webhooks, the CLI, MCP
- `docs/design/12-operations.md`: deployment, configuration, scaling, observability, upgrades
- `docs/design/13-decisions.md`: decisions, risks, open questions
- `docs/plan.md`: milestones and the M0 work breakdown
