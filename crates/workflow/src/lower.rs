use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::ir::{
    Action, Evaluable, JobKind, JobTemplate, RunsOn, Shell, Span, Step, Template, Trigger,
    Workflow, WorkflowDispatch,
};
use bosun_types::key::{JobKey, StepKey};

use crate::Code;
use crate::graph::{GraphJob, Need};
use crate::surface::{EVENTS, JOB, Known, RUNS_ON, STEP, Shape, WORKFLOW, WORKFLOW_DISPATCH};
use crate::yaml::{Node, Value};

pub(crate) struct Lowering<'f> {
    file: &'f str,
    diagnostics: Vec<Diagnostic>,
}

struct Entry<'n> {
    name: &'n str,
    key: &'n Node,
    value: &'n Node,
    path: Vec<PathSegment>,
}

struct Fields<'n> {
    entries: Vec<Entry<'n>>,
    complete: bool,
}

impl<'n> Fields<'n> {
    fn get(&self, name: &str) -> Option<&Entry<'n>> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

struct Item<'n> {
    text: String,
    node: &'n Node,
    path: Vec<PathSegment>,
}

fn optional<'n, T>(
    entry: Option<&Entry<'n>>,
    read: impl FnOnce(&Entry<'n>) -> Option<T>,
) -> Option<Option<T>> {
    match entry {
        None => Some(None),
        Some(entry) => read(entry).map(Some),
    }
}

fn child(path: &[PathSegment], key: &str) -> Vec<PathSegment> {
    let mut path = path.to_vec();
    path.push(PathSegment::Key(key.to_string()));
    path
}

fn index(path: &[PathSegment], index: usize) -> Vec<PathSegment> {
    let mut path = path.to_vec();
    path.push(PathSegment::Index(u64::try_from(index).unwrap_or(u64::MAX)));
    path
}

fn cover(start: Span, end: Span) -> Span {
    Span {
        line: start.line,
        column: start.column,
        end_line: end.end_line,
        end_column: end.end_column,
    }
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(number_text(*value)),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) => None,
    }
}

fn number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let scientific = format!("{value:.14e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if value < 0.0 { "-" } else { "" };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    if (-4..15).contains(&exponent) {
        if exponent < 0 {
            let zeros = "0".repeat(exponent.unsigned_abs() as usize - 1);
            format!("{sign}0.{zeros}{digits}")
        } else {
            let whole = exponent as usize + 1;
            if digits.len() <= whole {
                format!("{sign}{digits}{}", "0".repeat(whole - digits.len()))
            } else {
                format!("{sign}{}.{}", &digits[..whole], &digits[whole..])
            }
        }
    } else {
        let (first, rest) = digits.split_at(1);
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let direction = if exponent < 0 { '-' } else { '+' };
        format!(
            "{sign}{first}{fraction}E{direction}{:02}",
            exponent.unsigned_abs()
        )
    }
}

