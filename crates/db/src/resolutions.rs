use std::time::Duration;

use bosun_types::action_policy::ActionPolicy;
use bosun_types::id::{JobId, RunId};
use bosun_types::ir::{self, Action, ActionReference, JobKind, Span};
use bosun_types::key::JobKey;
use bosun_types::status::{JobStatus, WaitingReason};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use serde_json::Value;

use crate::row::statement;
use crate::{Error, row};

pub(crate) const CHANNEL: &str = "bosun_resolve";

const JOBS: &str = "job";

const MOST_DOUBLINGS: i32 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub step: u64,
    pub span: Span,
    pub reference: ActionReference,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Claimed {
    pub job_id: JobId,
    pub run_id: RunId,
    pub key: JobKey,
    pub token: Uuid,
    pub organisation: String,
    pub installation: Option<u64>,
    pub account: Option<String>,
    pub policy: ActionPolicy,
    pub uses: Vec<Use>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    pub policy: ActionPolicy,
    pub account: Option<String>,
    pub expired: bool,
}

pub(crate) async fn announce(transaction: &DatabaseTransaction) -> Result<(), Error> {
    transaction
        .execute(statement("SELECT pg_notify($1, '')", [CHANNEL.into()]))
        .await?;
    Ok(())
}

#[derive(Debug)]
pub struct Unreadable {
    pub job: Uuid,
    pub token: Uuid,
    pub error: Error,
}

#[derive(Debug, Default)]
pub struct Taken {
    pub claimed: Vec<Claimed>,
    pub unreadable: Vec<Unreadable>,
}

struct Raw {
    job: Uuid,
    token: Uuid,
    run: Uuid,
    key: String,
    ir: Value,
    organisation: String,
    installation: Option<i64>,
    account: Option<String>,
    policy: Value,
}

impl Raw {
    fn read(self) -> Result<Claimed, Error> {
        let key: JobKey = row::name(JOBS, self.key)?;
        let workflow: ir::Workflow = row::json("run", self.ir)?;
        let template = workflow
            .jobs
            .into_iter()
            .find(|template| template.key == key)
            .ok_or_else(|| Error::Row {
                table: "run",
                message: format!("the IR of the run has no job `{key}`"),
            })?;
        let JobKind::Steps(steps) = template.kind else {
            return Err(Error::Row {
                table: "run",
                message: format!("the job `{key}` runs no steps"),
            });
        };
        let uses = (0_u64..)
            .zip(steps)
            .filter_map(|(step, found)| match found.action {
                Action::Uses(reference) => Some(Use {
                    step,
                    span: found.span,
                    reference,
                }),
                Action::Run { .. } | Action::Local { .. } | Action::Docker { .. } => None,
            })
            .collect();
        Ok(Claimed {
            job_id: row::id(JOBS, self.job)?,
            run_id: row::id("run", self.run)?,
            key,
            token: self.token,
            organisation: self.organisation,
            installation: self
                .installation
                .map(|installation| row::unsigned("forge_installation", installation))
                .transpose()?,
            account: self.account,
            policy: row::json("organisation", self.policy)?,
            uses,
        })
    }
}

pub async fn claim<C: TransactionTrait>(
    db: &C,
    limit: u32,
    lease: Duration,
) -> Result<Taken, Error> {
    let transaction = db.begin().await?;
    let waiting = row::text(JOBS, JobStatus::Waiting)?;
    let resolving = row::text(JOBS, WaitingReason::Resolving)?;
    let stamped = transaction
        .query_all(statement(
            "WITH due AS (SELECT id FROM job WHERE status = $1 AND waiting_reason = $2 \
             AND (resolve_after IS NULL OR resolve_after <= now()) \
             AND (resolve_lease_until IS NULL OR resolve_lease_until <= now()) \
             ORDER BY coalesce(resolve_after, resolving_since), id LIMIT $3 \
             FOR UPDATE SKIP LOCKED) \
             UPDATE job SET resolve_claim = gen_random_uuid(), \
             resolve_lease_until = now() + make_interval(secs => $4) \
             FROM due WHERE job.id = due.id RETURNING job.id, job.resolve_claim",
            [
                waiting.into(),
                resolving.into(),
                i64::from(limit).into(),
                lease.as_secs_f64().into(),
            ],
        ))
        .await?;
    let mut raw = Vec::with_capacity(stamped.len());
    for stamped in stamped {
        let job: Uuid = stamped.try_get("", "id")?;
        let found = transaction
            .query_one(statement(
                "SELECT j.key, a.run_id, r.ir, o.slug AS organisation, o.action_policy, \
                 fi.id AS installation, fi.account_login FROM job j \
                 JOIN attempt a ON a.id = j.attempt_id JOIN run r ON r.id = a.run_id \
                 JOIN project p ON p.id = r.project_id \
                 JOIN organisation o ON o.id = p.organisation_id \
                 LEFT JOIN forge_installation fi ON fi.organisation_id = o.id \
                 WHERE j.id = $1",
                [job.into()],
            ))
            .await?
            .ok_or_else(|| Error::Vanished(format!("job {job}")))?;
        raw.push(Raw {
            job,
            token: stamped.try_get("", "resolve_claim")?,
            run: found.try_get("", "run_id")?,
            key: found.try_get("", "key")?,
            ir: found.try_get("", "ir")?,
            organisation: found.try_get("", "organisation")?,
            installation: found.try_get("", "installation")?,
            account: found.try_get("", "account_login")?,
            policy: found.try_get("", "action_policy")?,
        });
    }
    transaction.commit().await?;
    let mut taken = Taken::default();
    for raw in raw {
        let (job, token) = (raw.job, raw.token);
        match raw.read() {
            Ok(claimed) => taken.claimed.push(claimed),
            Err(error) => taken.unreadable.push(Unreadable { job, token, error }),
        }
    }
    Ok(taken)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    Lost,
    Held,
    Expired,
}

