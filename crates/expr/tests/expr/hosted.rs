use std::collections::BTreeSet;

use bosun_types::ir::Segment;
use serde_json::{Map, Value, json};

use crate::cases::{HOSTED, files};
use crate::evaluation::{expression_names, template_names};
use crate::key;

const STEP_ENV: &str = "jobs.<job_id>.steps.env";

type Case = Map<String, Value>;

fn supplied(value: &Value, at: &str, found: &mut Vec<String>) {
    match value {
        Value::String(text) if text.contains("${{") => found.push(format!(
            "`{at}` holds `${{{{`, which the target evaluates as an expression"
        )),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                supplied(item, &format!("{at}[{index}]"), found);
            }
        }
        Value::Object(pairs) => {
            let mut names = BTreeSet::new();
            for (name, item) in pairs {
                if !names.insert(name.to_uppercase()) {
                    found.push(format!(
                        "`{at}` has two properties named `{name}` ignoring case, which an object \
                         of the target cannot hold"
                    ));
                }
                supplied(item, &format!("{at}.{name}"), found);
            }
        }
        _ => {}
    }
}

fn contexts(case: &Case, found: &mut Vec<String>) {
    let Some(Value::Object(contexts)) = case.get("contexts") else {
        found.push("its `contexts` is not a mapping".to_string());
        return;
    };
    for (name, value) in contexts {
        match (name.as_str(), value) {
            ("env", Value::Object(variables)) => {
                for (variable, value) in variables {
                    match value {
                        Value::String(text) if text.contains('\0') => found.push(format!(
                            "`env.{variable}` holds U+0000, which no environment variable can"
                        )),
                        Value::String(_) if !variable.is_empty() => {}
                        Value::String(_) => {
                            found.push("`env` has a variable without a name".to_string());
                        }
                        _ => found.push(format!(
                            "`env.{variable}` is not a string, and an environment holds strings"
                        )),
                    }
                }
            }
            ("matrix", Value::Object(vectors)) if !vectors.is_empty() => {
                for vector in vectors.keys() {
                    if vector.is_empty()
                        || vector.eq_ignore_ascii_case("include")
                        || vector.eq_ignore_ascii_case("exclude")
                    {
                        found.push(format!(
                            "`matrix` has a property `{vector}`, which no vector of a matrix can \
                             be named"
                        ));
                    }
                }
            }
            ("env" | "matrix", _) => found.push(format!(
                "`{name}` is not a mapping with a property, which a job of the target can be \
                 given"
            )),
            (other, _) => found.push(format!(
                "it supplies `{other}`, which a hosted runner cannot be given"
            )),
        }
        supplied(value, name, found);
    }
}

pub fn obstacles(case: &Case) -> Vec<String> {
    let mut found = Vec::new();
    contexts(case, &mut found);
    let parsed = match (case.get("expression"), case.get("template")) {
        (Some(Value::String(expression)), None) => {
            bosun_expr::expression(&format!("toJSON({expression})")).map(|expr| {
                let names = expression_names(&expr);
                (vec![expr], names)
            })
        }
        (None, Some(Value::String(template))) => bosun_expr::template(template).map(|template| {
            let names = template_names(&template);
            let expressions = template
                .segments()
                .iter()
                .filter_map(|segment| match segment {
                    Segment::Expression(expr) => Some(expr.clone()),
                    Segment::Text(_) => None,
                })
                .collect();
            (expressions, names)
        }),
        _ => {
            found.push("it has neither an `expression` nor a `template`".to_string());
            return found;
        }
    };
    match parsed {
        Ok((expressions, names)) => {
            for expr in &expressions {
                for error in bosun_expr::check(expr, key(STEP_ENV)) {
                    found.push(format!(
                        "the target refuses the workflow before it runs, since {}",
                        error.message
                    ));
                }
            }
            if names.contains("bosun") {
                found.push("it names `bosun`, a context the target does not have".to_string());
            }
        }
        Err(error) => found.push(format!(
            "the step that evaluates it does not parse: {}",
            error.message
        )),
    }
    if case.contains_key("template")
        && case
            .get("result")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains('\0'))
    {
        found.push("it renders U+0000, which no environment variable can hold".to_string());
    }
    found
}

#[test]
fn a_case_is_marked_exactly_when_a_hosted_runner_cannot_reproduce_it() {
    let mut reproduced = 0;
    for (name, cases) in files() {
        for (index, case) in cases.iter().enumerate() {
            if !case.contains_key("contexts") {
                continue;
            }
            let label = format!("expr/{name}[{index}]");
            let obstacles = obstacles(case);
            if obstacles.is_empty() {
                assert!(
                    !case.contains_key(HOSTED),
                    "{label}: a hosted runner reproduces it, so it carries no `{HOSTED}`"
                );
                reproduced += 1;
            } else {
                assert!(
                    case.contains_key(HOSTED),
                    "{label}: a hosted runner cannot reproduce it: {}; mark it with \
                     `\"{HOSTED}\": false`",
                    obstacles.join("; ")
                );
            }
        }
    }
    assert!(reproduced > 0, "no case can be recorded on a hosted runner");
}

#[test]
fn the_rule_finds_every_obstacle() {
    let cases = [
        (
            json!({"expression": "github.sha", "contexts": {"github": {"sha": "a"}}}),
            "it supplies `github`",
        ),
        (
            json!({"expression": "env.A", "contexts": {"env": {"A": 1}}}),
            "`env.A` is not a string",
        ),
        (
            json!({"expression": "env.A", "contexts": {"env": {"A": "a\u{0}b"}}}),
            "`env.A` holds U+0000",
        ),
        (
            json!({"expression": "matrix.a", "contexts": {"matrix": {}}}),
            "`matrix` is not a mapping with a property",
        ),
        (
            json!({"expression": "matrix.include", "contexts": {"matrix": {"include": [1]}}}),
            "`matrix` has a property `include`",
        ),
        (
            json!({"expression": "matrix.a", "contexts": {"matrix": {"a": {"b": 1, "B": 2}}}}),
            "`matrix.a` has two properties named",
        ),
        (
            json!({"expression": "matrix.a", "contexts": {"matrix": {"a": ["${{ 1 }}"]}}}),
            "`matrix.a[0]` holds `${{`",
        ),
        (
            json!({"expression": "nope()", "contexts": {}}),
            "the target refuses the workflow before it runs, since unknown function `nope`",
        ),
        (
            json!({"expression": "success()", "contexts": {}}),
            "the target refuses the workflow before it runs",
        ),
        (
            json!({"expression": "bosun.url", "contexts": {}}),
            "it names `bosun`",
        ),
        (
            json!({"template": "${{ 1", "contexts": {}}),
            "the step that evaluates it does not parse",
        ),
        (
            json!({"template": "${{ format('{0}', fromJSON('\"\\u0000\"')) }}", "contexts": {}, "result": "\u{0}"}),
            "it renders U+0000",
        ),
    ];
    for (case, expected) in cases {
        let case = case.as_object().expect("a case is an object").clone();
        let found = obstacles(&case);
        assert!(
            found.iter().any(|obstacle| obstacle.contains(expected)),
            "{case:?}: expected an obstacle saying `{expected}`, found {found:?}"
        );
    }
    let reproducible = json!({
        "expression": "matrix.list[0] == env.A",
        "contexts": {"matrix": {"list": [1, {"x": null}]}, "env": {"A": "1"}}
    });
    assert_eq!(
        obstacles(reproducible.as_object().expect("a case is an object")),
        Vec::<String>::new()
    );
}
