use bosun_types::ir::{Condition, Expr, Segment, Span, Template};

use crate::availability::{Context, Function};
use crate::check;
use crate::compare;
use crate::condition;
use crate::error::{Code, Error};
use crate::functions::{self, Affix};
use crate::json;
use crate::value::{self, Value};

pub trait Scope {
    fn context(&self, context: Context) -> Value;

    fn status(&self, _status: Status) -> Option<bool> {
        None
    }

    fn hash_files(&self, _patterns: &[String]) -> Option<Result<String, String>> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Success,
    Failure,
    Cancelled,
}

pub(crate) struct Held {
    value: Value,
    filtered: bool,
}

impl Held {
    pub(crate) fn plain(value: Value) -> Held {
        Held {
            value,
            filtered: false,
        }
    }

    fn filtered(values: Vec<Value>) -> Held {
        Held {
            value: Value::Array(values.into()),
            filtered: true,
        }
    }
}

enum Selector {
    All,
    Key(Value),
}

fn children(value: &Value) -> Vec<Value> {
    match value {
        Value::Object(pairs) => pairs.iter().map(|(_, value)| value.clone()).collect(),
        Value::Array(items) => items.to_vec(),
        _ => Vec::new(),
    }
}

fn integer(index: &Value) -> Option<usize> {
    let number = index.number();
    if number.is_nan() || number < 0.0 {
        return None;
    }
    let number = number.floor();
    if number > f64::from(i32::MAX) {
        return None;
    }
    usize::try_from(number as i64).ok()
}

fn find(value: &Value, index: &Value) -> Option<Value> {
    match value {
        Value::Object(pairs) if index.primitive() => value::lookup(pairs, &index.text()).cloned(),
        Value::Array(items) => integer(index).and_then(|at| items.get(at).cloned()),
        _ => None,
    }
}

fn access(target: &Held, selector: &Selector) -> Held {
    if target.filtered {
        let items = children(&target.value);
        return Held::filtered(match selector {
            Selector::All => items.iter().flat_map(children).collect(),
            Selector::Key(index) => items.iter().filter_map(|item| find(item, index)).collect(),
        });
    }
    match selector {
        Selector::All => Held::filtered(children(&target.value)),
        Selector::Key(index) => Held::plain(find(&target.value, index).unwrap_or(Value::Null)),
    }
}

fn validate(expr: &Expr) -> Result<(), Error> {
    let mut errors = Vec::new();
    check::check(expr, None, &mut errors);
    match errors.into_iter().next() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) fn evaluate(expr: &Expr, scope: &dyn Scope) -> Result<Value, Error> {
    validate(expr)?;
    Evaluation::new(scope).value(expr)
}

pub(crate) fn render(template: &Template, scope: &dyn Scope) -> Result<String, Error> {
    for segment in template.segments() {
        if let Segment::Expression(expr) = segment {
            validate(expr)?;
        }
    }
    let mut evaluation = Evaluation::new(scope);
    let mut text = String::new();
    for segment in template.segments() {
        match segment {
            Segment::Text(part) => text.push_str(part),
            Segment::Expression(expr) => text.push_str(&evaluation.value(expr)?.text()),
        }
    }
    Ok(text)
}

pub(crate) fn holds(condition: &Condition, scope: &dyn Scope) -> Result<bool, Error> {
    match condition {
        Condition::Expression(expr) => validate(expr)?,
        Condition::Template(template) => {
            for segment in template.segments() {
                if let Segment::Expression(expr) = segment {
                    validate(expr)?;
                }
            }
        }
    }
    if !condition::calls_status(condition) {
        let succeeded = scope
            .status(Status::Success)
            .ok_or_else(|| unavailable(Function::Success, condition::span(condition)))?;
        if !succeeded {
            return Ok(false);
        }
    }
    match condition {
        Condition::Expression(expr) => Ok(Evaluation::new(scope).value(expr)?.truthy()),
        Condition::Template(template) => Ok(!render(template, scope)?.is_empty()),
    }
}

pub(crate) fn failed(span: Span, message: String) -> Error {
    Error::new(Code::FunctionFailed, span, message)
}

pub(crate) struct Evaluation<'s> {
    scope: &'s dyn Scope,
    resolved: Vec<(Context, Value)>,
}

