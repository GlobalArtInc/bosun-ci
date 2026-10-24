use std::collections::HashMap;

use bosun_types::ir::Span;
use saphyr_parser::{Event, Marker, Parser, ScalarStyle, Span as Range};

use crate::Code;

const MAX_DEPTH: usize = 64;
const MAX_ALIAS_NODES: usize = 100_000;

#[derive(Clone)]
pub(crate) struct Node {
    pub(crate) span: Span,
    pub(crate) value: Value,
}

#[derive(Clone)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Sequence(Vec<Node>),
    Mapping(Vec<(Node, Node)>),
}

pub(crate) struct Fault {
    pub(crate) code: Code,
    pub(crate) span: Span,
    pub(crate) message: String,
}

enum Frame {
    Sequence {
        start: Marker,
        anchor: usize,
        items: Vec<Node>,
    },
    Mapping {
        start: Marker,
        anchor: usize,
        entries: Vec<(Node, Node)>,
        key: Option<Node>,
    },
}

struct Loader {
    chars: Vec<char>,
    stack: Vec<Frame>,
    anchors: HashMap<usize, (Node, usize)>,
    expanded: usize,
    documents: usize,
    root: Option<Node>,
}

pub(crate) fn load(source: &str) -> Result<Node, Fault> {
    let mut loader = Loader {
        chars: source.chars().collect(),
        stack: Vec::new(),
        anchors: HashMap::new(),
        expanded: 0,
        documents: 0,
        root: None,
    };
    for event in Parser::new_from_str(source) {
        let (event, range) = event.map_err(|error| {
            let at = point(*error.marker());
            Fault {
                code: Code::YamlSyntax,
                span: span(at, at),
                message: format!("invalid YAML: {}", error.info()),
            }
        })?;
        loader.event(event, range)?;
    }
    Ok(loader.root.unwrap_or(Node {
        span: span((1, 1), (1, 1)),
        value: Value::Null,
    }))
}

impl Loader {
    fn event(&mut self, event: Event<'_>, range: Range) -> Result<(), Fault> {
        match event {
            Event::DocumentStart(_) => {
                self.documents += 1;
                if self.documents > 1 {
                    return Err(fault(
                        Code::UnsupportedYaml,
                        range,
                        "a workflow file holds exactly one YAML document",
                    ));
                }
            }
            Event::Alias(anchor) => {
                let Some((node, size)) = self.anchors.get(&anchor) else {
                    return Err(fault(
                        Code::YamlSyntax,
                        range,
                        "invalid YAML: an alias refers to a node that contains it",
                    ));
                };
                self.expanded += size;
                if self.expanded > MAX_ALIAS_NODES {
                    return Err(fault(
                        Code::LimitExceeded,
                        range,
                        &format!("aliases expand to more than {MAX_ALIAS_NODES} nodes"),
                    ));
                }
                let mut node = node.clone();
                node.span = span(point(range.start), point(range.end));
                self.complete(node, 0);
            }
            Event::Scalar(text, style, anchor, tag) => {
                if tag.is_some() {
                    return Err(fault(
                        Code::UnsupportedYaml,
                        range,
                        "tags are not supported",
                    ));
                }
                let value = if style == ScalarStyle::Plain {
                    resolve(&text)
                } else {
                    Value::String(text.into_owned())
                };
                let node = Node {
                    span: self.scalar_span(range, style),
                    value,
                };
                self.complete(node, anchor);
            }
            Event::SequenceStart(anchor, ref tag) | Event::MappingStart(anchor, ref tag) => {
                if tag.is_some() {
                    return Err(fault(
                        Code::UnsupportedYaml,
                        range,
                        "tags are not supported",
                    ));
                }
                if self.stack.len() >= MAX_DEPTH {
                    return Err(fault(
                        Code::LimitExceeded,
                        range,
                        &format!("collections nest deeper than {MAX_DEPTH} levels"),
                    ));
                }
                let start = range.start;
                self.stack
                    .push(if matches!(event, Event::SequenceStart(..)) {
                        Frame::Sequence {
                            start,
                            anchor,
                            items: Vec::new(),
                        }
                    } else {
                        Frame::Mapping {
                            start,
                            anchor,
                            entries: Vec::new(),
                            key: None,
                        }
                    });
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let (start, anchor, value, last) = match self.stack.pop() {
                    Some(Frame::Sequence {
                        start,
                        anchor,
                        items,
                    }) => {
                        let last = items.last().map(|node| node.span);
                        (start, anchor, Value::Sequence(items), last)
                    }
                    Some(Frame::Mapping {
                        start,
                        anchor,
                        entries,
                        ..
                    }) => {
                        let last = entries.last().map(|(_, node)| node.span);
                        (start, anchor, Value::Mapping(entries), last)
                    }
                    None => return Ok(()),
                };
                let start = point(start);
                let end = if range.end.index() > range.start.index() {
                    point(range.end)
                } else {
                    last.map_or(start, |last| (last.end_line, last.end_column))
                };
                self.complete(
                    Node {
                        span: span(start, end),
                        value,
                    },
                    anchor,
                );
            }
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
        }
        Ok(())
    }

