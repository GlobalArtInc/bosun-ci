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
true unless it is empty, so with text around it is always true; the parser warns about it with
`template_condition` ([02-workflows.md](02-workflows.md)).

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

Values are `null`, booleans, numbers (IEEE 754 doubles), strings, arrays and objects. An object
keeps its properties in the order they were written, which `toJSON` prints.

- **Truthiness.** `false`, `0`, `-0`, `""`, `null` and `NaN` are false; everything else is true,
  an empty array or object included.
- **Equality.** When both operands have the same type, they are compared directly: `null` equals
  `null`, and arrays and objects are equal only when they are the same instance, so
  `github.event == github.event` is true and `fromJSON('[]') == fromJSON('[]')` is false. Strings
  are compared ignoring case: every character is replaced by its simple uppercase mapping, and a
  character whose uppercase is several characters, such as `ß`, stays as it is, so
  `'straße' == 'STRASSE'` is false. When the types differ, both operands are converted to numbers
  and compared, so an array or an object never equals a value of another type.
- **Conversion to a number.** `null` is `0`; `true` is `1` and `false` is `0`; a string is
  trimmed of whitespace and parsed with the same number forms as literals (so `'0x10' == 16` is
  true and `'0xFFFFFFFF' == -1` is true), `Infinity` and `NaN` in any case among them; the empty
  string is `0`, and anything else is `NaN`; arrays and objects are `NaN`.
- **Comparison.** As equality, with the same conversion when types differ. Strings are ordered by
  the UTF-16 code units of their uppercase forms, so `'a' < 'B'` and `'a' < '_'` are true; `false`
  is less than `true`; `null`, arrays and objects are neither less nor greater than anything, and
  `<=` and `>=` also hold when the operands are equal, so `null >= null` is true. Any comparison
  with `NaN` is false, and `NaN != NaN` is true.
- **`&&` and `||`** short-circuit and return one of their operands, not a boolean: `a || b` is `a`
  when `a` is true and `b` otherwise, `a && b` is `a` when `a` is false and `b` otherwise, and a
  chain returns the operand that decided it, or its last.
- **Conversion to a string** in templates: `null` is empty; booleans are `true` and `false`;
  numbers are formatted as the target's general format `G15` does: the value is rounded to 15
  significant digits and trailing zeros are dropped; when the decimal exponent is below -4 or
  above 14 the number is written as one digit, the other digits after a point, `E`, the sign of
  the exponent and at least two of its digits. So `3.10` is `3.1`, `0.1` is `0.1`, `1e14` is
  `100000000000000`, `1e15` is `1E+15`, `1e21` is `1E+21`, `0.00001` is `1E-05` and
  `123456789012345678` is `1.23456789012346E+17`; `NaN`, `Infinity` and `-Infinity` are their
  names, and `-0` is `-0`. Arrays are `Array` and objects are `Object`. `toJSON` is the way to
  print a structure.

### Lookup and the object filter

`a.b` and `a['b']` look up the same property, and both are `null` when nothing is found:

- On an object, the index is converted to a string, so `a[1]` looks up the property `1`; an array
  or an object as the index finds nothing. A property whose name equals the index exactly is
  found first; otherwise the first one whose name equals it ignoring case, compared as strings
  compare.
- On an array, the index is converted to a number and rounded down; a negative index, `NaN`, one
  beyond the array and one above 2^31 - 1 find nothing. A property name is not a number, so
  `a.b` of an array is `null`.
- On any other value the lookup is `null`, and its index is not evaluated.

The object filter, `a.*` or `a[*]`, yields an array of the values of an object, in order, or of
the elements of an array, and an empty array of any other value. A lookup or a filter applied to
that array applies to each of its elements: elements that are neither arrays nor objects, and
lookups that find nothing, are left out, and a filter flattens one more level. So with `a` an
array of objects, `a.*.b` is the array of the `b` of every element that has one, and
`contains(github.event.pull_request.labels.*.name, 'bug')` asks whether a label is called `bug`.
The array keeps this behaviour while it flows unchanged through `( )`, `&&`, `||` and `case`; an
array of a context, or one that `fromJSON` returns, is an ordinary array.

## Functions

