pub mod github;
mod graph;
mod lower;
mod surface;
mod yaml;

use bosun_types::diagnostic::Diagnostic;
use bosun_types::ir::Workflow;

use crate::lower::Lowering;

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
        }
    }
}

pub fn parse(file: &str, source: &str) -> Result<Workflow, Vec<Diagnostic>> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lowering = Lowering::new(file);
    let workflow = match yaml::load(source) {
        Ok(root) => lowering.workflow(&root),
        Err(fault) => {
            lowering.error(fault.code, fault.span, &[], fault.message);
            None
        }
    };
    lowering.finish(workflow)
}
