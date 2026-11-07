use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bosun_db::Error;
use bosun_db::runs::{Called, Handed, JobState, Locked, NewChild, Passed, Shape};
use bosun_expr::Value;
use bosun_types::id::JobId;
use bosun_types::ir::{
    self, ArgumentValue, Call, CallInput, CallInputKind, CallSecrets, Evaluable, Expr, JobTemplate,
    SecretReferences, Segment, Template, Trigger, WorkflowCall,
};
use bosun_types::status::Conclusion;
use serde_json::Map;

use super::{
    Contexts, Fault, INVALID_TYPE, INVALID_VALUE, Place, World, at, described, effective, matrix,
    value,
};
use crate::callees::MOST_CALLED_JOBS;
use crate::keys::Keyring;
use crate::secrets::{self, Unopened};

const GITHUB_TOKEN: &str = "GITHUB_TOKEN";

const SEPARATOR: &str = " / ";

pub(super) fn template_level<'a>(jobs: &'a [JobState], job: &'a JobState) -> &'a JobState {
    if job.matrix.is_some() {
        job.group
            .and_then(|group| jobs.iter().find(|candidate| candidate.id == group))
            .unwrap_or(job)
    } else {
        job
    }
}

pub(super) fn crossing(jobs: &[JobState], added: usize) -> Option<(usize, usize)> {
    let held = jobs
        .iter()
        .filter(|job| job.key.is_member() || (job.kind == Shape::Call && job.matrix.is_some()))
        .count();
    (held.saturating_add(added) > MOST_CALLED_JOBS).then_some((held, added))
}

pub(super) fn scope_of(jobs: &[JobState], job: &JobState) -> Option<JobId> {
    template_level(jobs, job).group
}

pub(super) fn templates(
    jobs: &[JobState],
    scope: Option<JobId>,
) -> HashMap<&bosun_types::key::JobKey, &JobState> {
    jobs.iter()
        .filter(|job| job.matrix.is_none() && job.group == scope)
        .map(|job| (&job.key, job))
        .collect()
}

pub(super) fn instance<'a>(jobs: &'a [JobState], job: &JobState) -> Option<&'a JobState> {
    let scope = scope_of(jobs, job)?;
    jobs.iter().find(|candidate| candidate.id == scope)
}

pub(super) fn callers<'a>(jobs: &'a [JobState], job: &JobState) -> Vec<&'a JobState> {
    let mut found = Vec::new();
    let mut current = instance(jobs, job);
    while let Some(caller) = current {
        found.push(caller);
        current = instance(jobs, caller);
    }
    found.reverse();
    found
}

pub(super) fn inputs(world: &World, jobs: &[JobState], job: &JobState) -> Result<Value, Error> {
    let Some(caller) = instance(jobs, job) else {
        return Ok(world.inputs.clone());
    };
    let called = caller.call.as_ref().ok_or_else(|| Error::Row {
        table: "job",
        message: format!("job {} calls a workflow and holds no call", caller.id),
    })?;
    value(serde_json::to_value(&called.inputs)?)
}

pub(super) fn prefix(jobs: &[JobState], job: &JobState) -> String {
    match instance(jobs, job) {
        Some(caller) => format!("{}{SEPARATOR}", caller.name),
        None => String::new(),
    }
}

pub(super) fn callee<'a>(
    world: &'a World,
    owner: &ir::Workflow,
    template: &JobTemplate,
) -> Result<&'a ir::Workflow, Error> {
    let index = owner.calls.get(&template.key).ok_or_else(|| Error::Row {
        table: "run",
        message: format!(
            "the IR of the run holds no callee for job `{}`, which calls a workflow",
            template.key
        ),
    })?;
    world
        .ir
        .callees
        .get(*index)
        .map(|callee| &callee.workflow)
        .ok_or_else(|| Error::Row {
            table: "run",
            message: format!("the IR of the run has no callee {index}"),
        })
}

pub(super) fn accepted(callee: &ir::Workflow) -> Option<&WorkflowCall> {
    callee.triggers.iter().find_map(|trigger| match trigger {
        Trigger::WorkflowCall(accepted) => Some(accepted),
        _ => None,
    })
}

pub(super) struct Expanded {
    pub(super) called: Called,
    pub(super) children: Vec<NewChild>,
}

