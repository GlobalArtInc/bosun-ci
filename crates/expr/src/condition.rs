use bosun_types::ir::{Condition, Expr, Segment, Span};

use crate::availability::Function;

pub(crate) fn calls_status(condition: &Condition) -> bool {
    match condition {
        Condition::Expression(expr) => calls(expr),
        Condition::Template(template) => template.segments().iter().any(|segment| match segment {
            Segment::Expression(expr) => calls(expr),
            Segment::Text(_) => false,
        }),
    }
}

pub(crate) fn span(condition: &Condition) -> Span {
    match condition {
        Condition::Expression(expr) => expr.span(),
        Condition::Template(template) => template
            .segments()
            .iter()
            .find_map(|segment| match segment {
                Segment::Expression(expr) => Some(expr.span()),
                Segment::Text(_) => None,
            })
            .unwrap_or(Span {
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 1,
            }),
    }
}

fn calls(expr: &Expr) -> bool {
    match expr {
        Expr::Null { .. }
        | Expr::Boolean { .. }
        | Expr::Number { .. }
        | Expr::String { .. }
        | Expr::Context { .. } => false,
        Expr::Property { target, .. } | Expr::Filter { target, .. } => calls(target),
        Expr::Index { target, index, .. } => calls(target) || calls(index),
        Expr::Not { operand, .. } => calls(operand),
        Expr::Compare { left, right, .. } => calls(left) || calls(right),
        Expr::And { operands, .. } | Expr::Or { operands, .. } => operands.iter().any(calls),
        Expr::Call {
            function,
            arguments,
            ..
        } => {
            matches!(
                Function::find(function),
                Some(
                    Function::Success | Function::Failure | Function::Cancelled | Function::Always
                )
            ) || arguments.iter().any(calls)
        }
    }
}
