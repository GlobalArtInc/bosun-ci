#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Github,
    Env,
    Vars,
    Job,
    Jobs,
    Steps,
    Runner,
    Secrets,
    Strategy,
    Matrix,
    Needs,
    Inputs,
    Bosun,
}

impl Context {
    const ALL: [Context; 13] = [
        Context::Github,
        Context::Env,
        Context::Vars,
        Context::Job,
        Context::Jobs,
        Context::Steps,
        Context::Runner,
        Context::Secrets,
        Context::Strategy,
        Context::Matrix,
        Context::Needs,
        Context::Inputs,
        Context::Bosun,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Context::Github => "github",
            Context::Env => "env",
            Context::Vars => "vars",
            Context::Job => "job",
            Context::Jobs => "jobs",
            Context::Steps => "steps",
            Context::Runner => "runner",
            Context::Secrets => "secrets",
            Context::Strategy => "strategy",
            Context::Matrix => "matrix",
            Context::Needs => "needs",
            Context::Inputs => "inputs",
            Context::Bosun => "bosun",
        }
    }

    pub(crate) fn find(name: &str) -> Option<Context> {
        Context::ALL
            .into_iter()
            .find(|context| context.name().eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arity {
    Exactly(usize),
    Between(usize, usize),
    AtLeast(usize),
    Odd(usize),
}

impl Arity {
    pub(crate) fn admits(self, count: usize) -> bool {
        match self {
            Arity::Exactly(n) => count == n,
            Arity::Between(min, max) => (min..=max).contains(&count),
            Arity::AtLeast(min) => count >= min,
            Arity::Odd(min) => count >= min && count % 2 == 1,
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Arity::Odd(_) => "; they are pairs of a predicate and a value, then a default",
            _ => "",
        }
    }

    pub(crate) fn describe(self) -> String {
        let arguments = |n: usize| {
            if n == 1 {
                "1 argument".to_string()
            } else {
                format!("{n} arguments")
            }
        };
        match self {
            Arity::Exactly(0) => "no arguments".to_string(),
            Arity::Exactly(n) => arguments(n),
            Arity::Between(min, max) if max == min + 1 => format!("{min} or {max} arguments"),
            Arity::Between(min, max) => format!("{min} to {max} arguments"),
            Arity::AtLeast(min) => format!("at least {}", arguments(min)),
            Arity::Odd(min) => format!("an odd number of arguments, at least {min}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Contains,
    StartsWith,
    EndsWith,
    Format,
    Join,
    ToJson,
    FromJson,
    Case,
    HashFiles,
    Success,
    Failure,
    Cancelled,
    Always,
}

impl Function {
    const ALL: [Function; 13] = [
        Function::Contains,
        Function::StartsWith,
        Function::EndsWith,
        Function::Format,
        Function::Join,
        Function::ToJson,
        Function::FromJson,
        Function::Case,
        Function::HashFiles,
        Function::Success,
        Function::Failure,
        Function::Cancelled,
        Function::Always,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Function::Contains => "contains",
            Function::StartsWith => "startsWith",
            Function::EndsWith => "endsWith",
            Function::Format => "format",
            Function::Join => "join",
            Function::ToJson => "toJSON",
            Function::FromJson => "fromJSON",
            Function::Case => "case",
            Function::HashFiles => "hashFiles",
            Function::Success => "success",
            Function::Failure => "failure",
            Function::Cancelled => "cancelled",
            Function::Always => "always",
        }
    }

    pub(crate) fn find(name: &str) -> Option<Function> {
        Function::ALL
            .into_iter()
            .find(|function| function.name().eq_ignore_ascii_case(name))
    }

    pub(crate) fn arity(self) -> Arity {
        match self {
            Function::Contains | Function::StartsWith | Function::EndsWith => Arity::Exactly(2),
            Function::Format => Arity::AtLeast(1),
            Function::Join => Arity::Between(1, 2),
            Function::ToJson | Function::FromJson => Arity::Exactly(1),
            Function::Case => Arity::Odd(3),
            Function::HashFiles => Arity::Between(1, 255),
            Function::Success | Function::Failure | Function::Cancelled | Function::Always => {
                Arity::Exactly(0)
            }
        }
    }

    pub(crate) fn special(self) -> bool {
        matches!(
            self,
            Function::HashFiles
                | Function::Success
                | Function::Failure
                | Function::Cancelled
                | Function::Always
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Server,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    RunName,
    WorkflowConcurrency,
    WorkflowEnv,
    JobIf,
    JobName,
    JobStrategy,
    JobRunsOn,
    JobConcurrency,
    JobTimeoutMinutes,
    JobContinueOnError,
    JobEnv,
    JobDefaultsRun,
    JobOutputs,
    StepIf,
    StepName,
    StepRun,
    StepEnv,
    StepWith,
    StepWorkingDirectory,
    StepContinueOnError,
    StepTimeoutMinutes,
    ActionInputDefault,
    ActionPreIf,
    ActionPostIf,
}

const RUN: &[Context] = &[
    Context::Github,
    Context::Vars,
    Context::Inputs,
    Context::Bosun,
];

const WORKFLOW_ENV: &[Context] = &[
    Context::Github,
    Context::Vars,
    Context::Secrets,
    Context::Inputs,
    Context::Bosun,
];

const JOB_DECISION: &[Context] = &[
    Context::Github,
    Context::Vars,
    Context::Needs,
    Context::Inputs,
    Context::Bosun,
];

const JOB: &[Context] = &[
    Context::Github,
    Context::Vars,
    Context::Strategy,
    Context::Matrix,
    Context::Needs,
    Context::Inputs,
    Context::Bosun,
];

const JOB_ENV: &[Context] = &[
    Context::Github,
    Context::Vars,
    Context::Secrets,
    Context::Strategy,
    Context::Matrix,
    Context::Needs,
    Context::Inputs,
    Context::Bosun,
];

const JOB_DEFAULTS: &[Context] = &[
    Context::Github,
    Context::Env,
    Context::Vars,
    Context::Strategy,
    Context::Matrix,
    Context::Needs,
    Context::Inputs,
    Context::Bosun,
];

const STEP: &[Context] = &[
    Context::Github,
    Context::Env,
    Context::Vars,
    Context::Job,
    Context::Steps,
    Context::Runner,
    Context::Secrets,
    Context::Strategy,
    Context::Matrix,
    Context::Needs,
    Context::Inputs,
    Context::Bosun,
];

const STEP_IF: &[Context] = &[
    Context::Github,
    Context::Env,
    Context::Vars,
    Context::Job,
    Context::Steps,
    Context::Runner,
    Context::Strategy,
    Context::Matrix,
    Context::Needs,
    Context::Inputs,
    Context::Bosun,
];

const ACTION_INPUT_DEFAULT: &[Context] = &[
    Context::Github,
    Context::Job,
    Context::Runner,
    Context::Strategy,
    Context::Matrix,
    Context::Bosun,
];

const ACTION_PRE_IF: &[Context] = &[
    Context::Github,
    Context::Env,
    Context::Vars,
    Context::Job,
    Context::Runner,
    Context::Strategy,
    Context::Matrix,
    Context::Needs,
    Context::Inputs,
    Context::Bosun,
];

const STATUS: &[Function] = &[
    Function::Success,
    Function::Failure,
    Function::Cancelled,
    Function::Always,
];

const STEP_STATUS: &[Function] = &[
    Function::HashFiles,
    Function::Success,
    Function::Failure,
    Function::Cancelled,
    Function::Always,
];

const FILES: &[Function] = &[Function::HashFiles];

impl Key {
    pub const ALL: [Key; 24] = [
        Key::RunName,
        Key::WorkflowConcurrency,
        Key::WorkflowEnv,
        Key::JobIf,
        Key::JobName,
        Key::JobStrategy,
        Key::JobRunsOn,
        Key::JobConcurrency,
        Key::JobTimeoutMinutes,
        Key::JobContinueOnError,
        Key::JobEnv,
        Key::JobDefaultsRun,
        Key::JobOutputs,
        Key::StepIf,
        Key::StepName,
        Key::StepRun,
        Key::StepEnv,
        Key::StepWith,
        Key::StepWorkingDirectory,
        Key::StepContinueOnError,
        Key::StepTimeoutMinutes,
        Key::ActionInputDefault,
        Key::ActionPreIf,
        Key::ActionPostIf,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Key::RunName => "run-name",
            Key::WorkflowConcurrency => "concurrency",
            Key::WorkflowEnv => "env",
            Key::JobIf => "jobs.<job_id>.if",
            Key::JobName => "jobs.<job_id>.name",
            Key::JobStrategy => "jobs.<job_id>.strategy",
            Key::JobRunsOn => "jobs.<job_id>.runs-on",
            Key::JobConcurrency => "jobs.<job_id>.concurrency",
            Key::JobTimeoutMinutes => "jobs.<job_id>.timeout-minutes",
            Key::JobContinueOnError => "jobs.<job_id>.continue-on-error",
            Key::JobEnv => "jobs.<job_id>.env",
            Key::JobDefaultsRun => "jobs.<job_id>.defaults.run",
            Key::JobOutputs => "jobs.<job_id>.outputs.<output_id>",
            Key::StepIf => "jobs.<job_id>.steps.if",
            Key::StepName => "jobs.<job_id>.steps.name",
            Key::StepRun => "jobs.<job_id>.steps.run",
            Key::StepEnv => "jobs.<job_id>.steps.env",
            Key::StepWith => "jobs.<job_id>.steps.with",
            Key::StepWorkingDirectory => "jobs.<job_id>.steps.working-directory",
            Key::StepContinueOnError => "jobs.<job_id>.steps.continue-on-error",
            Key::StepTimeoutMinutes => "jobs.<job_id>.steps.timeout-minutes",
            Key::ActionInputDefault => "inputs.<input_id>.default",
            Key::ActionPreIf => "runs.pre-if",
            Key::ActionPostIf => "runs.post-if",
        }
    }

    pub fn contexts(self) -> &'static [Context] {
        match self {
            Key::RunName | Key::WorkflowConcurrency => RUN,
            Key::WorkflowEnv => WORKFLOW_ENV,
            Key::JobIf | Key::JobStrategy => JOB_DECISION,
            Key::JobName
            | Key::JobRunsOn
            | Key::JobConcurrency
            | Key::JobTimeoutMinutes
            | Key::JobContinueOnError => JOB,
            Key::JobEnv => JOB_ENV,
            Key::JobDefaultsRun => JOB_DEFAULTS,
            Key::StepIf | Key::ActionPostIf => STEP_IF,
            Key::ActionPreIf => ACTION_PRE_IF,
            Key::ActionInputDefault => ACTION_INPUT_DEFAULT,
            Key::JobOutputs
            | Key::StepName
            | Key::StepRun
            | Key::StepEnv
            | Key::StepWith
            | Key::StepWorkingDirectory
            | Key::StepContinueOnError
            | Key::StepTimeoutMinutes => STEP,
        }
    }

    pub fn special_functions(self) -> &'static [Function] {
        match self {
            Key::JobIf => STATUS,
            Key::StepIf | Key::ActionPreIf | Key::ActionPostIf => STEP_STATUS,
            Key::ActionInputDefault
            | Key::StepName
            | Key::StepRun
            | Key::StepEnv
            | Key::StepWith
            | Key::StepWorkingDirectory
            | Key::StepContinueOnError
            | Key::StepTimeoutMinutes => FILES,
            _ => &[],
        }
    }

    pub fn side(self) -> Side {
        let agent = self
            .contexts()
            .iter()
            .any(|context| matches!(context, Context::Job | Context::Runner | Context::Steps));
        if agent { Side::Agent } else { Side::Server }
    }
}
