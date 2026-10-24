use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use bosun_db::control::{self, Kind};
use bosun_db::runs::{JobState, Locked};
use bosun_types::actor::Actor;
use bosun_types::command::{Command, CommandRequest};
use bosun_types::error::ErrorCode;
use bosun_types::event::{CommandAccepted, CommandRefused, Event, RunEvent};
use bosun_types::id::RunId;
use bosun_types::status::{Conclusion, JobStatus, RunStatus, StatusReason};
use sea_orm::TransactionTrait;

use crate::auth::Principal;
use crate::envelope::ApiError;
use crate::request::{Body, Faults, Segment, database};
use crate::{AppState, idempotency, planner};

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/runs/{run}/commands", post(command))
}

#[derive(Debug)]
pub(crate) enum Applied {
    Accepted(Box<RunEvent>),
    Refused(String),
}

enum Target<'a> {
    Run,
    Job(&'a JobState),
}

async fn command(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Segment(reference): Segment,
    body: Result<Body<CommandRequest>, ApiError>,
) -> Result<Response, ApiError> {
    let missing = || {
        ApiError::new(
            ErrorCode::NotFound,
            format!("there is no run `{reference}`"),
        )
    };
    let run_id: RunId = reference.parse().map_err(|_| missing())?;
    let transaction = state.db.begin().await.map_err(database)?;
    let Some(mut run) = bosun_db::runs::lock(&transaction, run_id)
        .await
        .map_err(database)?
    else {
        return Err(missing());
    };
    let body = body?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    match apply(&mut run, &principal.actor(), &body.value).await? {
        Applied::Accepted(event) => {
            idempotency::commit(
                &state.db,
                transaction,
                body.idempotency.as_ref(),
                StatusCode::CREATED,
                &event,
            )
            .await
        }
        Applied::Refused(message) => {
            transaction.commit().await.map_err(database)?;
            Err(ApiError::new(ErrorCode::InvalidState, message))
        }
    }
}

pub(crate) async fn apply(
    run: &mut Locked<'_>,
    actor: &Actor,
    request: &CommandRequest,
) -> Result<Applied, ApiError> {
    let jobs = run.jobs().await.map_err(database)?;
    match target(run, &jobs, request)? {
        Target::Run => {
            if run.state().status == RunStatus::Completed {
                return refuse(
                    run,
                    None,
                    actor,
                    request,
                    format!(
                        "the run is completed; `{}` needs a run that is not completed",
                        name(&request.command)
                    ),
                )
                .await;
            }
            let accepted = accept(run, None, actor, request).await?;
            run.request_cancel().await.map_err(database)?;
            for job in &jobs {
                cancel(run, job, actor).await.map_err(database)?;
            }
            planner::plan(run).await.map_err(database)?;
            Ok(Applied::Accepted(Box::new(accepted)))
        }
        Target::Job(job) => {
            if job.status == JobStatus::Completed {
                return refuse(
                    run,
                    Some(job),
                    actor,
                    request,
                    format!(
                        "job `{}` is completed; `{}` needs a job that is not completed",
                        job.key,
                        name(&request.command)
                    ),
                )
                .await;
            }
            let accepted = accept(run, Some(job), actor, request).await?;
            cancel(run, job, actor).await.map_err(database)?;
            planner::plan(run).await.map_err(database)?;
            Ok(Applied::Accepted(Box::new(accepted)))
        }
    }
}

fn target<'a>(
    run: &Locked<'_>,
    jobs: &'a [JobState],
    request: &CommandRequest,
) -> Result<Target<'a>, ApiError> {
    let command = name(&request.command);
    let mut faults = Faults::default();
    let current = run.state().attempt;
    if request
        .target
        .attempt
        .is_some_and(|attempt| attempt != current)
    {
        faults.add(
            "target.attempt",
            format!("the run is on attempt {current}, and `{command}` acts on its current attempt"),
        );
    }
    if request.target.step.is_some() {
        faults.add("target.step", format!("`{command}` acts on no step"));
    }
    let target = match (&request.command, request.target.job) {
        (Command::RunCancel, None) => Some(Target::Run),
        (Command::RunCancel, Some(_)) => {
            faults.add(
                "target.job",
                format!("`{command}` acts on the whole run and names no job"),
            );
            None
        }
        (Command::JobCancel, None) => {
            faults.add(
                "target.job",
                format!("`{command}` needs the job it acts on"),
            );
            None
        }
        (Command::JobCancel, Some(id)) => match jobs.iter().find(|job| job.id == id) {
            Some(job) => Some(Target::Job(job)),
            None => {
                faults.add(
                    "target.job",
                    format!("the current attempt of the run has no job {id}"),
                );
                None
            }
        },
    };
    match target {
        Some(target) => {
            faults.finish("the command is not valid")?;
            Ok(target)
        }
        None => Err(faults.refuse("the command is not valid")),
    }
}

async fn accept(
    run: &Locked<'_>,
    job: Option<&JobState>,
    actor: &Actor,
    request: &CommandRequest,
) -> Result<RunEvent, ApiError> {
    run.record(
        job.map(|job| job.id),
        actor,
        Event::CommandAccepted(CommandAccepted {
            command: request.clone(),
        }),
    )
    .await
    .map_err(database)
}

async fn refuse(
    run: &Locked<'_>,
    job: Option<&JobState>,
    actor: &Actor,
    request: &CommandRequest,
    message: String,
) -> Result<Applied, ApiError> {
    run.record(
        job.map(|job| job.id),
        actor,
        Event::CommandRefused(CommandRefused {
            command: request.clone(),
            code: ErrorCode::InvalidState,
            message: message.clone(),
        }),
    )
    .await
    .map_err(database)?;
    Ok(Applied::Refused(message))
}

async fn cancel(run: &Locked<'_>, job: &JobState, actor: &Actor) -> Result<(), bosun_db::Error> {
    match job.status {
        JobStatus::Completed => Ok(()),
        JobStatus::Pending | JobStatus::Waiting | JobStatus::Queued => {
            run.complete_job(
                job.id,
                Conclusion::Cancelled,
                Some(StatusReason::Command),
                actor,
            )
            .await
        }
        JobStatus::Assigned | JobStatus::InProgress | JobStatus::Paused | JobStatus::Held => {
            let execution = job.execution.as_ref().ok_or_else(|| bosun_db::Error::Row {
                table: "job",
                message: format!("job {} is on an agent without an execution", job.id),
            })?;
            if job.status == JobStatus::Assigned {
                run.finish_execution(
                    execution.id,
                    Conclusion::Cancelled,
                    Some(StatusReason::Command),
                    actor,
                )
                .await?;
            }
            control::publish(run.transaction(), execution.id, Kind::Cancel).await?;
            Ok(())
        }
    }
}

fn name(command: &Command) -> &'static str {
    match command {
        Command::RunCancel => "run.cancel",
        Command::JobCancel => "job.cancel",
    }
}
