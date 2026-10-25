use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{Expr, SecretReferences, Span};

use crate::Code;
use crate::lower::{Lowering, number_text};

enum Read {
    Named(String),
    Computed(Span),
    Whole(Span),
}

fn secrets(expr: &Expr) -> bool {
    matches!(expr, Expr::Context { name, .. } if name.eq_ignore_ascii_case("secrets"))
}

fn reads(expr: &Expr, found: &mut Vec<Read>) {
    match expr {
        Expr::Property { target, name, .. } if secrets(target) => {
            found.push(Read::Named(name.to_ascii_uppercase()));
        }
        Expr::Index {
            target,
            index,
            span,
        } if secrets(target) => match index.as_ref() {
            Expr::String { value, .. } => found.push(Read::Named(value.to_ascii_uppercase())),
            Expr::Boolean { value, .. } => {
                found.push(Read::Named(value.to_string().to_ascii_uppercase()));
            }
            Expr::Number { value, .. } => {
                found.push(Read::Named(number_text(value.0).to_ascii_uppercase()));
            }
            Expr::Null { .. } => {}
            computed => {
                found.push(Read::Computed(*span));
                reads(computed, found);
            }
        },
        Expr::Context { span, .. } if secrets(expr) => found.push(Read::Whole(*span)),
        Expr::Null { .. }
        | Expr::Boolean { .. }
        | Expr::Number { .. }
        | Expr::String { .. }
        | Expr::Context { .. } => {}
        Expr::Property { target, .. } | Expr::Filter { target, .. } => reads(target, found),
        Expr::Index { target, index, .. } => {
            reads(target, found);
            reads(index, found);
        }
        Expr::Not { operand, .. } => reads(operand, found),
        Expr::Compare { left, right, .. } => {
            reads(left, found);
            reads(right, found);
        }
        Expr::And { operands, .. } | Expr::Or { operands, .. } => {
            for operand in operands {
                reads(operand, found);
            }
        }
        Expr::Call { arguments, .. } => {
            for argument in arguments {
                reads(argument, found);
            }
        }
    }
}

impl Lowering<'_> {
    pub(crate) fn read_secrets(&mut self, expr: &Expr, path: &[PathSegment]) {
        let mut found = Vec::new();
        reads(expr, &mut found);
        for read in found {
            let (span, how) = match read {
                Read::Named(name) => {
                    if let SecretReferences::Named(names) = self.secrets_mut() {
                        names.insert(name);
                    }
                    continue;
                }
                Read::Computed(span) => (span, "reads `secrets` by a computed name"),
                Read::Whole(span) => (span, "reads the whole `secrets` context"),
            };
            *self.secrets_mut() = SecretReferences::All;
            let receiver = match self.current_job() {
                Some(job) => format!("job `{job}`"),
                None => "every job of the workflow".to_string(),
            };
            self.warning(
                Code::AllSecrets,
                span,
                path,
                format!(
                    "this expression {how}, so {receiver} receives every secret in scope \
                     instead of only the ones it names"
                ),
            );
        }
    }
}