pub async fn renew<C: ConnectionTrait>(
    db: &C,
    job: JobId,
    token: Uuid,
    lease: Duration,
    give_up: Duration,
) -> Result<Renewal, Error> {
    let renewed = db
        .query_one(statement(
            "UPDATE job SET resolve_lease_until = now() + make_interval(secs => $3) \
             WHERE id = $1 AND resolve_claim = $2 AND status = $4 AND waiting_reason = $5 \
             RETURNING coalesce(resolving_since + make_interval(secs => $6) <= now(), false) \
             AS expired",
            [
                Uuid::from(job).into(),
                token.into(),
                lease.as_secs_f64().into(),
                row::text(JOBS, JobStatus::Waiting)?.into(),
                row::text(JOBS, WaitingReason::Resolving)?.into(),
                give_up.as_secs_f64().into(),
            ],
        ))
        .await?;
    Ok(match renewed {
        None => Renewal::Lost,
        Some(found) if found.try_get::<bool>("", "expired")? => Renewal::Expired,
        Some(_) => Renewal::Held,
    })
}

pub async fn overdue<C: ConnectionTrait>(
    db: &C,
    job: Uuid,
    token: Uuid,
    give_up: Duration,
) -> Result<bool, Error> {
    let found = db
        .query_one(statement(
            "SELECT coalesce(resolving_since + make_interval(secs => $4) <= now(), false) \
             AS overdue FROM job WHERE id = $1 AND status = $2 AND waiting_reason = $3 \
             AND resolve_claim = $5",
            [
                job.into(),
                row::text(JOBS, JobStatus::Waiting)?.into(),
                row::text(JOBS, WaitingReason::Resolving)?.into(),
                give_up.as_secs_f64().into(),
                token.into(),
            ],
        ))
        .await?;
    found
        .map(|found| Ok(found.try_get("", "overdue")?))
        .transpose()
        .map(|overdue| overdue.unwrap_or(false))
}

pub async fn standing(
    transaction: &DatabaseTransaction,
    job: JobId,
    token: Uuid,
    give_up: Duration,
) -> Result<Option<Standing>, Error> {
    let found = transaction
        .query_one(statement(
            "SELECT o.action_policy, fi.account_login, \
             coalesce(j.resolving_since + make_interval(secs => $5) <= now(), false) AS expired \
             FROM job j JOIN attempt a ON a.id = j.attempt_id JOIN run r ON r.id = a.run_id \
             JOIN project p ON p.id = r.project_id \
             JOIN organisation o ON o.id = p.organisation_id \
             LEFT JOIN forge_installation fi ON fi.organisation_id = o.id \
             WHERE j.id = $1 AND j.status = $2 AND j.waiting_reason = $3 AND j.resolve_claim = $4",
            [
                Uuid::from(job).into(),
                row::text(JOBS, JobStatus::Waiting)?.into(),
                row::text(JOBS, WaitingReason::Resolving)?.into(),
                token.into(),
                give_up.as_secs_f64().into(),
            ],
        ))
        .await?;
    found
        .map(|found| {
            Ok(Standing {
                policy: row::json("organisation", found.try_get("", "action_policy")?)?,
                account: found.try_get("", "account_login")?,
                expired: found.try_get("", "expired")?,
            })
        })
        .transpose()
}

pub async fn retry(
    transaction: &DatabaseTransaction,
    job: impl Into<Uuid>,
    token: Uuid,
    first: Duration,
    longest: Duration,
) -> Result<(), Error> {
    let job: Uuid = job.into();
    transaction
        .execute(statement(
            "UPDATE job SET resolve_after = now() + make_interval(secs => \
             least($3 * power(2::float8, least(resolve_attempts, $5)), $4)), \
             resolve_attempts = resolve_attempts + 1, resolve_claim = NULL, \
             resolve_lease_until = NULL WHERE id = $1 AND resolve_claim = $2",
            [
                job.into(),
                token.into(),
                first.as_secs_f64().into(),
                longest.as_secs_f64().into(),
                MOST_DOUBLINGS.into(),
            ],
        ))
        .await?;
    Ok(())
}

pub async fn release(
    transaction: &DatabaseTransaction,
    job: JobId,
    token: Uuid,
) -> Result<(), Error> {
    transaction
        .execute(statement(
            "UPDATE job SET resolve_after = NULL, resolve_claim = NULL, \
             resolve_lease_until = NULL WHERE id = $1 AND resolve_claim = $2",
            [Uuid::from(job).into(), token.into()],
        ))
        .await?;
    Ok(())
}
