mod cache;
mod http;
mod push;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bosun_db::projects::Binding;
use bosun_db::runs::NewRun;
use bosun_db::workflows::Blob;
use bosun_forge::{Comparison, EntryKind, Forge as _, Repository};
use bosun_types::actor::Actor;
use bosun_types::api::{Run, Workflow};
use bosun_types::id::RevisionId;
use bosun_types::ir::{self, Trigger};
use sea_orm::DatabaseTransaction;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::info;

pub(crate) use cache::Parses;
pub(crate) use http::receive;

use crate::forge::Forge;
use crate::planner;
use crate::site::Site;
use cache::{Counted, Lowered};
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
    "pull_request",
    "pull_request_target",
    "pull_request_review",
    "pull_request_review_comment",
    "merge_group",
    "workflow_dispatch",
];

const SENT_BY_JOBS: &[&str] = &["workflow_dispatch", "repository_dispatch"];

pub(crate) enum Failed {
    Database(bosun_db::Error),
    Forge(bosun_forge::Error),
    Payload(String),
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
}

struct Target {
    git_ref: Option<String>,
    sha: String,
    default_branch: bool,
    pushed: Option<Pushed>,
}

struct Pushed {
    git_ref: String,
    changes: Changes,
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
    let occurrence = Occurrence {
        event,
        action: payload.get("action").and_then(Value::as_str),
        payload,
        actor: &actor,
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
    let Some(target) = target(forge, &repository, &bound.default_branch, occurrence).await? else {
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
    let mut runs = Vec::new();
    for file in &files {
        let touched = target
            .pushed
            .as_ref()
            .is_some_and(|pushed| pushed.changes.touches(&file.workflow.name));
        let candidate = Candidate {
            workflow: &file.workflow,
            revision_id: file.revision_id,
            lowered: &file.lowered,
            touched,
        };
        if let Some(run) = plan(transaction, site, occurrence, Some(&target), candidate).await? {
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
        if let Some(run) = plan(transaction, site, occurrence, None, candidate).await? {
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
) -> Result<Option<Target>, Failed> {
    let payload = occurrence.payload;
    let default_ref = format!("{BRANCHES}{default_branch}");
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
                pushed: Some(Pushed {
                    git_ref: push.git_ref,
                    changes,
                }),
            }));
        }
        "deployment" | "deployment_status" => {
            return Ok(payload
                .pointer("/deployment/sha")
                .and_then(Value::as_str)
                .map(|sha| Target {
                    git_ref: None,
                    sha: sha.to_owned(),
                    default_branch: false,
                    pushed: None,
                }));
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
    Ok(Some(Target {
        default_branch: named == default_ref,
        git_ref: Some(named),
        sha,
        pushed: None,
    }))
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
        let revision =
            match bosun_db::workflows::revision_of_blob(transaction, workflow.id, blob).await? {
                Some(revision) => revision,
                None => {
                    let Some(file) = forge.app.file(repository, commit, path).await? else {
                        continue;
                    };
                    let source = String::from_utf8_lossy(&file.content).into_owned();
                    bosun_db::workflows::add_blob(
                        transaction,
                        workflow.id,
                        Blob {
                            sha: blob,
                            source: &source,
                            sha256: Sha256::digest(source.as_bytes()).to_vec(),
                        },
                    )
                    .await?
                }
            };
        files.push(File {
            workflow: workflow.clone(),
            revision_id: revision.id,
            lowered: parses.lowered(path, blob, &revision.source, &mut counted),
        });
    }
    Ok((files, counted))
}

async fn plan(
    transaction: &DatabaseTransaction,
    site: &Site,
    occurrence: &Occurrence<'_>,
    target: Option<&Target>,
    candidate: Candidate<'_>,
) -> Result<Option<Run>, bosun_db::Error> {
    let lowered = candidate.lowered;
    let pushed = target.and_then(|target| target.pushed.as_ref());
    let matched = match &lowered.triggers {
        Some(triggers) => bosun_workflow::matches(
            triggers,
            &bosun_workflow::Event {
                name: occurrence.event,
                action: occurrence.action,
                r#ref: pushed.map(|pushed| pushed.git_ref.as_str()),
                base_ref: None,
                changed_files: pushed.and_then(|pushed| pushed.changes.files()),
            },
        ),
        None => candidate.touched,
    };
    if !matched || !candidate.workflow.enabled {
        return Ok(None);
    }
    let new = |ir| NewRun {
        workflow: candidate.workflow,
        revision_id: candidate.revision_id,
        ir,
        event: occurrence.event,
        git_ref: target.and_then(|target| target.git_ref.as_deref()),
        sha: target.map(|target| target.sha.as_str()),
        payload: occurrence.payload,
        actor: occurrence.actor,
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
