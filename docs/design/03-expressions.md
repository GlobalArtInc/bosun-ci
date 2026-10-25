# Expressions

This document specifies the expression language of workflows: syntax, values and coercion,
functions, contexts, and which side evaluates which expression. The language is the one of the
target syntax; where this document and the observed behaviour of GitHub Actions disagree, the
behaviour wins and this document is corrected. The compatibility corpus pins it
([13-decisions.md](13-decisions.md)).

## Syntax

An expression appears inside a template as `${{ expression }}`. A template may hold several
expressions and text around them; its result is the text with every expression replaced by the
string form of its value. A template is split at each `${{` and at the first `}}` after it that is
not inside a string literal, so `${{ '}}' }}` is one expression whose value is `}}`. A `${{` without
its `}}` is an error, and the delimiter itself is written as a string: `${{ '${{' }}`.

The value of `if` is an expression with or without the delimiters: when the whole value, apart from
whitespace around it, is one `${{ }}`, its content is the expression; when there are no delimiters,
the whole value is. An `if` that is empty or holds only whitespace sets no condition. An `if` with
text around a `${{ }}`, or with several of them, is a template: it evaluates to a string, which is
true unless it is empty, so with text around it is always true; the linter warns about it.

Literals: `null`, `true`, `false`, numbers (integer, decimal, exponent, hexadecimal with `0x`,
octal with `0o`, `NaN`, `Infinity`), and strings in single quotes, where `''` stands for one quote.
A number carries its sign, as in `-1`, `+5` and `-Infinity`; there is no unary minus. A decimal may
start or end with its point (`.5`, `1.`). Hexadecimal and octal literals are 32-bit integers in
two's complement, so `0xFFFFFFFF` is `-1`, and one that needs more than 32 bits is not a number.

Names of contexts, properties and functions start with a letter or `_` and go on with letters,
digits, `_` and `-`, so `steps.my-step.outputs` is three lookups. A property whose name does not
fit, such as one that starts with a digit, is reached with an index: `steps['1st']`. Names are
case-insensitive, and so are the keywords `true`, `false`, `null`, `NaN` and `Infinity`:
`GITHUB.Event_Name` is `github.event_name`, `toJson(x)` calls `toJSON` and `TRUE` is `true`. After
a `.`, a keyword is a property name, so `a.true` looks up the property `true`. A call is a name
followed by `(`, with whitespace allowed between them. A literal is never followed by `.` or `[`:
`'abc'.length` is an error, while `(x).y` and `fromJSON(x)[0]` are not.

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
element; `a[*]` is the same filter as `a.*`. Comparisons and equality group from the left, so
`a == b != c` is `(a == b) != c`, and a chain of `&&`, or of `||`, is one node of the syntax tree
that holds all its operands.

An expression has at most 21 000 characters and nests at most 50 levels deep: every operator,
access, call and parenthesised group is a level, and a chain of `&&` or of `||` is one.

Every node of the syntax tree keeps its span. A span counts from the start of the text that was
parsed, which is a bare expression, a template or the value of an `if`. Lines and columns count from
1, columns in Unicode characters, and the end is exclusive, as in diagnostics
([02-workflows.md](02-workflows.md)); `bosun-workflow` moves the spans to where that text sits in
the file.

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
| `hashFiles(...patterns)` | SHA-256 over the SHA-256 of each matched file in the order the glob walk finds them, relative to the workspace; directories and files outside the workspace are skipped; `!` excludes; empty string when nothing matches; 1 to 255 patterns; evaluated on the agent only |
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

Every key that accepts an expression has a fixed set of contexts it may use, and only some keys
offer the special functions: the status functions in `if`, and `hashFiles` in the keys of a step;
every other function is available at every key, and `bosun` wherever `github` is, which is
everywhere. Bosun reproduces this availability table of the target syntax as data in `bosun-expr`,
pinned by `contracts/fixtures/expr/availability.json`, and validation rejects an expression that
names a context or calls a function unavailable at its key. Keys are named as the target names
them, from the root of the file: `jobs.<job_id>.steps.run` is the `run` of any step. The table in
the crate holds the keys of the milestones reached so far ([../plan.md](../plan.md)).

The table also decides which side evaluates the expression, by one rule: **an expression whose key
can see `job`, `runner` or `steps` is evaluated on the agent; every other expression is evaluated
on the server.** The server needs the server-side values to decide where and whether a job runs;
the agent alone knows the runner and the steps.

