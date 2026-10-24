# Expressions

This document specifies the expression language of workflows: syntax, values and coercion,
functions, contexts, and which side evaluates which expression. The language is the one of the
target syntax; where this document and the observed behaviour of GitHub Actions disagree, the
behaviour wins and this document is corrected. The compatibility corpus pins it
([13-decisions.md](13-decisions.md)).

## Syntax

An expression appears inside a template as `${{ expression }}`. A template may hold several
expressions and text around them; its result is the text with every expression replaced by the
string form of its value. The value of `if` is an expression with or without the delimiters: when
the whole value is one `${{ }}`, its content is the expression; when there are no delimiters, the
whole value is. An `if` with text around a `${{ }}` is a template, which evaluates to a non-empty
string and is therefore always true; the linter warns about it.

Literals: `null`, `true`, `false`, numbers (integer, decimal, exponent, hexadecimal with `0x`,
octal with `0o`, `NaN`, `Infinity`), and strings in single quotes, where `''` stands for one quote.

Operators, from the tightest binding to the loosest:

| Operators | Meaning |
|-----------|---------|
| `( )` | grouping |
| `[ ]`, `.` | index, property, and the object filter `.*` |
| `!` | not |
| `<`, `<=`, `>`, `>=` | comparison |
| `==`, `!=` | equality |
| `&&` | and |
| `\|\|` | or |

There is no arithmetic. `a.b` and `a['b']` are the same lookup; a missing property is `null`, not
an error. `a.*.b` applied to an array or an object yields the array of the `b` property of every
element.

## Values and coercion

Values are `null`, booleans, numbers (IEEE 754 doubles), strings, arrays and objects.

- **Truthiness.** `false`, `0`, `-0`, `""`, `null` and `NaN` are false; everything else is true.
- **Equality.** When both operands have the same type, they are compared directly; strings are
  compared ignoring case; arrays and objects are equal only when they are the same instance. When
  the types differ, both operands are converted to numbers and compared.
- **Conversion to a number.** `null` is `0`; `true` is `1` and `false` is `0`; a string is
  trimmed of whitespace and parsed with the same number forms as literals (so `'0x10' == 16` is
  true), the empty string is `0`, and anything else is `NaN`; arrays and objects are `NaN`.
- **Comparison.** As equality, with the same conversion when types differ; any comparison with
  `NaN` is false.
- **`&&` and `||`** short-circuit and return one of their operands, not a boolean: `a || b` is `a`
  when `a` is true and `b` otherwise.
- **Conversion to a string** in templates: `null` is empty; booleans are `true` and `false`;
  numbers are formatted with at most 15 significant digits, as the target's general format `G15`
  does; arrays are `Array` and objects are `Object`. `toJSON` is the way to print a structure.

## Functions

| Function | Meaning |
|----------|---------|
| `contains(search, item)` | for a string `search`, whether it contains `item` as a substring, ignoring case; for an array, whether an element equals `item` |
| `startsWith(s, prefix)`, `endsWith(s, suffix)` | ignoring case, both arguments converted to strings |
| `format(template, ...args)` | `{0}`, `{1}` replaced by the arguments; `{{` and `}}` are literal braces |
| `join(array, separator)` | elements converted to strings and joined; separator defaults to `,`; a string argument is returned as is |
| `toJSON(value)` | JSON with two-space indentation |
| `fromJSON(string)` | parsed JSON value; invalid JSON is an evaluation error |
| `case(pred1, value1, ..., default)` | the value after the first true predicate, or the last argument when none is true |
| `hashFiles(...patterns)` | SHA-256 over the SHA-256 of each matched file in the order the glob walk finds them, relative to the workspace; directories and files outside the workspace are skipped; `!` excludes; empty string when nothing matches; evaluated on the agent only |
| `success()`, `failure()`, `cancelled()`, `always()` | status functions, see below |
| `plugin(name, ...args)` | a function contributed by a plugin, evaluated on the server only ([10-plugins.md](10-plugins.md)) |

Calling an unknown function or a function with the wrong number of arguments is a validation
error, found before a run starts.

