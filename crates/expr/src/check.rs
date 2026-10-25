use bosun_types::ir::{Expr, Span};

use crate::availability::{Context, Function, Key};
use crate::error::{Code, Error};

pub(crate) fn check(expr: &Expr, key: Option<Key>, errors: &mut Vec<Error>) {
    match expr {
        Expr::Null { .. } | Expr::Boolean { .. } | Expr::Number { .. } | Expr::String { .. } => {}
        Expr::Context { name, span } => context(name, *span, key, errors),
        Expr::Property { target, .. } | Expr::Filter { target, .. } => check(target, key, errors),
        Expr::Index { target, index, .. } => {
            check(target, key, errors);
            check(index, key, errors);
        }
        Expr::Not { operand, .. } => check(operand, key, errors),
        Expr::Compare { left, right, .. } => {
            check(left, key, errors);
            check(right, key, errors);
        }
        Expr::And { operands, .. } | Expr::Or { operands, .. } => {
            for operand in operands {
                check(operand, key, errors);
            }
        }
        Expr::Call {
            function,
            arguments,
            span,
        } => {
            call(function, arguments.len(), *span, key, errors);
            for argument in arguments {
                check(argument, key, errors);
            }
        }
    }
}

pub(crate) fn unknown_context(name: &str, span: Span) -> Error {
    Error::new(
        Code::UnknownContext,
        span,
        format!("unknown context `{name}`"),
    )
}

pub(crate) fn unknown_function(name: &str, span: Span) -> Error {
    Error::new(
        Code::UnknownFunction,
        span,
        format!("unknown function `{name}`"),
    )
}

fn context(name: &str, span: Span, key: Option<Key>, errors: &mut Vec<Error>) {
    match (Context::find(name), key) {
        (None, _) => errors.push(unknown_context(name, span)),
        (Some(context), Some(key)) if !key.contexts().contains(&context) => {
            errors.push(Error::new(
                Code::UnavailableContext,
                span,
                format!(
                    "context `{}` is not available in `{}`",
                    context.name(),
                    key.path()
                ),
            ))
        }
        (Some(_), _) => {}
    }
}

fn call(name: &str, count: usize, span: Span, key: Option<Key>, errors: &mut Vec<Error>) {
    if name.eq_ignore_ascii_case("plugin") {
        errors.push(Error::new(
            Code::NotImplemented,
            span,
            "`plugin()` is not supported yet; it arrives in M5".to_string(),
        ));
        return;
    }
    let Some(function) = Function::find(name) else {
        errors.push(unknown_function(name, span));
        return;
    };
    if let Some(key) = key
        && function.special()
        && !key.special_functions().contains(&function)
    {
        errors.push(Error::new(
            Code::UnavailableFunction,
            span,
            format!(
                "function `{}` is not available in `{}`",
                function.name(),
                key.path()
            ),
        ));
        return;
    }
    let arity = function.arity();
    if !arity.admits(count) {
        errors.push(Error::new(
            Code::WrongArgumentCount,
            span,
            format!(
                "`{}` takes {}, not {count}{}",
                function.name(),
                arity.describe(),
                arity.hint()
            ),
        ));
    }
}