| Function | Meaning |
|----------|---------|
| `contains(search, item)` | for an array `search`, whether an element equals `item`; for an object, false; otherwise whether the string form of `search` contains the string form of `item`, ignoring case, which is true for an empty `item` and false when `item` is an array or an object |
| `startsWith(s, prefix)`, `endsWith(s, suffix)` | ignoring case, both arguments converted to strings; false when either is an array or an object |
| `format(template, ...args)` | the template, converted to a string, with `{0}`, `{1}` replaced by the string forms of the arguments; `{{` and `}}` are literal braces |
| `join(array, separator)` | the elements converted to strings and joined, so an empty array gives the empty string; the separator defaults to `,`, and one that is an array or an object is `,`; any other value is returned as its string form, except an object, which gives the empty string |
| `toJSON(value)` | JSON with two-space indentation, `": "` between a name and its value, and `[]` and `{}` for empty structures; numbers in their string form, so `NaN` and `1E+21` appear as such; strings escaped with `\"`, `\\`, `\b`, `\f`, `\n`, `\r` and `\t`, and `\u` with four lowercase hexadecimal digits for the other control characters and for U+0085, U+2028 and U+2029 |
| `fromJSON(string)` | the argument converted to a string and parsed as JSON (RFC 8259, with nothing after the value); numbers become doubles, and a property whose name equals an earlier one ignoring case replaces the earlier value, which keeps its name and position; invalid JSON is an evaluation error |
| `case(pred1, value1, ..., default)` | the value after the first predicate that is `true`, or the last argument when none is; a predicate that is not a boolean is an evaluation error |
| `hashFiles(...patterns)` | SHA-256 over the SHA-256 of each matched file in the order the glob walk finds them, relative to the workspace; directories and files outside the workspace are skipped; `!` excludes; empty string when nothing matches; 1 to 255 patterns, each the string form of its argument; evaluated on the agent only |
| `success()`, `failure()`, `cancelled()`, `always()` | status functions, see below |
| `plugin(name, ...args)` | a function contributed by a plugin, evaluated on the server only ([10-plugins.md](10-plugins.md)) |

In the template of `format`, a `{` that is not doubled starts a placeholder: the number of an
argument, from `0` to `255` in decimal digits, then `}`. A placeholder with format specifiers,
such as `{0:x}`, a number beyond the arguments given, and a single brace that does not form a
placeholder are evaluation errors; `{0:}` is `{0}`. Arguments after the template need not be used.

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
them, from the root of the file: `jobs.<job_id>.steps.run` is the `run` of any step, and
`runs.pre-if` the `pre-if` of an action's metadata ([02-workflows.md](02-workflows.md)). The table
in the crate holds the keys of the milestones reached so far ([../plan.md](../plan.md)). A key the
table does not list takes no expression, such as a step's `shell` or `uses`, as in the target
([02-workflows.md](02-workflows.md)).

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
| `inputs.<input_id>.default` | agent | when the step that uses the action is reached |
| `runs.pre-if` | agent | when the pre step of the action is reached |
| `runs.post-if` | agent | when the post step of the action is reached |

Expanding a matrix when the `needs` are complete is what makes a matrix computed by an earlier job
(`fromJSON(needs.setup.outputs.matrix)`) work.

The three keys of an action's metadata see what the runner of the target gives them. The default
of an input sees `github`, `job`, `runner`, `strategy` and `matrix` and calls `hashFiles`, as the
target's schema of action metadata says, so `${{ github.token }}` and `${{ runner.debug == '1' }}`
are defaults that public actions use, while `secrets`, `env` and `steps` are refused. `pre-if` and
`post-if` are read like a step's `if` and see what it sees, except that `pre-if` cannot see
`steps`, since no step has run before it.

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

## Evaluation

The evaluator walks the tree from the left and evaluates only what decides the value. `&&` and
`||` stop at the operand that decides them; a lookup on a value that is neither an array nor an
object does not evaluate its index; `contains`, `startsWith` and `endsWith` evaluate their second
argument only when the first can match; `join` evaluates its separator only when there are two
elements; `format` evaluates an argument when the template first names it, and once; `case`
evaluates the predicates up to the first true one and then only its value. So
`false && fromJSON('x')` is `false`, and `format('{0}', github.sha, fromJSON('x'))` succeeds.

A context is resolved when the evaluation first reaches it, and at most once per expression or
template; the caller is never asked for a context the expression does not name. The status
functions and `hashFiles` are asked of the caller in the same way: the server answers the status
functions in a job's `if`, and the agent those in a step's `if` and `hashFiles`; `always()` is true
without asking.

Before it evaluates anything, the evaluator applies the static checks that do not depend on the
key: unknown contexts and functions, the number of arguments and `plugin()`. An expression that
fails them never evaluates, even partly, and fails with the code of the check.

### Evaluation errors

An expression that fails at run time (invalid JSON for `fromJSON`, a `timeout-minutes` that is not
a number, a `runs-on` that is neither a string nor a list) fails the job when the server evaluates
it, and the step when the agent does. The error carries the span of the expression, so the run page
points at the line; an error of the evaluator itself is at the span of the call at fault.

| Code | Failure |
|------|---------|
| `function_failed` | a function fails: `fromJSON` of text that is not JSON, a `format` template that is not valid or names an argument that is not given, a `case` predicate that is not a boolean, or `hashFiles` failing on the agent |