impl<'f> Lowering<'f> {
    pub(crate) fn new(file: &'f str) -> Lowering<'f> {
        Lowering {
            file,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn error(&mut self, code: Code, span: Span, path: &[PathSegment], message: String) {
        self.diagnostics.push(Diagnostic {
            severity: Severity::Error,
            code: code.as_str().to_string(),
            message,
            file: self.file.to_string(),
            span,
            path: path.to_vec(),
        });
    }

    pub(crate) fn finish(
        mut self,
        workflow: Option<Workflow>,
    ) -> Result<Workflow, Vec<Diagnostic>> {
        self.diagnostics
            .sort_by_key(|diagnostic| (diagnostic.span.line, diagnostic.span.column));
        match workflow {
            Some(workflow) if self.diagnostics.is_empty() => Ok(workflow),
            _ => Err(self.diagnostics),
        }
    }

    fn not_implemented(
        &mut self,
        noun: &str,
        name: &str,
        place: &str,
        milestone: &str,
        span: Span,
        path: &[PathSegment],
    ) {
        self.error(
            Code::NotImplemented,
            span,
            path,
            format!("{noun} `{name}` in {place} is not supported yet; it arrives in {milestone}"),
        );
    }

    fn unknown(
        &mut self,
        noun: &str,
        name: &str,
        place: &str,
        suggestion: Option<&str>,
        span: Span,
        path: &[PathSegment],
    ) {
        let hint = suggestion
            .map(|suggestion| format!("; did you mean `{suggestion}`?"))
            .unwrap_or_default();
        self.error(
            Code::UnknownKey,
            span,
            path,
            format!("unknown {noun} `{name}` in {place}{hint}"),
        );
    }

    fn entries<'n>(
        &mut self,
        node: &'n Node,
        path: &[PathSegment],
        place: &str,
    ) -> Option<Fields<'n>> {
        let Value::Mapping(pairs) = &node.value else {
            self.error(
                Code::InvalidType,
                node.span,
                path,
                format!("{place} must be a mapping"),
            );
            return None;
        };
        let mut seen = BTreeSet::new();
        let mut fields = Fields {
            entries: Vec::new(),
            complete: true,
        };
        for (key, value) in pairs {
            let Value::String(name) = &key.value else {
                fields.complete = false;
                self.error(
                    Code::InvalidType,
                    key.span,
                    path,
                    format!("every key in {place} must be a string"),
                );
                continue;
            };
            let path = child(path, name);
            if name == "<<" {
                fields.complete = false;
                self.error(
                    Code::UnsupportedYaml,
                    key.span,
                    &path,
                    "merge keys (`<<`) are not supported".to_string(),
                );
            } else if !seen.insert(name.as_str()) {
                fields.complete = false;
                self.error(
                    Code::DuplicateKey,
                    key.span,
                    &path,
                    format!("key `{name}` appears twice in {place}"),
                );
            } else {
                fields.entries.push(Entry {
                    name,
                    key,
                    value,
                    path,
                });
            }
        }
        Some(fields)
    }

    fn fields<'n>(
        &mut self,
        node: &'n Node,
        shape: &Shape,
        place: &str,
        path: &[PathSegment],
        owner: Span,
    ) -> Option<Fields<'n>> {
        let found = self.entries(node, path, place)?;
        let mut fields = Fields {
            entries: Vec::new(),
            complete: found.complete,
        };
        for entry in found.entries {
            match shape.find(entry.name, &entry.value.value) {
                Known::Now => fields.entries.push(entry),
                Known::Later(milestone) => {
                    fields.complete = false;
                    self.not_implemented(
                        shape.noun,
                        entry.name,
                        place,
                        milestone,
                        entry.key.span,
                        &entry.path,
                    );
                }
                Known::Unknown(suggestion) => {
                    fields.complete = false;
                    self.unknown(
                        shape.noun,
                        entry.name,
                        place,
                        suggestion,
                        entry.key.span,
                        &entry.path,
                    );
                }
            }
        }
        if fields.complete {
            for required in shape.required {
                if fields.get(required).is_none() {
                    self.error(
                        Code::MissingKey,
                        owner,
                        path,
                        format!("missing key `{required}` in {place}"),
                    );
                }
            }
        }
        Some(fields)
    }

    fn string(&mut self, node: &Node, path: &[PathSegment], what: &str) -> Option<String> {
        let text = scalar_text(&node.value);
        if text.is_none() {
            self.error(
                Code::InvalidType,
                node.span,
                path,
                format!("{what} must be a string"),
            );
        }
        text
    }

    fn literal(&mut self, text: String, node: &Node, path: &[PathSegment]) -> Option<String> {
        if text.contains("${{") {
            self.error(
                Code::NotImplemented,
                node.span,
                path,
                "expressions are not supported yet; they arrive in M1".to_string(),
            );
            return None;
        }
        Some(text)
    }

    fn template(&mut self, node: &Node, path: &[PathSegment], what: &str) -> Option<Template> {
        let text = self.string(node, path, what)?;
        self.literal(text, node, path).map(Template::text)
    }

    fn strings<'n>(
        &mut self,
        node: &'n Node,
        path: &[PathSegment],
        what: &str,
    ) -> Option<Vec<Item<'n>>> {
        match &node.value {
            Value::Sequence(items) => {
                let mut texts = Vec::new();
                let mut complete = true;
                for (position, item) in items.iter().enumerate() {
                    let path = index(path, position);
                    match scalar_text(&item.value) {
                        Some(text) => texts.push(Item {
                            text,
                            node: item,
                            path,
                        }),
                        None => {
                            complete = false;
                            self.error(
                                Code::InvalidType,
                                item.span,
                                &path,
                                format!("every entry of {what} must be a string"),
                            );
                        }
                    }
                }
                complete.then_some(texts)
            }
            value => match scalar_text(value) {
                Some(text) => Some(vec![Item {
                    text,
                    node,
                    path: path.to_vec(),
                }]),
                None => {
                    self.error(
                        Code::InvalidType,
                        node.span,
                        path,
                        format!("{what} must be a string or a list of strings"),
                    );
                    None
                }
            },
        }
    }

    fn env(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        owner: &str,
    ) -> Option<BTreeMap<String, Template>> {
        let place = format!("`env` of {owner}");
        let mut env = BTreeMap::new();
        let found = self.entries(node, path, &place)?;
        let mut complete = found.complete;
        for entry in found.entries {
            if entry.name.is_empty() {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    entry.key.span,
                    &entry.path,
                    format!("a variable in {place} has an empty name"),
                );
                continue;
            }
            let what = format!("variable `{}` in {place}", entry.name);
            match self.template(entry.value, &entry.path, &what) {
                Some(template) => {
                    env.insert(entry.name.to_string(), template);
                }
                None => complete = false,
            }
        }
        complete.then_some(env)
    }

    pub(crate) fn workflow(&mut self, root: &Node) -> Option<Workflow> {
        let fields = self.fields(root, &WORKFLOW, "the workflow", &[], root.span)?;
        let name = optional(fields.get("name"), |entry| {
            self.string(entry.value, &entry.path, "`name` of the workflow")
        });
        let triggers = fields.get("on").and_then(|entry| self.triggers(entry));
        let env = optional(fields.get("env"), |entry| {
            self.env(entry.value, &entry.path, "the workflow")
        });
        let jobs = fields.get("jobs").and_then(|entry| self.jobs(entry));
        Some(Workflow {
            name: name?,
            triggers: triggers?,
            env: env?.unwrap_or_default(),
            jobs: jobs?,
        })
    }

    fn triggers(&mut self, entry: &Entry<'_>) -> Option<Vec<Trigger>> {
        let mut triggers = Vec::new();
        let mut complete = true;
        if let Value::Mapping(_) = entry.value.value {
            let fields = self.fields(entry.value, &EVENTS, "`on`", &entry.path, entry.key.span)?;
            complete = fields.complete;
            for event in &fields.entries {
                match self.dispatch(event) {
                    Some(trigger) => triggers.push(trigger),
                    None => complete = false,
                }
            }
        } else {
            let mut seen = BTreeSet::new();
            for item in self.strings(entry.value, &entry.path, "`on`")? {
                match EVENTS.find(&item.text, &item.node.value) {
                    Known::Now if !seen.insert(item.text.clone()) => {
                        complete = false;
                        self.error(
                            Code::DuplicateKey,
                            item.node.span,
                            &item.path,
                            format!("event `{}` appears twice in `on`", item.text),
                        );
                    }
                    Known::Now => {
                        triggers.push(Trigger::WorkflowDispatch(WorkflowDispatch::default()))
                    }
                    Known::Later(milestone) => {
                        complete = false;
                        self.not_implemented(
                            "event",
                            &item.text,
                            "`on`",
                            milestone,
                            item.node.span,
                            &item.path,
                        );
                    }
                    Known::Unknown(suggestion) => {
                        complete = false;
                        self.unknown(
                            "event",
                            &item.text,
                            "`on`",
                            suggestion,
                            item.node.span,
                            &item.path,
                        );
                    }
                }
            }
        }
        if complete && triggers.is_empty() {
            self.error(
                Code::InvalidValue,
                entry.value.span,
                &entry.path,
                "`on` names no events".to_string(),
            );
            return None;
        }
        complete.then_some(triggers)
    }

    fn dispatch(&mut self, event: &Entry<'_>) -> Option<Trigger> {
        match event.value.value {
            Value::Null => {}
            Value::Mapping(_) => {
                let fields = self.fields(
                    event.value,
                    &WORKFLOW_DISPATCH,
                    "`workflow_dispatch`",
                    &event.path,
                    event.key.span,
                )?;
                if !fields.complete {
                    return None;
                }
            }
            _ => {
                self.error(
                    Code::InvalidType,
                    event.value.span,
                    &event.path,
                    "`workflow_dispatch` must be empty or a mapping".to_string(),
                );
                return None;
            }
        }
        Some(Trigger::WorkflowDispatch(WorkflowDispatch::default()))
    }

    fn jobs(&mut self, entry: &Entry<'_>) -> Option<Vec<JobTemplate>> {
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
        let name = optional(fields.get("name"), |field| {
            self.template(field.value, &field.path, &format!("`name` of {place}"))
        });
        let needs = optional(fields.get("needs"), |field| {
            self.strings(field.value, &field.path, &format!("`needs` of {place}"))
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
        let runs_on = fields
            .get("runs-on")
            .and_then(|field| self.runs_on(field, &place));
        let env = optional(fields.get("env"), |field| {
            self.env(field.value, &field.path, &place)
        });
        let steps = fields
            .get("steps")
            .and_then(|field| self.steps(field, entry.name));
        graph.push(GraphJob {
            name: entry.name,
            needs: needs.clone().unwrap_or_default(),
        });
        let needs = needs?
            .iter()
            .map(|need| JobKey::from_str(&need.name).ok())
            .collect::<Option<Vec<_>>>();
        Some(JobTemplate {
            key: key?,
            name: name?,
            needs: needs?,
            runs_on: Evaluable::Literal(runs_on?),
            env: env?.unwrap_or_default(),
            kind: JobKind::Steps(steps?),
            span: cover(entry.key.span, entry.value.span),
        })
    }

    fn labels(&mut self, node: &Node, path: &[PathSegment], what: &str) -> Option<Vec<String>> {
        let mut labels = Vec::new();
        let mut complete = true;
        for item in self.strings(node, path, what)? {
            match self.literal(item.text, item.node, &item.path) {
                Some(label) => labels.push(label),
                None => complete = false,
            }
        }
        complete.then_some(labels)
    }

    fn runs_on(&mut self, entry: &Entry<'_>, place: &str) -> Option<RunsOn> {
        let what = format!("`runs-on` of {place}");
        if let Value::Mapping(_) = entry.value.value {
            let fields = self.fields(entry.value, &RUNS_ON, &what, &entry.path, entry.key.span)?;
            let group = optional(fields.get("group"), |field| {
                let text = self.string(field.value, &field.path, &format!("`group` of {what}"))?;
                self.literal(text, field.value, &field.path)
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
            Some(runs_on)
        } else {
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
            Some(RunsOn {
                group: None,
                labels,
            })
        }
    }

    fn steps(&mut self, entry: &Entry<'_>, job: &str) -> Option<Vec<Step>> {
        let what = format!("`steps` of job `{job}`");
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
            match self.step(item, &path, position, job, &mut keys) {
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
        job: &str,
        keys: &mut BTreeSet<String>,
    ) -> Option<Step> {
        let place = format!("step {} of job `{job}`", position + 1);
        let fields = self.fields(node, &STEP, &place, path, node.span)?;
        let key = optional(fields.get("id"), |field| {
            self.step_key(field, &place, job, keys)
        });
        let name = optional(fields.get("name"), |field| {
            self.template(field.value, &field.path, &format!("`name` of {place}"))
        });
        let script = fields.get("run").and_then(|field| {
            self.template(field.value, &field.path, &format!("`run` of {place}"))
        });
        let shell = optional(fields.get("shell"), |field| self.shell(field, &place));
        let working_directory = optional(fields.get("working-directory"), |field| {
            self.template(
                field.value,
                &field.path,
                &format!("`working-directory` of {place}"),
            )
        });
        let env = optional(fields.get("env"), |field| {
            self.env(field.value, &field.path, &place)
        });
        Some(Step {
            key: key?,
            name: name?,
            action: Action::Run { script: script? },
            shell: shell?,
            working_directory: working_directory?,
            env: env?.unwrap_or_default(),
            span: node.span,
        })
    }

    fn step_key(
        &mut self,
        field: &Entry<'_>,
        place: &str,
        job: &str,
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
                    format!("step key `{text}` is used twice in job `{job}`"),
                );
                None
            }
            Ok(key) => Some(key),
        }
    }

    fn shell(&mut self, field: &Entry<'_>, place: &str) -> Option<Shell> {
        let text = self.string(field.value, &field.path, &format!("`shell` of {place}"))?;
        let text = self.literal(text, field.value, &field.path)?;
        match text.parse::<Shell>() {
            Ok(shell) => Some(shell),
            Err(error) => {
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!("`shell` of {place} is {error}"),
                );
                None
            }
        }
    }
}
