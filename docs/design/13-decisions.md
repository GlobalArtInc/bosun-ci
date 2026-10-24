# Decisions, risks and open questions

This document records the key decisions of the design with the alternatives that were weighed,
the risks the design carries with their countermeasures, and the questions left open because
their answers would change the design. The decisions themselves are specified in the subject
documents; this one says why they were taken.

## Key decisions

The outcome column has two values: **chosen by the owner**, when the alternatives were put to the
product owner and the owner made the call, and **proposed**, when the design took the decision and
the owner has not overruled it.

| # | Decision | Alternatives | Outcome |
|---|----------|--------------|---------|
| 1 | A standalone service with its own server and agents ([01-overview.md](01-overview.md)) | a product on the Keel kernel, which brings sign-in, settings, audit and a shell for the interface | chosen by the owner |
| 2 | The workflow syntax of GitHub Actions with a closed list of extensions ([02-workflows.md](02-workflows.md)) | an own YAML with an embedded scripting language; pipelines as TypeScript code | chosen by the owner |
| 3 | Two capabilities, `shell` and rootless `podman` ([05-execution.md](05-execution.md)) | Kubernetes pods; a virtual machine per job | chosen by the owner; the executor extension point keeps the others possible |
| 4 | Run control, the workflow editor, plugins, and API, CLI and MCP as the four areas beyond a plain CI server | any subset | chosen by the owner |
| 5 | The agent opens one gRPC stream and keeps it ([05-execution.md](05-execution.md)) | HTTP long polling, as the target's own runner does; connections from the server to agents | proposed |
| 6 | Postgres is the queue and the bus between replicas ([07-storage.md](07-storage.md)) | Redis; NATS; Kafka | proposed |
| 7 | A journal with status columns projected in the same transaction ([04-runs.md](04-runs.md)) | status tables without history; pure event sourcing with replay on read | proposed |
| 8 | The agent evaluates step expressions; the server evaluates the rest ([03-expressions.md](03-expressions.md)) | everything on the server | proposed |
| 9 | The agent masks secrets before a line leaves it ([05-execution.md](05-execution.md)) | masking on the server | proposed |
| 10 | One slot per agent process, one unix user per instance ([05-execution.md](05-execution.md)) | one agent process with several slots | proposed |
| 11 | Extension keys are plain keys in a closed, published list ([02-workflows.md](02-workflows.md)) | an `x-` prefix; one `bosun:` block per job and step | proposed |
| 12 | Run inputs never change; mid-run values flow through gates and `job.mark-success` ([04-runs.md](04-runs.md)) | editable inputs of a running run | proposed |
| 13 | The runtime services speak the toolkit's protocols, including the subset of the block blob protocol its upload library uses ([06-runtime-services.md](06-runtime-services.md)) | patched forks of the artifact and cache actions | proposed |
| 14 | React and Vite, as in Astrum ([09-frontend.md](09-frontend.md)) | Angular and phoenix-ui, as in Keel | proposed |
| 15 | The editor edits a comment-keeping YAML document and validates with the server's parser compiled to WebAssembly ([09-frontend.md](09-frontend.md)) | a form model regenerated into YAML; validation by a request per change | proposed |
| 16 | In-process plugins are WebAssembly components; heavy ones are external processes over gRPC ([10-plugins.md](10-plugins.md)) | dynamic libraries; an embedded script engine | proposed |
| 17 | Three binaries: server, agent, CLI ([01-overview.md](01-overview.md)) | one binary with subcommands | proposed |
| 18 | UUIDv7 identifiers ([07-storage.md](07-storage.md)) | ULID as in Keel | proposed |
| 19 | Pull requests from forks do not run by default ([08-access.md](08-access.md)) | approval by default | proposed |
| 20 | Workflow YAML is read with `saphyr-parser` into a tree of Bosun's own ([02-workflows.md](02-workflows.md)) | `serde_yaml`; `yaml-rust2`; `marked-yaml` | proposed |
| 21 | One self-contained image that installs on any Linux host with only a database address; agents installed from the server with one command ([12-operations.md](12-operations.md)) | a delivery tied to the team's cluster and gitops; packages per distribution | chosen by the owner |

### Notes

**1. Standalone rather than on Keel.** Keel's delivery model is one installation per host, outside
containers, with peers between hosts. A CI server is the opposite shape: a server that belongs in
the cluster, and dozens of agents on other machines that come and go. Fitting that onto peers would
bend the kernel for one product. What Keel would have given for free (sign-in, settings, audit) is
specified here again at the size this product needs.

**2. The target syntax.** The deciding argument is the existing workflows of both organisations,
which then move without a rewrite, and the actions ecosystem, which then is available from day one.
The price is a long tail of compatibility details and a syntax Bosun does not control; the closed
list of extensions is the place where Bosun adds what it needs.