A status function or `hashFiles` that the caller does not supply fails with
`unavailable_function`.

## Implementation

`bosun-expr` holds a hand-written lexer, a precedence-climbing parser producing the syntax tree with
spans, the availability table with the static checks, and an evaluator over a `Value` enum. The
tree, which is `Expr`, the segments of a `Template` and the `Condition` of an `if`, is plain data in
`bosun-types`, so the IR holds it. Arrays and objects of a `Value` are shared, so a lookup returns
the instance it found and equality can compare instances. Contexts and the status functions and
`hashFiles` are supplied through one trait, `Scope`, so the server can resolve `needs` from the
database and the agent can resolve `steps` from its own state, each lazily. The crate does no
I/O: `hashFiles` is a hook the agent implements. The crate builds for WebAssembly, so the editor
offers the same completion and the same diagnostics as the server.

The cases live in `contracts/fixtures/expr/` as JSON files, each a list of cases:

- a syntax case has `expression` and the `tree` it parses to, or its `error` with `code`,
  `message` and `span`;
- a template case has `template` and its `segments`, or its `error`;
- an `if` case has `if` and its `condition`, which is `null` when it sets none, or its `error`;
- a check case has `expression`, a `key` of the availability table and the `errors` of the static
  checks;
- an evaluation case has `expression`, `contexts`, and `result` or `error`;
- a rendering case has `template`, `contexts`, and the rendered text as `result`, or its `error`;
- an evaluation or a rendering case that a hosted runner cannot reproduce also has
  `"hosted": false` ([Recordings on hosted runners](#recordings-on-hosted-runners)).

`contexts` maps the name of each context the case supplies to its value, and every context it
supplies is one the expression names. Evaluation and rendering cases run without the status
functions and `hashFiles`, which belong to the server and the agent. JSON has no `NaN` and no
infinities, so a case whose result would be one shows it through its string form, as in
`format('{0}', NaN)`.

`availability.json` beside them is the availability table.

### Recordings on hosted runners

The evaluation and rendering cases also run on GitHub's hosted runners, in a public repository whose
hosted minutes cost nothing, and a difference between a recorded result and Bosun's is a failing
test. `hack/compat-recordings.py generate` turns the cases into the workflow
`compat/recordings/expr.yml` for that repository, its `import` turns the log of a run of it into
`compat/recordings/expr.json`, and a test of `bosun-expr` compares every recording with Bosun
([compat/README.md](../../compat/README.md)).

A workflow gives a job no contexts but those it sets itself: the job's `env` and its matrix. A case
is recorded when all of the following hold, and every other evaluation or rendering case carries
`"hosted": false`; a test of `bosun-expr` checks that a case carries it exactly when one of them
fails:

- it supplies no context but `env` and `matrix`. The others, `github` first, are the runner's own,
  so a case that needs an arbitrary value supplies it as `matrix`, whose values may be any JSON,
  or a string as `env`;
- `env` maps names that are not empty to strings without U+0000, since the job's environment holds
  them;
- `matrix` has at least one property, and none is empty, `include` or `exclude`, since each property
  becomes a vector with one value;
- no object in a supplied value has two properties whose names are equal ignoring case, which an
  object of the target cannot hold, and no supplied string holds `${{`, which the target would
  evaluate;
- wrapped as the workflow evaluates it, every expression passes the static checks at
  `jobs.<job_id>.steps.env` and names no `bosun`. The target refuses a workflow with such an error
  before any job runs, so a case whose outcome is an error of the static checks is not recorded;
- a rendering case renders no U+0000.

The workflow has one job for each distinct `contexts`, which sets them as its `env` and as a matrix
with one combination, and a step for each case, at most 100 to a job. The step's `env` evaluates
`[${{ toJSON(<expression>) }}]` for an evaluation case and `[<template>]` for a rendering case, so
text always surrounds the template and every value becomes text exactly as a template renders it,
and the step prints that value in base64 on a line `bosun-result <case> <digest> <value>`. A step
whose `env` fails to evaluate fails and prints nothing, and the last step of the job, which runs in
any case, prints `bosun-outcome <case> <digest> <outcome>` for each. A case is named by its file and
its position, as `lookup.json[30]`, and its digest is the first 12 hexadecimal digits of the SHA-256
of its kind, its text and its contexts, so the importer refuses the log of a run of other cases than
the commit holds. A recording keeps the case's name, its `expression` or `template`, its `contexts`
and either `output`, what the runner printed between the brackets, or `failed: true`. The test
evaluates `toJSON(<expression>)` or renders the template with the recorded contexts, and fails,
naming the case, when Bosun prints other text, fails where the runner did not, or succeeds where the
runner failed. A recording stands on its own, so it stays valid when the cases change later.
