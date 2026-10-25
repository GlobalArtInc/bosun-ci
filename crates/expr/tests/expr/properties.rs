use bosun_types::ir::{Condition, Expr, Segment, Span, Template};
use proptest::prelude::*;

const FRAGMENTS: &[&str] = &[
    "${{",
    "}}",
    "{",
    "}",
    "$",
    "'",
    "''",
    "'x'",
    "a",
    "b.c",
    "_Z-9",
    "null",
    "TRUE",
    "NaN",
    "0x1F",
    "1e",
    ".5",
    "-",
    "+",
    " ",
    "\n",
    "\t",
    "é",
    "==",
    "!=",
    "<=",
    ">",
    "!",
    "&&",
    "||",
    "&",
    "|",
    "=",
    "(",
    ")",
    "[",
    "]",
    "*",
    ".",
    ",",
    "f(",
    "contains(",
];

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(FRAGMENTS), 0..24)
        .prop_map(|fragments| fragments.concat())
}

fn spans(expr: &Expr, found: &mut Vec<Span>) {
    found.push(expr.span());
    match expr {
        Expr::Null { .. }
        | Expr::Boolean { .. }
        | Expr::Number { .. }
        | Expr::String { .. }
        | Expr::Context { .. } => {}
        Expr::Property { target, .. } | Expr::Filter { target, .. } => spans(target, found),
        Expr::Not { operand, .. } => spans(operand, found),
        Expr::Index { target, index, .. } => {
            spans(target, found);
            spans(index, found);
        }
        Expr::Compare { left, right, .. } => {
            spans(left, found);
            spans(right, found);
        }
        Expr::And { operands, .. }
        | Expr::Or { operands, .. }
        | Expr::Call {
            arguments: operands,
            ..
        } => {
            for operand in operands {
                spans(operand, found);
            }
        }
    }
}

fn template_spans(template: &Template, found: &mut Vec<Span>) {
    for segment in template.segments() {
        if let Segment::Expression(expr) = segment {
            spans(expr, found);
        }
    }
}

fn inside(text: &str, span: Span) -> bool {
    let lengths: Vec<u32> = text
        .split('\n')
        .map(|line| u32::try_from(line.chars().count()).expect("a short line"))
        .collect();
    let valid = |line: u32, column: u32| {
        line >= 1
            && column >= 1
            && lengths
                .get(line as usize - 1)
                .is_some_and(|&length| column <= length + 1)
    };
    valid(span.line, span.column)
        && valid(span.end_line, span.end_column)
        && (span.line, span.column) <= (span.end_line, span.end_column)
}

fn check(text: &str, found: &[Span], failure: Option<Span>) {
    for &span in found.iter().chain(failure.iter()) {
        assert!(
            inside(text, span),
            "{text:?}: {span:?} lies outside the text"
        );
    }
}

proptest! {
    #[test]
    fn parsing_never_panics_and_spans_stay_inside_the_text(text in text()) {
        let mut found = Vec::new();
        match bosun_expr::expression(&text) {
            Ok(expr) => {
                spans(&expr, &mut found);
                let json = serde_json::to_value(&expr).expect("serialisable");
                let again: Expr = serde_json::from_value(json).expect("deserialisable");
                prop_assert_eq!(&again, &expr);
                check(&text, &found, None);
            }
            Err(error) => check(&text, &found, Some(error.span)),
        }
        found.clear();
        match bosun_expr::template(&text) {
            Ok(template) => {
                template_spans(&template, &mut found);
                check(&text, &found, None);
            }
            Err(error) => check(&text, &found, Some(error.span)),
        }
        found.clear();
        match bosun_expr::condition(&text) {
            Ok(Some(Condition::Expression(expr))) => spans(&expr, &mut found),
            Ok(Some(Condition::Template(template))) => template_spans(&template, &mut found),
            Ok(None) => prop_assert!(text.trim().is_empty()),
            Err(error) => found.push(error.span),
        }
        check(&text, &found, None);
    }

    #[test]
    fn a_template_without_delimiters_is_its_text(text in text()) {
        prop_assume!(!text.contains("${{"));
        prop_assert_eq!(
            bosun_expr::template(&text).expect("text without delimiters"),
            Template::text(text.clone())
        );
    }
}
