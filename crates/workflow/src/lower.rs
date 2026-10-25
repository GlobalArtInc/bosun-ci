use std::collections::{BTreeMap, BTreeSet};

use bosun_expr::Key;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::ir::{SecretReferences, Span, Template, Trigger, Workflow};

use crate::Code;
use crate::place::Text;
use crate::surface::{Known, Milestone, Shape, WORKFLOW};
use crate::yaml::{Node, Value};

pub(crate) struct Mark {
    pub(crate) span: Span,
    pub(crate) path: Vec<PathSegment>,
    pub(crate) what: String,
}

pub(crate) struct Lowering<'f> {
    file: &'f str,
    text: Text,
    diagnostics: Vec<Diagnostic>,
    pub(crate) triggers: Option<Vec<Trigger>>,
    job: Option<String>,
    secrets: SecretReferences,
    shared: SecretReferences,
    pub(crate) extensions: Vec<Mark>,
    pub(crate) bosun: Vec<Mark>,
    pub(crate) checkouts: Vec<Mark>,
}

pub(crate) struct Entry<'n> {
    pub(crate) name: &'n str,
    pub(crate) key: &'n Node,
    pub(crate) value: &'n Node,
    pub(crate) path: Vec<PathSegment>,
}

pub(crate) struct Fields<'n> {
    pub(crate) entries: Vec<Entry<'n>>,
    pub(crate) complete: bool,
}

impl<'n> Fields<'n> {
    pub(crate) fn get(&self, name: &str) -> Option<&Entry<'n>> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

pub(crate) struct Item<'n> {
    pub(crate) text: String,
    pub(crate) node: &'n Node,
    pub(crate) path: Vec<PathSegment>,
}

pub(crate) fn optional<'n, T>(
    entry: Option<&Entry<'n>>,
    read: impl FnOnce(&Entry<'n>) -> Option<T>,
) -> Option<Option<T>> {
    match entry {
        None => Some(None),
        Some(entry) => read(entry).map(Some),
    }
}

fn article(noun: &str) -> String {
    match noun.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => format!("an {noun}"),
        _ => format!("a {noun}"),
    }
}

pub(crate) fn child(path: &[PathSegment], key: &str) -> Vec<PathSegment> {
    let mut path = path.to_vec();
    path.push(PathSegment::Key(key.to_string()));
    path
}

pub(crate) fn index(path: &[PathSegment], index: usize) -> Vec<PathSegment> {
    let mut path = path.to_vec();
    path.push(PathSegment::Index(u64::try_from(index).unwrap_or(u64::MAX)));
    path
}

pub(crate) fn cover(start: Span, end: Span) -> Span {
    Span {
        line: start.line,
        column: start.column,
        end_line: end.end_line,
        end_column: end.end_column,
    }
}

pub(crate) fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(number_text(*value)),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) => None,
    }
}