    fn complete(&mut self, node: Node, anchor: usize) {
        if anchor != 0 {
            self.anchors.insert(anchor, (node.clone(), size(&node)));
        }
        match self.stack.last_mut() {
            None => self.root = Some(node),
            Some(Frame::Sequence { items, .. }) => items.push(node),
            Some(Frame::Mapping { entries, key, .. }) => match key.take() {
                Some(key) => entries.push((key, node)),
                None => *key = Some(node),
            },
        }
    }

    fn scalar_span(&self, range: Range, style: ScalarStyle) -> Span {
        let start = range.start.index().min(self.chars.len());
        let end = match style {
            ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted => self.closing_quote(start),
            ScalarStyle::Literal | ScalarStyle::Folded => {
                let mut end = range.end.index().clamp(start, self.chars.len());
                while end > start && matches!(self.chars[end - 1], ' ' | '\t' | '\n' | '\r') {
                    end -= 1;
                }
                end
            }
            ScalarStyle::Plain => return span(point(range.start), point(range.end)),
        };
        let (mut line, mut column) = (range.start.line(), range.start.col());
        let mut chars = self.chars[start..end].iter().peekable();
        while let Some(&c) = chars.next() {
            match c {
                '\r' if chars.peek() == Some(&&'\n') => {}
                '\n' | '\r' => {
                    line += 1;
                    column = 0;
                }
                _ => column += 1,
            }
        }
        span(point(range.start), position(line, column))
    }

    fn closing_quote(&self, start: usize) -> usize {
        let Some(&quote) = self.chars.get(start) else {
            return start;
        };
        let mut at = start + 1;
        while let Some(&c) = self.chars.get(at) {
            if c == '\\' && quote == '"' {
                at += 2;
            } else if c == quote {
                if quote == '\'' && self.chars.get(at + 1) == Some(&'\'') {
                    at += 2;
                } else {
                    return at + 1;
                }
            } else {
                at += 1;
            }
        }
        self.chars.len()
    }
}

fn size(node: &Node) -> usize {
    1 + match &node.value {
        Value::Sequence(items) => items.iter().map(size).sum(),
        Value::Mapping(entries) => entries
            .iter()
            .map(|(key, value)| size(key) + size(value))
            .sum(),
        _ => 0,
    }
}

fn fault(code: Code, range: Range, message: &str) -> Fault {
    Fault {
        code,
        span: span(point(range.start), point(range.end)),
        message: message.to_string(),
    }
}

fn point(marker: Marker) -> (u32, u32) {
    position(marker.line(), marker.col())
}

fn position(line: usize, column: usize) -> (u32, u32) {
    let narrow = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    (narrow(line), narrow(column).saturating_add(1))
}

fn span(start: (u32, u32), end: (u32, u32)) -> Span {
    Span {
        line: start.0,
        column: start.1,
        end_line: end.0,
        end_column: end.1,
    }
}

fn resolve(text: &str) -> Value {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        ".nan" | ".NaN" | ".NAN" => Value::Number(f64::NAN),
        _ => number(text).map_or_else(|| Value::String(text.to_string()), Value::Number),
    }
}

fn number(text: &str) -> Option<f64> {
    if let Some(digits) = text.strip_prefix("0o") {
        return radix(digits, 8);
    }
    if let Some(digits) = text.strip_prefix("0x") {
        return radix(digits, 16);
    }
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") {
        return Some(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    if decimal(unsigned) {
        text.parse().ok()
    } else {
        None
    }
}

fn radix(digits: &str, base: u32) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    digits.chars().try_fold(0.0, |value, c| {
        c.to_digit(base)
            .map(|digit| value * f64::from(base) + f64::from(digit))
    })
}

fn decimal(text: &str) -> bool {
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (text, None),
    };
    let mantissa = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (!whole.is_empty() || !fraction.is_empty()) && digits(whole) && digits(fraction)
        }
        None => !mantissa.is_empty() && digits(mantissa),
    };
    let exponent = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    mantissa && exponent
}
