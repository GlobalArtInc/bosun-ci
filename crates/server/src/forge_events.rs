mod cache;
mod http;
mod pull;
mod push;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bosun_db::outbox::ForkStatus;
use bosun_db::projects::Binding;
use bosun_db::runs::{NewRun, Trust};
use bosun_db::workflows::Blob;
use bosun_forge::{Comparison, EntryKind, Forge as _, Repository};
use bosun_types::actor::Actor;
use bosun_types::api::{ForkPolicy, Revision, Run, Workflow};
use bosun_types::id::RevisionId;
use bosun_types::ir::{self, Trigger};
use sea_orm::DatabaseTransaction;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tracing::info;

pub(crate) use cache::Parses;
pub(crate) use http::{full_name, readable, receive, refused};

use crate::forge::Forge;
use crate::planner;
use crate::site::Site;
use cache::{Counted, Lowered};
use pull::{PULL_REQUESTS, Pull, TARGET};
use push::{Changes, Pending, Read};

const DIRECTORIES: [&str; 2] = [".bosun/workflows", ".github/workflows"];

const EXTENSIONS: [&str; 2] = [".yml", ".yaml"];

const BRANCHES: &str = "refs/heads/";

const TAGS: &str = "refs/tags/";

const PLANNED_ELSEWHERE: &[&str] = &[
    "installation",
    "installation_repositories",
    "repository",
    "ping",
    TARGET,
    "workflow_dispatch",
];

const PULL_REQUEST: &str = "pull_request";

const MERGE_GROUP: &str = "merge_group";

const SENT_BY_JOBS: &[&str] = &["workflow_dispatch", "repository_dispatch"];

const MERGE_ATTEMPTS: u32 = 10;

pub(crate) enum Failed {
    Database(bosun_db::Error),
    Forge(bosun_forge::Error),
    Payload(String),
    Unmerged(String),
}

impl From<bosun_db::Error> for Failed {
    fn from(error: bosun_db::Error) -> Failed {
        Failed::Database(error)
    }
}

impl From<bosun_forge::Error> for Failed {
    fn from(error: bosun_forge::Error) -> Failed {
        Failed::Forge(error)
    }
}

struct Occurrence<'a> {
    event: &'a str,
    action: Option<&'a str>,
    payload: &'a Value,
    actor: &'a Actor,
    attempt: u32,
}

struct Target {
    git_ref: Option<String>,
    sha: String,
    default_branch: bool,
    scope: Scope,
}

enum Scope {
    Push(Pushed),
    Pull(Pulled),
    Queue { base_ref: String },
    Other,
}

struct Pushed {
    git_ref: String,
    changes: Changes,
}

struct Pulled {
    base_ref: String,
    number: u64,
    files: Option<Vec<String>>,
}

impl Target {
    fn other(git_ref: Option<String>, sha: String, default_branch: bool) -> Target {
        Target {
            git_ref,
            sha,
            default_branch,
            scope: Scope::Other,
        }
    }

    fn pulled(git_ref: String, sha: String, default_branch: bool, pull: &Pull) -> Target {
        Target {
            git_ref: Some(git_ref),
            sha,
            default_branch,
            scope: Scope::Pull(Pulled {
                base_ref: pull.base_ref(),
                number: pull.number,
                files: None,
            }),
        }
    }

    fn pushed(&self) -> Option<&Pushed> {
        match &self.scope {
            Scope::Push(pushed) => Some(pushed),
            Scope::Pull(_) | Scope::Queue { .. } | Scope::Other => None,
        }
    }

    async fn list_files(
        &mut self,
        forge: &Forge,
        repository: &Repository,
        event: &str,
        files: &[File],
    ) -> Result<(), Failed> {
        let Scope::Pull(pulled) = &mut self.scope else {
            return Ok(());
        };
        if files.iter().any(|file| filters_paths(&file.lowered, event)) {
            let listed = forge
                .app
                .pull_request_files(repository, pulled.number)
                .await?;
            pulled.files = (!listed.truncated).then_some(listed.files);
        }
        Ok(())
    }