pub(super) fn expand(
    job: &JobState,
    template: &JobTemplate,
    call: &Call,
    callee: &ir::Workflow,
    name: &str,
    contexts: &Contexts,
) -> Result<Expanded, Fault> {
    let Some(accepted) = accepted(callee) else {
        return Err(Fault {
            code: INVALID_VALUE,
            message: format!(
                "job `{}` calls a workflow that has no `workflow_call` trigger",
                template.key
            ),
            span: call.span,
            path: at(&template.key, &["uses"]),
        });
    };
    let inputs = inputs_of(template, call, accepted, contexts)?;
    let secrets = passed(template, call, contexts)?;
    let children = callee
        .jobs
        .iter()
        .map(|member| NewChild {
            key: job.key.member(&member.key),
            name: format!(
                "{name}{SEPARATOR}{}",
                member
                    .name
                    .as_ref()
                    .and_then(Template::as_literal)
                    .filter(|literal| !literal.trim().is_empty())
                    .unwrap_or_else(|| member.key.to_string())
            ),
            needs: member
                .needs
                .iter()
                .map(|need| job.key.member(need))
                .collect(),
            kind: Shape::from(&member.kind),
        })
        .collect();
    Ok(Expanded {
        called: Called { inputs, secrets },
        children,
    })
}

fn inputs_of(
    template: &JobTemplate,
    call: &Call,
    accepted: &WorkflowCall,
    contexts: &Contexts,
) -> Result<BTreeMap<String, serde_json::Value>, Fault> {
    let mut resolved: Vec<(String, Value)> = Vec::new();
    let mut scope = contexts.clone();
    for input in &accepted.inputs {
        let argument = call
            .with
            .iter()
            .find(|argument| argument.name.eq_ignore_ascii_case(&input.name));
        let value = match argument {
            Some(argument) => {
                let place = Place {
                    what: format!(
                        "input `{}` in `with` of job `{}`",
                        argument.name, template.key
                    ),
                    path: at(&template.key, &["with", &argument.name]),
                    span: argument.span,
                };
                let raw = match &argument.value {
                    ArgumentValue::Boolean(flag) => Value::Boolean(*flag),
                    ArgumentValue::Number(number) => Value::Number(number.0),
                    ArgumentValue::String(text) => place.typed(text, &scope)?,
                };
                fit(&place, input, raw)?
            }
            None => {
                let place = Place {
                    what: format!(
                        "the default of input `{}`, which `with` of job `{}` leaves out,",
                        input.name, template.key
                    ),
                    path: at(&template.key, &["uses"]),
                    span: call.span,
                };
                default(&place, input, &scope)?
            }
        };
        resolved.push((input.name.clone(), value));
        scope.inputs = Value::Object(resolved.clone().into());
    }
    Ok(resolved
        .iter()
        .map(|(name, value)| (name.clone(), matrix::json(value)))
        .collect())
}

fn fit(place: &Place, input: &CallInput, value: Value) -> Result<Value, Fault> {
    match (&input.kind, value) {
        (CallInputKind::String { .. }, Value::String(text)) => Ok(Value::String(text)),
        (CallInputKind::String { .. }, Value::Null) => Ok(Value::String(String::new())),
        (CallInputKind::String { .. }, Value::Boolean(flag)) => Ok(Value::String(flag.to_string())),
        (CallInputKind::String { .. }, Value::Number(number)) => {
            Ok(Value::String(number.to_string()))
        }
        (CallInputKind::Boolean { .. }, Value::Boolean(flag)) => Ok(Value::Boolean(flag)),
        (CallInputKind::Boolean { .. }, Value::String(text))
            if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false") =>
        {
            Ok(Value::Boolean(text.eq_ignore_ascii_case("true")))
        }
        (CallInputKind::Number { .. }, Value::Number(number)) if number.is_finite() => {
            Ok(Value::Number(number))
        }
        (CallInputKind::Number { .. }, Value::String(text))
            if text.trim().parse::<f64>().is_ok_and(f64::is_finite) =>
        {
            Ok(Value::Number(
                text.trim().parse::<f64>().unwrap_or_default(),
            ))
        }
        (kind, other) => Err(place.refuse(
            INVALID_TYPE,
            format!(
                "must be {} to fit the input of type `{}`, and it is {}",
                match kind {
                    CallInputKind::String { .. } => "a string, a number or a boolean",
                    CallInputKind::Boolean { .. } => "`true` or `false`",
                    CallInputKind::Number { .. } => "a finite number",
                },
                kind.name(),
                described(&other)
            ),
        )),
    }
}

