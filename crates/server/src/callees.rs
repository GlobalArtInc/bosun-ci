use std::collections::{BTreeMap, HashMap};

use bosun_db::resolutions::{self, Policy};
use bosun_forge::{Forge as _, Repository};
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::id::OrganisationId;
use bosun_types::ir::{
    self, ActionReference, Call, CallSecrets, Callee, JobKind, JobTemplate, Reusable,
};
use bosun_types::key::JobKey;
use sea_orm::ConnectionTrait;

use crate::forge::Forge;
use crate::planner::START;
use crate::resolver;

pub(crate) const MOST_DEPTH: usize = 10;

pub(crate) const MOST_CALLEES: usize = 50;

pub(crate) const MOST_CALLED_JOBS: usize = 500;

const LARGEST: usize = 1 << 20;

pub(crate) const DIRECTORIES: [&str; 2] = [".github/workflows/", ".bosun/workflows/"];

const EXTENSIONS: [&str; 2] = [".yml", ".yaml"];

const MISSING: &str = "callee_missing";

const INVALID: &str = "callee_invalid";

const FORBIDDEN: &str = "callee_forbidden";

const LIMIT: &str = "limit_exceeded";

const NOT_IMPLEMENTED: &str = "not_implemented";

pub(crate) enum Failure {
    Database(bosun_db::Error),
    Forge(bosun_forge::Error),
}

impl From<bosun_db::Error> for Failure {
    fn from(error: bosun_db::Error) -> Failure {
        Failure::Database(error)
    }
}

impl From<bosun_forge::Error> for Failure {
    fn from(error: bosun_forge::Error) -> Failure {
        Failure::Forge(error)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Home {
    pub(crate) owner: String,
    pub(crate) name: String,
    pub(crate) commit: String,
}

pub(crate) struct Source<'a> {
    pub(crate) organisation: OrganisationId,
    pub(crate) installation: Option<u64>,
    pub(crate) home: Option<Home>,
    pub(crate) file: &'a str,
}

pub(crate) enum Loaded {
    Unchanged,
    Ready(Box<ir::Workflow>, Vec<Diagnostic>),
    Refused(Vec<Diagnostic>),
}

pub(crate) async fn load<C: ConnectionTrait>(
    db: &C,
    forge: Option<&Forge>,
    source: &Source<'_>,
    workflow: &ir::Workflow,
) -> Result<Loaded, Failure> {
    if !workflow.has_calls() {
        return Ok(Loaded::Unchanged);
    }
    let mut walk = Walk {
        db,
        forge,
        installation: source.installation,
        organisation: source.organisation,
        policy: None,
        callees: Vec::new(),
        index: HashMap::new(),
        commits: HashMap::new(),
        faults: Vec::new(),
        warnings: Vec::new(),
        root: source.home.clone(),
    };
    let calls = walk
        .visit(workflow, source.file, source.home.as_ref(), 0)
        .await?;
    if walk.faults.is_empty() {
        walk.bounded(workflow, &calls, source.file);
    }
    if !walk.faults.is_empty() {
        let mut notices = walk.faults;
        notices.extend(walk.warnings);
        return Ok(Loaded::Refused(notices));
    }
    let mut ready = workflow.clone();
    ready.calls = calls;
    ready.callees = walk.callees;
    Ok(Loaded::Ready(Box::new(ready), walk.warnings))
}

fn weight(callees: &[Reusable], position: usize, memo: &mut [Option<usize>]) -> usize {
    if let Some(Some(known)) = memo.get(position) {
        return *known;
    }
    let Some(callee) = callees.get(position) else {
        return 0;
    };
    let mut total = 0usize;
    for template in &callee.workflow.jobs {
        total = total.saturating_add(1);
        if let Some(&nested) = callee.workflow.calls.get(&template.key) {
            total = total.saturating_add(weight(callees, nested, memo));
        }
    }
    if let Some(slot) = memo.get_mut(position) {
        *slot = Some(total);
    }
    total
}

struct Walk<'a, C> {
    db: &'a C,
    forge: Option<&'a Forge>,
    installation: Option<u64>,
    organisation: OrganisationId,
    policy: Option<Policy>,
    callees: Vec<Reusable>,
    index: HashMap<String, (usize, usize)>,
    commits: HashMap<String, Option<String>>,
    faults: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    root: Option<Home>,
}

struct Target<'a> {
    forge: &'a Forge,
    home: Home,
    path: String,
}

