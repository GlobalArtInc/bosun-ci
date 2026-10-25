use bosun_expr::Key;
use bosun_types::ir::{
    ActionInput, ActionMetadata, ActionOutput, Branding, Condition, NodeRuns, NodeRuntime, Runs,
    Template,
};

use crate::Code;
use crate::lower::{Entry, Lowering, child, optional};
use crate::surface::{ACTION, ACTION_INPUT, ACTION_OUTPUT, BRANDING, NODE_RUNS};
use crate::yaml::{Node, Value};

const RUNTIMES: &[(&str, NodeRuntime)] = &[
    ("node12", NodeRuntime::Node12),
    ("node16", NodeRuntime::Node16),
    ("node20", NodeRuntime::Node20),
    ("node24", NodeRuntime::Node24),
];

const LATER_KINDS: &[&str] = &["composite", "docker"];

fn later_kind(runs: &Node) -> Option<(&Node, String)> {
    let Value::Mapping(pairs) = &runs.value else {
        return None;
    };
    pairs
        .iter()
        .find_map(|(key, value)| match (&key.value, &value.value) {
            (Value::String(name), Value::String(using))
                if name == "using"
                    && LATER_KINDS.contains(&using.to_ascii_lowercase().as_str()) =>
            {
                Some((value, using.clone()))
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
        let outputs = optional(fields.get("outputs"), |entry| self.action_outputs(entry));
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

    fn action_outputs(&mut self, entry: &Entry<'_>) -> Option<Vec<ActionOutput>> {
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
            match description {
                Some(description) if fields.complete => outputs.push(ActionOutput {
                    name: item.name.to_string(),
                    description,
                }),
                _ => complete = false,
            }
        }
        complete.then_some(outputs)
    }

    fn runs(&mut self, entry: &Entry<'_>) -> Option<Runs> {
        let place = "`runs`";
        if let Some((node, using)) = later_kind(entry.value) {
            self.error(
                Code::NotImplemented,
                node.span,
                &child(&entry.path, "using"),
                format!("`using: {using}` in {place} is not supported yet; it arrives in M2"),
            );
            return None;
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
