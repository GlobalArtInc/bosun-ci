use bosun_types::ir::{Expr, Span};

use crate::compare;
use crate::error::Error;
use crate::evaluate::{Evaluation, Held, failed};
use crate::value::{Value, folded};

#[derive(Clone, Copy)]
pub(crate) enum Affix {
    Prefix,
    Suffix,
}

fn within(text: &[char], part: &[char]) -> bool {
    part.is_empty() || text.windows(part.len()).any(|window| window == part)
}

pub(crate) fn contains(
    evaluation: &mut Evaluation<'_>,
    arguments: &[Expr],
) -> Result<Value, Error> {
    let search = evaluation.value(&arguments[0])?;
    let found = match &search {
        Value::Array(items) if !items.is_empty() => {
            let item = evaluation.value(&arguments[1])?;
            items.iter().any(|element| compare::equal(&item, element))
        }
        Value::Array(_) | Value::Object(_) => false,
        _ => {
            let item = evaluation.value(&arguments[1])?;
            item.primitive() && within(&folded(&search.text()), &folded(&item.text()))
        }
    };
    Ok(Value::Boolean(found))
}

pub(crate) fn affix(
    evaluation: &mut Evaluation<'_>,
    arguments: &[Expr],
    affix: Affix,
) -> Result<Value, Error> {
    let text = evaluation.value(&arguments[0])?;
    if !text.primitive() {
        return Ok(Value::Boolean(false));
    }
    let part = evaluation.value(&arguments[1])?;
    if !part.primitive() {
        return Ok(Value::Boolean(false));
    }
    let (text, part) = (folded(&text.text()), folded(&part.text()));
    Ok(Value::Boolean(match affix {
        Affix::Prefix => text.starts_with(&part),
        Affix::Suffix => text.ends_with(&part),
    }))
}

pub(crate) fn join(evaluation: &mut Evaluation<'_>, arguments: &[Expr]) -> Result<Value, Error> {
    let items = evaluation.value(&arguments[0])?;
    let text = match &items {
        Value::Array(items) => match items.split_first() {
            None => String::new(),
            Some((first, rest)) => {
                let mut text = first.text().into_owned();
                if !rest.is_empty() {
                    let separator = match arguments.get(1) {
                        Some(argument) => {
                            let separator = evaluation.value(argument)?;
                            if separator.primitive() {
                                separator.text().into_owned()
                            } else {
                                ",".to_string()
                            }
                        }
                        None => ",".to_string(),
                    };
                    for item in rest {
                        text.push_str(&separator);
                        text.push_str(&item.text());
                    }
                }
                text
            }
        },
        Value::Object(_) => String::new(),
        primitive => primitive.text().into_owned(),
    };
    Ok(Value::String(text))
}

pub(crate) fn case(
    evaluation: &mut Evaluation<'_>,
    arguments: &[Expr],
    span: Span,
) -> Result<Held, Error> {
    let mut remaining = arguments;
    while let [predicate, value, rest @ ..] = remaining {
        match evaluation.value(predicate)? {
            Value::Boolean(true) => return evaluation.held(value),
            Value::Boolean(false) => remaining = rest,
            other => {
                return Err(failed(
                    span,
                    format!(
                        "a predicate of `case` must be a boolean, not {}",
                        other.kind()
                    ),
                ));
            }
        }
    }
    match remaining {
        [default] => evaluation.held(default),
        _ => Ok(Held::plain(Value::Null)),
    }
}

fn position(chars: &[char], from: usize, wanted: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == wanted)
        .map(|offset| from + offset)
}

fn argument(chars: &[char], start: usize) -> Option<(usize, usize)> {
    let length = chars[start..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if length == 0 {
        return None;
    }
    let digits: String = chars[start..start + length].iter().collect();
    let number: u8 = digits.parse().ok()?;
    Some((usize::from(number), start + length))
}

fn specifiers(chars: &[char], start: usize) -> Option<(String, usize)> {
    match chars.get(start) {
        Some('}') => Some((String::new(), start)),
        Some(':') => {
            let mut specifiers = String::new();
            let mut at = start + 1;
            loop {
                match chars.get(at) {
                    None => return None,
                    Some('}') if chars.get(at + 1) == Some(&'}') => {
                        specifiers.push('}');
                        at += 2;
                    }
                    Some('}') => return Some((specifiers, at)),
                    Some(&c) => {
                        specifiers.push(c);
                        at += 1;
                    }
                }
            }
        }
        _ => None,
    }
}

fn arguments_after(count: usize) -> String {
    match count {
        0 => "no argument follows it".to_string(),
        1 => "only 1 argument follows it".to_string(),
        count => format!("only {count} arguments follow it"),
    }
}

pub(crate) fn format(
    evaluation: &mut Evaluation<'_>,
    arguments: &[Expr],
    span: Span,
) -> Result<Value, Error> {
    let template = evaluation.value(&arguments[0])?.text().into_owned();
    let chars: Vec<char> = template.chars().collect();
    let given = arguments.len() - 1;
    let invalid = || {
        failed(
            span,
            format!(
                "the template of `format` is not valid: `{template}`; a brace that is not doubled \
                 encloses the number of an argument"
            ),
        )
    };
    let mut values: Vec<Option<String>> = vec![None; given];
    let mut text = String::new();
    let mut at = 0;
    while at < chars.len() {
        let open = position(&chars, at, '{');
        let close = position(&chars, at, '}');
        match open {
            Some(open) if close.is_none_or(|close| close > open) => {
                if chars.get(open + 1) == Some(&'{') {
                    text.extend(&chars[at..=open]);
                    at = open + 2;
                    continue;
                }
                if close.is_none_or(|close| close <= open + 1) {
                    return Err(invalid());
                }
                let (number, end) = argument(&chars, open + 1).ok_or_else(invalid)?;
                let (specifiers, close) = specifiers(&chars, end).ok_or_else(invalid)?;
                if number >= given {
                    return Err(failed(
                        span,
                        format!(
                            "the template of `format` refers to `{{{number}}}`, but {}",
                            arguments_after(given)
                        ),
                    ));
                }
                text.extend(&chars[at..open]);
                let value = match &values[number] {
                    Some(value) => value.clone(),
                    None => {
                        let value = evaluation
                            .value(&arguments[number + 1])?
                            .text()
                            .into_owned();
                        values[number] = Some(value.clone());
                        value
                    }
                };
                if !specifiers.is_empty() {
                    return Err(failed(
                        span,
                        format!("`format` does not support the format specifiers `{specifiers}`"),
                    ));
                }
                text.push_str(&value);
                at = close + 1;
            }
            _ => match close {
                Some(close) if chars.get(close + 1) == Some(&'}') => {
                    text.extend(&chars[at..=close]);
                    at = close + 2;
                }
                Some(_) => return Err(invalid()),
                None => {
                    text.extend(&chars[at..]);
                    at = chars.len();
                }
            },
        }
    }
    Ok(Value::String(text))
}
