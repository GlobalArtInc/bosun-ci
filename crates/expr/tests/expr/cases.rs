use std::collections::BTreeSet;
use std::fmt::Debug;
use std::fs;

use bosun_expr::Error;
use bosun_types::ir::{Condition, Expr, Template};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{error, fixtures, key, load};

pub const AVAILABILITY: &str = "availability.json";

type Case = Map<String, Value>;

pub fn files() -> Vec<(String, Vec<Case>)> {
    let directory = fixtures();
    let mut names: Vec<String> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != AVAILABILITY)
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            assert!(
                name.ends_with(".json"),
                "expr/{name}: the expression fixtures are JSON files"
            );
            let cases = match load(&directory.join(&name)) {
                Value::Array(cases) => cases,
                other => panic!("expr/{name}: a list of cases is expected, not {other}"),
            };
            let cases = cases
                .into_iter()
                .enumerate()
                .map(|(index, case)| match case {
                    Value::Object(case) => case,
                    other => panic!("expr/{name}[{index}]: a case is an object, not {other}"),
                })
                .collect();
            (name, cases)
        })
        .collect()
}

fn pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("serialisable")
}

fn outcome<T>(label: &str, case: &Case, field: &str, result: Result<T, Error>)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    match (result, case.get(field)) {
        (Ok(produced), Some(expected)) => {
            let expected: T = serde_json::from_value(expected.clone()).unwrap_or_else(|failure| {
                panic!("{label}: `{field}` does not deserialise: {failure}")
            });
            let again: T =
                serde_json::from_value(serde_json::to_value(&produced).expect("serialisable"))
                    .expect("what bosun-types serialises, it deserialises");
            assert_eq!(
                again, produced,
                "{label}: the outcome changes in a JSON round trip"
            );
            assert!(
                produced == expected,
                "{label}: `{field}` differs from the committed one; it is now\n{}",
                pretty(&produced)
            );
        }
        (Err(produced), None) => assert_eq!(
            error(&produced),
            case["error"],
            "{label}: the error differs from the committed one"
        ),
        (Ok(produced), None) => panic!(
            "{label}: an error is expected, but it yields\n{}",
            pretty(&produced)
        ),
        (Err(produced), Some(_)) => panic!(
            "{label}: `{field}` is expected, but it fails with\n{}",
            pretty(&error(&produced))
        ),
    }
}

fn text<'c>(label: &str, case: &'c Case, field: &str) -> &'c str {
    case[field]
        .as_str()
        .unwrap_or_else(|| panic!("{label}: `{field}` is a string"))
}

fn run(label: &str, case: &Case) {
    let fields: BTreeSet<&str> = case.keys().map(String::as_str).collect();
    let shape = |names: &[&str]| fields == names.iter().copied().collect();
    if shape(&["expression", "tree"]) || shape(&["expression", "error"]) {
        let source = text(label, case, "expression");
        outcome::<Expr>(label, case, "tree", bosun_expr::expression(source));
    } else if shape(&["template", "segments"]) || shape(&["template", "error"]) {
        let source = text(label, case, "template");
        outcome::<Template>(label, case, "segments", bosun_expr::template(source));
    } else if shape(&["if", "condition"]) || shape(&["if", "error"]) {
        let source = text(label, case, "if");
        outcome::<Option<Condition>>(label, case, "condition", bosun_expr::condition(source));
    } else if shape(&["expression", "key", "errors"]) {
        let source = text(label, case, "expression");
        let expression = bosun_expr::expression(source).unwrap_or_else(|failure| {
            panic!("{label}: the expression does not parse: {failure:?}")
        });
        let produced: Vec<Value> = bosun_expr::check(&expression, key(text(label, case, "key")))
            .iter()
            .map(error)
            .collect();
        assert_eq!(
            Value::Array(produced.clone()),
            case["errors"],
            "{label}: the errors differ from the committed ones; they are now\n{}",
            pretty(&produced)
        );
    } else {
        panic!(
            "{label}: a case has `expression` with `tree` or `error`, `template` with `segments` or \
             `error`, `if` with `condition` or `error`, or `expression` with `key` and `errors`; \
             this one has {fields:?}"
        );
    }
}

#[test]
fn every_case_yields_its_committed_outcome() {
    let files = files();
    assert!(!files.is_empty(), "no expression cases");
    for (name, cases) in files {
        assert!(!cases.is_empty(), "expr/{name} holds no cases");
        for (index, case) in cases.iter().enumerate() {
            run(&format!("expr/{name}[{index}]"), case);
        }
    }
}

fn without_spans(value: Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .filter(|(name, _)| name != "span")
                .map(|(name, value)| (name, without_spans(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(without_spans).collect()),
        other => other,
    }
}

fn structure(condition: Option<Condition>) -> Value {
    match condition {
        Some(Condition::Expression(expression)) => {
            without_spans(serde_json::to_value(expression).expect("serialisable"))
        }
        other => panic!("an expression is expected, not {other:?}"),
    }
}

#[test]
fn the_three_forms_of_if_give_the_same_expression() {
    let bare = "github.ref == 'refs/heads/main' && !cancelled()";
    let expected = structure(bosun_expr::condition(bare).expect("a bare expression parses"));
    for form in [
        format!("${{{{ {bare} }}}}"),
        format!(" ${{{{ {bare} }}}} "),
        format!("${{{{{bare}}}}}\n"),
    ] {
        let produced = structure(bosun_expr::condition(&form).expect("the form parses"));
        assert_eq!(produced, expected, "{form:?}");
    }
    for template in [
        format!("a ${{{{ {bare} }}}}"),
        format!("${{{{ {bare} }}}}."),
    ] {
        assert!(
            matches!(
                bosun_expr::condition(&template),
                Ok(Some(Condition::Template(_)))
            ),
            "{template:?} is a template"
        );
    }
}
