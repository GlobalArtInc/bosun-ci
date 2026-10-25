use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use bosun_expr::{Code, Context, Error, Scope, Status, Value};
use bosun_types::ir::{Expr, Segment, Template};
use serde_json::{Map, json};

use crate::error;

type Case = Map<String, serde_json::Value>;

struct Recorder {
    contexts: Value,
    asked: RefCell<Vec<&'static str>>,
}

impl Recorder {
    fn new(contexts: Value) -> Recorder {
        Recorder {
            contexts,
            asked: RefCell::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<&'static str> {
        self.asked.borrow().clone()
    }
}

impl Scope for Recorder {
    fn context(&self, context: Context) -> Value {
        self.asked.borrow_mut().push(context.name());
        field(&self.contexts, context.name()).unwrap_or(Value::Null)
    }
}

fn field(object: &Value, name: &str) -> Option<Value> {
    match object {
        Value::Object(pairs) => pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone()),
        _ => None,
    }
}

fn json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => json!(value),
        Value::Number(value) => json!(value),
        Value::String(value) => json!(value),
        Value::Array(items) => items.iter().map(json).collect(),
        Value::Object(pairs) => serde_json::Value::Object(
            pairs
                .iter()
                .map(|(name, value)| (name.clone(), json(value)))
                .collect(),
        ),
    }
}

fn names(expr: &Expr, found: &mut BTreeSet<String>) {
    match expr {
        Expr::Null { .. } | Expr::Boolean { .. } | Expr::Number { .. } | Expr::String { .. } => {}
        Expr::Context { name, .. } => {
            found.insert(name.to_ascii_lowercase());
        }
        Expr::Property { target, .. } | Expr::Filter { target, .. } => names(target, found),
        Expr::Not { operand, .. } => names(operand, found),
        Expr::Index { target, index, .. } => {
            names(target, found);
            names(index, found);
        }
        Expr::Compare { left, right, .. } => {
            names(left, found);
            names(right, found);
        }
        Expr::And { operands, .. }
        | Expr::Or { operands, .. }
        | Expr::Call {
            arguments: operands,
            ..
        } => {
            for operand in operands {
                names(operand, found);
            }
        }
    }
}

fn template_names(template: &Template) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for segment in template.segments() {
        if let Segment::Expression(expr) = segment {
            names(expr, &mut found);
        }
    }
    found
}

fn expression_names(expr: &Expr) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    names(expr, &mut found);
    found
}

fn text<'c>(label: &str, case: &'c Case, name: &str) -> &'c str {
    case[name]
        .as_str()
        .unwrap_or_else(|| panic!("{label}: `{name}` is a string"))
}

fn contexts(label: &str, ordered: &Value, named: &BTreeSet<String>) -> Value {
    let contexts = field(ordered, "contexts").expect("an evaluation case has contexts");
    let Value::Object(pairs) = &contexts else {
        panic!("{label}: `contexts` maps context names to values");
    };
    for (name, _) in pairs.iter() {
        assert!(
            named.contains(name),
            "{label}: the context `{name}` is supplied, but the case does not name it"
        );
    }
    contexts
}

fn outcome(label: &str, case: &Case, ordered: &Value, produced: Result<Value, Error>) {
    match (produced, field(ordered, "result")) {
        (Ok(produced), Some(expected)) => {
            assert!(
                produced == expected,
                "{label}: the result differs from the committed one; it is now\n{}",
                serde_json::to_string_pretty(&json(&produced)).expect("serialisable")
            );
        }
        (Err(produced), None) => assert_eq!(
            error(&produced),
            case["error"],
            "{label}: the error differs from the committed one"
        ),
        (Ok(produced), None) => panic!(
            "{label}: an error is expected, but it yields {}",
            serde_json::to_string_pretty(&json(&produced)).expect("serialisable")
        ),
        (Err(produced), Some(_)) => panic!(
            "{label}: a result is expected, but it fails with\n{}",
            serde_json::to_string_pretty(&error(&produced)).expect("serialisable")
        ),
    }
}

fn lazily(label: &str, recorder: &Recorder, named: &BTreeSet<String>) {
    let asked = recorder.asked();
    let distinct: BTreeSet<&str> = asked.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        asked.len(),
        "{label}: a context is asked for twice: {asked:?}"
    );
    for name in distinct {
        assert!(
            named.contains(name),
            "{label}: the scope is asked for `{name}`, which the case does not name"
        );
    }
}

pub fn expression(label: &str, case: &Case, ordered: &Value) {
    let source = text(label, case, "expression");
    let expr = bosun_expr::expression(source)
        .unwrap_or_else(|failure| panic!("{label}: the expression does not parse: {failure:?}"));
    let named = expression_names(&expr);
    let recorder = Recorder::new(contexts(label, ordered, &named));
    outcome(label, case, ordered, bosun_expr::evaluate(&expr, &recorder));
    lazily(label, &recorder, &named);
}

pub fn template(label: &str, case: &Case, ordered: &Value) {
    let source = text(label, case, "template");
    let template = bosun_expr::template(source)
        .unwrap_or_else(|failure| panic!("{label}: the template does not parse: {failure:?}"));
    let named = template_names(&template);
    let recorder = Recorder::new(contexts(label, ordered, &named));
    outcome(
        label,
        case,
        ordered,
        bosun_expr::render(&template, &recorder).map(Value::String),
    );
    lazily(label, &recorder, &named);
}

fn object(pairs: &[(&str, Value)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect::<Vec<_>>()
            .into(),
    )
}

fn evaluate(source: &str, scope: &dyn Scope) -> Result<Value, Error> {
    let expr = bosun_expr::expression(source).expect("the expression parses");
    bosun_expr::evaluate(&expr, scope)
}