fn default(place: &Place, input: &CallInput, scope: &Contexts) -> Result<Value, Fault> {
    match &input.kind {
        CallInputKind::String {
            default: Some(text),
        } => Ok(Value::String(place.text(text, scope)?)),
        CallInputKind::String { default: None } => Ok(Value::String(String::new())),
        CallInputKind::Boolean {
            default: Some(Evaluable::Literal(flag)),
        } => Ok(Value::Boolean(*flag)),
        CallInputKind::Boolean {
            default: Some(Evaluable::Template(text)),
        } => fit(place, input, place.typed(text, scope)?),
        CallInputKind::Boolean { default: None } => Ok(Value::Boolean(false)),
        CallInputKind::Number {
            default: Some(Evaluable::Literal(number)),
        } => Ok(Value::Number(number.0)),
        CallInputKind::Number {
            default: Some(Evaluable::Template(text)),
        } => fit(place, input, place.typed(text, scope)?),
        CallInputKind::Number { default: None } => Ok(Value::Number(0.0)),
    }
}

fn whole(text: &Template) -> Option<String> {
    let [Segment::Expression(expr)] = text.segments() else {
        return None;
    };
    let secrets = |target: &Expr| matches!(target, Expr::Context { name, .. } if name.eq_ignore_ascii_case("secrets"));
    match expr {
        Expr::Property { target, name, .. } if secrets(target) => Some(name.to_ascii_uppercase()),
        Expr::Index { target, index, .. } if secrets(target) => match index.as_ref() {
            Expr::String { value, .. } => Some(value.to_ascii_uppercase()),
            _ => None,
        },
        _ => None,
    }
}

fn passed(template: &JobTemplate, call: &Call, contexts: &Contexts) -> Result<Passed, Fault> {
    let CallSecrets::Named(list) = &call.secrets else {
        return Ok(Passed::Inherit);
    };
    let mut named = BTreeMap::new();
    for secret in list {
        let place = Place {
            what: format!(
                "secret `{}` in `secrets` of job `{}`",
                secret.name, template.key
            ),
            path: at(&template.key, &["secrets", &secret.name]),
            span: secret.span,
        };
        let handed = match whole(&secret.value) {
            Some(name) => Handed::Secret(name),
            None => {
                contexts.secrets_read.set(false);
                let text = place.text(&secret.value, contexts)?;
                if contexts.secrets_read.get() {
                    return Err(place.refuse(
                        INVALID_VALUE,
                        "passes a secret inside other text or an expression, and a call passes a \
                         secret of its caller only as `${{ secrets.NAME }}` alone"
                            .to_owned(),
                    ));
                }
                Handed::Text(text)
            }
        };
        named.insert(secret.name.to_ascii_uppercase(), handed);
    }
    Ok(Passed::Named(named))
}

pub(super) struct Ended {
    pub(super) conclusion: Conclusion,
    pub(super) outputs: BTreeMap<String, String>,
    pub(super) faults: Vec<Fault>,
}