impl<'a, C: ConnectionTrait> Walk<'a, C> {
    async fn visit(
        &mut self,
        workflow: &ir::Workflow,
        file: &str,
        home: Option<&Home>,
        depth: usize,
    ) -> Result<BTreeMap<JobKey, usize>, Failure> {
        let mut calls = BTreeMap::new();
        for template in &workflow.jobs {
            let JobKind::Call(call) = &template.kind else {
                continue;
            };
            if template.concurrency.is_some() {
                self.warn(Diagnostic {
                    severity: Severity::Warning,
                    code: NOT_IMPLEMENTED.to_owned(),
                    message: format!(
                        "the `concurrency` of job `{}`, which calls a workflow, does not apply \
                         yet, and the jobs of the called workflow run without it",
                        template.key
                    ),
                    file: file.to_owned(),
                    span: call.span,
                    path: vec![
                        PathSegment::Key("jobs".to_owned()),
                        PathSegment::Key(template.key.to_string()),
                        PathSegment::Key("concurrency".to_owned()),
                    ],
                });
            }
            if let Some(index) = self.call(template, call, file, home, depth + 1).await? {
                calls.insert(template.key.clone(), index);
            }
        }
        Ok(calls)
    }

    fn fault(
        &mut self,
        file: &str,
        template: &JobTemplate,
        call: &Call,
        code: &str,
        message: String,
    ) {
        let diagnostic = Diagnostic {
            severity: Severity::Error,
            code: code.to_owned(),
            message,
            file: file.to_owned(),
            span: call.span,
            path: vec![
                PathSegment::Key("jobs".to_owned()),
                PathSegment::Key(template.key.to_string()),
                PathSegment::Key("uses".to_owned()),
            ],
        };
        self.push(diagnostic);
    }

    fn push(&mut self, diagnostic: Diagnostic) {
        if !self.faults.contains(&diagnostic) {
            self.faults.push(diagnostic);
        }
    }

    fn warn(&mut self, diagnostic: Diagnostic) {
        if !self.warnings.contains(&diagnostic) {
            self.warnings.push(diagnostic);
        }
    }

    fn bounded(&mut self, workflow: &ir::Workflow, calls: &BTreeMap<JobKey, usize>, file: &str) {
        let mut memo = vec![None; self.callees.len()];
        let mut total = 0usize;
        for template in &workflow.jobs {
            let JobKind::Call(call) = &template.kind else {
                continue;
            };
            let Some(&position) = calls.get(&template.key) else {
                continue;
            };
            total = total.saturating_add(weight(&self.callees, position, &mut memo));
            if total > MOST_CALLED_JOBS {
                self.fault(
                    file,
                    template,
                    call,
                    LIMIT,
                    format!(
                        "the calls up to and including the one of job `{}` bring {total} jobs \
                         that come from reusable workflows, and a run holds at most \
                         {MOST_CALLED_JOBS} of them",
                        template.key
                    ),
                );
                return;
            }
        }
    }

