# contracts

Shapes that code on both sides of a boundary, and programs outside this repository, rely on.

- `fixtures/`: golden fixtures that tests load, from step 2 of M0.
- `schema/workflow.json`: the JSON Schema of workflows with Bosun's extensions, from M1.
- `openapi.json`: the description of the REST API generated from the handlers, from M1.

A change to a file here is a change of a contract, and its commit message says so.
