use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::Component;
use std::sync::Arc;
use std::time::Instant;

use bosun_blob::{Key, Store};
use bosun_db::actions::{MetadataFile, NewStored, Pin, Stored};
use bosun_db::queue::ClaimedJob;
use bosun_forge::{Forge as _, Repository};
use bosun_proto::assignment;
use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};
use bosun_types::ir::{Action, ActionReference, JobKind, NodeRuntime, Runs, Span};
use flate2::read::GzDecoder;
use sea_orm::{DatabaseConnection, TransactionTrait};
use tokio_util::io::SyncIoBridge;

use crate::digest::Digesting;
use crate::forge::Forge;
use crate::store::Backend;
use crate::tools::{self, Failure, LOCK_RETRY, LOCK_WAIT, Runtimes};

const METADATA: [&str; 2] = ["action.yml", "action.yaml"];

const LARGEST_METADATA: u64 = 1 << 20;

const UNRESOLVED: &str = "action_unresolved";

const INVALID: &str = "action_invalid";

const NOT_IMPLEMENTED: &str = "not_implemented";

const SUBSTITUTED: &str = "node_substituted";

pub(crate) struct Notice {
    severity: Severity,
    code: &'static str,
    message: String,
    span: Span,
    path: Vec<PathSegment>,
}

impl Notice {
    pub(crate) fn diagnostic(&self, file: &str) -> Diagnostic {
        Diagnostic {
            severity: self.severity,
            code: self.code.to_owned(),
            message: self.message.clone(),
            file: file.to_owned(),
            span: self.span,
            path: self.path.clone(),
        }
    }
}

pub(crate) enum Prepared {
    Ready {
        actions: Vec<assignment::Action>,
        pins: Option<Vec<Pin>>,
        notices: Vec<Notice>,
    },
    Failed(Vec<Notice>),
}

impl Prepared {
    pub(crate) fn into_actions(self) -> Vec<assignment::Action> {
        match self {
            Prepared::Ready { actions, .. } => actions,
            Prepared::Failed(_) => Vec::new(),
        }
    }
}

struct Refusal {
    code: &'static str,
    message: String,
}

impl Refusal {
    fn unresolved(message: String) -> Refusal {
        Refusal {
            code: UNRESOLVED,
            message,
        }
    }
}

pub(crate) struct Actions {
    db: DatabaseConnection,
    store: Arc<Backend>,
    forge: Option<Arc<Forge>>,
    runtimes: Runtimes,
}

impl Actions {
    pub(crate) fn new(
        db: DatabaseConnection,
        store: Arc<Backend>,
        forge: Option<Arc<Forge>>,
        runtimes: Runtimes,
    ) -> Actions {
        Actions {
            db,
            store,
            forge,
            runtimes,
        }
    }

    pub(crate) async fn prepare(&self, claimed: &ClaimedJob) -> Result<Prepared, Failure> {
        let JobKind::Steps(steps) = &claimed.job.kind;
        let mut pins = claimed.pins.clone().unwrap_or_default();
        let mut seen = HashSet::new();
        let mut actions = Vec::new();
        let mut notices = Vec::new();
        let mut faults = Vec::new();
        for (index, step) in (0_u64..).zip(steps) {
            let Action::Uses(reference) = &step.action else {
                continue;
            };
            if !seen.insert(reference.to_string()) {
                continue;
            }
            let path = vec![
                key("jobs"),
                key(claimed.key.as_str()),
                key("steps"),
                PathSegment::Index(index),
                key("uses"),
            ];
            match self.action(claimed, reference, &mut pins).await? {
                Ok((action, substituted)) => {
                    actions.push(action);
                    notices.extend(substituted.map(|message| Notice {
                        severity: Severity::Warning,
                        code: SUBSTITUTED,
                        message,
                        span: step.span,
                        path,
                    }));
                }
                Err(refusal) => faults.push(Notice {
                    severity: Severity::Error,
                    code: refusal.code,
                    message: refusal.message,
                    span: step.span,
                    path,
                }),
            }
        }
        if !faults.is_empty() {
            return Ok(Prepared::Failed(faults));
        }
        let first = claimed.pins.is_none() && !actions.is_empty();
        Ok(Prepared::Ready {
            actions,
            pins: first.then_some(pins),
            notices: if first { notices } else { Vec::new() },
        })
    }

