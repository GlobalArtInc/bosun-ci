use bosun_expr::Key;
use bosun_types::ir::{
    ActionInput, ActionMetadata, ActionOutput, Branding, CompositeRuns, Condition, DockerRuns,
    NodeRuns, NodeRuntime, Runs, Template,
};

use crate::Code;
use crate::lower::{Entry, Lowering, child, index, optional};
use crate::surface::{
    ACTION, ACTION_INPUT, ACTION_OUTPUT, BRANDING, COMPOSITE_RUNS, DOCKER_RUNS, NODE_RUNS,
    pending_form,
};
use crate::yaml::{Node, Value};

const RUNTIMES: &[(&str, NodeRuntime)] = &[
    ("node12", NodeRuntime::Node12),
    ("node16", NodeRuntime::Node16),
    ("node20", NodeRuntime::Node20),
    ("node24", NodeRuntime::Node24),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Composite,
    Docker,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Composite => "composite",
            Kind::Docker => "docker",
        }
    }
}

fn kind(runs: &Node) -> Option<(&Node, String, Kind)> {
    let Value::Mapping(pairs) = &runs.value else {
        return None;
    };
    pairs
        .iter()
        .find_map(|(key, value)| match (&key.value, &value.value) {
            (Value::String(name), Value::String(using)) if name == "using" => {
                let kind = match using.to_ascii_lowercase().as_str() {
                    "composite" => Kind::Composite,
                    "docker" => Kind::Docker,
                    _ => return None,
                };
                Some((value, using.clone(), kind))
            }
            _ => None,
        })
}

