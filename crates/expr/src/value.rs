use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::de::{Deserialize, Deserializer, Error, MapAccess, SeqAccess, Visitor};

use crate::number;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Arc<[Value]>),
    Object(Arc<[(String, Value)]>),
}

impl Value {
    pub(crate) fn primitive(&self) -> bool {
        !matches!(self, Value::Array(_) | Value::Object(_))
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Boolean(_) => "a boolean",
            Value::Number(_) => "a number",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
            Value::Object(_) => "an object",
        }
    }

    pub(crate) fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Boolean(value) => *value,
            Value::Number(value) => *value != 0.0 && !value.is_nan(),
            Value::String(value) => !value.is_empty(),
            Value::Array(_) | Value::Object(_) => true,
        }
    }

    pub(crate) fn number(&self) -> f64 {
        match self {
            Value::Null => 0.0,
            Value::Boolean(value) => f64::from(u8::from(*value)),
            Value::Number(value) => *value,
            Value::String(value) => number::convert(value),
            Value::Array(_) | Value::Object(_) => f64::NAN,
        }
    }

    pub(crate) fn text(&self) -> Cow<'_, str> {
        match self {
            Value::Null => Cow::Borrowed(""),
            Value::Boolean(true) => Cow::Borrowed("true"),
            Value::Boolean(false) => Cow::Borrowed("false"),
            Value::Number(value) => Cow::Owned(number::text(*value)),
            Value::String(value) => Cow::Borrowed(value),
            Value::Array(_) => Cow::Borrowed("Array"),
            Value::Object(_) => Cow::Borrowed("Object"),
        }
    }
}

pub(crate) fn upper(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let mut mapped = c.to_uppercase();
    match (mapped.next(), mapped.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

pub(crate) fn folded(text: &str) -> Vec<char> {
    text.chars().map(upper).collect()
}

pub(crate) fn same(left: &str, right: &str) -> bool {
    left.chars().map(upper).eq(right.chars().map(upper))
}

fn units(text: &str) -> Vec<u16> {
    let mut units = Vec::with_capacity(text.len());
    let mut buffer = [0; 2];
    for c in text.chars() {
        units.extend_from_slice(upper(c).encode_utf16(&mut buffer));
    }
    units
}

pub(crate) fn order(left: &str, right: &str) -> Ordering {
    units(left).cmp(&units(right))
}

pub(crate) fn lookup<'v>(pairs: &'v [(String, Value)], name: &str) -> Option<&'v Value> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .or_else(|| pairs.iter().find(|(key, _)| same(key, name)))
        .map(|(_, value)| value)
}

fn object(entries: Vec<(String, Value)>) -> Value {
    let mut positions: BTreeMap<String, usize> = BTreeMap::new();
    let mut pairs: Vec<(String, Value)> = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        let key: String = name.chars().map(upper).collect();
        match positions.get(&key) {
            Some(&at) => pairs[at].1 = value,
            None => {
                positions.insert(key, pairs.len());
                pairs.push((name, value));
            }
        }
    }
    Value::Object(pairs.into())
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        Value::deserialize(deserializer)
    }

    fn visit_bool<E: Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Boolean(value))
    }

    fn visit_i64<E: Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value as f64))
    }

    fn visit_u64<E: Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value as f64))
    }

    fn visit_f64<E: Error>(self, value: f64) -> Result<Value, E> {
        Ok(Value::Number(value))
    }

    fn visit_str<E: Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_string()))
    }

    fn visit_string<E: Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<Value, A::Error> {
        let mut values = Vec::with_capacity(items.size_hint().unwrap_or(0));
        while let Some(value) = items.next_element()? {
            values.push(value);
        }
        Ok(Value::Array(values.into()))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Value, A::Error> {
        let mut pairs = Vec::with_capacity(entries.size_hint().unwrap_or(0));
        while let Some(pair) = entries.next_entry::<String, Value>()? {
            pairs.push(pair);
        }
        Ok(object(pairs))
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}