    async fn action(
        &self,
        claimed: &ClaimedJob,
        reference: &ActionReference,
        pins: &mut Vec<Pin>,
    ) -> Result<Result<(assignment::Action, Option<String>), Refusal>, Failure> {
        let owner = reference.owner.to_lowercase();
        let repository = reference.repository.to_lowercase();
        let pinned = pins.iter().find(|pin| {
            pin.owner == owner && pin.repository == repository && pin.r#ref == reference.r#ref
        });
        let commit = match pinned {
            Some(pin) => pin.commit.clone(),
            None => match self.resolve(claimed, reference).await {
                Ok(commit) => {
                    pins.push(Pin {
                        owner: owner.clone(),
                        repository: repository.clone(),
                        r#ref: reference.r#ref.clone(),
                        commit: commit.clone(),
                    });
                    commit
                }
                Err(message) => return Ok(Err(Refusal::unresolved(message))),
            },
        };
        let stored = match self
            .stored(claimed.installation, reference, &commit)
            .await?
        {
            Ok(stored) => stored,
            Err(message) => return Ok(Err(Refusal::unresolved(message))),
        };
        let directory = reference
            .path
            .as_deref()
            .map(|path| format!("{path}/"))
            .unwrap_or_default();
        let found = METADATA.iter().find_map(|name| {
            let file = format!("{directory}{name}");
            stored.metadata.get(&file).map(|metadata| (file, metadata))
        });
        let (file, source) = match found {
            Some((file, MetadataFile::Source(source))) => (file, source),
            Some((file, MetadataFile::Oversized(size))) => {
                return Ok(Err(Refusal {
                    code: INVALID,
                    message: format!(
                        "the metadata {file} of the action `{reference}` at {commit} is {size} \
                         bytes, more than the 1 MiB the server reads"
                    ),
                }));
            }
            None => {
                return Ok(Err(Refusal {
                    code: INVALID,
                    message: format!(
                        "the action `{reference}` has neither {directory}action.yml nor \
                         {directory}action.yaml at {commit}"
                    ),
                }));
            }
        };
        let parsed = match bosun_workflow::action(&file, source) {
            Ok(parsed) => parsed,
            Err(diagnostics) => return Ok(Err(refused(reference, &diagnostics))),
        };
        let Runs::Node(runs) = &parsed.metadata.runs;
        let node = self.runtimes.version(runs.using).to_owned();
        let declared = declared(runs.using);
        let substituted = (tools::major(&node) != declared).then(|| {
            format!(
                "the action `{reference}` declares `node{declared}`, which this server runs on \
                 Node {node}"
            )
        });
        Ok(Ok((
            assignment::Action {
                uses: reference.clone(),
                download: format!("/_runtime/actions/{owner}/{repository}/{commit}.tar.gz"),
                commit,
                metadata: parsed.metadata,
                node,
            },
            substituted,
        )))
    }

    async fn resolve(
        &self,
        claimed: &ClaimedJob,
        reference: &ActionReference,
    ) -> Result<String, String> {
        let unresolved =
            |problem: String| format!("the action `{reference}` cannot be resolved: {problem}");
        let Some(forge) = &self.forge else {
            return Err(unresolved(
                "this server has no GitHub App to resolve actions with".to_owned(),
            ));
        };
        let (owner, name, wanted) = (
            reference.owner.as_str(),
            reference.repository.as_str(),
            reference.r#ref.as_str(),
        );
        let resolved = match claimed.installation {
            Some(installation) => {
                forge
                    .app
                    .resolve(
                        &Repository {
                            installation,
                            owner: owner.to_owned(),
                            name: name.to_owned(),
                        },
                        wanted,
                    )
                    .await
            }
            None => forge.app.resolve_public(owner, name, wanted).await,
        };
        match resolved {
            Ok(Some(commit)) => Ok(commit.to_lowercase()),
            Ok(None) => Err(unresolved(match claimed.installation {
                Some(_) => format!(
                    "the installation of the GitHub App of organisation `{}` sees no repository \
                     `{owner}/{name}` with a ref `{wanted}`",
                    claimed.organisation
                ),
                None => format!(
                    "organisation `{}` has no installation of the GitHub App, so only public \
                     actions resolve, and no public repository `{owner}/{name}` has a ref \
                     `{wanted}`",
                    claimed.organisation
                ),
            })),
            Err(error) => Err(unresolved(error.to_string())),
        }
    }

