use bosun_db::Error;
use bosun_db::runs::{ExecutionState, Locked};
use bosun_types::actor::Actor;
use bosun_types::id::{AgentId, ExecutionId};
use bosun_types::status::{Conclusion, ExecutionStatus, StatusReason};
use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};

use crate::planner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Progress {
    Applied,
    Ignored(String),
}

#[derive(Debug, Clone)]
pub(crate) struct Reporter {
    pub(crate) id: AgentId,
    pub(crate) name: String,
}

impl Reporter {
    fn actor(&self) -> Actor {
        Actor::Agent {
            id: self.id,
            name: self.name.clone(),
        }
    }
}

pub(crate) struct StepEnd {
    pub(crate) index: u32,
    pub(crate) at: Option<DateTime<Utc>>,
    pub(crate) outcome: Conclusion,
    pub(crate) conclusion: Conclusion,
}

enum Opened<'a> {
    Found(Locked<'a>, ExecutionState),
    Ignored(String),
}

pub(crate) async fn accepted(
    db: &DatabaseConnection,
    agent: &Reporter,
    execution: ExecutionId,
) -> Result<Progress, Error> {
    let transaction = db.begin().await?;
    let (mut run, state) = match open(&transaction, agent, execution).await? {
        Opened::Found(run, state) => (run, state),
        Opened::Ignored(reason) => return Ok(Progress::Ignored(reason)),
    };
    if state.status != ExecutionStatus::Assigned {
        return Ok(Progress::Ignored(format!(
            "execution {execution} is not waiting for its agent to accept it"
        )));
    }
    run.start_execution(execution, &agent.actor()).await?;
    planner::plan(&mut run).await?;
    transaction.commit().await?;
    Ok(Progress::Applied)
}

pub(crate) async fn rejected(
    db: &DatabaseConnection,
    agent: &Reporter,
    execution: ExecutionId,
) -> Result<Progress, Error> {
    let transaction = db.begin().await?;
    let run = match open(&transaction, agent, execution).await? {
        Opened::Found(run, _) => run,
        Opened::Ignored(reason) => return Ok(Progress::Ignored(reason)),
    };
    let rejected = run.reject(execution, &agent.actor()).await;
    if let Some(reason) = tolerated(rejected)? {
        return Ok(Progress::Ignored(reason));
    }
    transaction.commit().await?;
    Ok(Progress::Applied)
}

pub(crate) async fn step_started(
    db: &DatabaseConnection,
    agent: &Reporter,
    execution: ExecutionId,
    index: u32,
    at: Option<DateTime<Utc>>,
) -> Result<Progress, Error> {
    let transaction = db.begin().await?;
    let run = match open(&transaction, agent, execution).await? {
        Opened::Found(run, _) => run,
        Opened::Ignored(reason) => return Ok(Progress::Ignored(reason)),
    };
    let started = run.start_step(execution, index, at, &agent.actor()).await;
    if let Some(reason) = tolerated(started)? {
        return Ok(Progress::Ignored(reason));
    }
    transaction.commit().await?;
    Ok(Progress::Applied)
}

pub(crate) async fn step_finished(
    db: &DatabaseConnection,
    agent: &Reporter,
    execution: ExecutionId,
    end: StepEnd,
) -> Result<Progress, Error> {
    let transaction = db.begin().await?;
    let run = match open(&transaction, agent, execution).await? {
        Opened::Found(run, _) => run,
        Opened::Ignored(reason) => return Ok(Progress::Ignored(reason)),
    };
    let finished = run
        .finish_step(
            execution,
            end.index,
            end.at,
            end.outcome,
            end.conclusion,
            &agent.actor(),
        )
        .await;
    if let Some(reason) = tolerated(finished)? {
        return Ok(Progress::Ignored(reason));
    }
    transaction.commit().await?;
    Ok(Progress::Applied)
}

pub(crate) async fn job_finished(
    db: &DatabaseConnection,
    agent: &Reporter,
    execution: ExecutionId,
    conclusion: Conclusion,
) -> Result<Progress, Error> {
    let transaction = db.begin().await?;
    let (mut run, state) = match open(&transaction, agent, execution).await? {
        Opened::Found(run, state) => (run, state),
        Opened::Ignored(reason) => return Ok(Progress::Ignored(reason)),
    };
    if state.status != ExecutionStatus::InProgress {
        return Ok(Progress::Ignored(format!(
            "execution {execution} is not in progress, so it cannot finish"
        )));
    }
    run.finish_execution(execution, conclusion, None, &agent.actor())
        .await?;
    planner::plan(&mut run).await?;
    transaction.commit().await?;
    Ok(Progress::Applied)
}

pub(crate) async fn lose_after_grace(
    db: &DatabaseConnection,
    execution: ExecutionId,
) -> Result<bool, Error> {
    lose(db, execution, None).await
}

pub(crate) async fn lose_forgotten(
    db: &DatabaseConnection,
    agent: AgentId,
    execution: ExecutionId,
) -> Result<bool, Error> {
    lose(db, execution, Some(agent)).await
}

async fn lose(
    db: &DatabaseConnection,
    execution: ExecutionId,
    returned: Option<AgentId>,
) -> Result<bool, Error> {
    let transaction = db.begin().await?;
    let Some((mut run, state)) = bosun_db::runs::lock_execution(&transaction, execution).await?
    else {
        return Ok(false);
    };
    let abandoned = bosun_db::agents::RUNNING.contains(&state.status)
        && match returned {
            None => !state.agent_online,
            Some(agent) => state.agent_id == agent,
        };
    if !abandoned {
        return Ok(false);
    }
    run.finish_execution(
        execution,
        Conclusion::Failure,
        Some(StatusReason::AgentLost),
        &Actor::System,
    )
    .await?;
    planner::plan(&mut run).await?;
    transaction.commit().await?;
    Ok(true)
}

async fn open<'a>(
    transaction: &'a DatabaseTransaction,
    agent: &Reporter,
    execution: ExecutionId,
) -> Result<Opened<'a>, Error> {
    let Some((run, state)) = bosun_db::runs::lock_execution(transaction, execution).await? else {
        return Ok(Opened::Ignored(format!(
            "there is no execution {execution}"
        )));
    };
    if state.agent_id != agent.id {
        return Ok(Opened::Ignored(format!(
            "execution {execution} belongs to another agent"
        )));
    }
    Ok(Opened::Found(run, state))
}

fn tolerated(result: Result<(), Error>) -> Result<Option<String>, Error> {
    match result {
        Ok(()) => Ok(None),
        Err(
            refused @ (Error::Transition { .. }
            | Error::NoStep { .. }
            | Error::NotInProgress { .. }),
        ) => Ok(Some(refused.to_string())),
        Err(failure) => Err(failure),
    }
}
