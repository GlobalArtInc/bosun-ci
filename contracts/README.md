# contracts

Shapes that code on both sides of a boundary, and programs outside this repository, rely on.

- `fixtures/`: golden fixtures that tests load, from step 2 of M0.
- `fixtures/workflows/`: workflow sources, from step 3 of M0. Each one lowers to the IR of the same
  name in `fixtures/ir/`, or produces exactly the diagnostic of the same name in
  `fixtures/diagnostics/`.
- `schema/workflow.json`: the JSON Schema of workflows with Bosun's extensions, from M1.
- `openapi.json`: the description of the REST API generated from the handlers, from M1.

A change to a file here is a change of a contract, and its commit message says so.
