use bosun_expr::Key;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::ir::{Condition, Evaluable, Expr, Segment, Template};

use crate::Code;
use crate::lower::Lowering;
use crate::place::Placement;
use crate::yaml::{Node, Value};

pub(crate) const OPEN: &str = "${{";

fn expressions(template: &Template) -> Vec<&Expr> {
    template
        .segments()
        .iter()
        .filter_map(|segment| match segment {
            Segment::Expression(expr) => Some(expr),
            Segment::Text(_) => None,
        })
        .collect()
}

impl Lowering<'_> {
    fn expression_error(&mut self, error: bosun_expr::Error, path: &[PathSegment]) {
        self.push(Diagnostic {
            severity: Severity::Error,
            code: error.code.as_str().to_string(),
            message: error.message,
            file: self.file().to_string(),
            span: error.span,
            path: path.to_vec(),
        });
    }

    fn placed(&self, node: &Node, text: &str) -> Placement {
        self.text().place(node.span, text)
    }

    fn checked(&mut self, found: &[&Expr], key: Key, path: &[PathSegment]) -> bool {
        let mut valid = true;
        for expr in found {
            for error in bosun_expr::check(expr, key) {
                valid = false;
                self.expression_error(error, path);
            }
        }
        if valid {
            for expr in found {
                self.read_secrets(expr, path);
            }
        }
        valid
    }

    pub(crate) fn parse_template(
        &mut self,
        text: &str,
        node: &Node,
        path: &[PathSegment],
        key: Key,
    ) -> Option<Template> {
        if !text.contains(OPEN) {
            return Some(Template::text(text));
        }
        let placement = self.placed(node, text);
        let template = match bosun_expr::template(text) {
            Ok(template) => placement.template(&template),
            Err(mut error) => {
                error.span = placement.span(error.span);
                self.expression_error(error, path);
                return None;
            }
        };
        self.checked(&expressions(&template), key, path)
            .then_some(template)
    }

    pub(crate) fn template(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        key: Key,
    ) -> Option<Template> {
        let text = self.string(node, path, what)?;
        self.parse_template(&text, node, path, key)
    }

    pub(crate) fn literal(
        &mut self,
        text: String,
        node: &Node,
        path: &[PathSegment],
        what: &str,
    ) -> Option<String> {
        if text.contains(OPEN) {
            self.error(
                Code::InvalidValue,
                node.span,
                path,
                format!("{what} takes no expressions"),
            );
            return None;
        }
        Some(text)
    }

    pub(crate) fn condition(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        key: Key,
    ) -> Option<Option<Condition>> {
        if let Value::Null = node.value {
            return Some(None);
        }
        let text = self.string(node, path, what)?;
        let placement = self.placed(node, &text);
        let condition = match bosun_expr::condition(&text) {
            Ok(condition) => condition,
            Err(mut error) => {
                error.span = placement.span(error.span);
                self.expression_error(error, path);
                return None;
            }
        };
        match condition {
            None => Some(None),
            Some(Condition::Expression(mut expr)) => {
                placement.expression(&mut expr);
                self.checked(&[&expr], key, path)
                    .then_some(Some(Condition::Expression(expr)))
            }
            Some(Condition::Template(template)) => {
                let template = placement.template(&template);
                if !self.checked(&expressions(&template), key, path) {
                    return None;
                }
                self.warning(
                    Code::TemplateCondition,
                    node.span,
                    path,
                    format!(
                        "{what} has text around its `${{{{ }}}}`, or several of them, so it is a \
                         string, which is true unless it is empty; write the whole condition inside \
                         one `${{{{ }}}}`"
                    ),
                );
                Some(Some(Condition::Template(template)))
            }
        }
    }

    pub(crate) fn flag(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        key: Key,
    ) -> Option<Evaluable<bool>> {
        match &node.value {
            Value::Bool(value) => Some(Evaluable::Literal(*value)),
            Value::String(text) if text.contains(OPEN) => self
                .parse_template(text, node, path, key)
                .map(Evaluable::Template),
            _ => {
                self.error(
                    Code::InvalidType,
                    node.span,
                    path,
                    format!("{what} must be `true`, `false` or an expression"),
                );
                None
            }
        }
    }

    pub(crate) fn whole(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        key: Key,
        least: u32,
    ) -> Option<Evaluable<u32>> {
        match &node.value {
            Value::Number(value) => {
                let fits = value.fract() == 0.0
                    && *value >= f64::from(least)
                    && *value <= f64::from(u32::MAX);
                if fits {
                    return Some(Evaluable::Literal(*value as u32));
                }
                self.error(
                    Code::InvalidValue,
                    node.span,
                    path,
                    format!("{what} must be a whole number of at least {least}"),
                );
                None
            }
            Value::String(text) if text.contains(OPEN) => self
                .parse_template(text, node, path, key)
                .map(Evaluable::Template),
            _ => {
                self.error(
                    Code::InvalidType,
                    node.span,
                    path,
                    format!("{what} must be a number or an expression"),
                );
                None
            }
        }
    }
}