impl Lowering<'_> {
    pub(crate) fn metadata(&mut self, root: &Node) -> Option<ActionMetadata> {
        let place = "the action's metadata";
        let fields = self.fields(root, &ACTION, place, &[], root.span)?;
        let text = |lowering: &mut Self, entry: &Entry<'_>| {
            let what = format!("`{}` of the action", entry.name);
            lowering.string(entry.value, &entry.path, &what)
        };
        let name = optional(fields.get("name"), |entry| text(self, entry));
        let description = optional(fields.get("description"), |entry| text(self, entry));
        let author = optional(fields.get("author"), |entry| text(self, entry));
        let branding = optional(fields.get("branding"), |entry| self.branding(entry));
        let inputs = optional(fields.get("inputs"), |entry| self.action_inputs(entry));
        let composite = fields
            .get("runs")
            .and_then(|entry| kind(entry.value))
            .is_some_and(|(_, _, kind)| kind == Kind::Composite);
        let outputs = optional(fields.get("outputs"), |entry| {
            self.action_outputs(entry, composite)
        });
        let runs = fields.get("runs").and_then(|entry| self.runs(entry));
        Some(ActionMetadata {
            name: name?,
            description: description?,
            author: author?,
            branding: branding?,
            inputs: inputs?.unwrap_or_default(),
            outputs: outputs?.unwrap_or_default(),
            runs: runs?,
        })
    }

    fn branding(&mut self, entry: &Entry<'_>) -> Option<Branding> {
        let place = "`branding`";
        let fields = self.fields(entry.value, &BRANDING, place, &entry.path, entry.key.span)?;
        let mut text = |key: &str| {
            optional(fields.get(key), |field| {
                self.string(field.value, &field.path, &format!("`{key}` of {place}"))
            })
        };
        let icon = text("icon");
        let color = text("color");
        Some(Branding {
            icon: icon?,
            color: color?,
        })
    }

    fn named<'n>(&mut self, entry: &Entry<'n>, noun: &str) -> Option<Vec<Entry<'n>>> {
        let place = format!("`{}` of the action", entry.name);
        let found = self.entries(entry.value, &entry.path, &place)?;
        let mut complete = found.complete;
        let mut named = Vec::new();
        for item in found.entries {
            if item.name.is_empty() {
                complete = false;
                self.error(
                    Code::InvalidValue,
                    item.key.span,
                    &item.path,
                    format!("{noun} in {place} has an empty name"),
                );
            } else {
                named.push(item);
            }
        }
        complete.then_some(named)
    }

    fn action_inputs(&mut self, entry: &Entry<'_>) -> Option<Vec<ActionInput>> {
        let mut inputs = Vec::new();
        let mut complete = true;
        for item in self.named(entry, "an input")? {
            match self.action_input(&item) {
                Some(input) => inputs.push(input),
                None => complete = false,
            }
        }
        complete.then_some(inputs)
    }

    fn action_input(&mut self, entry: &Entry<'_>) -> Option<ActionInput> {
        let place = format!("input `{}`", entry.name);
        let fields = self.fields(
            entry.value,
            &ACTION_INPUT,
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
        let default = optional(fields.get("default"), |field| {
            self.input_default(field, &what("default"))
        });
        let deprecation_message = optional(fields.get("deprecationMessage"), |field| {
            self.string(field.value, &field.path, &what("deprecationMessage"))
        });
        Some(ActionInput {
            name: entry.name.to_string(),
            description: description?,
            required: required?.unwrap_or(false),
            default: default?.flatten(),
            deprecation_message: deprecation_message?,
        })
    }

    fn input_default(&mut self, field: &Entry<'_>, what: &str) -> Option<Option<Template>> {
        if let Value::Null = field.value.value {
            return Some(None);
        }
        self.template(field.value, &field.path, what, Key::ActionInputDefault)
            .map(Some)
    }

    fn action_outputs(&mut self, entry: &Entry<'_>, composite: bool) -> Option<Vec<ActionOutput>> {
        let mut outputs = Vec::new();
        let mut complete = true;
        for item in self.named(entry, "an output")? {
            let place = format!("output `{}`", item.name);
            let Some(fields) = self.fields(
                item.value,
                &ACTION_OUTPUT,
                &place,
                &item.path,
                item.key.span,
            ) else {
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
            let value = optional(fields.get("value"), |field| {
                self.template(
                    field.value,
                    &field.path,
                    &format!("`value` of {place}"),
                    Key::ActionOutputValue,
                )
            });
            if composite && fields.complete && fields.get("value").is_none() {
                complete = false;
                self.error(
                    Code::MissingKey,
                    item.key.span,
                    &item.path,
                    format!("missing key `value` in {place}, whose action is composite"),
                );
                continue;
            }
            match (description, value) {
                (Some(description), Some(value)) if fields.complete => outputs.push(ActionOutput {
                    name: item.name.to_string(),
                    description,
                    value,
                }),
                _ => complete = false,
            }
        }
        complete.then_some(outputs)
    }

    fn runs(&mut self, entry: &Entry<'_>) -> Option<Runs> {
        let place = "`runs`";
        if let Some((node, using, kind)) = kind(entry.value) {
            let form = format!("using: {}", kind.name());
            if self.pending_refused() && pending_form("runs", &form) {
                self.error(
                    Code::NotImplemented,
                    node.span,
                    &child(&entry.path, "using"),
                    format!("`using: {using}` in {place} is not supported yet; it arrives in M2"),
                );
            }
            return match kind {
                Kind::Composite => self.composite(entry),
                Kind::Docker => self.docker(entry),
            };
        }
        let fields = self.fields(entry.value, &NODE_RUNS, place, &entry.path, entry.key.span)?;
        let what = |key: &str| format!("`{key}` of {place}");
        let using = fields.get("using").and_then(|field| self.using(field));
        let main = fields
            .get("main")
            .and_then(|field| self.entry_point(field, &what("main")));
        let pre = optional(fields.get("pre"), |field| {
            self.entry_point(field, &what("pre"))
        });
        let post = optional(fields.get("post"), |field| {
            self.entry_point(field, &what("post"))
        });
        let pre_if = optional(fields.get("pre-if"), |field| {
            self.hook_condition(field, &what("pre-if"), Key::ActionPreIf)
        });
        let post_if = optional(fields.get("post-if"), |field| {
            self.hook_condition(field, &what("post-if"), Key::ActionPostIf)
        });
        let runs = NodeRuns {
            using: using?,
            main: main?,
            pre: pre?,
            pre_if: pre_if?,
            post: post?,
            post_if: post_if?,
        };
        fields.complete.then_some(Runs::Node(runs))
    }

    fn composite(&mut self, entry: &Entry<'_>) -> Option<Runs> {
        let place = "`runs`";
        let fields = self.fields(
            entry.value,
            &COMPOSITE_RUNS,
            place,
            &entry.path,
            entry.key.span,
        )?;
        let steps = fields
            .get("steps")
            .and_then(|field| self.steps(field, "the action", true));
        let steps = steps?;
        fields
            .complete
            .then_some(Runs::Composite(CompositeRuns { steps }))
    }

    fn docker(&mut self, entry: &Entry<'_>) -> Option<Runs> {
        let place = "`runs`";
        let fields = self.fields(
            entry.value,
            &DOCKER_RUNS,
            place,
            &entry.path,
            entry.key.span,
        )?;
        let what = |key: &str| format!("`{key}` of {place}");
        let image = fields
            .get("image")
            .and_then(|field| self.entry_point(field, &what("image")));
        let args = optional(fields.get("args"), |field| {
            self.docker_args(field, &what("args"))
        });
        let env = optional(fields.get("env"), |field| {
            self.templates(
                field.value,
                &field.path,
                &what("env"),
                "variable",
                Key::ActionDockerEnv,
            )
        });
        let entrypoint = optional(fields.get("entrypoint"), |field| {
            self.entry_point(field, &what("entrypoint"))
        });
        let pre_entrypoint = optional(fields.get("pre-entrypoint"), |field| {
            self.entry_point(field, &what("pre-entrypoint"))
        });
        let post_entrypoint = optional(fields.get("post-entrypoint"), |field| {
            self.entry_point(field, &what("post-entrypoint"))
        });
        let pre_if = optional(fields.get("pre-if"), |field| {
            self.hook_condition(field, &what("pre-if"), Key::ActionPreIf)
        });
        let post_if = optional(fields.get("post-if"), |field| {
            self.hook_condition(field, &what("post-if"), Key::ActionPostIf)
        });
        let runs = DockerRuns {
            image: image?,
            args: args?.unwrap_or_default(),
            env: env?.unwrap_or_default(),
            entrypoint: entrypoint?,
            pre_entrypoint: pre_entrypoint?,
            pre_if: pre_if?,
            post_entrypoint: post_entrypoint?,
            post_if: post_if?,
        };
        fields.complete.then_some(Runs::Docker(runs))
    }

    fn docker_args(&mut self, field: &Entry<'_>, what: &str) -> Option<Vec<Template>> {
        let Value::Sequence(items) = &field.value.value else {
            self.error(
                Code::InvalidType,
                field.value.span,
                &field.path,
                format!("{what} must be a list"),
            );
            return None;
        };
        let mut args = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            let path = index(&field.path, position);
            match self.template(
                item,
                &path,
                &format!("every entry of {what}"),
                Key::ActionDockerArgs,
            ) {
                Some(template) => args.push(template),
                None => complete = false,
            }
        }
        complete.then_some(args)
    }

    fn filled(&mut self, field: &Entry<'_>, what: &str) -> Option<String> {
        let text = self.string(field.value, &field.path, what)?;
        if text.trim().is_empty() {
            self.error(
                Code::InvalidValue,
                field.value.span,
                &field.path,
                format!("{what} is empty"),
            );
            return None;
        }
        Some(text)
    }

    fn using(&mut self, field: &Entry<'_>) -> Option<NodeRuntime> {
        let what = "`using` of `runs`";
        let text = self.filled(field, what)?;
        let text = self.literal(text, field.value, &field.path, what)?;
        let lower = text.to_ascii_lowercase();
        if let Some(&(_, runtime)) = RUNTIMES.iter().find(|(name, _)| *name == lower) {
            return Some(runtime);
        }
        self.error(
            Code::InvalidValue,
            field.value.span,
            &field.path,
            format!(
                "{what} is `{text}`, which is not `node12`, `node16`, `node20`, `node24`, \
                 `composite` or `docker`"
            ),
        );
        None
    }

    fn entry_point(&mut self, field: &Entry<'_>, what: &str) -> Option<String> {
        let text = self.filled(field, what)?;
        self.literal(text, field.value, &field.path, what)
    }

    fn hook_condition(&mut self, field: &Entry<'_>, what: &str, key: Key) -> Option<Condition> {
        self.filled(field, what)?;
        self.condition(field.value, &field.path, what, key)?
    }
}