    fn event<'a>(&'a self, occurrence: &Occurrence<'a>) -> bosun_workflow::Event<'a> {
        let (r#ref, base_ref, changed_files) = match &self.scope {
            Scope::Push(pushed) => (Some(pushed.git_ref.as_str()), None, pushed.changes.files()),
            Scope::Pull(pulled) => (
                None,
                Some(pulled.base_ref.as_str()),
                pulled.files.as_deref(),
            ),
            Scope::Queue { base_ref } => (None, Some(base_ref.as_str()), None),
            Scope::Other => (None, None, None),
        };
        bosun_workflow::Event {
            name: occurrence.event,
            action: occurrence.action,
            r#ref,
            base_ref,
            changed_files,
        }
    }
}

fn filters_paths(lowered: &Lowered, event: &str) -> bool {
    lowered
        .triggers
        .iter()
        .flatten()
        .any(|trigger| match trigger {
            Trigger::PullRequest(filters) => event == PULL_REQUEST && filters.paths.is_some(),
            Trigger::PullRequestTarget(filters) => event == TARGET && filters.paths.is_some(),
            _ => false,
        })
}

struct File {
    workflow: Workflow,
    revision_id: RevisionId,
    lowered: Arc<Lowered>,
}

struct Candidate<'a> {
    workflow: &'a Workflow,
    revision_id: RevisionId,
    lowered: &'a Lowered,
    touched: bool,
}

#[derive(Default)]
struct Planned {
    runs: Vec<Run>,
    files: usize,
    counted: Counted,
}

pub(crate) async fn delivered(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    parses: &Parses,
    site: &Site,
    event: &str,
    payload: &Value,
    attempt: u32,
) -> Result<(), Failed> {
    if PLANNED_ELSEWHERE.contains(&event) {
        return Ok(());
    }
    let Some(repository) = payload.pointer("/repository/id").and_then(Value::as_u64) else {
        return Ok(());
    };
    let Some(binding) = bosun_db::projects::bound_to(transaction, repository).await? else {
        return Ok(());
    };
    let Some(installation) = binding.installation else {
        info!(
            event,
            repository_id = repository,
            project_id = %binding.project.id,
            "an event came for a repository that no installation of the GitHub App sees, and \
             starts no run"
        );
        return Ok(());
    };
    let actor = sender(payload);
    if let Actor::Forge { id, login } = &actor
        && !SENT_BY_JOBS.contains(&event)
        && *id == forge.bot().await?
    {
        info!(
            event,
            sender = login,
            project_id = %binding.project.id,
            "an event the GitHub App's own bot caused starts no run"
        );
        return Ok(());
    }
    let events: &[&str] = if event == PULL_REQUEST {
        &[PULL_REQUEST, TARGET]
    } else {
        &[event]
    };
    for event in events {
        let occurrence = Occurrence {
            event,
            action: payload.get("action").and_then(Value::as_str),
            payload,
            actor: &actor,
            attempt,
        };
        let planned = repository_runs(
            transaction,
            forge,
            parses,
            site,
            &binding,
            installation,
            &occurrence,
        )
        .await?;
        info!(
            event,
            project_id = %binding.project.id,
            files = planned.files,
            parsed = planned.counted.parsed,
            reused = planned.counted.reused,
            runs = planned.runs.len(),
            "planned the runs of an event"
        );
    }
    Ok(())
}

fn sender(payload: &Value) -> Actor {
    let sender = payload.get("sender");
    match (
        sender
            .and_then(|sender| sender.get("id"))
            .and_then(Value::as_u64),
        sender
            .and_then(|sender| sender.get("login"))
            .and_then(Value::as_str),
    ) {
        (Some(id), Some(login)) => Actor::Forge {
            id,
            login: login.to_owned(),
        },
        _ => Actor::System,
    }
}

