use std::cmp::Ordering;
use std::sync::Arc;

use bosun_types::ir::Comparison;

use crate::value::{self, Value};

enum Pair<'v> {
    Nulls,
    Booleans(bool, bool),
    Numbers(f64, f64),
    Strings(&'v str, &'v str),
    Instances(bool),
    Unrelated,
}

fn pair<'v>(left: &'v Value, right: &'v Value) -> Pair<'v> {
    match (left, right) {
        (Value::Null, Value::Null) => Pair::Nulls,
        (Value::Boolean(left), Value::Boolean(right)) => Pair::Booleans(*left, *right),
        (Value::Number(left), Value::Number(right)) => Pair::Numbers(*left, *right),
        (Value::String(left), Value::String(right)) => Pair::Strings(left, right),
        (Value::Array(left), Value::Array(right)) => Pair::Instances(Arc::ptr_eq(left, right)),
        (Value::Object(left), Value::Object(right)) => Pair::Instances(Arc::ptr_eq(left, right)),
        (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => {
            Pair::Unrelated
        }
        _ => Pair::Numbers(left.number(), right.number()),
    }
}

pub(crate) fn equal(left: &Value, right: &Value) -> bool {
    match pair(left, right) {
        Pair::Nulls => true,
        Pair::Booleans(left, right) => left == right,
        Pair::Numbers(left, right) => left == right,
        Pair::Strings(left, right) => value::same(left, right),
        Pair::Instances(same) => same,
        Pair::Unrelated => false,
    }
}

fn ordering(left: &Value, right: &Value) -> Option<Ordering> {
    match pair(left, right) {
        Pair::Booleans(left, right) => Some(left.cmp(&right)),
        Pair::Numbers(left, right) => left.partial_cmp(&right),
        Pair::Strings(left, right) => Some(value::order(left, right)),
        Pair::Nulls | Pair::Instances(_) | Pair::Unrelated => None,
    }
}

pub(crate) fn compare(operator: Comparison, left: &Value, right: &Value) -> bool {
    let is = |expected: Ordering| ordering(left, right) == Some(expected);
    match operator {
        Comparison::Equal => equal(left, right),
        Comparison::NotEqual => !equal(left, right),
        Comparison::Less => is(Ordering::Less),
        Comparison::Greater => is(Ordering::Greater),
        Comparison::LessOrEqual => equal(left, right) || is(Ordering::Less),
        Comparison::GreaterOrEqual => equal(left, right) || is(Ordering::Greater),
    }
}
