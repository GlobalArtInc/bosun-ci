use bosun_expr::Key;
use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{Concurrency, Evaluable, Queue, Template};

use crate::Code;
use crate::lower::{Entry, Lowering, optional};
use crate::surface::CONCURRENCY;
use crate::yaml::{Node, Value};

impl Lowering<'_> {
    pub(crate) fn concurrency(
        &mut self,
        entry: &Entry<'_>,
        place: &str,
        key: Key,
    ) -> Option<Concurrency> {
        let what = format!("`concurrency` of {place}");
        let Value::Mapping(_) = entry.value.value else {
            let group = self.group(entry.value, &entry.path, &what, key)?;
            return Some(Concurrency {
                group,
                cancel_in_progress: Evaluable::Literal(false),
                queue: Queue::Single,
            });
        };
        let fields = self.fields(
            entry.value,
            &CONCURRENCY,
            &what,
            &entry.path,
            entry.key.span,
        )?;
        let group = fields.get("group").and_then(|field| {
            self.group(field.value, &field.path, &format!("`group` of {what}"), key)
        });
        let cancel = optional(fields.get("cancel-in-progress"), |field| {
            self.flag(
                field.value,
                &field.path,
                &format!("`cancel-in-progress` of {what}"),
                key,
            )
        });
        let queue = optional(fields.get("queue"), |field| self.queue(field, &what));
        if !fields.complete {
            return None;
        }
        let concurrency = Concurrency {
            group: group?,
            cancel_in_progress: cancel?.unwrap_or(Evaluable::Literal(false)),
            queue: queue?.unwrap_or(Queue::Single),
        };
        if let (Queue::Max, Evaluable::Literal(true), Some(field)) = (
            concurrency.queue,
            &concurrency.cancel_in_progress,
            fields.get("cancel-in-progress"),
        ) {
            self.error(
                Code::InvalidValue,
                field.value.span,
                &field.path,
                format!(
                    "`cancel-in-progress` of {what} cannot be `true` together with `queue: max`, \
                     which keeps every waiting member and cancels none"
                ),
            );
            return None;
        }
        Some(concurrency)
    }

    fn group(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        key: Key,
    ) -> Option<Template> {
        let group = self.template(node, path, what, key)?;
        if group.as_literal().is_some_and(|text| text.is_empty()) {
            self.error(
                Code::InvalidValue,
                node.span,
                path,
                format!("{what} names no group"),
            );
            return None;
        }
        Some(group)
    }

    fn queue(&mut self, field: &Entry<'_>, what: &str) -> Option<Queue> {
        let what = format!("`queue` of {what}");
        let text = self.string(field.value, &field.path, &what)?;
        match text.as_str() {
            "single" => Some(Queue::Single),
            "max" => Some(Queue::Max),
            _ => {
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!("{what} is `{text}`, which is neither `single` nor `max`"),
                );
                None
            }
        }
    }
}