    async fn call(
        &mut self,
        template: &JobTemplate,
        call: &Call,
        file: &str,
        home: Option<&Home>,
        level: usize,
    ) -> Result<Option<usize>, Failure> {
        let named = match &call.workflow {
            Callee::Local { path } => format!("./{path}"),
            Callee::Remote(reference) => reference.to_string(),
        };
        if level > MOST_DEPTH {
            self.fault(
                file,
                template,
                call,
                LIMIT,
                format!(
                    "the reusable workflow `{named}` is at level {level} of nesting, and a run \
                     nests reusable workflows at most {MOST_DEPTH} levels deep"
                ),
            );
            return Ok(None);
        }
        let Some(target) = self.target(template, call, file, home, &named).await? else {
            return Ok(None);
        };
        let identity = format!(
            "{}/{}/{}@{}",
            target.home.owner.to_ascii_lowercase(),
            target.home.name.to_ascii_lowercase(),
            target.path,
            target.home.commit.to_ascii_lowercase()
        );
        if let Some(&(position, deepest)) = self.index.get(&identity) {
            self.checked(file, template, position);
            if level > deepest {
                self.index.insert(identity, (position, level));
                let nested = self.callees[position].workflow.clone();
                let label = self.label(&target);
                Box::pin(self.visit(&nested, &label, Some(&target.home), level)).await?;
            }
            return Ok(Some(position));
        }
        if self.callees.len() >= MOST_CALLEES {
            self.fault(
                file,
                template,
                call,
                LIMIT,
                format!(
                    "the reusable workflow `{named}` would be the {}th distinct one of the run, \
                     and a run calls at most {MOST_CALLEES} distinct reusable workflows",
                    self.callees.len() + 1
                ),
            );
            return Ok(None);
        }
        let Some(text) = self.read(&target).await? else {
            self.fault(
                file,
                template,
                call,
                MISSING,
                format!(
                    "the reusable workflow `{named}` does not exist: the repository `{}/{}` has \
                     no file `{}` at the commit `{}`",
                    target.home.owner, target.home.name, target.path, target.home.commit
                ),
            );
            return Ok(None);
        };
        let label = self.label(&target);
        if text.len() > LARGEST {
            self.fault(
                file,
                template,
                call,
                INVALID,
                format!(
                    "the reusable workflow `{named}` is {} bytes, and a workflow file is at most \
                     {LARGEST}",
                    text.len()
                ),
            );
            return Ok(None);
        }
        let parsed = match bosun_workflow::parse(&label, &text) {
            Ok(parsed) => parsed,
            Err(diagnostics) => {
                self.fault(
                    file,
                    template,
                    call,
                    INVALID,
                    format!(
                        "the reusable workflow `{named}` is not valid: {}",
                        diagnostics
                            .first()
                            .map_or("it has no readable content", |first| first.message.as_str())
                    ),
                );
                for diagnostic in diagnostics {
                    self.push(diagnostic);
                }
                return Ok(None);
            }
        };
        for warning in parsed.warnings {
            self.warn(warning);
        }
        if parsed.workflow.concurrency.is_some() {
            self.warn(Diagnostic {
                severity: Severity::Warning,
                code: NOT_IMPLEMENTED.to_owned(),
                message: format!(
                    "the `concurrency` of the reusable workflow `{named}` does not apply yet, and \
                     the jobs of the workflow run without it"
                ),
                file: label.clone(),
                span: START,
                path: vec![PathSegment::Key("concurrency".to_owned())],
            });
        }
        let position = self.callees.len();
        self.callees.push(Reusable {
            repository: format!("{}/{}", target.home.owner, target.home.name),
            path: target.path.clone(),
            commit: target.home.commit.to_ascii_lowercase(),
            workflow: parsed.workflow.clone(),
        });
        self.index.insert(identity, (position, level));
        self.checked(file, template, position);
        let nested =
            Box::pin(self.visit(&parsed.workflow, &label, Some(&target.home), level)).await?;
        self.callees[position].workflow.calls = nested;
        Ok(Some(position))
    }

    fn checked(&mut self, file: &str, template: &JobTemplate, position: usize) {
        let found = bosun_workflow::check_call(file, template, &self.callees[position].workflow);
        for diagnostic in found {
            self.push(diagnostic);
        }
    }

    fn label(&self, target: &Target<'_>) -> String {
        let same = self.root.as_ref().is_some_and(|root| {
            root.owner.eq_ignore_ascii_case(&target.home.owner)
                && root.name.eq_ignore_ascii_case(&target.home.name)
        });
        if same {
            target.path.clone()
        } else {
            format!("{}/{}/{}", target.home.owner, target.home.name, target.path)
        }
    }

