use std::collections::BTreeSet;
use std::str::FromStr;

use bosun_expr::Key;
use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{Action, Evaluable, Shell, Step};
use bosun_types::key::StepKey;

use crate::Code;
use crate::lower::{Entry, Fields, Lowering, index, optional};
use crate::surface::{STEP, pending_form};
use crate::uses::{self, Refusal, Target};
use crate::yaml::{Node, Value};

struct StepKeys {
    r#if: Key,
    name: Key,
    run: Key,
    working_directory: Key,
    with: Key,
    env: Key,
    continue_on_error: Key,
    timeout_minutes: Key,
}

const JOB_STEP: StepKeys = StepKeys {
    r#if: Key::StepIf,
    name: Key::StepName,
    run: Key::StepRun,
    working_directory: Key::StepWorkingDirectory,
    with: Key::StepWith,
    env: Key::StepEnv,
    continue_on_error: Key::StepContinueOnError,
    timeout_minutes: Key::StepTimeoutMinutes,
};

const ACTION_STEP: StepKeys = StepKeys {
    r#if: Key::ActionStepIf,
    name: Key::ActionStep,
    run: Key::ActionStep,
    working_directory: Key::ActionStep,
    with: Key::ActionStep,
    env: Key::ActionStep,
    continue_on_error: Key::ActionStep,
    timeout_minutes: Key::ActionStep,
};