pub(super) fn ended(
    world: &World,
    instance: &JobState,
    members: &[&JobState],
    callee: &ir::Workflow,
    conclusion: Conclusion,
) -> Result<Ended, Error> {
    let mut jobs_context = Map::new();
    for member in members {
        let result = match effective(member) {
            Some(Conclusion::Success) => "success",
            Some(Conclusion::Failure | Conclusion::TimedOut) => "failure",
            Some(Conclusion::Cancelled) => "cancelled",
            Some(Conclusion::Skipped) => "skipped",
            None => "",
        };
        jobs_context.insert(
            member.key.local().to_owned(),
            serde_json::json!({ "result": result, "outputs": member.outputs }),
        );
    }
    let called = instance.call.as_ref().ok_or_else(|| Error::Row {
        table: "job",
        message: format!("job {} calls a workflow and holds no call", instance.id),
    })?;
    let contexts = Contexts {
        github: world.github(instance.key.local())?,
        needs: Value::Null,
        inputs: value(serde_json::to_value(&called.inputs)?)?,
        bosun: world.bosun.clone(),
        env: Value::Null,
        strategy: Value::Null,
        matrix: Value::Null,
        vars: world.vars.clone(),
        secrets: Value::Null,
        secrets_read: std::cell::Cell::new(false),
        statuses: None,
        jobs: value(serde_json::Value::Object(jobs_context))?,
    };
    let mut outputs = BTreeMap::new();
    let mut faults = Vec::new();
    let mut conclusion = conclusion;
    if let Some(accepted) = accepted(callee) {
        for output in &accepted.outputs {
            match bosun_expr::render(&output.value, &contexts) {
                Ok(text) => {
                    outputs.insert(output.name.clone(), text);
                }
                Err(error) => {
                    let what = format!("output `{}` of `workflow_call`", output.name);
                    faults.push(Fault::expression(
                        error,
                        &what,
                        vec![
                            super::key("on"),
                            super::key("workflow_call"),
                            super::key("outputs"),
                            super::key(&output.name),
                            super::key("value"),
                        ],
                    ));
                }
            }
        }
    }
    if !faults.is_empty() && conclusion == Conclusion::Success {
        conclusion = Conclusion::Failure;
    }
    Ok(Ended {
        conclusion,
        outputs,
        faults,
    })
}

fn needed(callers: &[&JobState], leaf: &SecretReferences) -> Result<SecretReferences, Error> {
    let mut wanted = leaf.clone();
    for caller in callers.iter().rev() {
        let called = caller.call.as_ref().ok_or_else(|| Error::Row {
            table: "job",
            message: format!("job {} calls a workflow and holds no call", caller.id),
        })?;
        wanted = match &called.secrets {
            Passed::Inherit => wanted,
            Passed::Named(passed) => {
                let mut names = BTreeSet::new();
                for (name, handed) in passed {
                    let asked = match &wanted {
                        SecretReferences::All => true,
                        SecretReferences::Named(wanted) => wanted.contains(name),
                    };
                    if asked && let Handed::Secret(secret) = handed {
                        names.insert(secret.clone());
                    }
                }
                SecretReferences::Named(names)
            }
        };
    }
    Ok(wanted)
}

pub(super) async fn visible(
    run: &Locked<'_>,
    keys: &Keyring,
    world: &World,
    callers: &[&JobState],
    template: &JobTemplate,
    token: &str,
) -> Result<Result<BTreeMap<String, String>, Unopened>, Error> {
    let wanted = needed(callers, &template.secrets)?;
    let read = if run.state().trusted {
        secrets::read(
            run.transaction(),
            keys,
            world.facts.organisation_id,
            world.facts.project_id,
            &wanted,
        )
        .await?
    } else {
        Ok(BTreeMap::new())
    };
    let read = match read {
        Ok(read) => read,
        Err(unopened) => return Ok(Err(unopened)),
    };
    let mut values: BTreeMap<String, String> = read
        .into_iter()
        .map(|(name, value)| (name.to_ascii_uppercase(), value))
        .collect();
    values.insert(GITHUB_TOKEN.to_owned(), token.to_owned());
    for caller in callers {
        let called = caller.call.as_ref().ok_or_else(|| Error::Row {
            table: "job",
            message: format!("job {} calls a workflow and holds no call", caller.id),
        })?;
        if let Passed::Named(passed) = &called.secrets {
            let before = values;
            values = passed
                .iter()
                .map(|(name, handed)| {
                    let value = match handed {
                        Handed::Text(text) => text.clone(),
                        Handed::Secret(secret) => before.get(secret).cloned().unwrap_or_default(),
                    };
                    (name.clone(), value)
                })
                .collect();
            values.insert(GITHUB_TOKEN.to_owned(), token.to_owned());
        }
    }
    Ok(Ok(values))
}

pub(super) fn declared(
    world: &World,
    key: &bosun_types::key::JobKey,
) -> Result<HashSet<String>, Error> {
    let (workflow, _) = world.template(key)?;
    Ok(accepted(workflow)
        .map(|accepted| {
            accepted
                .secrets
                .iter()
                .map(|secret| secret.name.to_ascii_uppercase())
                .collect()
        })
        .unwrap_or_default())
}
