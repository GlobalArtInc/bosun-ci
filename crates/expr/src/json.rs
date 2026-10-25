use crate::number;
use crate::value::Value;

pub(crate) fn write(value: &Value) -> String {
    let mut out = String::new();
    element(value, 0, &mut out);
    out
}

pub(crate) fn read(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|error| error.to_string())
}

fn line(level: usize, out: &mut String) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn element(value: &Value, level: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Boolean(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => out.push_str(&number::text(*value)),
        Value::String(value) => quote(value, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(pairs) if pairs.is_empty() => out.push_str("{}"),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                line(level + 1, out);
                element(item, level + 1, out);
            }
            line(level, out);
            out.push(']');
        }
        Value::Object(pairs) => {
            out.push('{');
            for (index, (name, item)) in pairs.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                line(level + 1, out);
                quote(name, out);
                out.push_str(": ");
                element(item, level + 1, out);
            }
            line(level, out);
            out.push('}');
        }
    }
}

fn quote(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0}'..='\u{1f}' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