### Status functions

In a job's `if`, the status functions look at the job's ancestors, the jobs it needs directly or
through other jobs: `success()` is true when all of them succeeded, `failure()` when any failed,
`cancelled()` when the run is being cancelled, and `always()` is always true. In a step's `if` they look at the previous steps of the job in the same
way. An `if` that calls none of them behaves as `success() && (<expression>)`.

A job or a step skipped by an operator ([04-runs.md](04-runs.md)) counts as skipped for these
functions, exactly as one skipped by its own `if`. A job marked succeeded by an operator counts as
succeeded.

## Contexts

`github`, `env`, `vars`, `job`, `jobs`, `steps`, `runner`, `secrets`, `strategy`, `matrix`,
`needs` and `inputs` carry the meaning and the property names of the target syntax. The contents
of `github` are listed in [06-runtime-services.md](06-runtime-services.md).

Bosun adds one context, `bosun`, available wherever `github` is:

| Property | Meaning |
|----------|---------|
| `bosun.url` | base address of the installation |
| `bosun.run_url` | page of this run |
| `bosun.run_id` | the run's identifier as Bosun stores it |
| `bosun.attempt` | number of the attempt, same as `github.run_attempt` |
| `bosun.draft` | `true` when the run was started from a draft |
| `bosun.project` | `org/project` path of the project |
| `bosun.trigger` | who or what started the run: `{ kind, login }` |

Using `bosun` in a file under `.github/workflows/` produces the extension warning of
[02-workflows.md](02-workflows.md).

## Where each expression is evaluated

Every key that accepts an expression has a fixed set of contexts it may use. Bosun reproduces the
availability table of the target syntax as data in `bosun-expr`, pinned by a fixture, and
validation rejects an expression that names a context unavailable at its key.

The table also decides which side evaluates the expression, by one rule: **an expression whose key
can see `job`, `runner` or `steps` is evaluated on the agent; every other expression is evaluated
on the server.** The server needs the server-side values to decide where and whether a job runs;
the agent alone knows the runner and the steps.

| Key | Evaluated by | When |
|-----|--------------|------|
| `run-name`, workflow `concurrency` | server | when the run is created |
| workflow `env` | server | when a job is assigned |
| job `if`, `name`, `strategy`, `runs-on`, `environment` (without `url`), `concurrency`, `timeout-minutes`, `continue-on-error`, `container` (image and credentials), `services` (image and credentials), `env`, `defaults`, `with` and `secrets` of a reusable call | server | when the job's `needs` are complete; matrix expansion happens at the same moment |
| gate `message` and the defaults of gate `inputs` | server | when the gate starts waiting |
| `container.env`, `services.<id>.env`, `environment.url`, job `outputs` | agent | at job start or job end |
| every step key | agent | when the step is reached |
| `on.workflow_call` input defaults and output values | server | when the call starts and when it ends |

Expanding a matrix when the `needs` are complete is what makes a matrix computed by an earlier job
(`fromJSON(needs.setup.outputs.matrix)`) work.

## Evaluation errors

An expression that fails at run time (invalid JSON for `fromJSON`, a `timeout-minutes` that is not
a number, a `runs-on` that is neither a string nor a list) fails the job when the server evaluates
it, and the step when the agent does. The error carries the span of the expression, so the run
page points at the line.

## Implementation

`bosun-expr` holds a hand-written lexer, a precedence-climbing parser producing an AST with spans,
and an evaluator over a `Value` enum. Contexts are supplied through a trait, so the server can
resolve `needs` from the database and the agent can resolve `steps` from its own state, each
lazily. The crate does no I/O: `hashFiles` is a hook the agent implements. The crate builds for
WebAssembly, so the editor offers the same completion and the same diagnostics as the server.

The cases live in `contracts/fixtures/expr/` as JSON (`expression`, `contexts`, `result` or
`error`). The same cases are run through GitHub Actions in a public repository, whose hosted
minutes cost nothing, and a difference between the recorded result and Bosun's is a failing test.
