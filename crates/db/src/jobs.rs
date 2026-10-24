use bosun_types::actor::Actor;
use bosun_types::event::{
    Event, ExecutionCreated, ExecutionStatusChanged, JobStatusChanged, StepCreated,
    StepStatusChanged,
};
use bosun_types::id::{AgentId, ExecutionId, JobId, PoolId, StepId};
use bosun_types::ir::{self, Action, JobKind, Template};
use bosun_types::key::JobKey;
use bosun_types::status::{
    Conclusion, ConclusionSource, ExecutionStatus, JobStatus, StatusReason, StepStatus,
};
use sea_orm::ConnectionTrait;
use sea_orm::prelude::{DateTimeUtc, Uuid};

use crate::row::statement;
use crate::runs::{Locked, refuse};
use crate::{Error, row};

const JOBS: &str = "job";

const EXECUTIONS: &str = "execution";

const STEPS: &str = "step";

struct StepEnd {
    outcome: Conclusion,
    conclusion: Conclusion,
    at: Option<DateTimeUtc>,
}

impl Locked<'_> {
    pub async fn queue_job(
        &self,
        job: JobId,
        labels: &[String],
        pools: &[PoolId],
    ) -> Result<(), Error> {
        let (from, _) = self.job(job).await?;
        if !from.can_transition(JobStatus::Queued) {
            return Err(refuse("job", job, from, JobStatus::Queued));
        }
        let mut lowered: Vec<String> = Vec::with_capacity(labels.len());
        for label in labels {
            let label = label.to_lowercase();
            if !lowered.contains(&label) {
                lowered.push(label);
            }
        }
        let pools: Vec<Uuid> = pools.iter().copied().map(Uuid::from).collect();
        self.transaction()
            .execute(statement(
                "UPDATE job SET status = $2, waiting_reason = NULL, labels = $3, pool_ids = $4, \
                 queued_at = now() WHERE id = $1",
                [
                    Uuid::from(job).into(),
                    row::text(JOBS, JobStatus::Queued)?.into(),
                    lowered.into(),
                    pools.into(),
                ],
            ))
            .await?;
        self.append(
            Some(job),
            None,
            None,
            &Actor::System,
            Event::JobStatusChanged(JobStatusChanged {
                from,
                to: JobStatus::Queued,
                waiting_reason: None,
                conclusion: None,
                conclusion_source: None,
                reason: None,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn complete_job(
        &self,
        job: JobId,
        conclusion: Conclusion,
        reason: Option<StatusReason>,
        actor: &Actor,
    ) -> Result<(), Error> {
        let (from, _) = self.job(job).await?;
        if !matches!(
            from,
            JobStatus::Pending | JobStatus::Waiting | JobStatus::Queued
        ) {
            return Err(refuse("job", job, from, JobStatus::Completed));
        }
        self.close_job(job, from, conclusion, reason, actor).await
    }

    pub async fn assign_job(&self, job: JobId, agent: AgentId) -> Result<ExecutionId, Error> {
        let (from, _) = self.job(job).await?;
        if from != JobStatus::Queued {
            return Err(refuse("job", job, from, JobStatus::Assigned));
        }
        let execution: ExecutionId = row::id(EXECUTIONS, Uuid::now_v7())?;
        let created = self
            .transaction()
            .query_one(statement(
                "INSERT INTO execution (id, job_id, number, agent_id, status) \
                 SELECT $1, $2, coalesce(max(number), 0) + 1, $3, $4 FROM execution \
                 WHERE job_id = $2 RETURNING number",
                [
                    Uuid::from(execution).into(),
                    Uuid::from(job).into(),
                    Uuid::from(agent).into(),
                    row::text(EXECUTIONS, ExecutionStatus::Assigned)?.into(),
                ],
            ))
            .await?
            .ok_or_else(|| Error::Vanished(format!("execution {execution}")))?;
        let number: i32 = created.try_get("", "number")?;
        self.set_job(job, JobStatus::Assigned).await?;
        self.append(
            Some(job),
            Some(execution),
            None,
            &Actor::System,
            Event::ExecutionCreated(ExecutionCreated {
                number: u32::try_from(number).map_err(|error| Error::Row {
                    table: EXECUTIONS,
                    message: error.to_string(),
                })?,
                agent_id: agent,
            }),
        )
        .await?;
        self.append(
            Some(job),
            Some(execution),
            None,
            &Actor::System,
            job_moved(from, JobStatus::Assigned),
        )
        .await?;
        Ok(execution)
    }

    pub async fn start_execution(
        &self,
        execution: ExecutionId,
        actor: &Actor,
    ) -> Result<(), Error> {
        let (job, from) = self.execution(execution).await?;
        if from != ExecutionStatus::Assigned {
            return Err(refuse(
                "execution",
                execution,
                from,
                ExecutionStatus::InProgress,
            ));
        }
        let (job_from, key) = self.job(job).await?;
        if job_from != JobStatus::Assigned {
            return Err(refuse("job", job, job_from, JobStatus::InProgress));
        }
        let steps = self.steps_of(&key).await?;
        self.transaction()
            .execute(statement(
                "UPDATE execution SET status = $2, started_at = now() WHERE id = $1",
                [
                    Uuid::from(execution).into(),
                    row::text(EXECUTIONS, ExecutionStatus::InProgress)?.into(),
                ],
            ))
            .await?;
        self.transaction()
            .execute(statement(
                "UPDATE job SET status = $2, started_at = coalesce(started_at, now()) \
                 WHERE id = $1",
                [
                    Uuid::from(job).into(),
                    row::text(JOBS, JobStatus::InProgress)?.into(),
                ],
            ))
            .await?;
        self.append(
            Some(job),
            Some(execution),
            None,
            actor,
            Event::ExecutionStatusChanged(ExecutionStatusChanged {
                from,
                to: ExecutionStatus::InProgress,
                conclusion: None,
                reason: None,
            }),
        )
        .await?;
        self.append(
            Some(job),
            Some(execution),
            None,
            actor,
            job_moved(job_from, JobStatus::InProgress),
        )
        .await?;
        let pending = row::text(STEPS, StepStatus::Pending)?;
        for (index, step) in steps.iter().enumerate() {
            let index = u32::try_from(index).map_err(|error| Error::Row {
                table: STEPS,
                message: error.to_string(),
            })?;
            let id: StepId = row::id(STEPS, Uuid::now_v7())?;
            let name = step_name(step);
            self.transaction()
                .execute(statement(
                    "INSERT INTO step (id, execution_id, index, name, status) \
                     VALUES ($1, $2, $3, $4, $5)",
                    [
                        Uuid::from(id).into(),
                        Uuid::from(execution).into(),
                        i64::from(index).into(),
                        name.clone().into(),
                        pending.clone().into(),
                    ],
                ))
                .await?;
            self.append(
                Some(job),
                Some(execution),
                Some(id),
                actor,
                Event::StepCreated(StepCreated { index, name }),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn start_step(
        &self,
        execution: ExecutionId,
        index: u32,
        at: Option<DateTimeUtc>,
        actor: &Actor,
    ) -> Result<(), Error> {
        let job = self.running(execution).await?;
        let (step, from) = self.step(execution, index).await?;
        if from != StepStatus::Pending {
            return Err(refuse("step", step, from, StepStatus::InProgress));
        }
        self.transaction()
            .execute(statement(
                "UPDATE step SET status = $2, started_at = coalesce($3, now()) WHERE id = $1",
                [
                    Uuid::from(step).into(),
                    row::text(STEPS, StepStatus::InProgress)?.into(),
                    at.into(),
                ],
            ))
            .await?;
        self.append(
            Some(job),
            Some(execution),
            Some(step),
            actor,
            Event::StepStatusChanged(StepStatusChanged {
                from,
                to: StepStatus::InProgress,
                outcome: None,
                conclusion: None,
                conclusion_source: None,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn finish_step(
        &self,
        execution: ExecutionId,
        index: u32,
        at: Option<DateTimeUtc>,
        outcome: Conclusion,
        conclusion: Conclusion,
        actor: &Actor,
    ) -> Result<(), Error> {
        let job = self.running(execution).await?;
        let (step, from) = self.step(execution, index).await?;
        if !matches!(from, StepStatus::Pending | StepStatus::InProgress) {
            return Err(refuse("step", step, from, StepStatus::Completed));
        }
        self.close_step(
            (job, execution, step),
            from,
            StepEnd {
                outcome,
                conclusion,
                at,
            },
            actor,
        )
        .await
    }

    pub async fn finish_execution(
        &self,
        execution: ExecutionId,
        conclusion: Conclusion,
        reason: Option<StatusReason>,
        actor: &Actor,
    ) -> Result<(), Error> {
        let (job, from) = self.execution(execution).await?;
        if !from.can_transition(ExecutionStatus::Completed) {
            return Err(refuse(
                "execution",
                execution,
                from,
                ExecutionStatus::Completed,
            ));
        }
        let open = self
            .transaction()
            .query_all(statement(
                "SELECT id, status FROM step WHERE execution_id = $1 AND status <> $2 \
                 ORDER BY index",
                [
                    Uuid::from(execution).into(),
                    row::text(STEPS, StepStatus::Completed)?.into(),
                ],
            ))
            .await?;
        for found in open {
            let step: StepId = row::id(STEPS, found.try_get::<Uuid>("", "id")?)?;
            let status: StepStatus = row::name(STEPS, found.try_get("", "status")?)?;
            let ended = if status == StepStatus::Pending {
                Conclusion::Skipped
            } else {
                Conclusion::Cancelled
            };
            self.close_step(
                (job, execution, step),
                status,
                StepEnd {
                    outcome: ended,
                    conclusion: ended,
                    at: None,
                },
                actor,
            )
            .await?;
        }
        self.transaction()
            .execute(statement(
                "UPDATE execution SET status = $2, conclusion = $3, reason = $4, \
                 completed_at = now() WHERE id = $1",
                [
                    Uuid::from(execution).into(),
                    row::text(EXECUTIONS, ExecutionStatus::Completed)?.into(),
                    row::text(EXECUTIONS, conclusion)?.into(),
                    reason
                        .map(|reason| row::text(EXECUTIONS, reason))
                        .transpose()?
                        .into(),
                ],
            ))
            .await?;
        self.append(
            Some(job),
            Some(execution),
            None,
            actor,
            Event::ExecutionStatusChanged(ExecutionStatusChanged {
                from,
                to: ExecutionStatus::Completed,
                conclusion: Some(conclusion),
                reason,
            }),
        )
        .await?;
        let (job_from, _) = self.job(job).await?;
        if matches!(
            job_from,
            JobStatus::Assigned | JobStatus::InProgress | JobStatus::Paused | JobStatus::Held
        ) {
            self.close_job(job, job_from, conclusion, reason, actor)
                .await?;
        }
        Ok(())
    }

    async fn close_job(
        &self,
        job: JobId,
        from: JobStatus,
        conclusion: Conclusion,
        reason: Option<StatusReason>,
        actor: &Actor,
    ) -> Result<(), Error> {
        self.transaction()
            .execute(statement(
                "UPDATE job SET status = $2, waiting_reason = NULL, conclusion = $3, \
                 conclusion_source = $4, completed_at = now() WHERE id = $1",
                [
                    Uuid::from(job).into(),
                    row::text(JOBS, JobStatus::Completed)?.into(),
                    row::text(JOBS, conclusion)?.into(),
                    row::text(JOBS, ConclusionSource::Natural)?.into(),
                ],
            ))
            .await?;
        self.append(
            Some(job),
            None,
            None,
            actor,
            Event::JobStatusChanged(JobStatusChanged {
                from,
                to: JobStatus::Completed,
                waiting_reason: None,
                conclusion: Some(conclusion),
                conclusion_source: Some(ConclusionSource::Natural),
                reason,
            }),
        )
        .await?;
        Ok(())
    }

    async fn close_step(
        &self,
        (job, execution, step): (JobId, ExecutionId, StepId),
        from: StepStatus,
        end: StepEnd,
        actor: &Actor,
    ) -> Result<(), Error> {
        let StepEnd {
            outcome,
            conclusion,
            at,
        } = end;
        self.transaction()
            .execute(statement(
                "UPDATE step SET status = $2, outcome = $3, conclusion = $4, \
                 conclusion_source = $5, completed_at = coalesce($6, now()) WHERE id = $1",
                [
                    Uuid::from(step).into(),
                    row::text(STEPS, StepStatus::Completed)?.into(),
                    row::text(STEPS, outcome)?.into(),
                    row::text(STEPS, conclusion)?.into(),
                    row::text(STEPS, ConclusionSource::Natural)?.into(),
                    at.into(),
                ],
            ))
            .await?;
        self.append(
            Some(job),
            Some(execution),
            Some(step),
            actor,
            Event::StepStatusChanged(StepStatusChanged {
                from,
                to: StepStatus::Completed,
                outcome: Some(outcome),
                conclusion: Some(conclusion),
                conclusion_source: Some(ConclusionSource::Natural),
            }),
        )
        .await?;
        Ok(())
    }

    async fn set_job(&self, job: JobId, to: JobStatus) -> Result<(), Error> {
        self.transaction()
            .execute(statement(
                "UPDATE job SET status = $2 WHERE id = $1",
                [Uuid::from(job).into(), row::text(JOBS, to)?.into()],
            ))
            .await?;
        Ok(())
    }

    async fn job(&self, job: JobId) -> Result<(JobStatus, JobKey), Error> {
        let found = self
            .transaction()
            .query_one(statement(
                "SELECT status, key FROM job WHERE id = $1 AND attempt_id = $2",
                [
                    Uuid::from(job).into(),
                    Uuid::from(self.state().attempt_id).into(),
                ],
            ))
            .await?
            .ok_or_else(|| Error::Outside(format!("job {job}"), self.state().id))?;
        Ok((
            row::name(JOBS, found.try_get("", "status")?)?,
            row::name(JOBS, found.try_get("", "key")?)?,
        ))
    }

    async fn execution(&self, execution: ExecutionId) -> Result<(JobId, ExecutionStatus), Error> {
        let found = self
            .transaction()
            .query_one(statement(
                "SELECT e.job_id, e.status FROM execution e JOIN job j ON j.id = e.job_id \
                 WHERE e.id = $1 AND j.attempt_id = $2",
                [
                    Uuid::from(execution).into(),
                    Uuid::from(self.state().attempt_id).into(),
                ],
            ))
            .await?
            .ok_or_else(|| Error::Outside(format!("execution {execution}"), self.state().id))?;
        Ok((
            row::id(EXECUTIONS, found.try_get::<Uuid>("", "job_id")?)?,
            row::name(EXECUTIONS, found.try_get("", "status")?)?,
        ))
    }

    async fn running(&self, execution: ExecutionId) -> Result<JobId, Error> {
        let (job, status) = self.execution(execution).await?;
        if status != ExecutionStatus::InProgress {
            return Err(Error::NotInProgress {
                execution,
                status: row::text(EXECUTIONS, status)?,
            });
        }
        Ok(job)
    }

    async fn step(
        &self,
        execution: ExecutionId,
        index: u32,
    ) -> Result<(StepId, StepStatus), Error> {
        let found = self
            .transaction()
            .query_one(statement(
                "SELECT id, status FROM step WHERE execution_id = $1 AND index = $2",
                [Uuid::from(execution).into(), i64::from(index).into()],
            ))
            .await?
            .ok_or(Error::NoStep { execution, index })?;
        Ok((
            row::id(STEPS, found.try_get::<Uuid>("", "id")?)?,
            row::name(STEPS, found.try_get("", "status")?)?,
        ))
    }

    async fn steps_of(&self, key: &JobKey) -> Result<Vec<ir::Step>, Error> {
        let workflow = self.ir().await?;
        let template = workflow
            .jobs
            .into_iter()
            .find(|template| &template.key == key)
            .ok_or_else(|| Error::Row {
                table: "run",
                message: format!("the IR of the run has no job `{key}`"),
            })?;
        match template.kind {
            JobKind::Steps(steps) => Ok(steps),
        }
    }
}

fn job_moved(from: JobStatus, to: JobStatus) -> Event {
    Event::JobStatusChanged(JobStatusChanged {
        from,
        to,
        waiting_reason: None,
        conclusion: None,
        conclusion_source: None,
        reason: None,
    })
}

fn step_name(step: &ir::Step) -> String {
    if let Some(name) = step.name.as_ref().and_then(Template::as_literal) {
        return name;
    }
    match &step.action {
        Action::Run { script } => {
            let script = script.as_literal().unwrap_or_default();
            match script.lines().map(str::trim).find(|line| !line.is_empty()) {
                Some(line) => format!("Run {line}"),
                None => "Run".to_owned(),
            }
        }
    }
}