async fn repository_runs(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    parses: &Parses,
    site: &Site,
    binding: &Binding,
    installation: u64,
    occurrence: &Occurrence<'_>,
) -> Result<Planned, Failed> {
    let Some(bound) = &binding.project.repository else {
        return Ok(Planned::default());
    };
    let repository = Repository {
        installation,
        owner: bound.owner.clone(),
        name: bound.name.clone(),
    };
    let pull = if PULL_REQUESTS.contains(&occurrence.event) {
        Some(Pull::of(occurrence.payload).map_err(Failed::Payload)?)
    } else {
        None
    };
    let trust = match pull.as_ref().filter(|pull| pull.fork) {
        None => Trust::Trusted,
        Some(pull) => match binding.fork_policy {
            ForkPolicy::Disabled => {
                return refuse_fork(
                    transaction,
                    forge,
                    parses,
                    binding,
                    &repository,
                    occurrence,
                    pull,
                )
                .await;
            }
            ForkPolicy::Approval => Trust::Untrusted { admitted: false },
            ForkPolicy::Automatic => Trust::Untrusted { admitted: true },
        },
    };
    let Some(mut target) = target(
        forge,
        &repository,
        &bound.default_branch,
        occurrence,
        pull.as_ref(),
    )
    .await?
    else {
        return Ok(Planned::default());
    };
    let (files, counted) = read(
        transaction,
        forge,
        parses,
        binding,
        &repository,
        &target.sha,
    )
    .await?;
    target
        .list_files(forge, &repository, occurrence.event, &files)
        .await?;
    let mut runs = Vec::new();
    for file in &files {
        let candidate = Candidate::of(file, Some(&target));
        if let Some(run) = plan(
            transaction,
            site,
            occurrence,
            Some(&target),
            trust,
            candidate,
        )
        .await?
        {
            runs.push(run);
        }
        if target.default_branch {
            bosun_db::workflows::make_current(transaction, file.workflow.id, file.revision_id)
                .await?;
        }
    }
    Ok(Planned {
        runs,
        files: files.len(),
        counted,
    })
}

async fn refuse_fork(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    parses: &Parses,
    binding: &Binding,
    repository: &Repository,
    occurrence: &Occurrence<'_>,
    pull: &Pull,
) -> Result<Planned, Failed> {
    let Some(bound) = &binding.project.repository else {
        return Ok(Planned::default());
    };
    let base_ref = pull.base_ref();
    let Some(sha) = forge.app.resolve(repository, &base_ref).await? else {
        return Ok(Planned::default());
    };
    let mut target = Target::pulled(base_ref, sha, false, pull);
    let (files, counted) =
        read(transaction, forge, parses, binding, repository, &target.sha).await?;
    target
        .list_files(forge, repository, occurrence.event, &files)
        .await?;
    let refused = files
        .iter()
        .filter(|file| {
            file.workflow.enabled && matched(occurrence, Some(&target), &Candidate::of(file, None))
        })
        .count();
    if refused > 0 {
        bosun_db::outbox::refuse_fork(
            transaction,
            &ForkStatus {
                repository_id: bound.id,
                sha: pull.head_sha.clone(),
            },
        )
        .await?;
    }
    info!(
        event = occurrence.event,
        project_id = %binding.project.id,
        pull_request = pull.number,
        workflows = refused,
        "a pull request from a fork starts no run, since the project's fork policy is disabled"
    );
    Ok(Planned {
        runs: Vec::new(),
        files: files.len(),
        counted,
    })
}

