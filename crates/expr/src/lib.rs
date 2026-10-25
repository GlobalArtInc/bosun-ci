mod availability;
mod check;
mod compare;
mod error;
mod evaluate;
mod functions;
mod json;
mod lexer;
mod number;
mod parser;
mod source;
mod template;
mod value;

use bosun_types::ir::{Condition, Expr, Template};

pub use crate::availability::{Context, Function, Key, Side};
pub use crate::error::{Code, Error};
pub use crate::evaluate::{Scope, Status};
use crate::source::Source;
pub use crate::value::Value;

pub fn expression(text: &str) -> Result<Expr, Error> {
    let source = Source::new(text);
    parser::parse(&source, 0, source.len(), (0, source.len()))
}

pub fn template(text: &str) -> Result<Template, Error> {
    template::template(text)
}

pub fn condition(text: &str) -> Result<Option<Condition>, Error> {
    template::condition(text)
}

pub fn check(expr: &Expr, key: Key) -> Vec<Error> {
    let mut errors = Vec::new();
    check::check(expr, Some(key), &mut errors);
    errors
}

pub fn evaluate(expr: &Expr, scope: &dyn Scope) -> Result<Value, Error> {
    evaluate::evaluate(expr, scope)
}

pub fn render(template: &Template, scope: &dyn Scope) -> Result<String, Error> {
    evaluate::render(template, scope)
}
