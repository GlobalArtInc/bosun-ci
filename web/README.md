# @globalart/bosun-web

The web interface of Bosun: TypeScript, React, Vite, Radix Themes, TanStack Query and React Router.
[`docs/design/09-frontend.md`](../docs/design/09-frontend.md) specifies it. pnpm runs through
corepack, at the version `packageManager` pins.

```sh
corepack pnpm install --frozen-lockfile
corepack pnpm lint
corepack pnpm test
corepack pnpm build
```

`pnpm test` and `pnpm build` first pull the dictionaries of the interface's OwnLate project, `bosun-web`, into
`generated/dictionaries.json`; `pnpm build` also generates the API client's types from
`../contracts/openapi.json` into `generated/api.ts`, checks the types and writes the interface to
`dist/interface/`, which the next `cargo build` embeds in `bosun-server`.

A string a person reads is a key of the OwnLate project, in English and Russian, and gets there
before the change that uses it is pushed; a key the code uses and a dictionary lacks fails
`pnpm test`.

`corepack pnpm dev` serves the interface with live reload and passes `/api` to the server at
`BOSUN_WEB_SERVER`, `http://127.0.0.1:8080` unless set.

The Playwright scenarios in `e2e/` run through `hack/e2e.sh interface`, which starts the servers
they need.
