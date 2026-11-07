use std::collections::BTreeMap;
use std::path::PathBuf;

use bosun_expr::{Context, Scope, Status, Value};
use bosun_proto::assignment::Contexts;
use bosun_types::ir::{Evaluable, Segment, Template};
use bosun_types::status::Conclusion;
use bosun_workflow::github::RunnerFacts;
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::hash;

#[derive(Clone)]
pub(crate) struct Hashing {
    workspace: PathBuf,
    cancel: watch::Receiver<bool>,
    deadline: Option<Instant>,
}

impl Hashing {
    pub(crate) fn new(
        workspace: PathBuf,
        cancel: watch::Receiver<bool>,
        deadline: Option<Instant>,
    ) -> Hashing {
        Hashing {
            workspace,
            cancel,
            deadline,
        }
    }

    fn reason(&self) -> Option<&'static str> {
        if *self.cancel.borrow() {
            Some("the job was cancelled")
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Some("the job ran beyond its `timeout-minutes`")
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub(crate) struct Fixed {
    needs: Value,
    strategy: Value,
    matrix: Value,
    inputs: Value,
    vars: Value,
    secrets: Value,
    bosun: Value,
    runner: Value,
}

impl Fixed {
    pub(crate) fn new(contexts: &Contexts, runner: &RunnerFacts) -> Fixed {
        let map = |map: &serde_json::Map<String, serde_json::Value>| {
            value(serde_json::Value::Object(map.clone()))
        };
        Fixed {
            needs: map(&contexts.needs),
            strategy: value(contexts.strategy.clone()),
            matrix: value(contexts.matrix.clone()),
            inputs: map(&contexts.inputs),
            vars: map(&contexts.vars),
            secrets: map(&contexts.secrets),
            bosun: map(&contexts.bosun),
            runner: runner_object(runner),
        }
    }

    pub(crate) fn set_runner(&mut self, runner: &RunnerFacts) {
        self.runner = runner_object(runner);
    }

    pub(crate) fn set_secret(&mut self, name: &str, secret: &str) {
        let mut entries: Vec<(String, Value)> = match &self.secrets {
            Value::Object(entries) => entries
                .iter()
                .filter(|(known, _)| known != name)
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        entries.push((name.to_owned(), Value::String(secret.to_owned())));
        self.secrets = Value::Object(entries.into());
    }
}

fn runner_object(runner: &RunnerFacts) -> Value {
    object([
        ("os", runner.os.clone()),
        ("arch", runner.arch.clone()),
        ("name", runner.name.clone()),
        ("environment", "self-hosted".to_owned()),
        ("tool_cache", runner.tool_cache.clone()),
        ("temp", runner.temp.clone()),
        ("workspace", runner.workspace.clone()),
    ])
}

pub(crate) fn value(json: serde_json::Value) -> Value {
    serde_json::from_value(json).unwrap_or(Value::Null)
}

fn object<'a>(pairs: impl IntoIterator<Item = (&'a str, String)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(name, text)| (name.to_owned(), Value::String(text)))
            .collect::<Vec<_>>()
            .into(),
    )
}

pub(crate) fn strings(map: &BTreeMap<String, String>) -> Value {
    Value::Object(
        map.iter()
            .map(|(name, text)| (name.clone(), Value::String(text.clone())))
            .collect::<Vec<_>>()
            .into(),
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Standing {
    pub(crate) failed: bool,
    pub(crate) cancelled: bool,
}

impl Standing {
    fn name(self) -> &'static str {
        if self.cancelled {
            "cancelled"
        } else if self.failed {
            "failure"
        } else {
            "success"
        }
    }

    pub(crate) fn succeeding(self) -> bool {
        !self.cancelled && !self.failed
    }
}

pub(crate) struct Record {
    pub(crate) key: String,
    pub(crate) outputs: BTreeMap<String, String>,
    pub(crate) outcome: Conclusion,
    pub(crate) conclusion: Conclusion,
}

fn result(conclusion: Conclusion) -> &'static str {
    match conclusion {
        Conclusion::Success => "success",
        Conclusion::Failure | Conclusion::TimedOut => "failure",
        Conclusion::Cancelled => "cancelled",
        Conclusion::Skipped => "skipped",
    }
}

fn steps(records: &[Record]) -> Value {
    Value::Object(
        records
            .iter()
            .map(|record| {
                let step = vec![
                    ("outputs".to_owned(), strings(&record.outputs)),
                    (
                        "outcome".to_owned(),
                        Value::String(result(record.outcome).to_owned()),
                    ),
                    (
                        "conclusion".to_owned(),
                        Value::String(result(record.conclusion).to_owned()),
                    ),
                ];
                (record.key.clone(), Value::Object(step.into()))
            })
            .collect::<Vec<_>>()
            .into(),
    )
}

pub(crate) struct StepScope {
    fixed: Fixed,
    github: Value,
    pub(crate) env: Value,
    job: Value,
    steps: Value,
    standing: Standing,
    hashing: Hashing,
}

