use std::collections::{BTreeMap, BTreeSet, HashMap};

use saphyr_parser::{Event, Parser, ScalarStyle};
use serde_json::{Map, Number, Value};

use crate::documents::{arrivals, extensions, milestone, surface_place};
use crate::fixtures::{cases, fixtures, json};
use crate::{FILE, read};

const PLACES: &[&str] = &["on", "workflow_dispatch", "job", "step"];

fn schema() -> Value {
    json(&fixtures().join("../schema/workflow.json"))
}

fn scalar(text: &str) -> Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    let radix = [("0x", 16), ("0o", 8)].iter().find_map(|(prefix, radix)| {
        text.strip_prefix(prefix)
            .and_then(|digits| i64::from_str_radix(digits, *radix).ok())
    });
    if let Some(value) = radix.or_else(|| text.parse::<i64>().ok()) {
        return Value::Number(value.into());
    }
    let decimal = text
        .trim_start_matches(['-', '+'])
        .starts_with(|c: char| c.is_ascii_digit() || c == '.');
    match text
        .parse::<f64>()
        .ok()
        .filter(|_| decimal)
        .and_then(Number::from_f64)
    {
        Some(number) => Value::Number(number),
        None => Value::String(text.to_string()),
    }
}

enum Open {
    List(Vec<Value>, usize),
    Object(Map<String, Value>, Option<String>, usize),
}

fn key(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn yaml(source: &str) -> Value {
    let mut open: Vec<Open> = Vec::new();
    let mut anchors: HashMap<usize, Value> = HashMap::new();
    let mut root = Value::Null;
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    for event in Parser::new_from_str(source) {
        let (event, _) = event.expect("well-formed YAML");
        let (value, anchor) = match event {
            Event::Scalar(text, style, anchor, _) => {
                let value = if style == ScalarStyle::Plain {
                    scalar(&text)
                } else {
                    Value::String(text.into_owned())
                };
                (value, anchor)
            }
            Event::Alias(anchor) => (anchors[&anchor].clone(), 0),
            Event::SequenceStart(anchor, _) => {
                open.push(Open::List(Vec::new(), anchor));
                continue;
            }
            Event::MappingStart(anchor, _) => {
                open.push(Open::Object(Map::new(), None, anchor));
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => match open.pop() {
                Some(Open::List(items, anchor)) => (Value::Array(items), anchor),
                Some(Open::Object(entries, _, anchor)) => (Value::Object(entries), anchor),
                None => continue,
            },
            _ => continue,
        };
        if anchor != 0 {
            anchors.insert(anchor, value.clone());
        }
        match open.last_mut() {
            None => root = value,
            Some(Open::List(items, _)) => items.push(value),
            Some(Open::Object(entries, pending, _)) => match pending.take() {
                Some(name) => {
                    entries.insert(name, value);
                }
                None => *pending = Some(key(&value)),
            },
        }
    }
    root
}

fn annotated(value: &Value, pointer: &mut Vec<String>, found: &mut Vec<(Vec<String>, Value)>) {
    match value {
        Value::Object(members) => {
            if members.contains_key("x-bosun-from") {
                found.push((pointer.clone(), value.clone()));
            }
            for (name, member) in members {
                pointer.push(name.clone());
                annotated(member, pointer, found);
                pointer.pop();
            }
        }
        Value::Array(items) => {
            for (position, item) in items.iter().enumerate() {
                pointer.push(position.to_string());
                annotated(item, pointer, found);
                pointer.pop();
            }
        }
        _ => {}
    }
}

#[test]
fn the_schema_is_valid_json_schema() {
    jsonschema::meta::validate(&schema()).expect("a valid JSON Schema");
}

#[test]
fn workflow_fixtures_the_parser_accepts_validate_and_unknown_keys_do_not() {
    let validator = jsonschema::validator_for(&schema()).expect("a usable schema");
    let root = fixtures();
    let mut accepted = 0;
    let mut unknown = 0;
    for case in cases(&root.join("workflows"), "yml") {
        let source = read(&root.join("workflows").join(format!("{case}.yml")));
        let diagnostic = root.join("diagnostics").join(format!("{case}.json"));
        let code = diagnostic
            .exists()
            .then(|| json(&diagnostic)["code"].as_str().map(str::to_string))
            .flatten();
        if bosun_workflow::parse(FILE, &source).is_ok() {
            accepted += 1;
            let errors: Vec<String> = validator
                .iter_errors(&yaml(&source))
                .map(|error| format!("{} at {}", error, error.instance_path()))
                .collect();
            assert!(errors.is_empty(), "{case} does not validate: {errors:#?}");
        } else if code.as_deref() == Some("unknown_key") {
            unknown += 1;
            assert!(
                !validator.is_valid(&yaml(&source)),
                "{case} has an unknown key and validates"
            );
        }
    }
    assert!(accepted > 30, "only {accepted} fixtures lower");
    assert!(unknown >= 2, "only {unknown} fixtures have an unknown key");
}

#[test]
fn the_schema_lists_exactly_the_keys_of_the_tables_with_their_milestone() {
    let mut found = Vec::new();
    annotated(&schema(), &mut Vec::new(), &mut found);
    let mut listed = BTreeMap::new();
    let mut marked = BTreeSet::new();
    for (pointer, annotation) in found {
        let parts: Vec<&str> = pointer.iter().map(String::as_str).collect();
        let (place, key) = match parts.as_slice() {
            ["properties", key] => ("workflow", *key),
            ["$defs", place, "properties", key] if PLACES.contains(place) => (*place, *key),
            _ => panic!(
                "`x-bosun-from` at /{} is no key of the tables",
                pointer.join("/")
            ),
        };
        let from = annotation["x-bosun-from"]
            .as_str()
            .expect("a milestone")
            .to_string();
        milestone(&from);
        let previous = listed.insert((place.to_string(), key.to_string()), from);
        assert!(previous.is_none(), "`{key}` of {place} is listed twice");
        match annotation.get("x-bosun-extension") {
            None => {}
            Some(Value::Bool(true)) => {
                marked.insert((place.to_string(), key.to_string()));
            }
            Some(other) => panic!("`x-bosun-extension` of `{key}` of {place} is {other}"),
        }
    }
    let tables: BTreeMap<(String, String), String> = arrivals()
        .into_iter()
        .map(|(place, key, from)| ((surface_place(&place).to_string(), key), from))
        .collect();
    assert_eq!(listed, tables);
    let extensions: BTreeSet<(String, String)> = extensions()
        .into_iter()
        .map(|(place, key)| (surface_place(&place).to_string(), key))
        .collect();
    assert_eq!(marked, extensions);
}
