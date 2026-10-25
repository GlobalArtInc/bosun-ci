use std::collections::BTreeMap;

use bosun_types::ir::{Input, InputKind};
use serde_json::{Map, Number, Value};

use crate::request::Faults;

const EXACT: f64 = 9_007_199_254_740_992.0;

#[derive(Default)]
pub(crate) struct Inputs {
    pub(crate) typed: Map<String, Value>,
    pub(crate) texts: BTreeMap<String, Value>,
}

pub(crate) fn check(
    declared: &[Input],
    given: &BTreeMap<String, Value>,
    faults: &mut Faults,
) -> Inputs {
    let mut inputs = Inputs::default();
    let mut refused = BTreeMap::new();
    for input in declared {
        let taken = match given.get(&input.name) {
            Some(value) => accept(input, value).map(Some),
            None => match preset(&input.kind) {
                Some(taken) => Ok(Some(taken)),
                None if input.required => Err(format!(
                    "the input `{}` is required, and has no default",
                    input.name
                )),
                None => Ok(None),
            },
        };
        match taken {
            Ok(Some((typed, text))) => {
                inputs.typed.insert(input.name.clone(), typed);
                inputs.texts.insert(input.name.clone(), Value::String(text));
            }
            Ok(None) => {}
            Err(message) => {
                refused.insert(input.name.clone(), message);
            }
        }
    }
    for name in given.keys() {
        if !declared.iter().any(|input| &input.name == name) {
            refused.insert(
                name.clone(),
                format!("the workflow declares no input `{name}`"),
            );
        }
    }
    for (name, message) in refused {
        faults.add(format!("inputs.{name}"), message);
    }
    inputs
}

fn accept(input: &Input, value: &Value) -> Result<(Value, String), String> {
    let name = &input.name;
    let shown = shown(value);
    match (&input.kind, value) {
        (InputKind::String { .. } | InputKind::Environment { .. }, Value::String(text)) => {
            Ok((value.clone(), text.clone()))
        }
        (InputKind::String { .. }, _) => Err(format!(
            "the input `{name}` is a string, and `{shown}` is not text"
        )),
        (InputKind::Environment { .. }, _) => Err(format!(
            "the input `{name}` names an environment, and `{shown}` is not text"
        )),
        (InputKind::Choice { options, .. }, Value::String(text)) if options.contains(text) => {
            Ok((value.clone(), text.clone()))
        }
        (InputKind::Choice { options, .. }, _) => Err(format!(
            "the input `{name}` is one of {}, and `{shown}` is not",
            choices(options)
        )),
        (InputKind::Boolean { .. }, Value::Bool(flag)) => {
            Ok((Value::Bool(*flag), flag.to_string()))
        }
        (InputKind::Boolean { .. }, Value::String(text)) if text == "true" || text == "false" => {
            Ok((Value::Bool(text == "true"), text.clone()))
        }
        (InputKind::Boolean { .. }, _) => Err(format!(
            "the input `{name}` is a boolean, which is `true` or `false`, and `{shown}` is neither"
        )),
        (InputKind::Number { .. }, Value::Number(given)) => {
            let text = if given.is_f64() {
                given.as_f64().map(written).unwrap_or_default()
            } else {
                given.to_string()
            };
            Ok((number(given.as_f64().unwrap_or_default()), text))
        }
        (InputKind::Number { .. }, Value::String(text)) => match read(text) {
            Some(parsed) => Ok((number(parsed), text.clone())),
            None => Err(format!(
                "the input `{name}` is a number, and `{shown}` does not read as one"
            )),
        },
        (InputKind::Number { .. }, _) => Err(format!(
            "the input `{name}` is a number, and `{shown}` is not one"
        )),
    }
}

fn preset(kind: &InputKind) -> Option<(Value, String)> {
    match kind {
        InputKind::String { default }
        | InputKind::Environment { default }
        | InputKind::Choice { default, .. } => default
            .as_ref()
            .map(|text| (Value::String(text.clone()), text.clone())),
        InputKind::Boolean { default } => default.map(|flag| (Value::Bool(flag), flag.to_string())),
        InputKind::Number { default } => default.map(|value| (number(value), written(value))),
    }
}

fn read(text: &str) -> Option<f64> {
    if text.trim() != text {
        return None;
    }
    serde_json::from_str::<Number>(text)
        .ok()
        .and_then(|number| number.as_f64())
        .filter(|value| value.is_finite())
}

fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < EXACT {
        Value::from(value as i64)
    } else {
        Number::from_f64(value).map_or(Value::Null, Value::Number)
    }
}

fn written(value: f64) -> String {
    format!("{value}")
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn choices(options: &[String]) -> String {
    let quoted: Vec<String> = options.iter().map(|option| format!("`{option}`")).collect();
    match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => "no option".to_owned(),
    }
}