    async fn stored(
        &self,
        installation: Option<u64>,
        reference: &ActionReference,
        commit: &str,
    ) -> Result<Result<Stored, String>, Failure> {
        let owner = reference.owner.to_lowercase();
        let repository = reference.repository.to_lowercase();
        if let Some(stored) = bosun_db::actions::find(&self.db, &owner, &repository, commit).await?
        {
            return Ok(Ok(stored));
        }
        let name = format!("actions/{owner}/{repository}/{commit}.tar.gz");
        let key = match Key::new(&name) {
            Ok(key) => key,
            Err(error) => {
                return Ok(Err(format!(
                    "the action `{reference}` cannot be stored: {error}"
                )));
            }
        };
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            let transaction = self.db.begin().await?;
            if bosun_db::objects::lock(&transaction, &name).await? {
                if let Some(stored) =
                    bosun_db::actions::find(&transaction, &owner, &repository, commit).await?
                {
                    return Ok(Ok(stored));
                }
                let fetched = match self.fetch(installation, reference, commit, &key).await? {
                    Ok(fetched) => fetched,
                    Err(refused) => return Ok(Err(refused)),
                };
                bosun_db::actions::record(
                    &transaction,
                    NewStored {
                        owner: &owner,
                        repository: &repository,
                        commit,
                        key: &name,
                        size: fetched.size,
                        sha256: &fetched.sha256,
                        metadata: &fetched.metadata,
                    },
                )
                .await?;
                transaction.commit().await?;
                return Ok(Ok(fetched));
            }
            drop(transaction);
            if Instant::now() >= deadline {
                return Err(Failure::Locked {
                    key: name,
                    waited: LOCK_WAIT,
                });
            }
            tokio::time::sleep(LOCK_RETRY).await;
            if let Some(stored) =
                bosun_db::actions::find(&self.db, &owner, &repository, commit).await?
            {
                return Ok(Ok(stored));
            }
        }
    }

    async fn fetch(
        &self,
        installation: Option<u64>,
        reference: &ActionReference,
        commit: &str,
        key: &Key,
    ) -> Result<Result<Stored, String>, Failure> {
        let undownloaded = |problem: &str| {
            format!("the action `{reference}` cannot be downloaded at {commit}: {problem}")
        };
        let Some(forge) = &self.forge else {
            return Ok(Err(undownloaded(
                "this server has no GitHub App to download actions with",
            )));
        };
        let (owner, name) = (reference.owner.as_str(), reference.repository.as_str());
        let download = match installation {
            Some(installation) => {
                forge
                    .app
                    .tarball(
                        &Repository {
                            installation,
                            owner: owner.to_owned(),
                            name: name.to_owned(),
                        },
                        commit,
                    )
                    .await
            }
            None => forge.app.public_tarball(owner, name, commit).await,
        };
        let download = match download {
            Ok(download) => download,
            Err(error) => return Ok(Err(undownloaded(&error.to_string()))),
        };
        let mut digesting = Digesting::new(download);
        let put = self.store.put(key, &mut digesting).await;
        match (put, digesting.failure()) {
            (Ok(()), _) => {}
            (Err(_), Some(failure)) => return Ok(Err(undownloaded(failure))),
            (Err(error), None) => return Err(error.into()),
        }
        let object = self.store.get(key, ..).await?;
        let bridge = SyncIoBridge::new(object);
        let scanned = tokio::task::spawn_blocking(move || scan(bridge))
            .await
            .map_err(|error| Failure::Task(error.to_string()))?;
        let metadata = match scanned {
            Ok(metadata) => metadata,
            Err(problem) => {
                self.store.delete(key).await?;
                return Ok(Err(format!(
                    "the action `{reference}` at {commit} is not a gzip-compressed tar archive: \
                     {problem}"
                )));
            }
        };
        Ok(Ok(Stored {
            key: key.to_string(),
            size: digesting.size(),
            sha256: digesting.digest().unwrap_or_default().to_vec(),
            metadata,
        }))
    }
}

