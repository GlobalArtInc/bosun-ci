use bosun_expr::Key;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::ir::{
    Argument, ArgumentValue, Call, CallInput, CallInputKind, CallOutput, CallSecret, CallSecrets,
    Callee, Evaluable, JobKind, JobTemplate, Number, PassedSecret, Trigger, Workflow, WorkflowCall,
};

use crate::Code;
use crate::lower::{Entry, Fields, Lowering, cover, optional};
use crate::on::identifier;
use crate::surface::{CALL_INPUT, CALL_OUTPUT, CALL_SECRET, suggest};
use crate::uses::{self, Refusal, Target};
use crate::yaml::{Node, Value};

const CALL_TYPES: &[&str] = &["string", "boolean", "number"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallType {
    String,
    Boolean,
    Number,
}

impl Lowering<'_> {
    pub(crate) fn workflow_call(&mut self, fields: Option<&Fields<'_>>) -> Option<WorkflowCall> {
        let Some(fields) = fields else {
            return Some(WorkflowCall::default());
        };
        let inputs = optional(fields.get("inputs"), |entry| self.call_inputs(entry));
        let secrets = optional(fields.get("secrets"), |entry| self.call_secrets(entry));
        let outputs = optional(fields.get("outputs"), |entry| self.call_outputs(entry));
        Some(WorkflowCall {
            inputs: inputs?.unwrap_or_default(),
            secrets: secrets?.unwrap_or_default(),
            outputs: outputs?.unwrap_or_default(),
        })
    }

    fn named_entries<'n>(&mut self, entry: &Entry<'n>, noun: &str) -> Option<Vec<Entry<'n>>> {
        let place = format!("`{}` of `workflow_call`", entry.name);
        let found = self.entries(entry.value, &entry.path, &place)?;
        let mut complete = found.complete;
        let mut named = Vec::new();
        for item in found.entries {
            if identifier(item.name) {
                named.push(item);
            } else {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    item.key.span,
                    &item.path,
                    format!(
                        "the name of {noun} `{}` of `workflow_call` must start with a letter or `_` and hold only letters, digits, `-` and `_`",
                        item.name
                    ),
                );
            }
        }
        complete.then_some(named)
    }

    fn call_inputs(&mut self, entry: &Entry<'_>) -> Option<Vec<CallInput>> {
        let mut inputs = Vec::new();
        let mut complete = true;
        for item in self.named_entries(entry, "input")? {
            match self.call_input(&item) {
                Some(input) => inputs.push(input),
                None => complete = false,
            }
        }
        complete.then_some(inputs)
    }

    fn call_input(&mut self, entry: &Entry<'_>) -> Option<CallInput> {
        let place = format!("input `{}` of `workflow_call`", entry.name);
        let fields = self.fields(
            entry.value,
            &CALL_INPUT,
            &place,
            &entry.path,
            entry.key.span,
        )?;
        let what = |key: &str| format!("`{key}` of {place}");
        let description = optional(fields.get("description"), |field| {
            self.string(field.value, &field.path, &what("description"))
        });
        let required = optional(fields.get("required"), |field| {
            self.boolean(field, &what("required"))
        });
        let kind = fields
            .get("type")
            .and_then(|field| self.call_type(field, &place));
        let default = fields.get("default");
        let kind = kind.and_then(|kind| self.call_default(kind, default, &what("default")));
        if !fields.complete {
            return None;
        }
        Some(CallInput {
            name: entry.name.to_string(),
            description: description?,
            required: required?.unwrap_or(false),
            kind: kind?,
        })
    }

    fn call_type(&mut self, field: &Entry<'_>, place: &str) -> Option<CallType> {
        let what = format!("`type` of {place}");
        let text = self.string(field.value, &field.path, &what)?;
        match text.as_str() {
            "string" => Some(CallType::String),
            "boolean" => Some(CallType::Boolean),
            "number" => Some(CallType::Number),
            _ => {
                let hint = suggest(&text, CALL_TYPES.iter().copied())
                    .map(|kind| format!("; did you mean `{kind}`?"))
                    .unwrap_or_default();
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!(
                        "{what} is `{text}`, which is not `string`, `boolean` or `number`{hint}"
                    ),
                );
                None
            }
        }
    }

    fn call_default(
        &mut self,
        kind: CallType,
        default: Option<&Entry<'_>>,
        what: &str,
    ) -> Option<CallInputKind> {
        let key = Key::WorkflowCallInputDefault;
        Some(match kind {
            CallType::String => CallInputKind::String {
                default: optional(default, |field| {
                    self.template(field.value, &field.path, what, key)
                })?,
            },
            CallType::Boolean => CallInputKind::Boolean {
                default: optional(default, |field| {
                    self.flag(field.value, &field.path, what, key)
                })?,
            },
            CallType::Number => CallInputKind::Number {
                default: optional(default, |field| {
                    self.number_or_expression(field.value, &field.path, what, key)
                })?,
            },
        })
    }

    fn number_or_expression(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        key: Key,
    ) -> Option<Evaluable<Number>> {
        match &node.value {
            Value::Number(value) if value.is_finite() => Some(Evaluable::Literal(Number(*value))),
            Value::Number(_) => {
                self.error(
                    Code::InvalidValue,
                    node.span,
                    path,
                    format!("{what} is not a finite number"),
                );
                None
            }
            Value::String(text) if text.contains(crate::expression::OPEN) => self
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

    fn call_secrets(&mut self, entry: &Entry<'_>) -> Option<Vec<CallSecret>> {
        let mut secrets = Vec::new();
        let mut complete = true;
        for item in self.named_entries(entry, "secret")? {
            let place = format!("secret `{}` of `workflow_call`", item.name);
            if let Value::Null = item.value.value {
                secrets.push(CallSecret {
                    name: item.name.to_string(),
                    description: None,
                    required: false,
                });
                continue;
            }
            let Some(fields) =
                self.fields(item.value, &CALL_SECRET, &place, &item.path, item.key.span)
            else {
                complete = false;
                continue;
            };
            let description = optional(fields.get("description"), |field| {
                self.string(
                    field.value,
                    &field.path,
                    &format!("`description` of {place}"),
                )
            });
            let required = optional(fields.get("required"), |field| {
                self.boolean(field, &format!("`required` of {place}"))
            });
            match (description, required) {
                (Some(description), Some(required)) if fields.complete => {
                    secrets.push(CallSecret {
                        name: item.name.to_string(),
                        description,
                        required: required.unwrap_or(false),
                    })
                }
                _ => complete = false,
            }
        }
        complete.then_some(secrets)
    }

    fn call_outputs(&mut self, entry: &Entry<'_>) -> Option<Vec<CallOutput>> {
        let mut outputs = Vec::new();
        let mut complete = true;
        for item in self.named_entries(entry, "output")? {
            let place = format!("output `{}` of `workflow_call`", item.name);
            let Some(fields) =
                self.fields(item.value, &CALL_OUTPUT, &place, &item.path, item.key.span)
            else {
                complete = false;
                continue;
            };
            let description = optional(fields.get("description"), |field| {
                self.string(
                    field.value,
                    &field.path,
                    &format!("`description` of {place}"),
                )
            });
            let value = fields.get("value").and_then(|field| {
                self.template(
                    field.value,
                    &field.path,
                    &format!("`value` of {place}"),
                    Key::WorkflowCallOutputValue,
                )
            });
            match (description, value) {
                (Some(description), Some(value)) if fields.complete => outputs.push(CallOutput {
                    name: item.name.to_string(),
                    description,
                    value,
                }),
                _ => complete = false,
            }
        }
        complete.then_some(outputs)
    }

    pub(crate) fn call(
        &mut self,
        uses: &Entry<'_>,
        with: Option<&Entry<'_>>,
        secrets: Option<&Entry<'_>>,
        owner: &str,
    ) -> Option<Call> {
        let workflow = self.callee(uses, owner);
        let with = optional(with, |field| self.arguments(field, owner));
        let secrets = match secrets {
            Some(field) => self.passed_secrets(field, owner),
            None => Some(CallSecrets::Named(Vec::new())),
        };
        Some(Call {
            workflow: workflow?,
            with: with?.unwrap_or_default(),
            secrets: secrets?,
            span: uses.value.span,
        })
    }

    fn callee(&mut self, field: &Entry<'_>, owner: &str) -> Option<Callee> {
        let what = format!("`uses` of {owner}");
        let text = self.string(field.value, &field.path, &what)?;
        let text = self.literal(text, field.value, &field.path, &what)?;
        let invalid = |lowering: &mut Self, reason: String| {
            lowering.error(
                Code::InvalidValue,
                field.value.span,
                &field.path,
                format!("{what} is `{text}`, which {reason}"),
            );
            None
        };
        match uses::target(&text) {
            Ok(Target::Local(path)) if path.is_empty() || path.ends_with('/') => invalid(
                self,
                "names a directory and not a workflow file".to_string(),
            ),
            Ok(Target::Local(path)) if path.contains('@') => invalid(
                self,
                "has a ref, which only a workflow of another repository takes".to_string(),
            ),
            Ok(Target::Local(path)) => Some(Callee::Local { path }),
            Ok(Target::Remote(reference)) if reference.path.is_none() => invalid(
                self,
                format!(
                    "names a repository and not a workflow file; write it as `{}/{}/<path>@{}`",
                    reference.owner, reference.repository, reference.r#ref
                ),
            ),
            Ok(Target::Remote(reference))
                if !reference
                    .path
                    .as_deref()
                    .is_some_and(|path| path.ends_with(".yml") || path.ends_with(".yaml")) =>
            {
                invalid(
                    self,
                    "names a file that does not end in `.yml` or `.yaml`".to_string(),
                )
            }
            Ok(Target::Remote(reference)) => Some(Callee::Remote(reference)),
            Ok(Target::Docker(_)) => invalid(
                self,
                "is a container image, and only a step can run one".to_string(),
            ),
            Err(Refusal::Invalid(reason)) => invalid(self, reason),
        }
    }

    fn arguments(&mut self, entry: &Entry<'_>, owner: &str) -> Option<Vec<Argument>> {
        let place = format!("`with` of {owner}");
        let found = self.entries(entry.value, &entry.path, &place)?;
        let mut complete = found.complete;
        let mut arguments = Vec::new();
        for item in &found.entries {
            if item.name.is_empty() {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    item.key.span,
                    &item.path,
                    format!("an input in {place} has an empty name"),
                );
                continue;
            }
            let what = format!("input `{}` in {place}", item.name);
            let value = match &item.value.value {
                Value::Bool(value) => Some(ArgumentValue::Boolean(*value)),
                Value::Number(value) if value.is_finite() => {
                    Some(ArgumentValue::Number(Number(*value)))
                }
                Value::Number(_) => {
                    self.error(
                        Code::InvalidValue,
                        item.value.span,
                        &item.path,
                        format!("{what} is not a finite number"),
                    );
                    None
                }
                Value::String(_) => self
                    .template(item.value, &item.path, &what, Key::JobWith)
                    .map(ArgumentValue::String),
                _ => {
                    self.error(
                        Code::InvalidType,
                        item.value.span,
                        &item.path,
                        format!("{what} must be a string, a number or a boolean"),
                    );
                    None
                }
            };
            match value {
                Some(value) => arguments.push(Argument {
                    name: item.name.to_string(),
                    value,
                    span: cover(item.key.span, item.value.span),
                }),
                None => complete = false,
            }
        }
        complete.then_some(arguments)
    }

    fn passed_secrets(&mut self, entry: &Entry<'_>, owner: &str) -> Option<CallSecrets> {
        let place = format!("`secrets` of {owner}");
        if let Value::String(text) = &entry.value.value {
            if text == "inherit" {
                return Some(CallSecrets::Inherit);
            }
            self.error(
                Code::InvalidValue,
                entry.value.span,
                &entry.path,
                format!("{place} is `{text}`, which is neither `inherit` nor a mapping of secrets"),
            );
            return None;
        }
        let found = self.entries(entry.value, &entry.path, &place)?;
        let mut complete = found.complete;
        let mut passed = Vec::new();
        for item in &found.entries {
            if item.name.is_empty() {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    item.key.span,
                    &item.path,
                    format!("a secret in {place} has an empty name"),
                );
                continue;
            }
            let what = format!("secret `{}` in {place}", item.name);
            match self.template(item.value, &item.path, &what, Key::JobSecrets) {
                Some(value) => passed.push(PassedSecret {
                    name: item.name.to_string(),
                    value,
                    span: cover(item.key.span, item.value.span),
                }),
                None => complete = false,
            }
        }
        complete.then_some(CallSecrets::Named(passed))
    }
}