impl<'s> Evaluation<'s> {
    fn new(scope: &'s dyn Scope) -> Evaluation<'s> {
        Evaluation {
            scope,
            resolved: Vec::new(),
        }
    }

    pub(crate) fn value(&mut self, expr: &Expr) -> Result<Value, Error> {
        Ok(self.held(expr)?.value)
    }

    pub(crate) fn held(&mut self, expr: &Expr) -> Result<Held, Error> {
        let value = match expr {
            Expr::Null { .. } => Value::Null,
            Expr::Boolean { value, .. } => Value::Boolean(*value),
            Expr::Number { value, .. } => Value::Number(value.0),
            Expr::String { value, .. } => Value::String(value.clone()),
            Expr::Context { name, span } => self.context(name, *span)?,
            Expr::Property { target, name, .. } => {
                let target = self.held(target)?;
                return Ok(access(&target, &Selector::Key(Value::String(name.clone()))));
            }
            Expr::Index { target, index, .. } => {
                let target = self.held(target)?;
                if target.value.primitive() {
                    return Ok(Held::plain(Value::Null));
                }
                let index = self.value(index)?;
                return Ok(access(&target, &Selector::Key(index)));
            }
            Expr::Filter { target, .. } => {
                let target = self.held(target)?;
                return Ok(access(&target, &Selector::All));
            }
            Expr::Not { operand, .. } => Value::Boolean(!self.value(operand)?.truthy()),
            Expr::Compare {
                operator,
                left,
                right,
                ..
            } => {
                let left = self.value(left)?;
                let right = self.value(right)?;
                Value::Boolean(compare::compare(*operator, &left, &right))
            }
            Expr::And { operands, .. } => return self.logic(operands, false),
            Expr::Or { operands, .. } => return self.logic(operands, true),
            Expr::Call {
                function,
                arguments,
                span,
            } => return self.call(function, arguments, *span),
        };
        Ok(Held::plain(value))
    }

    fn logic(&mut self, operands: &[Expr], decisive: bool) -> Result<Held, Error> {
        let mut last = Held::plain(Value::Null);
        for operand in operands {
            last = self.held(operand)?;
            if last.value.truthy() == decisive {
                break;
            }
        }
        Ok(last)
    }

    fn context(&mut self, name: &str, span: Span) -> Result<Value, Error> {
        let context = Context::find(name).ok_or_else(|| check::unknown_context(name, span))?;
        if let Some((_, value)) = self.resolved.iter().find(|(known, _)| *known == context) {
            return Ok(value.clone());
        }
        let value = self.scope.context(context);
        self.resolved.push((context, value.clone()));
        Ok(value)
    }

    fn call(&mut self, name: &str, arguments: &[Expr], span: Span) -> Result<Held, Error> {
        let function = Function::find(name).ok_or_else(|| check::unknown_function(name, span))?;
        let value = match function {
            Function::Contains => functions::contains(self, arguments)?,
            Function::StartsWith => functions::affix(self, arguments, Affix::Prefix)?,
            Function::EndsWith => functions::affix(self, arguments, Affix::Suffix)?,
            Function::Format => functions::format(self, arguments, span)?,
            Function::Join => functions::join(self, arguments)?,
            Function::ToJson => Value::String(json::write(&self.value(&arguments[0])?)),
            Function::FromJson => {
                let text = self.value(&arguments[0])?;
                json::read(&text.text()).map_err(|error| {
                    failed(
                        span,
                        format!("the argument of `fromJSON` is not JSON: {error}"),
                    )
                })?
            }
            Function::Case => return functions::case(self, arguments, span),
            Function::HashFiles => self.hash_files(arguments, span)?,
            Function::Success => self.status(Status::Success, function, span)?,
            Function::Failure => self.status(Status::Failure, function, span)?,
            Function::Cancelled => self.status(Status::Cancelled, function, span)?,
            Function::Always => Value::Boolean(true),
        };
        Ok(Held::plain(value))
    }

    fn status(&self, status: Status, function: Function, span: Span) -> Result<Value, Error> {
        self.scope
            .status(status)
            .map(Value::Boolean)
            .ok_or_else(|| unavailable(function, span))
    }

    fn hash_files(&mut self, arguments: &[Expr], span: Span) -> Result<Value, Error> {
        let mut patterns = Vec::with_capacity(arguments.len());
        for argument in arguments {
            patterns.push(self.value(argument)?.text().into_owned());
        }
        match self.scope.hash_files(&patterns) {
            None => Err(unavailable(Function::HashFiles, span)),
            Some(Ok(hash)) => Ok(Value::String(hash)),
            Some(Err(message)) => Err(failed(span, format!("`hashFiles` failed: {message}"))),
        }
    }
}

fn unavailable(function: Function, span: Span) -> Error {
    Error::new(
        Code::UnavailableFunction,
        span,
        format!(
            "function `{}` is not available where this expression is evaluated",
            function.name()
        ),
    )
}
