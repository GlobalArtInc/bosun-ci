use std::collections::BTreeMap;

use bosun_types::ir::{ActionReference, Expr, Segment, Span, Template, Trigger};

use crate::Code;
use crate::lower::{Entry, Lowering, Mark, child};
use crate::yaml::Value;

const HEAD: &[&[&str]] = &[
    &["github", "head_ref"],
    &["github", "event", "pull_request", "head"],
    &["github", "event", "pull_request", "merge_commit_sha"],
    &["github", "event", "pull_request", "number"],
    &["github", "event", "number"],
];

const CHECKOUT_INPUTS: &[&str] = &["ref", "repository"];

fn children(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::Null { .. }
        | Expr::Boolean { .. }
        | Expr::Number { .. }
        | Expr::String { .. }
        | Expr::Context { .. } => Vec::new(),
        Expr::Property { target, .. } | Expr::Filter { target, .. } => vec![target],
        Expr::Index { target, index, .. } => vec![target, index],
        Expr::Not { operand, .. } => vec![operand],
        Expr::Compare { left, right, .. } => vec![left, right],
        Expr::And { operands, .. } | Expr::Or { operands, .. } => operands.iter().collect(),
        Expr::Call { arguments, .. } => arguments.iter().collect(),
    }
}

fn nodes(expr: &Expr) -> Vec<&Expr> {
    let mut found = vec![expr];
    let mut at = 0;
    while let Some(&node) = found.get(at) {
        found.extend(children(node));
        at += 1;
    }
    found
}

fn access(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Context { name, .. } => Some(vec![name.to_ascii_lowercase()]),
        Expr::Property { target, name, .. } => {
            let mut chain = access(target)?;
            chain.push(name.to_ascii_lowercase());
            Some(chain)
        }
        Expr::Index { target, index, .. } => match index.as_ref() {
            Expr::String { value, .. } => {
                let mut chain = access(target)?;
                chain.push(value.to_ascii_lowercase());
                Some(chain)
            }
            _ => None,
        },
        _ => None,
    }
}

fn expressions(template: &Template) -> impl Iterator<Item = &Expr> {
    template
        .segments()
        .iter()
        .filter_map(|segment| match segment {
            Segment::Expression(expr) => Some(expr),
            Segment::Text(_) => None,
        })
}

fn reads_head(template: &Template) -> bool {
    expressions(template)
        .flat_map(nodes)
        .any(|node| access(node).is_some_and(|chain| HEAD.iter().any(|head| chain == *head)))
}

pub(crate) fn bosun(expr: &Expr) -> Vec<Span> {
    nodes(expr)
        .into_iter()
        .filter_map(|node| match node {
            Expr::Context { name, span } if name.eq_ignore_ascii_case("bosun") => Some(*span),
            _ => None,
        })
        .collect()
}

fn checkout(reference: &ActionReference) -> bool {
    reference.owner.eq_ignore_ascii_case("actions")
        && reference.repository.eq_ignore_ascii_case("checkout")
        && reference.path.is_none()
}

fn github_workflow(file: &str) -> bool {
    let directory = file.rsplit_once('/').map_or("", |(directory, _)| directory);
    directory == ".github/workflows" || directory.ends_with("/.github/workflows")
}

impl Lowering<'_> {
    pub(crate) fn mark_checkout(
        &mut self,
        reference: &ActionReference,
        with: &BTreeMap<String, Template>,
        field: &Entry<'_>,
        place: &str,
    ) {
        let Value::Mapping(pairs) = &field.value.value else {
            return;
        };
        if !checkout(reference) {
            return;
        }
        for (key, value) in pairs {
            let Value::String(name) = &key.value else {
                continue;
            };
            if CHECKOUT_INPUTS.contains(&name.as_str()) && with.get(name).is_some_and(reads_head) {
                self.checkouts.push(Mark {
                    span: value.span,
                    path: child(&field.path, name),
                    what: format!("input `{name}` of {place}"),
                });
            }
        }
    }

    pub(crate) fn lint(&mut self) {
        if github_workflow(self.file()) {
            let marks: Vec<Mark> = self
                .extensions
                .drain(..)
                .chain(self.bosun.drain(..))
                .collect();
            for mark in marks {
                self.warning(
                    Code::Extension,
                    mark.span,
                    &mark.path,
                    format!(
                        "{} is an extension of Bosun, which GitHub Actions refuses in this file \
                         under `.github/workflows/`; move the file to `.bosun/workflows/`",
                        mark.what
                    ),
                );
            }
        }
        let target = self.triggers.as_ref().is_some_and(|triggers| {
            triggers
                .iter()
                .any(|trigger| matches!(trigger, Trigger::PullRequestTarget(_)))
        });
        if target {
            for mark in std::mem::take(&mut self.checkouts) {
                self.warning(
                    Code::UntrustedCheckout,
                    mark.span,
                    &mark.path,
                    format!(
                        "{} checks out the head of the pull request in a workflow that \
                         `pull_request_target` triggers, which runs code from the pull request \
                         with the secrets and the forge token of the base repository",
                        mark.what
                    ),
                );
            }
        }
    }
}