| Key | Evaluated by | When |
|-----|--------------|------|
| `run-name` | server | when the run is created |
| `concurrency` | server | when the run is created |
| `env` | server | when a job is assigned |
| `jobs.<job_id>.if` | server | when the job's `needs` are complete |
| `jobs.<job_id>.name` | server | when the job's `needs` are complete |
| `jobs.<job_id>.strategy` | server | when the job's `needs` are complete; matrix expansion happens at the same moment |
| `jobs.<job_id>.runs-on` | server | when the job's `needs` are complete |
| `jobs.<job_id>.environment` | server | when the job's `needs` are complete |
| `jobs.<job_id>.concurrency` | server | when the job's `needs` are complete |
| `jobs.<job_id>.timeout-minutes` | server | when the job's `needs` are complete |
| `jobs.<job_id>.continue-on-error` | server | when the job's `needs` are complete |
| `jobs.<job_id>.container` | server | when the job's `needs` are complete |
| `jobs.<job_id>.container.image` | server | when the job's `needs` are complete |
| `jobs.<job_id>.container.credentials` | server | when the job's `needs` are complete |
| `jobs.<job_id>.services` | server | when the job's `needs` are complete |
| `jobs.<job_id>.services.<service_id>.credentials` | server | when the job's `needs` are complete |
| `jobs.<job_id>.env` | server | when the job's `needs` are complete |
| `jobs.<job_id>.defaults.run` | server | when the job's `needs` are complete |
| `jobs.<job_id>.with.<with_id>` | server | when the job's `needs` are complete |
| `jobs.<job_id>.secrets.<secrets_id>` | server | when the job's `needs` are complete |
| `jobs.<job_id>.gate.message` | server | when the gate starts waiting |
| `jobs.<job_id>.gate.inputs.<input_id>.default` | server | when the gate starts waiting |
| `jobs.<job_id>.container.env.<env_id>` | agent | at job start |
| `jobs.<job_id>.services.<service_id>.env.<env_id>` | agent | at job start |
| `jobs.<job_id>.environment.url` | agent | at job end |
| `jobs.<job_id>.outputs.<output_id>` | agent | at job end |
| `jobs.<job_id>.steps.if` | agent | when the step is reached |
| `jobs.<job_id>.steps.name` | agent | when the step is reached |
| `jobs.<job_id>.steps.run` | agent | when the step is reached |
| `jobs.<job_id>.steps.env` | agent | when the step is reached |
| `jobs.<job_id>.steps.with` | agent | when the step is reached |
| `jobs.<job_id>.steps.working-directory` | agent | when the step is reached |
| `jobs.<job_id>.steps.continue-on-error` | agent | when the step is reached |
| `jobs.<job_id>.steps.timeout-minutes` | agent | when the step is reached |
| `on.workflow_call.inputs.<inputs_id>.default` | server | when the call starts |
| `on.workflow_call.outputs.<output_id>.value` | server | when the call ends |

Expanding a matrix when the `needs` are complete is what makes a matrix computed by an earlier job
(`fromJSON(needs.setup.outputs.matrix)`) work.

## Static checks

Before a run starts, every expression is checked without evaluating anything. Each fault is an
error at the span of the context or the call at fault, or of the syntax that does not parse:

| Code | Meaning |
|------|---------|
| `expression_syntax` | the expression or the template does not parse, or a `${{` is not closed |
| `limit_exceeded` | the expression is longer than 21 000 characters or nests deeper than 50 levels |
| `unknown_context` | a name that is no context, such as `foo` in `foo.bar` |
| `unavailable_context` | a context the key cannot see, such as `steps` in `runs-on` |
| `unknown_function` | a function the language does not have |
| `unavailable_function` | a status function outside `if`, or `hashFiles` outside the keys of a step |
| `wrong_argument_count` | a call with a number of arguments the function does not take; `case` takes an odd number, at least three |
| `not_implemented` | a call of `plugin()`, which arrives in M5 |

## Evaluation errors

An expression that fails at run time (invalid JSON for `fromJSON`, a `timeout-minutes` that is not
a number, a `runs-on` that is neither a string nor a list) fails the job when the server evaluates
it, and the step when the agent does. The error carries the span of the expression, so the run
page points at the line.

## Implementation

`bosun-expr` holds a hand-written lexer, a precedence-climbing parser producing the syntax tree with
spans, the availability table with the static checks, and an evaluator over a `Value` enum. The
tree, which is `Expr`, the segments of a `Template` and the `Condition` of an `if`, is plain data in
`bosun-types`, so the IR holds it. Contexts are supplied through a trait, so the server can resolve
`needs` from the database and the agent can resolve `steps` from its own state, each lazily. The
crate does no I/O: `hashFiles` is a hook the agent implements. The crate builds for WebAssembly, so
the editor offers the same completion and the same diagnostics as the server.

The cases live in `contracts/fixtures/expr/` as JSON files, each a list of cases:

- a syntax case has `expression` and the `tree` it parses to, or its `error` with `code`,
  `message` and `span`;
- a template case has `template` and its `segments`, or its `error`;
- an `if` case has `if` and its `condition`, which is `null` when it sets none, or its `error`;
- a check case has `expression`, a `key` of the availability table and the `errors` of the static
  checks;
- an evaluation case has `expression`, `contexts`, and `result` or `error`.

`availability.json` beside them is the availability table. The same cases are run through GitHub
Actions in a public repository, whose hosted minutes cost nothing, and a difference between the
recorded result and Bosun's is a failing test.