fn describe(value: &ArgumentValue) -> &'static str {
    match value {
        ArgumentValue::Boolean(_) => "a boolean",
        ArgumentValue::Number(_) => "a number",
        ArgumentValue::String(_) => "a string",
    }
}

fn fits(kind: &CallInputKind, value: &ArgumentValue) -> bool {
    match (kind, value) {
        (CallInputKind::String { .. }, _) => true,
        (CallInputKind::Boolean { .. }, ArgumentValue::Boolean(_)) => true,
        (CallInputKind::Number { .. }, ArgumentValue::Number(_)) => true,
        (CallInputKind::Boolean { .. }, ArgumentValue::String(template)) => template
            .as_literal()
            .is_none_or(|text| matches!(text.as_str(), "true" | "false")),
        (CallInputKind::Number { .. }, ArgumentValue::String(template)) => template
            .as_literal()
            .is_none_or(|text| text.trim().parse::<f64>().is_ok_and(f64::is_finite)),
        _ => false,
    }
}

fn fault(
    code: Code,
    file: &str,
    span: bosun_types::ir::Span,
    path: Vec<PathSegment>,
    message: String,
) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: code.as_str().to_string(),
        message,
        file: file.to_string(),
        span,
        path,
    }
}

fn path(job: &str, parts: &[&str]) -> Vec<PathSegment> {
    ["jobs", job]
        .iter()
        .chain(parts)
        .map(|part| PathSegment::Key((*part).to_string()))
        .collect()
}