fn everything() -> Value {
    let inner = object(&[("a", Value::Number(1.0)), ("b", Value::Number(2.0))]);
    object(&[
        ("github", inner.clone()),
        ("env", inner.clone()),
        ("vars", inner.clone()),
        ("inputs", inner),
    ])
}

#[test]
fn a_context_is_asked_for_when_the_evaluation_reaches_it_and_only_once() {
    let cases: Vec<(&str, &[&str])> = vec![
        ("false && github.a", &[]),
        ("true || env.a", &[]),
        ("github.a || github.b", &["github"]),
        ("github.a && env.a && github.b", &["github", "env"]),
        ("vars.missing[github.a]", &["vars"]),
        ("format('x', env.a)", &[]),
        ("format('{1}{1}', env.a, github.a)", &["github"]),
        ("case(false, env.a, true, vars.b, inputs.c)", &["vars"]),
        ("join(fromJSON('[1]'), env.a)", &[]),
        ("join(fromJSON('[1, 2]'), env.a)", &["env"]),
        ("contains(github, env.a)", &["github"]),
        ("startsWith(fromJSON('[]'), env.a)", &[]),
        ("!github || inputs.a", &["github", "inputs"]),
    ];
    for (source, expected) in cases {
        let recorder = Recorder::new(everything());
        evaluate(source, &recorder).unwrap_or_else(|failure| panic!("{source}: {failure:?}"));
        assert_eq!(recorder.asked(), expected, "{source}");
    }
    let recorder = Recorder::new(everything());
    let template = bosun_expr::template("${{ github.a }}-${{ github.b }}").expect("a template");
    assert_eq!(
        bosun_expr::render(&template, &recorder).expect("it renders"),
        "1-2"
    );
    assert_eq!(recorder.asked(), ["github"]);
}

#[test]
fn a_lookup_yields_the_same_instance_each_time() {
    let recorder = Recorder::new(object(&[("github", object(&[("event", object(&[]))]))]));
    assert_eq!(
        evaluate("github.event == github.event", &recorder),
        Ok(Value::Boolean(true))
    );
    assert_eq!(
        evaluate("github.event == fromJSON('{}')", &recorder),
        Ok(Value::Boolean(false))
    );
}

#[test]
fn a_property_is_found_by_its_exact_name_before_ignoring_case() {
    let github = Value::Object(Arc::from(vec![
        ("name".to_string(), Value::Number(1.0)),
        ("Name".to_string(), Value::Number(2.0)),
    ]));
    let recorder = Recorder::new(object(&[("github", github)]));
    for (source, expected) in [
        ("github.name", 1.0),
        ("github.Name", 2.0),
        ("github.NAME", 1.0),
        ("github['Name']", 2.0),
    ] {
        assert_eq!(
            evaluate(source, &recorder),
            Ok(Value::Number(expected)),
            "{source}"
        );
    }
}

struct Hooks {
    failed: bool,
    patterns: RefCell<Vec<String>>,
}

impl Scope for Hooks {
    fn context(&self, _context: Context) -> Value {
        Value::Null
    }

    fn status(&self, status: Status) -> Option<bool> {
        Some(match status {
            Status::Success => !self.failed,
            Status::Failure => self.failed,
            Status::Cancelled => false,
        })
    }

    fn hash_files(&self, patterns: &[String]) -> Option<Result<String, String>> {
        self.patterns.borrow_mut().extend_from_slice(patterns);
        Some(if self.failed {
            Err("the workspace cannot be read".to_string())
        } else {
            Ok("e3b0c442".to_string())
        })
    }
}

#[test]
fn the_status_functions_and_hash_files_come_from_the_scope() {
    for failed in [false, true] {
        let hooks = Hooks {
            failed,
            patterns: RefCell::new(Vec::new()),
        };
        assert_eq!(evaluate("success()", &hooks), Ok(Value::Boolean(!failed)));
        assert_eq!(evaluate("FAILURE()", &hooks), Ok(Value::Boolean(failed)));
        assert_eq!(evaluate("cancelled()", &hooks), Ok(Value::Boolean(false)));
        assert_eq!(evaluate("always()", &hooks), Ok(Value::Boolean(true)));
        let hashed = evaluate("hashFiles('**/Cargo.lock', 1, null, 3.10)", &hooks);
        assert_eq!(
            hooks.patterns.borrow().as_slice(),
            ["**/Cargo.lock", "1", "", "3.1"]
        );
        if failed {
            let failure = hashed.expect_err("a failing hook fails the evaluation");
            assert_eq!(failure.code, Code::FunctionFailed);
            assert_eq!(
                failure.message,
                "`hashFiles` failed: the workspace cannot be read"
            );
            assert_eq!((failure.span.column, failure.span.end_column), (1, 42));
        } else {
            assert_eq!(hashed, Ok(Value::String("e3b0c442".to_string())));
        }
    }
}

#[test]
fn without_its_hook_a_status_function_or_hash_files_is_unavailable() {
    let recorder = Recorder::new(object(&[]));
    assert_eq!(evaluate("always()", &recorder), Ok(Value::Boolean(true)));
    for (source, name, end) in [
        ("success()", "success", 10),
        ("failure()", "failure", 10),
        ("cancelled()", "cancelled", 12),
        ("hashFiles('x')", "hashFiles", 15),
    ] {
        let failure = evaluate(source, &recorder).expect_err(source);
        assert_eq!(failure.code, Code::UnavailableFunction, "{source}");
        assert_eq!(
            failure.message,
            format!("function `{name}` is not available where this expression is evaluated")
        );
        assert_eq!((failure.span.column, failure.span.end_column), (1, end));
    }
}