impl Lowering<'_> {
    pub(crate) fn steps(
        &mut self,
        entry: &Entry<'_>,
        owner: &str,
        composite: bool,
    ) -> Option<Vec<Step>> {
        let what = format!("`steps` of {owner}");
        let Value::Sequence(items) = &entry.value.value else {
            self.error(
                Code::InvalidType,
                entry.value.span,
                &entry.path,
                format!("{what} must be a list"),
            );
            return None;
        };
        if items.is_empty() {
            self.error(
                Code::InvalidValue,
                entry.value.span,
                &entry.path,
                format!("{what} lists no steps"),
            );
            return None;
        }
        let mut keys = BTreeSet::new();
        let mut steps = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            let path = index(&entry.path, position);
            match self.step(item, &path, position, owner, composite, &mut keys) {
                Some(step) => steps.push(step),
                None => complete = false,
            }
        }
        complete.then_some(steps)
    }

    fn step(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        position: usize,
        owner: &str,
        composite: bool,
        keys: &mut BTreeSet<String>,
    ) -> Option<Step> {
        let place = format!("step {} of {owner}", position + 1);
        let fields = self.fields(node, &STEP, &place, path, node.span)?;
        let what = |key: &str| format!("`{key}` of {place}");
        let expr = if composite { &ACTION_STEP } else { &JOB_STEP };
        let key = optional(fields.get("id"), |field| {
            self.step_key(field, &place, owner, keys)
        });
        let condition = optional(fields.get("if"), |field| {
            self.condition(field.value, &field.path, &what("if"), expr.r#if)
        });
        let name = optional(fields.get("name"), |field| {
            self.template(field.value, &field.path, &what("name"), expr.name)
        });
        let action = self.action(&fields, node, path, &place, expr.run);
        let shell = optional(fields.get("shell"), |field| self.shell(field, &place));
        if composite
            && fields.complete
            && matches!(action, Some(Action::Run { .. }))
            && fields.get("shell").is_none()
        {
            self.error(
                Code::MissingKey,
                node.span,
                path,
                format!(
                    "missing key `shell` in {place}, which runs a script in a composite action"
                ),
            );
            return None;
        }
        let working_directory = optional(fields.get("working-directory"), |field| {
            self.template(
                field.value,
                &field.path,
                &what("working-directory"),
                expr.working_directory,
            )
        });
        let with = optional(fields.get("with"), |field| {
            self.templates(field.value, &field.path, &what("with"), "input", expr.with)
        });
        if let (Some(Action::Uses(reference)), Some(Some(with)), Some(field)) =
            (&action, &with, fields.get("with"))
        {
            self.mark_checkout(reference, with, field, &place);
        }
        let env = optional(fields.get("env"), |field| {
            self.env(field.value, &field.path, &place, expr.env)
        });
        let continue_on_error = optional(fields.get("continue-on-error"), |field| {
            self.flag(
                field.value,
                &field.path,
                &what("continue-on-error"),
                expr.continue_on_error,
            )
        });
        let timeout_minutes = optional(fields.get("timeout-minutes"), |field| {
            self.whole(
                field.value,
                &field.path,
                &what("timeout-minutes"),
                expr.timeout_minutes,
                0,
            )
        });
        Some(Step {
            key: key?,
            name: name?,
            r#if: condition?.flatten(),
            action: action?,
            shell: shell?,
            working_directory: working_directory?,
            with: with?.unwrap_or_default(),
            env: env?.unwrap_or_default(),
            continue_on_error: continue_on_error?.unwrap_or(Evaluable::Literal(false)),
            timeout_minutes: timeout_minutes?,
            span: node.span,
        })
    }

    fn action(
        &mut self,
        fields: &Fields<'_>,
        node: &Node,
        path: &[PathSegment],
        place: &str,
        script: Key,
    ) -> Option<Action> {
        let (run, uses) = (fields.get("run"), fields.get("uses"));
        let mut valid = true;
        let misplaced: &[(&str, &str)] = match (run, uses) {
            (Some(_), None) => &[("with", "uses an action")],
            (None, Some(_)) => &[
                ("shell", "runs a script"),
                ("working-directory", "runs a script"),
            ],
            _ => &[],
        };
        for &(key, kind) in misplaced {
            if let Some(field) = fields.get(key) {
                valid = false;
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!("`{key}` of {place} applies only to a step that {kind}"),
                );
            }
        }
        let action = match (run, uses) {
            (Some(_), Some(uses)) => {
                self.error(
                    Code::InvalidValue,
                    uses.value.span,
                    &uses.path,
                    format!("`uses` of {place} cannot be used together with `run`"),
                );
                None
            }
            (Some(run), None) => self
                .template(run.value, &run.path, &format!("`run` of {place}"), script)
                .map(|script| Action::Run { script }),
            (None, Some(uses)) => self.uses(uses, place),
            (None, None) => {
                if fields.complete {
                    self.error(
                        Code::MissingKey,
                        node.span,
                        path,
                        format!("missing key `run` or `uses` in {place}"),
                    );
                }
                None
            }
        };
        action.filter(|_| valid)
    }

    fn uses(&mut self, field: &Entry<'_>, place: &str) -> Option<Action> {
        let what = format!("`uses` of {place}");
        let text = self.string(field.value, &field.path, &what)?;
        let text = self.literal(text, field.value, &field.path, &what)?;
        match uses::target(&text) {
            Ok(Target::Remote(reference)) => Some(Action::Uses(reference)),
            Ok(Target::Local(path)) => {
                self.refuse_form(&what, &text, "a local action", "uses: ./path", field);
                Some(Action::Local { path })
            }
            Ok(Target::Docker(image)) => {
                self.refuse_form(&what, &text, "a container image", "uses: docker://", field);
                Some(Action::Docker { image })
            }
            Err(Refusal::Invalid(reason)) => {
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!("{what} is `{text}`, which {reason}"),
                );
                None
            }
        }
    }

    fn refuse_form(&mut self, what: &str, text: &str, kind: &str, form: &str, field: &Entry<'_>) {
        if self.pending_refused() && pending_form("step", form) {
            self.error(
                Code::NotImplemented,
                field.value.span,
                &field.path,
                format!("{what} is `{text}`, {kind}, which is not supported yet; it arrives in M2"),
            );
        }
    }

    fn step_key(
        &mut self,
        field: &Entry<'_>,
        place: &str,
        owner: &str,
        keys: &mut BTreeSet<String>,
    ) -> Option<StepKey> {
        let text = self.string(field.value, &field.path, &format!("`id` of {place}"))?;
        match StepKey::from_str(&text) {
            Err(error) => {
                self.error(
                    Code::InvalidStepKey,
                    field.value.span,
                    &field.path,
                    error.to_string(),
                );
                None
            }
            Ok(_) if !keys.insert(text.clone()) => {
                self.error(
                    Code::RepeatedStepKey,
                    field.value.span,
                    &field.path,
                    format!("step key `{text}` is used twice in {owner}"),
                );
                None
            }
            Ok(key) => Some(key),
        }
    }

    fn shell(&mut self, field: &Entry<'_>, place: &str) -> Option<Shell> {
        let what = format!("`shell` of {place}");
        let text = self.string(field.value, &field.path, &what)?;
        let text = self.literal(text, field.value, &field.path, &what)?;
        self.shell_named(&text, field, &what)
    }
}
