# Working in this repository

Bosun is a CI/CD server that runs GitHub Actions workflows on agents the team owns. Read
`docs/design/01-overview.md` before touching anything; the rest of `docs/design/` is the
specification, and `docs/plan.md` is the order of work. When code and design disagree, fix one of
them in the same change.

## Rules

- English everywhere that lands in git: code, docs, tests, CI, commit messages.
- No comments in code. Explain in the commit message or in `docs/`. The only exception is a link
  to a specification next to the code that implements it.
- Commit titles follow Conventional Commits, `type(scope): message` in the imperative, lower case,
  no trailing period. Commit messages end with their last line of content: no attribution trailers.
- Push straight to `main`. One task is one change; do not split a task across branches.
- Verify with the same commands CI runs before pushing: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` against a local
  Postgres (`BOSUN_TEST_DATABASE_URL`), the `wasm32-unknown-unknown` build of the pure crates,
  `hack/check-deps.sh`, and `hack/e2e.sh` once it exists; `pnpm lint && pnpm test && pnpm build`
  in `web/` once it exists.
- Postgres is the only database. Tests get a database per test from a template; no in-memory
  substitutes.
- A behaviour of the target syntax is implemented together with a case that pins it, in
  `contracts/fixtures/` or in `compat/`. "It works like GitHub" is shown by a case, not asserted.
- Other CI products are named only where the compatibility contract requires it: the GitHub Actions
  syntax, the toolkit's protocols and the environment variable names actions read. Everything else
  uses Bosun's own vocabulary.

## Boundaries that CI enforces

- `bosun-types`, `bosun-expr` and `bosun-workflow` do no I/O, depend on no async runtime and build
  for `wasm32-unknown-unknown`.
- `bosun-runner`, `bosun-agent` and `bosun-cli` depend on neither `bosun-db` nor `bosun-server`.
- `bosun-server` does not depend on `bosun-runner`.
- Migrations inside a major version are additive only.
- Every change to the state of a run is a journal event written in the same transaction as the
  status columns it changes.

## Names

Product Bosun, id `bosun`. Binaries `bosun-server`, `bosun-agent`, `bosun`; crates `bosun-*`; npm
`@globalart/bosun-web` and `@globalart/bosun-wasm`. Workflows in `.bosun/workflows/`. Database and
schema `bosun`; directories `/etc/bosun`, `/var/lib/bosun`; agent users `bosun-agent-<n>` and units
`bosun-agent@<n>.service`. Token prefixes `bst_`, `brt_`, `bat_`, `bpt_`, `bss_`; agent protocol
`bosun.agent.v1`.
