use std::collections::BTreeSet;

use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{
    Activity, Filter, GenericEvent, Input, InputKind, MergeGroup, PullRequest, Push, Schedule,
    Trigger, WorkflowDispatch,
};

use crate::Code;
use crate::lower::{Entry, Fields, Item, Lowering, Mark, index, optional, scalar_text};
use crate::pattern;
use crate::schedule;
use crate::surface::{
    ACTIVITY, EVENTS, INPUT, Known, MERGE_GROUP, PULL_REQUEST, PUSH, SCHEDULE, Shape,
    WORKFLOW_DISPATCH, suggest,
};
use crate::yaml::{Node, Value};

const INPUT_TYPES: &[&str] = &["string", "boolean", "choice", "number", "environment"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputType {
    String,
    Boolean,
    Choice,
    Number,
    Environment,
}

fn shape(event: &str) -> &'static Shape {
    match event {
        "push" => &PUSH,
        "pull_request" | "pull_request_target" => &PULL_REQUEST,
        "merge_group" => &MERGE_GROUP,
        "workflow_dispatch" => &WORKFLOW_DISPATCH,
        _ => &ACTIVITY,
    }
}

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Lowering<'_> {
    pub(crate) fn on(&mut self, entry: &Entry<'_>) -> Option<Vec<Trigger>> {
        let mut triggers = Vec::new();
        let mut complete = true;
        if let Value::Mapping(_) = entry.value.value {
            let fields = self.fields(entry.value, &EVENTS, "`on`", &entry.path, entry.key.span)?;
            complete = fields.complete;
            for event in &fields.entries {
                match self.mapped(event) {
                    Some(trigger) => triggers.push(trigger),
                    None => complete = false,
                }
            }
        } else {
            let mut seen = BTreeSet::new();
            for item in self.strings(entry.value, &entry.path, "`on`")? {
                if EVENTS.extension(&item.text) {
                    self.extensions.push(Mark {
                        span: item.node.span,
                        path: item.path.clone(),
                        what: format!("event `{}`", item.text),
                    });
                }
                match EVENTS.find(&item.text) {
                    Known::Now if !seen.insert(item.text.clone()) => {
                        complete = false;
                        self.error(
                            Code::DuplicateKey,
                            item.node.span,
                            &item.path,
                            format!("event `{}` appears twice in `on`", item.text),
                        );
                    }
                    Known::Now => match self.listed(&item) {
                        Some(trigger) => triggers.push(trigger),
                        None => complete = false,
                    },
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

    fn listed(&mut self, item: &Item<'_>) -> Option<Trigger> {
        if item.text == "schedule" {
            self.error(
                Code::InvalidValue,
                item.node.span,
                &item.path,
                "`schedule` needs a list of `cron` entries, which only the mapping form of `on` can give it".to_string(),
            );
            return None;
        }
        self.trigger(&item.text, None)
    }

    fn mapped(&mut self, event: &Entry<'_>) -> Option<Trigger> {
        if event.name == "schedule" {
            return self.schedules(event).map(Trigger::Schedule);
        }
        match event.value.value {
            Value::Null => self.trigger(event.name, None),
            Value::Mapping(_) => {
                let place = format!("`{}`", event.name);
                let fields = self.fields(
                    event.value,
                    shape(event.name),
                    &place,
                    &event.path,
                    event.key.span,
                )?;
                let trigger = self.trigger(event.name, Some(&fields));
                trigger.filter(|_| fields.complete)
            }
            _ => {
                self.error(
                    Code::InvalidType,
                    event.value.span,
                    &event.path,
                    format!("`{}` must be empty or a mapping", event.name),
                );
                None
            }
        }
    }

    fn trigger(&mut self, name: &str, fields: Option<&Fields<'_>>) -> Option<Trigger> {
        let place = format!("`{name}`");
        let get = |key: &str| fields.and_then(|fields| fields.get(key));
        match name {
            "push" => {
                let branches = self.filter(get("branches"), get("branches-ignore"), &place);
                let tags = self.filter(get("tags"), get("tags-ignore"), &place);
                let paths = self.filter(get("paths"), get("paths-ignore"), &place);
                Some(Trigger::Push(Push {
                    branches: branches?,
                    tags: tags?,
                    paths: paths?,
                }))
            }
            "pull_request" | "pull_request_target" => {
                let types = optional(get("types"), |entry| self.types(entry, &place));
                let branches = self.filter(get("branches"), get("branches-ignore"), &place);
                let paths = self.filter(get("paths"), get("paths-ignore"), &place);
                let pull_request = PullRequest {
                    types: types?,
                    branches: branches?,
                    paths: paths?,
                };
                Some(if name == "pull_request" {
                    Trigger::PullRequest(pull_request)
                } else {
                    Trigger::PullRequestTarget(pull_request)
                })
            }
            "merge_group" => {
                let types = optional(get("types"), |entry| self.types(entry, &place));
                let branches = self.filter(get("branches"), get("branches-ignore"), &place);
                Some(Trigger::MergeGroup(MergeGroup {
                    types: types?,
                    branches: branches?,
                }))
            }
            "workflow_dispatch" => {
                let inputs = optional(get("inputs"), |entry| self.inputs(entry));
                Some(Trigger::WorkflowDispatch(WorkflowDispatch {
                    inputs: inputs?.unwrap_or_default(),
                }))
            }
            "repository_dispatch" => {
                let types = optional(get("types"), |entry| self.types(entry, &place));
                Some(Trigger::RepositoryDispatch(Activity { types: types? }))
            }
            _ => {
                let types = optional(get("types"), |entry| self.types(entry, &place));
                Some(Trigger::Generic(GenericEvent {
                    name: name.to_string(),
                    types: types?,
                }))
            }
        }
    }

    fn filter(
        &mut self,
        include: Option<&Entry<'_>>,
        exclude: Option<&Entry<'_>>,
        place: &str,
    ) -> Option<Option<Filter>> {
        let included = optional(include, |entry| self.patterns(entry, place));
        let excluded = optional(exclude, |entry| self.patterns(entry, place));
        match (include, exclude) {
            (Some(include), Some(exclude)) => {
                self.error(
                    Code::InvalidValue,
                    exclude.value.span,
                    &exclude.path,
                    format!(
                        "`{}` of {place} cannot be used together with `{}`; exclude with `!` patterns in `{}` instead",
                        exclude.name, include.name, include.name
                    ),
                );
                None
            }
            (Some(_), None) => included?.map(Filter::Include).map(Some),
            (None, Some(_)) => excluded?.map(Filter::Exclude).map(Some),
            (None, None) => Some(None),
        }
    }

    fn listing<'n>(&mut self, entry: &Entry<'n>, place: &str) -> Option<Vec<Item<'n>>> {
        let what = format!("`{}` of {place}", entry.name);
        let items = self.strings(entry.value, &entry.path, &what)?;
        if items.is_empty() {
            self.error(
                Code::InvalidValue,
                entry.value.span,
                &entry.path,
                format!("{what} lists nothing"),
            );
            return None;
        }
        Some(items)
    }

    fn patterns(&mut self, entry: &Entry<'_>, place: &str) -> Option<Vec<String>> {
        let mut patterns = Vec::new();
        let mut complete = true;
        for item in self.listing(entry, place)? {
            match pattern::check(&item.text) {
                Ok(()) => patterns.push(item.text),
                Err(reason) => {
                    complete = false;
                    self.error(
                        Code::InvalidValue,
                        item.node.span,
                        &item.path,
                        format!(
                            "`{}` in `{}` of {place} is not a valid pattern: {reason}",
                            item.text, entry.name
                        ),
                    );
                }
            }
        }
        complete.then_some(patterns)
    }

    fn types(&mut self, entry: &Entry<'_>, place: &str) -> Option<Vec<String>> {
        let mut types = Vec::new();
        let mut complete = true;
        for item in self.listing(entry, place)? {
            if item.text.is_empty() {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    item.node.span,
                    &item.path,
                    format!("`types` of {place} names an empty activity type"),
                );
            } else {
                types.push(item.text);
            }
        }
        complete.then_some(types)
    }

    fn schedules(&mut self, event: &Entry<'_>) -> Option<Vec<Schedule>> {
        let Value::Sequence(items) = &event.value.value else {
            self.error(
                Code::InvalidType,
                event.value.span,
                &event.path,
                "`schedule` must be a list of `cron` entries".to_string(),
            );
            return None;
        };
        if items.is_empty() {
            self.error(
                Code::InvalidValue,
                event.value.span,
                &event.path,
                "`schedule` lists no `cron` entries".to_string(),
            );
            return None;
        }
        let mut schedules = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            let path = index(&event.path, position);
            match self.schedule(item, &path, position) {
                Some(schedule) => schedules.push(schedule),
                None => complete = false,
            }
        }
        complete.then_some(schedules)
    }

    fn schedule(&mut self, node: &Node, path: &[PathSegment], position: usize) -> Option<Schedule> {
        let place = format!("entry {} of `schedule`", position + 1);
        let fields = self.fields(node, &SCHEDULE, &place, path, node.span)?;
        let cron = fields.get("cron").and_then(|field| {
            let what = format!("`cron` of {place}");
            let text = self.string(field.value, &field.path, &what)?;
            match schedule::cron(&text) {
                Ok(cron) => Some(cron),
                Err(reason) => {
                    self.error(
                        Code::InvalidValue,
                        field.value.span,
                        &field.path,
                        format!("{what} is `{text}`, which is not a valid cron: {reason}"),
                    );
                    None
                }
            }
        });
        let timezone = optional(fields.get("timezone"), |field| {
            let what = format!("`timezone` of {place}");
            let text = self.string(field.value, &field.path, &what)?;
            match schedule::zone(&text) {
                Ok(()) => Some(text),
                Err(suggestion) => {
                    let hint = suggestion
                        .map(|zone| format!("; did you mean `{zone}`?"))
                        .unwrap_or_default();
                    self.error(
                        Code::InvalidValue,
                        field.value.span,
                        &field.path,
                        format!(
                            "{what} is `{text}`, which is not a time zone of the IANA database{hint}"
                        ),
                    );
                    None
                }
            }
        });
        if !fields.complete {
            return None;
        }
        Some(Schedule {
            cron: cron?,
            timezone: timezone?,
        })
    }

    fn inputs(&mut self, entry: &Entry<'_>) -> Option<Vec<Input>> {
        let found = self.entries(entry.value, &entry.path, "`inputs` of `workflow_dispatch`")?;
        let mut inputs = Vec::new();
        let mut complete = found.complete;
        for input in &found.entries {
            match self.input(input) {
                Some(input) => inputs.push(input),
                None => complete = false,
            }
        }
        complete.then_some(inputs)
    }

    fn input(&mut self, entry: &Entry<'_>) -> Option<Input> {
        let place = format!("input `{}` of `workflow_dispatch`", entry.name);
        let named = identifier(entry.name);
        if !named {
            self.error(
                Code::InvalidValue,
                entry.key.span,
                &entry.path,
                format!(
                    "the name of {place} must start with a letter or `_` and hold only letters, digits, `-` and `_`"
                ),
            );
        }
        let fields = self.fields(entry.value, &INPUT, &place, &entry.path, entry.key.span)?;
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
        let kind = optional(fields.get("type"), |field| self.input_type(field, &place));
        let options = optional(fields.get("options"), |field| self.options(field, &place));
        let kind = kind?.unwrap_or(InputType::String);
        let mut valid = named && fields.complete;
        match (kind, fields.get("options")) {
            (InputType::Choice, None) => {
                valid = false;
                if fields.complete {
                    self.error(
                        Code::MissingKey,
                        entry.key.span,
                        &entry.path,
                        format!("missing key `options` in {place}, whose type is `choice`"),
                    );
                }
            }
            (InputType::Choice, Some(_)) | (_, None) => {}
            (_, Some(field)) => {
                valid = false;
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!("`options` of {place} apply only to inputs of type `choice`"),
                );
            }
        }
        let kind = self.input_kind(kind, options?, fields.get("default"), &place);
        if !valid {
            return None;
        }
        Some(Input {
            name: entry.name.to_string(),
            description: description?,
            required: required?.unwrap_or(false),
            kind: kind?,
        })
    }

    pub(crate) fn boolean(&mut self, field: &Entry<'_>, what: &str) -> Option<bool> {
        if let Value::Bool(value) = field.value.value {
            return Some(value);
        }
        self.error(
            Code::InvalidType,
            field.value.span,
            &field.path,
            format!("{what} must be `true` or `false`"),
        );
        None
    }

    fn input_type(&mut self, field: &Entry<'_>, place: &str) -> Option<InputType> {
        let what = format!("`type` of {place}");
        let text = self.string(field.value, &field.path, &what)?;
        let kind = match text.as_str() {
            "string" => InputType::String,
            "boolean" => InputType::Boolean,
            "choice" => InputType::Choice,
            "number" => InputType::Number,
            "environment" => InputType::Environment,
            _ => {
                let hint = suggest(&text, INPUT_TYPES.iter().copied())
                    .map(|kind| format!("; did you mean `{kind}`?"))
                    .unwrap_or_default();
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!(
                        "{what} is `{text}`, which is not `string`, `boolean`, `choice`, `number` or `environment`{hint}"
                    ),
                );
                return None;
            }
        };
        Some(kind)
    }

    fn options(&mut self, field: &Entry<'_>, place: &str) -> Option<Vec<String>> {
        let what = format!("`options` of {place}");
        let Value::Sequence(items) = &field.value.value else {
            self.error(
                Code::InvalidType,
                field.value.span,
                &field.path,
                format!("{what} must be a list of strings"),
            );
            return None;
        };
        if items.is_empty() {
            self.error(
                Code::InvalidValue,
                field.value.span,
                &field.path,
                format!("{what} lists no options"),
            );
            return None;
        }
        let mut options = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            match scalar_text(&item.value) {
                Some(text) => options.push(text),
                None => {
                    complete = false;
                    self.error(
                        Code::InvalidType,
                        item.span,
                        &index(&field.path, position),
                        format!("every entry of {what} must be a string"),
                    );
                }
            }
        }
        complete.then_some(options)
    }

    fn input_kind(
        &mut self,
        kind: InputType,
        options: Option<Vec<String>>,
        default: Option<&Entry<'_>>,
        place: &str,
    ) -> Option<InputKind> {
        let what = format!("`default` of {place}");
        Some(match kind {
            InputType::String => InputKind::String {
                default: optional(default, |field| {
                    self.string(field.value, &field.path, &what)
                })?,
            },
            InputType::Environment => InputKind::Environment {
                default: optional(default, |field| {
                    self.string(field.value, &field.path, &what)
                })?,
            },
            InputType::Boolean => InputKind::Boolean {
                default: optional(default, |field| self.boolean(field, &what))?,
            },
            InputType::Number => InputKind::Number {
                default: optional(default, |field| self.number(field, &what))?,
            },
            InputType::Choice => {
                let options = options?;
                let default = optional(default, |field| {
                    let text = self.string(field.value, &field.path, &what)?;
                    if options.contains(&text) {
                        return Some(text);
                    }
                    self.error(
                        Code::InvalidValue,
                        field.value.span,
                        &field.path,
                        format!("{what} is `{text}`, which is not one of its options"),
                    );
                    None
                });
                InputKind::Choice {
                    options,
                    default: default?,
                }
            }
        })
    }

    fn number(&mut self, field: &Entry<'_>, what: &str) -> Option<f64> {
        match field.value.value {
            Value::Number(value) if value.is_finite() => Some(value),
            Value::Number(_) => {
                self.error(
                    Code::InvalidValue,
                    field.value.span,
                    &field.path,
                    format!("{what} is not a finite number"),
                );
                None
            }
            _ => {
                self.error(
                    Code::InvalidType,
                    field.value.span,
                    &field.path,
                    format!("{what} must be a number"),
                );
                None
            }
        }
    }
}
