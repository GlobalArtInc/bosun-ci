use std::collections::BTreeSet;

use bosun_types::actor::Actor;
use bosun_types::api::{Project, ProjectRef, Run};
use bosun_types::event::{Event, SkipReason, TriggerKind, TriggerSkipped};
use bosun_types::id::{AttemptId, ProjectId, RevisionId, RunId, WorkflowId};
use bosun_types::ir::Trigger;
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult, SqlErr};
use serde_json::Value;

use crate::journal::{self, NewEvent};
use crate::outbox::Transition;
use crate::row::{self, statement};
use crate::{Error, runs, views};

const SUBSCRIPTIONS: &str = "upstream_subscription";

const TRIGGERED: &str = "triggered_run";

const SKIPPED: &str = "trigger.skipped";

const FOLLOWERS: &str = "upstream_follower";

const REPOSITORY: &str = "repository";

pub enum Scope {
    Workflow(WorkflowId),
    Repository(ProjectId),
}

pub struct Entry<'a> {
    pub workflow_id: WorkflowId,
    pub revision_id: RevisionId,
    pub triggers: &'a [Trigger],
}

pub struct Origin<'a> {
    pub git_ref: Option<&'a str>,
    pub sha: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Replaced {
    pub added: u64,
    pub kept: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub workflow_id: WorkflowId,
    pub revision_id: RevisionId,
    pub project_id: ProjectId,
    pub kind: TriggerKind,
    pub trigger: Trigger,
    pub git_ref: Option<String>,
    pub sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Unreadable {
    pub id: Uuid,
    pub project_id: ProjectId,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Subscribers {
    pub found: Vec<Subscription>,
    pub unreadable: Vec<Unreadable>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub awaiting_admission: bool,
    pub run: Run,
    pub attempt: u32,
    pub project: Project,
    pub workflow: String,
    pub file: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub workflow_id: WorkflowId,
    pub transition: Transition,
}

fn subscribed(trigger: &Trigger) -> Option<(TriggerKind, Option<&str>)> {
    match trigger {
        Trigger::WorkflowRun(_) => Some((TriggerKind::WorkflowRun, None)),
        Trigger::Upstream(upstream) => Some((TriggerKind::Upstream, upstream.project.as_deref())),
        _ => None,
    }
}

pub async fn replace<C: ConnectionTrait>(
    db: &C,
    scope: Scope,
    entries: &[Entry<'_>],
    origin: Origin<'_>,
) -> Result<Replaced, Error> {
    let workflows: Vec<Uuid> = match scope {
        Scope::Workflow(id) => {
            db.execute(statement(
                "SELECT id FROM workflow WHERE id = $1 FOR NO KEY UPDATE",
                [Uuid::from(id).into()],
            ))
            .await?;
            vec![Uuid::from(id)]
        }
        Scope::Repository(project) => {
            db.execute(statement(
                "SELECT id FROM project WHERE id = $1 FOR NO KEY UPDATE",
                [Uuid::from(project).into()],
            ))
            .await?;
            db.query_all(statement(
                "SELECT id FROM workflow WHERE project_id = $1 AND source_kind = $2",
                [Uuid::from(project).into(), REPOSITORY.into()],
            ))
            .await?
            .into_iter()
            .map(|found| found.try_get::<Uuid>("", "id"))
            .collect::<Result<_, _>>()?
        }
    };
    let mut existing: BTreeSet<(Uuid, String)> = db
        .query_all(statement(
            "SELECT workflow_id, trigger FROM upstream_subscription WHERE workflow_id = ANY($1)",
            [workflows.clone().into()],
        ))
        .await?
        .into_iter()
        .map(|found| {
            Ok((
                found.try_get("", "workflow_id")?,
                found.try_get("", "trigger")?,
            ))
        })
        .collect::<Result<_, Error>>()?;
    let mut replaced = Replaced::default();
    for entry in entries {
        for trigger in entry.triggers {
            let Some((kind, project)) = subscribed(trigger) else {
                continue;
            };
            let kind = row::text(SUBSCRIPTIONS, kind)?;
            let key = (Uuid::from(entry.workflow_id), kind.clone());
            if existing.remove(&key) {
                replaced.kept += 1;
            } else {
                replaced.added += 1;
            }
            db.execute(statement(
                "INSERT INTO upstream_subscription \
                 (id, workflow_id, revision_id, trigger, source_project, filter, git_ref, sha) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (workflow_id, trigger) DO UPDATE SET revision_id = EXCLUDED.revision_id, \
                 source_project = EXCLUDED.source_project, filter = EXCLUDED.filter, \
                 git_ref = EXCLUDED.git_ref, sha = EXCLUDED.sha",
                [
                    Uuid::now_v7().into(),
                    Uuid::from(entry.workflow_id).into(),
                    Uuid::from(entry.revision_id).into(),
                    kind.into(),
                    project.map(str::to_owned).into(),
                    serde_json::to_value(trigger)?.into(),
                    origin.git_ref.map(str::to_owned).into(),
                    origin.sha.map(str::to_owned).into(),
                ],
            ))
            .await?;
        }
    }
    for (workflow, kind) in existing {
        db.execute(statement(
            "DELETE FROM upstream_subscription WHERE workflow_id = $1 AND trigger = $2",
            [workflow.into(), kind.into()],
        ))
        .await?;
        replaced.removed += 1;
    }
    Ok(replaced)
}

pub(crate) async fn forget_repository<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<(), Error> {
    db.execute(statement(
        "DELETE FROM upstream_subscription WHERE workflow_id IN \
         (SELECT id FROM workflow WHERE project_id = $1 AND source_kind = $2)",
        [Uuid::from(project).into(), REPOSITORY.into()],
    ))
    .await?;
    Ok(())
}

pub async fn subscribers<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    path: &str,
) -> Result<Subscribers, Error> {
    let rows = db
        .query_all(statement(
            "SELECT s.id, s.workflow_id, s.revision_id, s.trigger, s.filter, s.git_ref, s.sha, \
             w.project_id FROM upstream_subscription s JOIN workflow w ON w.id = s.workflow_id \
             WHERE (s.source_project IS NULL AND w.project_id = $1) OR s.source_project = $2 \
             ORDER BY s.trigger DESC, s.id",
            [Uuid::from(project).into(), path.into()],
        ))
        .await?;
    let mut subscribers = Subscribers::default();
    for found in &rows {
        let id: Uuid = found.try_get("", "id")?;
        match subscription(found) {
            Ok(subscription) => subscribers.found.push(subscription),
            Err(Error::Row { message, .. }) => subscribers.unreadable.push(Unreadable {
                id,
                project_id: row::id(SUBSCRIPTIONS, found.try_get::<Uuid>("", "project_id")?)?,
                error: message,
            }),
            Err(other) => return Err(other),
        }
    }
    Ok(subscribers)
}

fn subscription(found: &QueryResult) -> Result<Subscription, Error> {
    Ok(Subscription {
        workflow_id: row::id(SUBSCRIPTIONS, found.try_get::<Uuid>("", "workflow_id")?)?,
        revision_id: row::id(SUBSCRIPTIONS, found.try_get::<Uuid>("", "revision_id")?)?,
        project_id: row::id(SUBSCRIPTIONS, found.try_get::<Uuid>("", "project_id")?)?,
        kind: row::name(SUBSCRIPTIONS, found.try_get("", "trigger")?)?,
        trigger: row::json(SUBSCRIPTIONS, found.try_get("", "filter")?)?,
        git_ref: found.try_get("", "git_ref")?,
        sha: found.try_get("", "sha")?,
    })
}

pub async fn source<C: ConnectionTrait>(
    db: &C,
    run: RunId,
    attempt: AttemptId,
) -> Result<Option<Source>, Error> {
    let Some(found) = db
        .query_one(statement(
            "SELECT r.trigger -> 'payload' AS payload, r.ir ->> 'name' AS ir_name, \
             w.name AS file, a.number AS attempt, \
             NOT r.trusted AND r.admitted_at IS NULL AS awaiting_admission FROM run r \
             JOIN workflow w ON w.id = r.workflow_id \
             JOIN attempt a ON a.run_id = r.id AND a.id = $2 WHERE r.id = $1",
            [Uuid::from(run).into(), Uuid::from(attempt).into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let Some(run) = views::run(db, run).await? else {
        return Ok(None);
    };
    let Some(project) = crate::projects::find(db, run.project_id).await? else {
        return Ok(None);
    };
    let file: String = found.try_get("", "file")?;
    let named = found
        .try_get::<Option<String>>("", "ir_name")?
        .filter(|name| !name.trim().is_empty());
    let attempt = found.try_get::<i32>("", "attempt")?;
    Ok(Some(Source {
        awaiting_admission: found.try_get("", "awaiting_admission")?,
        run,
        attempt: u32::try_from(attempt).map_err(|error| Error::Row {
            table: TRIGGERED,
            message: error.to_string(),
        })?,
        project,
        workflow: named.unwrap_or_else(|| file.clone()),
        file,
        payload: found
            .try_get::<Option<Value>>("", "payload")?
            .unwrap_or(Value::Null),
    }))
}

pub async fn depth<C: ConnectionTrait>(db: &C, run: RunId) -> Result<u32, Error> {
    let found = db
        .query_one(statement(
            "SELECT depth FROM triggered_run WHERE run_id = $1",
            [Uuid::from(run).into()],
        ))
        .await?;
    match found {
        Some(found) => {
            u32::try_from(found.try_get::<i32>("", "depth")?).map_err(|error| Error::Row {
                table: TRIGGERED,
                message: error.to_string(),
            })
        }
        None => Ok(0),
    }
}

pub async fn reserve<C: ConnectionTrait>(
    db: &C,
    key: &Key,
    depth: u32,
    skipped: Option<SkipReason>,
) -> Result<bool, Error> {
    let reason = skipped
        .map(|reason| row::text(TRIGGERED, reason))
        .transpose()?;
    let reserved = db
        .execute(statement(
            "INSERT INTO triggered_run \
             (source_run_id, attempt_id, workflow_id, transition, skipped, depth) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            [
                Uuid::from(key.run_id).into(),
                Uuid::from(key.attempt_id).into(),
                Uuid::from(key.workflow_id).into(),
                key.transition.name().into(),
                reason.into(),
                i32::try_from(depth).unwrap_or(i32::MAX).into(),
            ],
        ))
        .await?
        .rows_affected();
    Ok(reserved > 0)
}

pub async fn attach<C: ConnectionTrait>(db: &C, key: &Key, run: RunId) -> Result<(), Error> {
    let attached = db
        .execute(statement(
            "UPDATE triggered_run SET run_id = $5 WHERE source_run_id = $1 AND attempt_id = $2 \
             AND workflow_id = $3 AND transition = $4 AND run_id IS NULL AND skipped IS NULL",
            [
                Uuid::from(key.run_id).into(),
                Uuid::from(key.attempt_id).into(),
                Uuid::from(key.workflow_id).into(),
                key.transition.name().into(),
                Uuid::from(run).into(),
            ],
        ))
        .await?
        .rows_affected();
    if attached == 0 {
        return Err(Error::Vanished(format!(
            "the reservation of the run triggered by {}",
            key.run_id
        )));
    }
    Ok(())
}

pub async fn record_skip(
    transaction: &DatabaseTransaction,
    key: &Key,
    skipped: TriggerSkipped,
) -> Result<bool, Error> {
    let Some(_locked) = runs::lock(transaction, key.run_id).await? else {
        return Ok(false);
    };
    let recorded = transaction
        .query_one(statement(
            "SELECT 1 AS found FROM run_event WHERE run_id = $1 AND attempt_id = $2 \
             AND kind = $3 AND data ->> 'project_id' = $4",
            [
                Uuid::from(key.run_id).into(),
                Uuid::from(key.attempt_id).into(),
                SKIPPED.into(),
                Uuid::from(skipped.project_id).to_string().into(),
            ],
        ))
        .await?;
    if recorded.is_some() {
        return Ok(false);
    }
    journal::append(
        transaction,
        NewEvent {
            run_id: key.run_id,
            attempt_id: Some(key.attempt_id),
            job_id: None,
            execution_id: None,
            step_id: None,
            actor: Actor::System,
            event: Event::TriggerSkipped(skipped),
        },
    )
    .await?;
    Ok(true)
}

pub async fn follows<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    follower: ProjectId,
) -> Result<bool, Error> {
    Ok(db
        .query_one(statement(
            "SELECT 1 AS found FROM upstream_follower WHERE project_id = $1 AND follower_id = $2",
            [Uuid::from(project).into(), Uuid::from(follower).into()],
        ))
        .await?
        .is_some())
}

pub async fn followers<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<ProjectRef>, Error> {
    db.query_all(statement(
        "SELECT p.id, p.organisation_id, o.slug || '/' || p.slug AS path \
         FROM upstream_follower f JOIN project p ON p.id = f.follower_id \
         JOIN organisation o ON o.id = p.organisation_id \
         WHERE f.project_id = $1 AND ($2::uuid IS NULL OR f.follower_id > $2) \
         ORDER BY f.follower_id LIMIT $3",
        [
            Uuid::from(project).into(),
            after.into(),
            i64::try_from(limit).unwrap_or(i64::MAX).into(),
        ],
    ))
    .await?
    .iter()
    .map(|found| {
        Ok(ProjectRef {
            id: row::id(FOLLOWERS, found.try_get::<Uuid>("", "id")?)?,
            organisation_id: row::id(FOLLOWERS, found.try_get::<Uuid>("", "organisation_id")?)?,
            path: found.try_get("", "path")?,
        })
    })
    .collect()
}

pub async fn follow<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    follower: ProjectId,
) -> Result<bool, Error> {
    let added = db
        .execute(statement(
            "INSERT INTO upstream_follower (project_id, follower_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
            [Uuid::from(project).into(), Uuid::from(follower).into()],
        ))
        .await
        .map_err(|error| match error.sql_err() {
            Some(SqlErr::ForeignKeyConstraintViolation(_)) => {
                Error::Vanished(format!("project {follower}"))
            }
            _ => Error::Database(error),
        })?;
    Ok(added.rows_affected() > 0)
}

pub async fn unfollow<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    follower: ProjectId,
) -> Result<bool, Error> {
    Ok(db
        .execute(statement(
            "DELETE FROM upstream_follower WHERE project_id = $1 AND follower_id = $2",
            [Uuid::from(project).into(), Uuid::from(follower).into()],
        ))
        .await?
        .rows_affected()
        > 0)
}
