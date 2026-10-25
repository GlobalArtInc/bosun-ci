use bosun_types::status::{ExecutionStatus, JobStatus, RunStatus};
use chrono::{DateTime, Utc};
use sea_orm::{AccessMode, ConnectionTrait, DatabaseTransaction, IsolationLevel, TransactionTrait};

use crate::row::statement;
use crate::{Error, row};

const JOBS: &str = "job";

const AGENTS: &str = "agent";

const OUTBOX: &str = "outbox";

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub queue: Vec<Queued>,
    pub jobs: Vec<(String, u64)>,
    pub agents: Vec<Agents>,
    pub outbox: Vec<Outbox>,
    pub runs: Durations,
    pub jobs_that_ran: Durations,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Durations {
    pub completed: u64,
    pub percentiles: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    pub pool: String,
    pub labels: Vec<String>,
    pub jobs: u64,
    pub longest_wait: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agents {
    pub pool: String,
    pub state: String,
    pub drain: bool,
    pub agents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbox {
    pub kind: String,
    pub backlog: u64,
    pub dead: u64,
}

pub struct Recent<'a> {
    pub since: DateTime<Utc>,
    pub fractions: &'a [f64],
}

pub async fn read<C: TransactionTrait>(
    db: &C,
    now: DateTime<Utc>,
    recent: Recent<'_>,
) -> Result<Snapshot, Error> {
    let transaction = db
        .begin_with_config(
            Some(IsolationLevel::RepeatableRead),
            Some(AccessMode::ReadOnly),
        )
        .await?;
    let snapshot = Snapshot {
        queue: queue(&transaction, now).await?,
        jobs: jobs(&transaction).await?,
        agents: agents(&transaction).await?,
        outbox: outbox(&transaction).await?,
        runs: durations(
            &transaction,
            "run",
            row::text("run", RunStatus::Completed)?,
            &recent,
        )
        .await?,
        jobs_that_ran: durations(
            &transaction,
            JOBS,
            row::text(JOBS, JobStatus::Completed)?,
            &recent,
        )
        .await?,
    };
    transaction.commit().await?;
    Ok(snapshot)
}

async fn queue(
    transaction: &DatabaseTransaction,
    now: DateTime<Utc>,
) -> Result<Vec<Queued>, Error> {
    let found = transaction
        .query_all(statement(
            "SELECT coalesce(p.name, '') AS pool, q.labels, count(*) AS jobs, \
             greatest(extract(epoch FROM $1 - min(q.queued_at)), 0)::float8 AS longest_wait \
             FROM (SELECT j.queued_at, j.pool_ids, ARRAY(SELECT label FROM unnest(j.labels) \
             AS label ORDER BY label COLLATE \"C\") AS labels FROM job j WHERE j.status = $2) q \
             LEFT JOIN LATERAL unnest(q.pool_ids) AS placed(pool_id) ON true \
             LEFT JOIN pool p ON p.id = placed.pool_id \
             GROUP BY 1, 2 ORDER BY 1, 2",
            [now.into(), row::text(JOBS, JobStatus::Queued)?.into()],
        ))
        .await?;
    found
        .into_iter()
        .map(|found| {
            Ok(Queued {
                pool: found.try_get("", "pool")?,
                labels: found.try_get("", "labels")?,
                jobs: row::unsigned(JOBS, found.try_get("", "jobs")?)?,
                longest_wait: found
                    .try_get::<Option<f64>>("", "longest_wait")?
                    .unwrap_or_default(),
            })
        })
        .collect()
}

async fn jobs(transaction: &DatabaseTransaction) -> Result<Vec<(String, u64)>, Error> {
    let found = transaction
        .query_all(statement(
            "SELECT j.status, count(*) AS jobs FROM job j \
             WHERE NOT EXISTS (SELECT 1 FROM job m WHERE m.group_id = j.id) GROUP BY j.status",
            [],
        ))
        .await?;
    let mut counted: Vec<(String, u64)> = Vec::with_capacity(found.len());
    for found in found {
        counted.push((
            found.try_get("", "status")?,
            row::unsigned(JOBS, found.try_get("", "jobs")?)?,
        ));
    }
    JobStatus::ALL
        .into_iter()
        .map(|status| {
            let name = row::text(JOBS, status)?;
            let jobs = counted
                .iter()
                .find(|(counted, _)| *counted == name)
                .map_or(0, |(_, jobs)| *jobs);
            Ok((name, jobs))
        })
        .collect()
}

async fn agents(transaction: &DatabaseTransaction) -> Result<Vec<Agents>, Error> {
    let found = transaction
        .query_all(statement(
            "SELECT p.name AS pool, s.state, d.drain, count(a.id) AS agents FROM pool p \
             CROSS JOIN unnest(ARRAY['offline', 'idle', 'busy']) WITH ORDINALITY AS s(state, place) \
             CROSS JOIN unnest(ARRAY[false, true]) AS d(drain) \
             LEFT JOIN (SELECT a.id, a.pool_id, a.drain, CASE WHEN NOT a.online THEN 'offline' \
             WHEN EXISTS (SELECT 1 FROM execution e WHERE e.agent_id = a.id AND e.status <> $1) \
             THEN 'busy' ELSE 'idle' END AS state FROM agent a WHERE a.deleted_at IS NULL) a \
             ON a.pool_id = p.id AND a.state = s.state AND a.drain = d.drain \
             GROUP BY p.name, s.state, s.place, d.drain ORDER BY p.name COLLATE \"C\", s.place, d.drain",
            [row::text("execution", ExecutionStatus::Completed)?.into()],
        ))
        .await?;
    found
        .into_iter()
        .map(|found| {
            Ok(Agents {
                pool: found.try_get("", "pool")?,
                state: found.try_get("", "state")?,
                drain: found.try_get("", "drain")?,
                agents: row::unsigned(AGENTS, found.try_get("", "agents")?)?,
            })
        })
        .collect()
}

async fn outbox(transaction: &DatabaseTransaction) -> Result<Vec<Outbox>, Error> {
    let found = transaction
        .query_all(statement(
            "SELECT kind, count(*) FILTER (WHERE dead_at IS NULL) AS backlog, \
             count(*) FILTER (WHERE dead_at IS NOT NULL) AS dead FROM outbox \
             GROUP BY kind ORDER BY kind COLLATE \"C\"",
            [],
        ))
        .await?;
    found
        .into_iter()
        .map(|found| {
            Ok(Outbox {
                kind: found.try_get("", "kind")?,
                backlog: row::unsigned(OUTBOX, found.try_get("", "backlog")?)?,
                dead: row::unsigned(OUTBOX, found.try_get("", "dead")?)?,
            })
        })
        .collect()
}

async fn durations(
    transaction: &DatabaseTransaction,
    table: &'static str,
    completed: String,
    recent: &Recent<'_>,
) -> Result<Durations, Error> {
    let found = transaction
        .query_one(statement(
            &format!(
                "SELECT count(*) AS completed, percentile_cont($3::float8[]) WITHIN GROUP \
                 (ORDER BY extract(epoch FROM completed_at - started_at)::float8) AS percentiles \
                 FROM {table} WHERE status = $1 AND started_at IS NOT NULL AND completed_at > $2"
            ),
            [
                completed.into(),
                recent.since.into(),
                recent.fractions.to_vec().into(),
            ],
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("the durations of the table {table}")))?;
    Ok(Durations {
        completed: row::unsigned(table, found.try_get("", "completed")?)?,
        percentiles: found
            .try_get::<Option<Vec<f64>>>("", "percentiles")?
            .unwrap_or_default(),
    })
}
