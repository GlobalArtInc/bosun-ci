mod availability;
mod check;
mod error;
mod lexer;
mod number;
mod parser;
mod source;
mod template;

use bosun_types::ir::{Condition, Expr, Template};

pub use crate::availability::{Context, Function, Key, Side};
pub use crate::error::{Code, Error};
use crate::source::Source;

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
    check::check(expr, key, &mut errors);
    errors
}