impl StepScope {
    pub(crate) fn new(
        fixed: &Fixed,
        github: Value,
        env: Value,
        records: &[Record],
        standing: Standing,
        container: Option<(&str, &str)>,
        hashing: Hashing,
    ) -> StepScope {
        let container = match container {
            Some((id, network)) => object([("id", id.to_owned()), ("network", network.to_owned())]),
            None => Value::Null,
        };
        StepScope {
            fixed: fixed.clone(),
            github,
            env,
            job: Value::Object(
                vec![
                    (
                        "status".to_owned(),
                        Value::String(standing.name().to_owned()),
                    ),
                    ("container".to_owned(), container),
                ]
                .into(),
            ),
            steps: steps(records),
            standing,
            hashing,
        }
    }
}

impl Scope for StepScope {
    fn context(&self, context: Context) -> Value {
        match context {
            Context::Github => self.github.clone(),
            Context::Env => self.env.clone(),
            Context::Vars => self.fixed.vars.clone(),
            Context::Job => self.job.clone(),
            Context::Jobs => Value::Null,
            Context::Steps => self.steps.clone(),
            Context::Runner => self.fixed.runner.clone(),
            Context::Secrets => self.fixed.secrets.clone(),
            Context::Strategy => self.fixed.strategy.clone(),
            Context::Matrix => self.fixed.matrix.clone(),
            Context::Needs => self.fixed.needs.clone(),
            Context::Inputs => self.fixed.inputs.clone(),
            Context::Bosun => self.fixed.bosun.clone(),
        }
    }

    fn status(&self, status: Status) -> Option<bool> {
        let standing = self.standing;
        Some(match status {
            Status::Success => standing.succeeding(),
            Status::Failure => !standing.cancelled && standing.failed,
            Status::Cancelled => standing.cancelled,
        })
    }

    fn hash_files(&self, patterns: &[String]) -> Option<Result<String, String>> {
        let hash = || {
            hash::files(&self.hashing.workspace, patterns, &hash::LIMITS, &|| {
                self.hashing.reason()
            })
        };
        Some(
            if Handle::try_current()
                .is_ok_and(|handle| matches!(handle.runtime_flavor(), RuntimeFlavor::MultiThread))
            {
                tokio::task::block_in_place(hash)
            } else {
                hash()
            },
        )
    }
}

pub(crate) fn fault(what: &str, error: &bosun_expr::Error) -> String {
    format!(
        "{what} could not be evaluated at line {}, column {}: {}",
        error.span.line, error.span.column, error.message
    )
}

fn typed(template: &Template, scope: &dyn Scope) -> Result<Value, bosun_expr::Error> {
    match template.segments() {
        [Segment::Expression(expr)] => bosun_expr::evaluate(expr, scope),
        _ => bosun_expr::render(template, scope).map(Value::String),
    }
}

fn described(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Boolean(flag) => format!("the boolean {flag}"),
        Value::Number(number) => format!("the number {number}"),
        Value::String(_) => "a string".to_owned(),
        Value::Array(_) => "an array".to_owned(),
        Value::Object(_) => "an object".to_owned(),
    }
}

pub(crate) fn flag(
    what: &str,
    evaluable: &Evaluable<bool>,
    scope: &dyn Scope,
) -> Result<bool, String> {
    match evaluable {
        Evaluable::Literal(flag) => Ok(*flag),
        Evaluable::Template(template) => {
            match typed(template, scope).map_err(|error| fault(what, &error))? {
                Value::Boolean(flag) => Ok(flag),
                other => Err(format!(
                    "{what} must be `true` or `false`, and its expression gives {}",
                    described(&other)
                )),
            }
        }
    }
}

pub(crate) fn minutes(
    what: &str,
    evaluable: &Evaluable<u32>,
    scope: &dyn Scope,
) -> Result<u32, String> {
    match evaluable {
        Evaluable::Literal(minutes) => Ok(*minutes),
        Evaluable::Template(template) => {
            match typed(template, scope).map_err(|error| fault(what, &error))? {
                Value::Number(minutes)
                    if minutes.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&minutes) =>
                {
                    Ok(minutes as u32)
                }
                other => Err(format!(
                    "{what} must be a whole number of at least 0, and its expression gives {}",
                    described(&other)
                )),
            }
        }
    }
}

pub(crate) fn text(what: &str, template: &Template, scope: &dyn Scope) -> Result<String, String> {
    bosun_expr::render(template, scope).map_err(|error| fault(what, &error))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::sync::watch;
    use tokio::time::Instant;

    use super::Hashing;

    #[test]
    fn a_hash_halts_when_the_job_is_cancelled_or_its_deadline_has_passed() {
        let (cancel, cancelled) = watch::channel(false);
        let halt = Hashing::new(
            PathBuf::new(),
            cancelled.clone(),
            Some(Instant::now() + std::time::Duration::from_secs(3600)),
        );
        assert_eq!(halt.reason(), None);
        cancel.send_replace(true);
        assert_eq!(halt.reason(), Some("the job was cancelled"));
        let late = Hashing::new(
            PathBuf::new(),
            watch::channel(false).1,
            Some(Instant::now()),
        );
        assert_eq!(
            late.reason(),
            Some("the job ran beyond its `timeout-minutes`")
        );
    }
}