pub(crate) fn number_text(value: f64) -> String {
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
    pub(crate) fn new(file: &'f str, source: &str) -> Lowering<'f> {
        Lowering {
            file,
            text: Text::new(source),
            diagnostics: Vec::new(),
            triggers: None,
            job: None,
            secrets: SecretReferences::Named(BTreeSet::new()),
            shared: SecretReferences::Named(BTreeSet::new()),
            extensions: Vec::new(),
            bosun: Vec::new(),
            checkouts: Vec::new(),
        }
    }

    pub(crate) fn file(&self) -> &str {
        self.file
    }

    pub(crate) fn text(&self) -> &Text {
        &self.text
    }

    pub(crate) fn current_job(&self) -> Option<&str> {
        self.job.as_deref()
    }

    pub(crate) fn secrets_mut(&mut self) -> &mut SecretReferences {
        &mut self.secrets
    }

    pub(crate) fn enter(&mut self, job: &str) {
        self.job = Some(job.to_string());
        self.secrets = self.shared.clone();
    }

    pub(crate) fn referenced(&mut self) -> SecretReferences {
        std::mem::replace(&mut self.secrets, SecretReferences::Named(BTreeSet::new()))
    }

    pub(crate) fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    fn report(
        &mut self,
        severity: Severity,
        code: Code,
        span: Span,
        path: &[PathSegment],
        message: String,
    ) {
        self.diagnostics.push(Diagnostic {
            severity,
            code: code.as_str().to_string(),
            message,
            file: self.file.to_string(),
            span,
            path: path.to_vec(),
        });
    }

    pub(crate) fn error(&mut self, code: Code, span: Span, path: &[PathSegment], message: String) {
        self.report(Severity::Error, code, span, path, message);
    }

    pub(crate) fn warning(
        &mut self,
        code: Code,
        span: Span,
        path: &[PathSegment],
        message: String,
    ) {
        self.report(Severity::Warning, code, span, path, message);
    }

    fn sorted(&mut self) -> bool {
        self.diagnostics
            .sort_by_key(|diagnostic| (diagnostic.span.line, diagnostic.span.column));
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }

    pub(crate) fn finish<T>(
        mut self,
        lowered: Option<T>,
    ) -> Result<(T, Vec<Diagnostic>), Vec<Diagnostic>> {
        let failed = self.sorted();
        match lowered {
            Some(lowered) if !failed => Ok((lowered, self.diagnostics)),
            _ => Err(self.diagnostics),
        }
    }

    pub(crate) fn finish_lint(mut self) -> Vec<Diagnostic> {
        self.sorted();
        self.diagnostics
    }

    pub(crate) fn finish_triggers(mut self) -> (Option<Vec<Trigger>>, Vec<Diagnostic>) {
        self.sorted();
        let on = PathSegment::Key("on".to_string());
        let triggers = self.triggers.filter(|_| {
            !self.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error && diagnostic.path.first() == Some(&on)
            })
        });
        (triggers, self.diagnostics)
    }

    pub(crate) fn not_implemented(
        &mut self,
        noun: &str,
        name: &str,
        place: &str,
        milestone: Milestone,
        span: Span,
        path: &[PathSegment],
    ) {
        self.error(
            Code::NotImplemented,
            span,
            path,
            format!(
                "{noun} `{name}` in {place} is not supported yet; it arrives in {}",
                milestone.name()
            ),
        );
    }

    pub(crate) fn unknown(
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

    pub(crate) fn entries<'n>(
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

    pub(crate) fn fields<'n>(
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
            if shape.extension(entry.name) {
                self.extensions.push(Mark {
                    span: entry.key.span,
                    path: entry.path.clone(),
                    what: format!("{} `{}`", shape.noun, entry.name),
                });
            }
            match shape.find(entry.name) {
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
                Known::Unknown(suggestion) if shape.loose => {
                    let hint = suggestion
                        .map(|suggestion| format!("; did you mean `{suggestion}`?"))
                        .unwrap_or_default();
                    self.warning(
                        Code::IgnoredKey,
                        entry.key.span,
                        &entry.path,
                        format!(
                            "unknown {} `{}` in {place} is ignored{hint}",
                            shape.noun, entry.name
                        ),
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

    pub(crate) fn string(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
    ) -> Option<String> {
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

    pub(crate) fn strings<'n>(
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

    pub(crate) fn templates(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        place: &str,
        noun: &str,
        key: Key,
    ) -> Option<BTreeMap<String, Template>> {
        let mut templates = BTreeMap::new();
        let found = self.entries(node, path, place)?;
        let mut complete = found.complete;
        for entry in found.entries {
            if entry.name.is_empty() {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    entry.key.span,
                    &entry.path,
                    format!("{} in {place} has an empty name", article(noun)),
                );
                continue;
            }
            let what = format!("{noun} `{}` in {place}", entry.name);
            match self.template(entry.value, &entry.path, &what, key) {
                Some(template) => {
                    templates.insert(entry.name.to_string(), template);
                }
                None => complete = false,
            }
        }
        complete.then_some(templates)
    }

    pub(crate) fn env(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        owner: &str,
        key: Key,
    ) -> Option<BTreeMap<String, Template>> {
        self.templates(node, path, &format!("`env` of {owner}"), "variable", key)
    }

    pub(crate) fn workflow(&mut self, root: &Node) -> Option<Workflow> {
        let place = "the workflow";
        let fields = self.fields(root, &WORKFLOW, place, &[], root.span)?;
        let name = optional(fields.get("name"), |entry| {
            self.string(entry.value, &entry.path, "`name` of the workflow")
        });
        let triggers = fields.get("on").and_then(|entry| self.on(entry));
        self.triggers.clone_from(&triggers);
        let env = optional(fields.get("env"), |entry| {
            self.env(entry.value, &entry.path, place, Key::WorkflowEnv)
        });
        self.shared = self.referenced();
        let run_name = optional(fields.get("run-name"), |entry| {
            self.template(entry.value, &entry.path, "`run-name`", Key::RunName)
        });
        let permissions = optional(fields.get("permissions"), |entry| {
            self.permissions(entry, place)
        });
        let defaults = optional(fields.get("defaults"), |entry| {
            self.defaults(entry, place, None)
        });
        let concurrency = optional(fields.get("concurrency"), |entry| {
            self.concurrency(entry, place, Key::WorkflowConcurrency)
        });
        let jobs = fields.get("jobs").and_then(|entry| self.jobs(entry));
        Some(Workflow {
            name: name?,
            run_name: run_name?,
            triggers: triggers?,
            permissions: permissions?,
            env: env?.unwrap_or_default(),
            defaults: defaults?.unwrap_or_default(),
            concurrency: concurrency?,
            jobs: jobs?,
        })
    }
}