fn trigger(callee: &Workflow) -> Option<&WorkflowCall> {
    callee.triggers.iter().find_map(|trigger| match trigger {
        Trigger::WorkflowCall(call) => Some(call),
        _ => None,
    })
}

fn declared<'d, T>(items: &'d [T], name: impl Fn(&T) -> &str, wanted: &str) -> Option<&'d T> {
    items
        .iter()
        .find(|item| name(item).eq_ignore_ascii_case(wanted))
}

pub fn check_call(file: &str, caller: &JobTemplate, callee: &Workflow) -> Vec<Diagnostic> {
    let JobKind::Call(call) = &caller.kind else {
        return Vec::new();
    };
    let job = caller.key.to_string();
    let Some(accepted) = trigger(callee) else {
        return vec![fault(
            Code::InvalidValue,
            file,
            call.span,
            path(&job, &["uses"]),
            format!(
                "the called workflow has no `workflow_call` trigger, so job `{job}` cannot call it"
            ),
        )];
    };
    let mut found = Vec::new();
    for argument in &call.with {
        let Some(input) = declared(&accepted.inputs, |input| &input.name, &argument.name) else {
            let hint = suggest(
                &argument.name,
                accepted.inputs.iter().map(|input| input.name.as_str()),
            )
            .map(|name| format!("; did you mean `{name}`?"))
            .unwrap_or_default();
            found.push(fault(
                Code::UnknownKey,
                file,
                argument.span,
                path(&job, &["with", &argument.name]),
                format!(
                    "the called workflow declares no input `{}`, which `with` of job `{job}` passes{hint}",
                    argument.name
                ),
            ));
            continue;
        };
        if !fits(&input.kind, &argument.value) {
            found.push(fault(
                Code::InvalidType,
                file,
                argument.span,
                path(&job, &["with", &argument.name]),
                format!(
                    "input `{}` of the called workflow has type `{}`, and `with` of job `{job}` passes {}",
                    input.name,
                    input.kind.name(),
                    describe(&argument.value)
                ),
            ));
        }
    }
    for input in &accepted.inputs {
        let given = call
            .with
            .iter()
            .any(|argument| argument.name.eq_ignore_ascii_case(&input.name));
        if input.required && !input.kind.has_default() && !given {
            found.push(fault(
                Code::MissingKey,
                file,
                call.span,
                path(&job, &["uses"]),
                format!(
                    "the called workflow requires input `{}`, which `with` of job `{job}` does not pass",
                    input.name
                ),
            ));
        }
    }
    if let CallSecrets::Named(passed) = &call.secrets {
        for secret in passed {
            if declared(&accepted.secrets, |declared| &declared.name, &secret.name).is_none() {
                let hint = suggest(
                    &secret.name,
                    accepted.secrets.iter().map(|secret| secret.name.as_str()),
                )
                .map(|name| format!("; did you mean `{name}`?"))
                .unwrap_or_default();
                found.push(fault(
                    Code::UnknownKey,
                    file,
                    secret.span,
                    path(&job, &["secrets", &secret.name]),
                    format!(
                        "the called workflow declares no secret `{}`, which `secrets` of job `{job}` passes{hint}",
                        secret.name
                    ),
                ));
            }
        }
        for secret in &accepted.secrets {
            let given = passed
                .iter()
                .any(|passed| passed.name.eq_ignore_ascii_case(&secret.name));
            if secret.required && !given {
                found.push(fault(
                    Code::MissingKey,
                    file,
                    call.span,
                    path(&job, &["uses"]),
                    format!(
                        "the called workflow requires secret `{}`, which `secrets` of job `{job}` does not pass",
                        secret.name
                    ),
                ));
            }
        }
    }
    found.sort_by_key(|diagnostic| (diagnostic.span.line, diagnostic.span.column));
    found
}