    async fn target(
        &mut self,
        template: &JobTemplate,
        call: &Call,
        file: &str,
        home: Option<&Home>,
        named: &str,
    ) -> Result<Option<Target<'a>>, Failure> {
        let Some(forge) = self.forge else {
            self.fault(
                file,
                template,
                call,
                MISSING,
                format!(
                    "the reusable workflow `{named}` cannot be read: this server has no GitHub \
                     App to read it with"
                ),
            );
            return Ok(None);
        };
        match &call.workflow {
            Callee::Local { path } => {
                if !located(path) {
                    self.fault(file, template, call, INVALID, misplaced(named));
                    return Ok(None);
                }
                let Some(home) = home else {
                    self.fault(
                        file,
                        template,
                        call,
                        MISSING,
                        format!(
                            "the reusable workflow `{named}` cannot be read: the workflow has \
                             no repository and no commit to read it from"
                        ),
                    );
                    return Ok(None);
                };
                Ok(Some(Target {
                    forge,
                    home: home.clone(),
                    path: path.clone(),
                }))
            }
            Callee::Remote(reference) => {
                let Some(path) = reference.path.clone().filter(|path| located(path)) else {
                    self.fault(file, template, call, INVALID, misplaced(named));
                    return Ok(None);
                };
                if self.forbidden(reference).await? {
                    let policy = self.policy().await?;
                    let allowed = resolver::permitted(&policy.policy, policy.account.as_deref());
                    let organisation = policy.organisation.clone();
                    self.fault(
                        file,
                        template,
                        call,
                        FORBIDDEN,
                        format!(
                            "the reusable workflow `{named}` is forbidden by the action policy \
                             of organisation `{organisation}`: {allowed}"
                        ),
                    );
                    return Ok(None);
                }
                if matches!(call.secrets, CallSecrets::Inherit)
                    && !home.is_some_and(|home| home.owner.eq_ignore_ascii_case(&reference.owner))
                {
                    self.fault(
                        file,
                        template,
                        call,
                        FORBIDDEN,
                        format!(
                            "the reusable workflow `{named}` belongs to the account `{}`, so the \
                             call cannot hand it every secret: `secrets: inherit` is allowed only \
                             for a callee of the same account as the calling workflow; pass the \
                             secrets by name",
                            reference.owner
                        ),
                    );
                    return Ok(None);
                }
                let Some(commit) = self.commit(forge, reference).await? else {
                    self.fault(
                        file,
                        template,
                        call,
                        MISSING,
                        format!(
                            "the reusable workflow `{named}` does not exist: {}",
                            self.nowhere(reference)
                        ),
                    );
                    return Ok(None);
                };
                Ok(Some(Target {
                    forge,
                    home: Home {
                        owner: reference.owner.clone(),
                        name: reference.repository.clone(),
                        commit,
                    },
                    path,
                }))
            }
        }
    }

    fn nowhere(&self, reference: &ActionReference) -> String {
        match self.installation {
            Some(_) => format!(
                "the installation of the GitHub App sees no repository `{}/{}` with a ref `{}`, \
                 and a repository outside the installation must be public",
                reference.owner, reference.repository, reference.r#ref
            ),
            None => format!(
                "the organisation has no installation of the GitHub App, so only a public \
                 repository resolves, and no public repository `{}/{}` has a ref `{}`",
                reference.owner, reference.repository, reference.r#ref
            ),
        }
    }

    async fn policy(&mut self) -> Result<&Policy, Failure> {
        let policy = match self.policy.take() {
            Some(policy) => policy,
            None => resolutions::policy(self.db, self.organisation).await?,
        };
        Ok(self.policy.insert(policy))
    }

    async fn forbidden(&mut self, reference: &ActionReference) -> Result<bool, Failure> {
        let policy = self.policy().await?;
        Ok(!policy.policy.permits(policy.account.as_deref(), reference))
    }

    async fn commit(
        &mut self,
        forge: &Forge,
        reference: &ActionReference,
    ) -> Result<Option<String>, Failure> {
        let key = format!(
            "{}/{}@{}",
            reference.owner.to_ascii_lowercase(),
            reference.repository.to_ascii_lowercase(),
            reference.r#ref
        );
        if let Some(known) = self.commits.get(&key) {
            return Ok(known.clone());
        }
        let found = match self.installation {
            Some(installation) => {
                forge
                    .app
                    .resolve(
                        &Repository {
                            installation,
                            owner: reference.owner.clone(),
                            name: reference.repository.clone(),
                        },
                        &reference.r#ref,
                    )
                    .await?
            }
            None => {
                forge
                    .app
                    .resolve_public(&reference.owner, &reference.repository, &reference.r#ref)
                    .await?
            }
        }
        .map(|commit| commit.to_ascii_lowercase());
        self.commits.insert(key, found.clone());
        Ok(found)
    }

    async fn read(&self, target: &Target<'_>) -> Result<Option<String>, Failure> {
        let forge = target.forge;
        let home = &target.home;
        let file = match self.installation {
            Some(installation) => {
                forge
                    .app
                    .file(
                        &Repository {
                            installation,
                            owner: home.owner.clone(),
                            name: home.name.clone(),
                        },
                        &home.commit,
                        &target.path,
                    )
                    .await?
            }
            None => {
                forge
                    .app
                    .public_file(&home.owner, &home.name, &home.commit, &target.path)
                    .await?
            }
        };
        Ok(file.map(|file| String::from_utf8_lossy(&file.content).into_owned()))
    }
}

fn located(path: &str) -> bool {
    DIRECTORIES.iter().any(|directory| {
        path.strip_prefix(directory).is_some_and(|rest| {
            !rest.is_empty()
                && !rest.contains('/')
                && EXTENSIONS.iter().any(|extension| rest.ends_with(extension))
        })
    })
}

fn misplaced(named: &str) -> String {
    format!(
        "the reusable workflow `{named}` is not a file `.yml` or `.yaml` directly in \
         `.github/workflows/` or `.bosun/workflows/`"
    )
}
