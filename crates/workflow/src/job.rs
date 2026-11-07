use std::str::FromStr;

use bosun_expr::Key;
use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{Evaluable, JobKind, JobTemplate, RunsOn, Template};
use bosun_types::key::JobKey;

use crate::Code;
use crate::expression::OPEN;
use crate::graph::{GraphJob, Need};
use crate::lower::{Entry, Fields, Lowering, cover, optional};
use crate::surface::{JOB, RUNS_ON};
use crate::yaml::{Node, Value};

const DEFAULT_TIMEOUT_MINUTES: u32 = 360;

const CALL_KEYS: &[&str] = &[
    "name",
    "needs",
    "if",
    "uses",
    "with",
    "secrets",
    "permissions",
    "concurrency",
    "strategy",
];

impl Lowering<'_> {
    pub(crate) fn jobs(&mut self, entry: &Entry<'_>) -> Option<Vec<JobTemplate>> {
        let found = self.entries(entry.value, &entry.path, "`jobs`")?;
        if found.entries.is_empty() && found.complete {
            self.error(
                Code::InvalidValue,
                entry.value.span,
                &entry.path,
                "`jobs` defines no jobs".to_string(),
            );
            return None;
        }
        let mut graph = Vec::new();
        let mut jobs = Vec::new();
        let mut complete = found.complete;
        for job in &found.entries {
            self.enter(job.name);
            match self.job(job, &mut graph) {
                Some(job) => jobs.push(job),
                None => complete = false,
            }
        }
        self.graph(&graph);
        complete.then_some(jobs)
    }

    fn job<'n>(&mut self, entry: &Entry<'n>, graph: &mut Vec<GraphJob<'n>>) -> Option<JobTemplate> {
        let key = match JobKey::from_str(entry.name) {
            Ok(key) => Some(key),
            Err(error) => {
                self.error(
                    Code::InvalidJobKey,
                    entry.key.span,
                    &entry.path,
                    error.to_string(),
                );
                None
            }
        };
        let place = format!("job `{}`", entry.name);
        let Some(fields) = self.fields(entry.value, &JOB, &place, &entry.path, entry.key.span)
        else {
            graph.push(GraphJob {
                name: entry.name,
                needs: Vec::new(),
            });
            return None;
        };
        let what = |key: &str| format!("`{key}` of {place}");
        let calls = fields.get("uses").is_some();
        let own = |key: &str| if calls { None } else { fields.get(key) };
        let name = optional(fields.get("name"), |field| {
            self.template(field.value, &field.path, &what("name"), Key::JobName)
        });
        let needs = optional(fields.get("needs"), |field| {
            self.strings(field.value, &field.path, &what("needs"))
        })
        .map(|items| {
            items
                .unwrap_or_default()
                .into_iter()
                .map(|item| Need {
                    name: item.text,
                    span: item.node.span,
                    path: item.path,
                })
                .collect::<Vec<_>>()
        });
        let condition = optional(fields.get("if"), |field| {
            self.condition(field.value, &field.path, &what("if"), Key::JobIf)
        });
        let permissions = optional(fields.get("permissions"), |field| {
            self.permissions(field, &place)
        });
        let concurrency = optional(fields.get("concurrency"), |field| {
            self.concurrency(field, &place, Key::JobConcurrency)
        });
        let strategy = optional(fields.get("strategy"), |field| self.strategy(field, &place));
        let env = optional(own("env"), |field| {
            self.env(field.value, &field.path, &place, Key::JobEnv)
        });
        let defaults = optional(own("defaults"), |field| {
            self.defaults(field, &place, Some(Key::JobDefaultsRun))
        });
        let timeout_minutes = optional(own("timeout-minutes"), |field| {
            self.whole(
                field.value,
                &field.path,
                &what("timeout-minutes"),
                Key::JobTimeoutMinutes,
                0,
            )
        });
        let continue_on_error = optional(own("continue-on-error"), |field| {
            self.flag(
                field.value,
                &field.path,
                &what("continue-on-error"),
                Key::JobContinueOnError,
            )
        });
        let outputs = optional(own("outputs"), |field| {
            self.templates(
                field.value,
                &field.path,
                &what("outputs"),
                "output",
                Key::JobOutputs,
            )
        });
        let kind = self.job_kind(&fields, &place, entry);
        let container = optional(own("container"), |field| self.container(field, &place));
        let services = optional(own("services"), |field| self.services(field, &place));
        let cache_mode = optional(own("cache-mode"), |field| self.cache_mode(field, &place));
        let cache_volumes = optional(own("cache-volumes"), |field| {
            self.cache_volumes(field, &place)
        });
        let priority = optional(own("priority"), |field| self.priority(field, &place));
        graph.push(GraphJob {
            name: entry.name,
            needs: needs.clone().unwrap_or_default(),
        });
        let needs = needs?
            .iter()
            .map(|need| JobKey::from_str(&need.name).ok())
            .collect::<Option<Vec<_>>>();
        let (runs_on, kind) = kind?;
        Some(JobTemplate {
            key: key?,
            name: name?,
            needs: needs?,
            r#if: condition?.flatten(),
            runs_on,
            permissions: permissions?,
            concurrency: concurrency?,
            strategy: strategy?,
            env: env?.unwrap_or_default(),
            defaults: defaults?.unwrap_or_default(),
            timeout_minutes: timeout_minutes?
                .unwrap_or(Evaluable::Literal(DEFAULT_TIMEOUT_MINUTES)),
            continue_on_error: continue_on_error?.unwrap_or(Evaluable::Literal(false)),
            outputs: outputs?.unwrap_or_default(),
            secrets: self.referenced(),
            container: container?,
            services: services?.unwrap_or_default(),
            cache_mode: cache_mode?,
            cache_volumes: cache_volumes?.unwrap_or_default(),
            priority: priority?,
            kind,
            span: cover(entry.key.span, entry.value.span),
        })
    }

    fn job_kind(
        &mut self,
        fields: &Fields<'_>,
        place: &str,
        entry: &Entry<'_>,
    ) -> Option<(Option<Evaluable<RunsOn>>, JobKind)> {
        let name = entry.name;
        let Some(uses) = fields.get("uses") else {
            for key in ["with", "secrets"] {
                if let Some(field) = fields.get(key) {
                    self.error(
                        Code::InvalidValue,
                        field.value.span,
                        &field.path,
                        format!("`{key}` of {place} applies only to a job that calls a workflow with `uses`"),
                    );
                }
            }
            if fields.complete {
                for required in ["runs-on", "steps"] {
                    if fields.get(required).is_none() {
                        self.error(
                            Code::MissingKey,
                            entry.key.span,
                            &entry.path,
                            format!("missing key `{required}` in {place}"),
                        );
                    }
                }
            }
            let runs_on = fields
                .get("runs-on")
                .and_then(|field| self.runs_on(field, place));
            let steps = fields
                .get("steps")
                .and_then(|field| self.steps(field, &format!("job `{name}`"), false));
            return Some((Some(runs_on?), JobKind::Steps(steps?)));
        };
        let mut valid = true;
        for field in &fields.entries {
            if !CALL_KEYS.contains(&field.name) {
                valid = false;
                self.error(
                    Code::InvalidValue,
                    field.key.span,
                    &field.path,
                    format!(
                        "`{}` of {place} cannot be used together with `uses`, which calls a workflow",
                        field.name
                    ),
                );
            }
        }
        let call = self.call(uses, fields.get("with"), fields.get("secrets"), place);
        let call = call.filter(|_| valid);
        Some((None, JobKind::Call(call?)))
    }

    fn labels(&mut self, node: &Node, path: &[PathSegment], what: &str) -> Option<Vec<Template>> {
        let mut labels = Vec::new();
        let mut complete = true;
        for item in self.strings(node, path, what)? {
            match self.parse_template(&item.text, item.node, &item.path, Key::JobRunsOn) {
                Some(label) => labels.push(label),
                None => complete = false,
            }
        }
        complete.then_some(labels)
    }

    fn runs_on(&mut self, entry: &Entry<'_>, place: &str) -> Option<Evaluable<RunsOn>> {
        let what = format!("`runs-on` of {place}");
        match &entry.value.value {
            Value::Mapping(_) => {
                let fields =
                    self.fields(entry.value, &RUNS_ON, &what, &entry.path, entry.key.span)?;
                let group = optional(fields.get("group"), |field| {
                    self.template(
                        field.value,
                        &field.path,
                        &format!("`group` of {what}"),
                        Key::JobRunsOn,
                    )
                });
                let labels = optional(fields.get("labels"), |field| {
                    self.labels(field.value, &field.path, &format!("`labels` of {what}"))
                });
                let runs_on = RunsOn {
                    group: group?,
                    labels: labels?.unwrap_or_default(),
                };
                if !fields.complete {
                    return None;
                }
                if runs_on.group.is_none() && runs_on.labels.is_empty() {
                    self.error(
                        Code::InvalidValue,
                        entry.value.span,
                        &entry.path,
                        format!("{what} names neither a group nor labels"),
                    );
                    return None;
                }
                Some(Evaluable::Literal(runs_on))
            }
            Value::String(text) if text.contains(OPEN) => self
                .parse_template(text, entry.value, &entry.path, Key::JobRunsOn)
                .map(Evaluable::Template),
            _ => {
                let labels = self.labels(entry.value, &entry.path, &what)?;
                if labels.is_empty() {
                    self.error(
                        Code::InvalidValue,
                        entry.value.span,
                        &entry.path,
                        format!("{what} names no labels"),
                    );
                    return None;
                }
                Some(Evaluable::Literal(RunsOn {
                    group: None,
                    labels,
                }))
            }
        }
    }
}
