use bosun_expr::Key;
use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{Evaluable, Matrix, MatrixField, MatrixValue, Number, Strategy, Vector};

use crate::Code;
use crate::expression::OPEN;
use crate::lower::{Entry, Lowering, index, optional};
use crate::surface::STRATEGY;
use crate::yaml::{Node, Value};

const KEY: Key = Key::JobStrategy;

impl Lowering<'_> {
    fn computed<T>(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        expected: &str,
    ) -> Option<Option<Evaluable<T>>> {
        match &node.value {
            Value::String(text) if text.contains(OPEN) => Some(
                self.parse_template(text, node, path, KEY)
                    .map(Evaluable::Template),
            ),
            Value::Sequence(_) | Value::Mapping(_) => None,
            _ => {
                self.error(
                    Code::InvalidType,
                    node.span,
                    path,
                    format!("{what} must be {expected} or an expression"),
                );
                Some(None)
            }
        }
    }

    pub(crate) fn strategy(
        &mut self,
        entry: &Entry<'_>,
        place: &str,
    ) -> Option<Evaluable<Strategy>> {
        let what = format!("`strategy` of {place}");
        if let Some(computed) = self.computed(entry.value, &entry.path, &what, "a mapping") {
            return computed;
        }
        let fields = self.fields(entry.value, &STRATEGY, &what, &entry.path, entry.key.span)?;
        let matrix = optional(fields.get("matrix"), |field| self.matrix(field, place));
        let fail_fast = optional(fields.get("fail-fast"), |field| {
            self.flag(
                field.value,
                &field.path,
                &format!("`fail-fast` of {what}"),
                KEY,
            )
        });
        let max_parallel = optional(fields.get("max-parallel"), |field| {
            self.whole(
                field.value,
                &field.path,
                &format!("`max-parallel` of {what}"),
                KEY,
                1,
            )
        });
        if !fields.complete {
            return None;
        }
        Some(Evaluable::Literal(Strategy {
            matrix: matrix?,
            fail_fast: fail_fast?.unwrap_or(Evaluable::Literal(true)),
            max_parallel: max_parallel?,
        }))
    }

    fn matrix(&mut self, entry: &Entry<'_>, place: &str) -> Option<Evaluable<Matrix>> {
        let what = format!("the matrix of {place}");
        if let Some(computed) = self.computed(entry.value, &entry.path, &what, "a mapping") {
            return computed;
        }
        let found = self.entries(entry.value, &entry.path, &what)?;
        let mut complete = found.complete;
        let mut matrix = Matrix {
            vectors: Vec::new(),
            include: None,
            exclude: None,
        };
        for field in &found.entries {
            match field.name {
                "include" | "exclude" => {
                    let combinations = self.combinations(field, &what);
                    match (field.name, combinations) {
                        (_, None) => complete = false,
                        ("include", Some(combinations)) => matrix.include = Some(combinations),
                        (_, Some(combinations)) => matrix.exclude = Some(combinations),
                    }
                }
                name => match self.vector(field, &what) {
                    Some(values) => matrix.vectors.push(Vector {
                        name: name.to_string(),
                        values,
                    }),
                    None => complete = false,
                },
            }
        }
        complete.then_some(Evaluable::Literal(matrix))
    }

    fn vector(&mut self, field: &Entry<'_>, what: &str) -> Option<Evaluable<Vec<MatrixValue>>> {
        let what = format!("vector `{}` of {what}", field.name);
        if let Some(computed) = self.computed(field.value, &field.path, &what, "a list") {
            return computed;
        }
        let Value::Sequence(items) = &field.value.value else {
            self.error(
                Code::InvalidType,
                field.value.span,
                &field.path,
                format!("{what} must be a list or an expression"),
            );
            return None;
        };
        let mut values = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            match self.matrix_value(item, &index(&field.path, position), &what) {
                Some(value) => values.push(value),
                None => complete = false,
            }
        }
        complete.then_some(Evaluable::Literal(values))
    }

    fn combinations(
        &mut self,
        field: &Entry<'_>,
        what: &str,
    ) -> Option<Evaluable<Vec<Vec<MatrixField>>>> {
        let what = format!("`{}` of {what}", field.name);
        let expected = "a list of mappings";
        if let Some(computed) = self.computed(field.value, &field.path, &what, expected) {
            return computed;
        }
        let Value::Sequence(items) = &field.value.value else {
            self.error(
                Code::InvalidType,
                field.value.span,
                &field.path,
                format!("{what} must be {expected} or an expression"),
            );
            return None;
        };
        let mut combinations = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            let path = index(&field.path, position);
            let entry = format!("entry {} of {what}", position + 1);
            match self.object(item, &path, &entry) {
                Some(combination) => combinations.push(combination),
                None => complete = false,
            }
        }
        complete.then_some(Evaluable::Literal(combinations))
    }

    fn object(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
    ) -> Option<Vec<MatrixField>> {
        let found = self.entries(node, path, what)?;
        let mut fields = Vec::new();
        let mut complete = found.complete;
        for entry in &found.entries {
            let inner = format!("`{}` in {what}", entry.name);
            match self.matrix_value(entry.value, &entry.path, &inner) {
                Some(value) => fields.push(MatrixField {
                    name: entry.name.to_string(),
                    value,
                }),
                None => complete = false,
            }
        }
        complete.then_some(fields)
    }

    fn matrix_value(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
    ) -> Option<MatrixValue> {
        match &node.value {
            Value::Null => Some(MatrixValue::Null),
            Value::Bool(value) => Some(MatrixValue::Boolean(*value)),
            Value::Number(value) => Some(MatrixValue::Number(Number(*value))),
            Value::String(text) => self
                .parse_template(text, node, path, KEY)
                .map(MatrixValue::String),
            Value::Sequence(items) => {
                let mut values = Vec::new();
                let mut complete = true;
                for (position, item) in items.iter().enumerate() {
                    match self.matrix_value(item, &index(path, position), what) {
                        Some(value) => values.push(value),
                        None => complete = false,
                    }
                }
                complete.then_some(MatrixValue::List(values))
            }
            Value::Mapping(_) => self.object(node, path, what).map(MatrixValue::Object),
        }
    }
}