**5. A stream rather than long polling.** Run control needs the server to reach a running job at
once: a pause, an abort, a breakpoint, a terminal. With long polling, each of these waits for the
next poll or needs a second channel. One stream opened by the agent works through NAT and closed
networks, which the agents of this installation live in.

**6. Postgres as the queue.** One stateful dependency is what one engineer can operate. `SKIP
LOCKED` gives a correct multi-consumer queue and `LISTEN`/`NOTIFY` a sufficient bus, because
payloads are identifiers and the data is read from tables. The risk is load from log ingestion;
see risks.

**10. One slot per process.** Separate unix users give each concurrent job its own processes, files
and rootless podman, so the podman socket handed to a job sees only that job's containers. A
multi-slot agent would need privileges to switch users, which the agent then must not have.

**11. Plain extension keys.** Workflows are read more often than they are written, and
`retry: { attempts: 3 }` reads better than any prefixed form. The risk is that the target syntax
later adds a key of the same name with another meaning; the list is closed and published, so such
a collision is found by the compatibility corpus, and the decision on it is recorded here. It has
happened once already, in the favourable direction: `timezone` on schedules was drafted as an
extension and turned out to be part of the target syntax, with the same meaning, so it moved to the
compatibility surface.

**12. Immutable inputs.** A run whose inputs changed halfway has jobs that saw different values,
and its record no longer explains its result. Gates and `job.mark-success` give the same power at
named points, with the values in the journal.

**13. Speaking the toolkit's protocols.** Patched copies of the cache and artifact actions would
break the promise that workflows run unchanged, and would have to follow every upstream release.
The block blob subset is three requests.

**14. React.** The editor depends on a mature graph library and a code editor with schema
completion; both are first-class in React. Astrum's interface is React and Vite already, so the
team has the tooling.

**20. saphyr-parser.** The parser had two criteria: a span on every node, and the core schema of
YAML 1.2, under which `on` is a string. `saphyr-parser` reports the start and the end of every
event and leaves the schema to its caller, so Bosun resolves plain scalars itself and keeps exactly
the tree its diagnostics need. It is pure Rust without `std`, so the WebAssembly build needs
nothing more. `serde_yaml` is deprecated and knows no spans, `yaml-rust2` marks only where an
event starts, and `marked-yaml` turns every scalar into a string and refuses aliases. Two of its
spans are corrected on top of it: the end of a quoted scalar includes the blanks and the comment
after it, and the end of a block scalar its trailing blank lines.

## Risks

| Risk | Countermeasure |
|------|----------------|
| The compatibility tail is longer than expected | the corpus of real workflows and actions from both organisations, inventoried in M1; differential runs on GitHub's hosted runners in a public repository, which cost no minutes; actions ranked by use, so the common ones are exact first |
| GitHub changes the toolkit's protocols, as it did for caches | the corpus pins action versions; a new protocol is added next to the old one, which keeps serving older actions |
| The target syntax keeps growing (`cache-mode`, `concurrency.queue`, `case()` arrived recently) | the parser rejects unknown keys loudly rather than ignoring them, so a new key shows up as a diagnostic in some workflow before it can be misread; the corpus is refreshed from the published syntax every milestone |
| A job reads its agent's credential and obtains later jobs of the pool | the pool is declared the secret boundary ([08-access.md](08-access.md)); projects with secrets that must stay apart use different pools |
| Workflows assume software preinstalled on hosted images | label aliases to images with the needed tools; `shell` agents with the tools installed; the corpus lists what each workflow needs |
| Postgres becomes the bottleneck for log ingestion and fan-out | a load test in M2 with many agents writing logs; ingestion sits behind one interface, so chunks can go to the object store directly if needed |
| Paused and held jobs occupy slots and starve the queue | caps on pause and hold, visible occupied slots, an alert on queue time |
| A terminal on a `shell` agent is access to the host | off unless the agent's own configuration allows it; `job.debug` required; every session recorded |
| Operator overrides hide real failures | the source of a conclusion is shown everywhere, in the API and in the forge report |
| Installation tokens expire after an hour | the agent refreshes them; actions that copied the token are a documented limitation |
| The forge's API rate limit | workflow files cached by blob hash; conditional requests; status reports collapsed in the outbox |
| Agents sit in internal networks | forks do not run by default; untrusted pools for those that should; trusted pools never receive untrusted jobs, enforced in the claim |
| The volume of work for one engineer | the order of milestones puts the first real payoff (the CI of Keel moving) at M2; plugins come last |

## Open questions

- The final name and the public address of the installation.
- A second forge: whether GitLab is needed, and whether Bosun then ships its own checkout, since
  `actions/checkout` speaks GitHub's API.
- An identity token service for `id-token: write`, for workflows that log in to clouds.
- Executors beyond `shell` and `podman`: Kubernetes pods or virtual machines through an executor
  plugin, and when.
- Agents that update themselves from the server.
- macOS agents.
- Whether Bosun stays private or is published.
- Plugins that contribute pages to the interface.
