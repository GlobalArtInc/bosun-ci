mod action;
mod concurrency;
mod defaults;
mod expression;
pub mod github;
mod graph;
mod inventory;
mod job;
mod lint;
mod lower;
mod matching;
mod on;
mod pattern;
mod permissions;
mod place;
mod schedule;
mod secrets;
mod step;
mod strategy;
mod surface;
mod uses;
mod yaml;

use bosun_types::diagnostic::Diagnostic;
use bosun_types::ir::{ActionMetadata, Trigger, Workflow};

use crate::lower::Lowering;

pub use crate::inventory::{Inventory, KeyUse, Uses, inventory};
pub use crate::matching::{Event, matches};
pub use crate::surface::{MILESTONE, Milestone, PENDING};

#[derive(Clone, Copy)]
pub(crate) enum Code {
    YamlSyntax,
    UnsupportedYaml,
    LimitExceeded,
    DuplicateKey,
    InvalidType,
    InvalidValue,
    MissingKey,
    UnknownKey,
    NotImplemented,
    InvalidJobKey,
    InvalidStepKey,
    RepeatedStepKey,
    UnknownJob,
    NeedsCycle,
    TemplateCondition,
    AllSecrets,
    IgnoredKey,
    Extension,
    UntrustedCheckout,
}

impl Code {
    fn as_str(self) -> &'static str {
        match self {
            Code::YamlSyntax => "yaml_syntax",
            Code::UnsupportedYaml => "unsupported_yaml",
            Code::LimitExceeded => "limit_exceeded",
            Code::DuplicateKey => "duplicate_key",
            Code::InvalidType => "invalid_type",
            Code::InvalidValue => "invalid_value",
            Code::MissingKey => "missing_key",
            Code::UnknownKey => "unknown_key",
            Code::NotImplemented => "not_implemented",
            Code::InvalidJobKey => "invalid_job_key",
            Code::InvalidStepKey => "invalid_step_key",
            Code::RepeatedStepKey => "repeated_step_key",
            Code::UnknownJob => "unknown_job",
            Code::NeedsCycle => "needs_cycle",
            Code::TemplateCondition => "template_condition",
            Code::AllSecrets => "all_secrets",
            Code::IgnoredKey => "ignored_key",
            Code::Extension => "extension",
            Code::UntrustedCheckout => "untrusted_checkout",
        }
    }
}

#[derive(Debug)]
pub struct Parsed {
    pub workflow: Workflow,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct ParsedAction {
    pub metadata: ActionMetadata,
    pub warnings: Vec<Diagnostic>,
}

fn load<'f, T>(
    file: &'f str,
    source: &str,
    read: impl FnOnce(&mut Lowering<'f>, &yaml::Node) -> Option<T>,
) -> (Lowering<'f>, Option<T>) {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lowering = Lowering::new(file, source);
    let lowered = match yaml::load(source) {
        Ok(root) => read(&mut lowering, &root),
        Err(fault) => {
            lowering.error(fault.code, fault.span, &[], fault.message);
            None
        }
    };
    (lowering, lowered)
}

fn lower<'f>(file: &'f str, source: &str) -> (Lowering<'f>, Option<Workflow>) {
    load(file, source, |lowering, root| lowering.workflow(root))
}

pub fn parse(file: &str, source: &str) -> Result<Parsed, Vec<Diagnostic>> {
    let (lowering, workflow) = lower(file, source);
    lowering
        .finish(workflow)
        .map(|(workflow, warnings)| Parsed { workflow, warnings })
}

pub fn lint(file: &str, source: &str) -> Vec<Diagnostic> {
    let (mut lowering, _) = lower(file, source);
    lowering.lint();
    lowering.finish_lint()
}

pub fn action(file: &str, source: &str) -> Result<ParsedAction, Vec<Diagnostic>> {
    let (lowering, metadata) = load(file, source, |lowering, root| lowering.metadata(root));
    lowering
        .finish(metadata)
        .map(|(metadata, warnings)| ParsedAction { metadata, warnings })
}

pub fn triggers(file: &str, source: &str) -> (Option<Vec<Trigger>>, Vec<Diagnostic>) {
    let (lowering, _) = lower(file, source);
    lowering.finish_triggers()
}