async fn server_runs(
    transaction: &DatabaseTransaction,
    site: &Site,
    binding: &Binding,
    occurrence: &Occurrence<'_>,
) -> Result<Vec<Run>, bosun_db::Error> {
    let mut runs = Vec::new();
    for workflow in bosun_db::workflows::server_side(transaction, binding.project.id).await? {
        let Some(revision_id) = workflow.revision_id else {
            continue;
        };
        let Some(revision) = bosun_db::workflows::revision(transaction, revision_id).await? else {
            continue;
        };
        let lowered = Lowered::new(&workflow.name, &revision.source);
        let candidate = Candidate {
            workflow: &workflow,
            revision_id,
            lowered: &lowered,
            touched: false,
        };
        if let Some(run) = plan(
            transaction,
            site,
            occurrence,
            None,
            Trust::Trusted,
            candidate,
        )
        .await?
        {
            runs.push(run);
        }
    }
    Ok(runs)
}

async fn target(
    forge: &Forge,
    repository: &Repository,
    default_branch: &str,
    occurrence: &Occurrence<'_>,
    pull: Option<&Pull>,
) -> Result<Option<Target>, Failed> {
    let payload = occurrence.payload;
    let default_ref = format!("{BRANCHES}{default_branch}");
    if let Some(pull) = pull {
        let (git_ref, sha) = match &pull.merged {
            Some(merge) => (pull.base_ref(), merge.clone()),
            None => {
                let merge_ref = pull.merge_ref();
                let Some(sha) = forge.app.resolve(repository, &merge_ref).await? else {
                    return Ok(None);
                };
                if !merges_head(forge, repository, &sha, pull, occurrence).await? {
                    return Ok(None);
                }
                (merge_ref, sha)
            }
        };
        return Ok(Some(Target::pulled(git_ref, sha, false, pull)));
    }
    let named = match occurrence.event {
        "push" => {
            let push = push::plan(payload, default_branch).map_err(Failed::Payload)?;
            let changes = match push.changes {
                Pending::Known(changes) => changes,
                Pending::Compare { base, head } => {
                    push::compared(comparison(forge, repository, &base, &head).await?)
                }
            };
            let (git_ref, sha) = match push.read {
                Read::After(sha) => (push.git_ref.clone(), sha),
                Read::DefaultBranch => match forge.app.resolve(repository, &default_ref).await? {
                    Some(sha) => (default_ref.clone(), sha),
                    None => return Ok(None),
                },
            };
            return Ok(Some(Target {
                default_branch: git_ref == default_ref,
                git_ref: Some(git_ref),
                sha,
                scope: Scope::Push(Pushed {
                    git_ref: push.git_ref,
                    changes,
                }),
            }));
        }
        TARGET => {
            let pull = Pull::of(payload).map_err(Failed::Payload)?;
            let base_ref = pull.base_ref();
            let Some(sha) = forge.app.resolve(repository, &base_ref).await? else {
                return Ok(None);
            };
            let default_branch = base_ref == default_ref;
            return Ok(Some(Target::pulled(base_ref, sha, default_branch, &pull)));
        }
        MERGE_GROUP => {
            let text = |field: &str| {
                payload
                    .pointer(&format!("/merge_group/{field}"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| Failed::Payload(format!("the merge group names no `{field}`")))
            };
            return Ok(Some(Target {
                git_ref: Some(text("head_ref")?),
                sha: text("head_sha")?,
                default_branch: false,
                scope: Scope::Queue {
                    base_ref: text("base_ref")?,
                },
            }));
        }
        "deployment" | "deployment_status" => {
            return Ok(payload
                .pointer("/deployment/sha")
                .and_then(Value::as_str)
                .map(|sha| Target::other(None, sha.to_owned(), false)));
        }
        "create" => {
            let name = payload.get("ref").and_then(Value::as_str);
            match (name, payload.get("ref_type").and_then(Value::as_str)) {
                (Some(name), Some("branch")) => format!("{BRANCHES}{name}"),
                (Some(name), Some("tag")) => format!("{TAGS}{name}"),
                _ => return Ok(None),
            }
        }
        "release" => match payload.pointer("/release/tag_name").and_then(Value::as_str) {
            Some(tag) => format!("{TAGS}{tag}"),
            None => return Ok(None),
        },
        _ => default_ref.clone(),
    };
    let Some(sha) = forge.app.resolve(repository, &named).await? else {
        return Ok(None);
    };
    Ok(Some(Target::other(
        Some(named.clone()),
        sha,
        named == default_ref,
    )))
}

async fn merges_head(
    forge: &Forge,
    repository: &Repository,
    merge: &str,
    pull: &Pull,
    occurrence: &Occurrence<'_>,
) -> Result<bool, Failed> {
    let parents = forge.app.parents(repository, merge).await?;
    if parents.is_some_and(|parents| parents.contains(&pull.head_sha)) {
        return Ok(true);
    }
    if occurrence.attempt < MERGE_ATTEMPTS - 1 {
        return Err(Failed::Unmerged(format!(
            "the merge ref of pull request #{} names {merge}, which the forge has not made of \
             its head {} yet",
            pull.number, pull.head_sha
        )));
    }
    info!(
        event = occurrence.event,
        pull_request = pull.number,
        merge,
        head = pull.head_sha,
        attempts = MERGE_ATTEMPTS,
        "the merge ref of a pull request still names no merge of its head, as for a pull \
         request with conflicts, and the event starts no run at it"
    );
    Ok(false)
}

async fn comparison(
    forge: &Forge,
    repository: &Repository,
    base: &str,
    head: &str,
) -> Result<Option<Comparison>, Failed> {
    match forge.app.compare(repository, base, head).await {
        Ok(comparison) => Ok(Some(comparison)),
        Err(bosun_forge::Error::Status { status: 404, .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn read(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    parses: &Parses,
    binding: &Binding,
    repository: &Repository,
    commit: &str,
) -> Result<(Vec<File>, Counted), Failed> {
    let listed = listed(forge, binding, repository, commit).await?;
    let mut counted = Counted::default();
    if listed.is_empty() {
        return Ok((Vec::new(), counted));
    }
    let paths: Vec<&str> = listed.iter().map(|(path, _)| path.as_str()).collect();
    let workflows =
        bosun_db::workflows::repository_files(transaction, binding.project.id, &paths).await?;
    let mut files = Vec::with_capacity(listed.len());
    for (path, blob) in &listed {
        let Some(workflow) = workflows.iter().find(|workflow| &workflow.name == path) else {
            return Err(bosun_db::Error::Vanished(format!("the workflow `{path}`")).into());
        };
        let Some(revision) =
            revision(transaction, forge, repository, commit, workflow, blob).await?
        else {
            continue;
        };
        files.push(File {
            workflow: workflow.clone(),
            revision_id: revision.id,
            lowered: parses.lowered(path, blob, &revision.source, &mut counted),
        });
    }
    Ok((files, counted))
}

pub(crate) async fn file_at(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    parses: &Parses,
    binding: &Binding,
    repository: &Repository,
    commit: &str,
    workflow: &Workflow,
) -> Result<Option<(RevisionId, Arc<Lowered>)>, Failed> {
    let listed = listed(forge, binding, repository, commit).await?;
    let Some((path, blob)) = listed.iter().find(|(path, _)| *path == workflow.name) else {
        return Ok(None);
    };
    let Some(revision) = revision(transaction, forge, repository, commit, workflow, blob).await?
    else {
        return Ok(None);
    };
    let mut counted = Counted::default();
    let lowered = parses.lowered(path, blob, &revision.source, &mut counted);
    Ok(Some((revision.id, lowered)))
}

async fn listed(
    forge: &Forge,
    binding: &Binding,
    repository: &Repository,
    commit: &str,
) -> Result<Vec<(String, String)>, Failed> {
    let directories = if binding.github_workflows {
        &DIRECTORIES[..]
    } else {
        &DIRECTORIES[..1]
    };
    let mut names = BTreeSet::new();
    let mut listed = Vec::new();
    for directory in directories {
        let mut entries: Vec<_> = forge
            .app
            .directory(repository, commit, directory)
            .await?
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| {
                entry.kind == EntryKind::File
                    && EXTENSIONS
                        .iter()
                        .any(|extension| entry.name.ends_with(extension))
            })
            .collect();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        for entry in entries {
            if names.insert(entry.name.clone()) {
                listed.push((format!("{directory}/{}", entry.name), entry.sha));
            }
        }
    }
    Ok(listed)
}

async fn revision(
    transaction: &DatabaseTransaction,
    forge: &Forge,
    repository: &Repository,
    commit: &str,
    workflow: &Workflow,
    blob: &str,
) -> Result<Option<Revision>, Failed> {
    if let Some(revision) =
        bosun_db::workflows::revision_of_blob(transaction, workflow.id, blob).await?
    {
        return Ok(Some(revision));
    }
    let Some(file) = forge.app.file(repository, commit, &workflow.name).await? else {
        return Ok(None);
    };
    let source = String::from_utf8_lossy(&file.content).into_owned();
    let revision = bosun_db::workflows::add_blob(
        transaction,
        workflow.id,
        Blob {
            sha: blob,
            source: &source,
            sha256: Sha256::digest(source.as_bytes()).to_vec(),
        },
    )
    .await?;
    Ok(Some(revision))
}

impl<'a> Candidate<'a> {
    fn of(file: &'a File, target: Option<&Target>) -> Candidate<'a> {
        Candidate {
            workflow: &file.workflow,
            revision_id: file.revision_id,
            lowered: &file.lowered,
            touched: target
                .and_then(Target::pushed)
                .is_some_and(|pushed| pushed.changes.touches(&file.workflow.name)),
        }
    }
}

fn matched(
    occurrence: &Occurrence<'_>,
    target: Option<&Target>,
    candidate: &Candidate<'_>,
) -> bool {
    match &candidate.lowered.triggers {
        Some(triggers) => {
            let event = match target {
                Some(target) => target.event(occurrence),
                None => bosun_workflow::Event {
                    name: occurrence.event,
                    action: occurrence.action,
                    r#ref: None,
                    base_ref: None,
                    changed_files: None,
                },
            };
            bosun_workflow::matches(triggers, &event)
        }
        None => candidate.touched,
    }
}

async fn plan(
    transaction: &DatabaseTransaction,
    site: &Site,
    occurrence: &Occurrence<'_>,
    target: Option<&Target>,
    trust: Trust,
    candidate: Candidate<'_>,
) -> Result<Option<Run>, bosun_db::Error> {
    if !matched(occurrence, target, &candidate) || !candidate.workflow.enabled {
        return Ok(None);
    }
    let lowered = candidate.lowered;
    let inputs = Map::new();
    let new = |ir| NewRun {
        workflow: candidate.workflow,
        revision_id: candidate.revision_id,
        ir,
        event: occurrence.event,
        git_ref: target.and_then(|target| target.git_ref.as_deref()),
        sha: target.map(|target| target.sha.as_str()),
        payload: occurrence.payload,
        inputs: &inputs,
        actor: occurrence.actor,
        trust,
    };
    let mut notices = lowered.diagnostics.clone();
    let name = match &lowered.workflow {
        Some(workflow) => match planner::unapplied_at(workflow) {
            None => {
                return planner::dispatch(transaction, new(workflow), &lowered.warnings(), site)
                    .await
                    .map(Some);
            }
            Some(unapplied) => {
                notices.push(unapplied.notice(&candidate.workflow.name));
                workflow.name.clone()
            }
        },
        None => None,
    };
    let failed = bare(name, lowered.triggers.as_deref());
    planner::fail_startup(transaction, new(&failed), &notices)
        .await
        .map(Some)
}

fn bare(name: Option<String>, triggers: Option<&[Trigger]>) -> ir::Workflow {
    ir::Workflow {
        name,
        run_name: None,
        triggers: triggers.map(<[Trigger]>::to_vec).unwrap_or_default(),
        permissions: None,
        env: BTreeMap::new(),
        defaults: ir::Defaults::default(),
        concurrency: None,
        jobs: Vec::new(),
    }
}