fn scan(reader: impl Read) -> Result<BTreeMap<String, MetadataFile>, String> {
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    let mut found = BTreeMap::new();
    for entry in archive.entries().map_err(|error| error.to_string())? {
        let mut entry = entry.map_err(|error| error.to_string())?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| error.to_string())?
            .into_owned();
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                _ => {
                    parts.clear();
                    break;
                }
            }
        }
        let inner = parts.get(1..).unwrap_or_default();
        if !inner
            .last()
            .is_some_and(|name| METADATA.contains(&name.as_str()))
        {
            continue;
        }
        let size = entry.size();
        let file = if size > LARGEST_METADATA {
            MetadataFile::Oversized(size)
        } else {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            MetadataFile::Source(String::from_utf8_lossy(&bytes).into_owned())
        };
        found.insert(inner.join("/"), file);
    }
    Ok(found)
}

fn refused(reference: &ActionReference, diagnostics: &[Diagnostic]) -> Refusal {
    let errors: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect();
    let Some(first) = errors.first() else {
        return Refusal {
            code: INVALID,
            message: format!("the metadata of the action `{reference}` cannot be read"),
        };
    };
    let more = match errors.len() {
        1 => String::new(),
        2 => ", and 1 more error".to_owned(),
        count => format!(", and {} more errors", count - 1),
    };
    Refusal {
        code: if first.code == NOT_IMPLEMENTED {
            NOT_IMPLEMENTED
        } else {
            INVALID
        },
        message: format!(
            "the action `{reference}` cannot run: {} ({} {}:{}){more}",
            first.message, first.file, first.span.line, first.span.column
        ),
    }
}

fn declared(using: NodeRuntime) -> &'static str {
    match using {
        NodeRuntime::Node12 => "12",
        NodeRuntime::Node16 => "16",
        NodeRuntime::Node20 => "20",
        NodeRuntime::Node24 => "24",
    }
}

fn key(name: &str) -> PathSegment {
    PathSegment::Key(name.to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bosun_db::actions::MetadataFile;

    use super::{LARGEST_METADATA, scan};

    #[test]
    fn the_metadata_of_a_tarball_is_every_action_file_below_its_top_directory() {
        let mut packed = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let oversized = format!("name: huge\n{}", " ".repeat(1 << 20));
        for (path, content) in [
            ("acme-tools-0123456/action.yml", "name: root\n"),
            ("acme-tools-0123456/setup/action.yaml", "name: setup\n"),
            ("acme-tools-0123456/dist/index.js", "run()\n"),
            ("acme-tools-0123456/docs/action.yml.md", "not metadata\n"),
            ("acme-tools-0123456/huge/action.yml", oversized.as_str()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            packed
                .append_data(&mut header, path, content.as_bytes())
                .expect("an entry");
        }
        let bytes = packed
            .into_inner()
            .expect("an archive")
            .finish()
            .expect("a compressed archive");
        let scanned = scan(bytes.as_slice()).expect("the metadata files");
        assert!(oversized.len() as u64 > LARGEST_METADATA);
        assert_eq!(
            scanned,
            BTreeMap::from([
                (
                    "action.yml".to_owned(),
                    MetadataFile::Source("name: root\n".to_owned())
                ),
                (
                    "huge/action.yml".to_owned(),
                    MetadataFile::Oversized(oversized.len() as u64)
                ),
                (
                    "setup/action.yaml".to_owned(),
                    MetadataFile::Source("name: setup\n".to_owned())
                ),
            ])
        );
        assert!(scan(&b"not gzip"[..]).is_err());
    }
}
