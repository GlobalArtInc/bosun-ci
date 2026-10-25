use std::time::Duration;

use bosun_types::id::{ExecutionId, RunId};
use sea_orm::DbErr;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Database(#[from] DbErr),
    #[error(transparent)]
    Migration(#[from] sqlmig::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("a row of {table} holds a value bosun-types does not accept: {message}")]
    Row {
        table: &'static str,
        message: String,
    },
    #[error("{0} does not exist; the migrations create it")]
    Missing(&'static str),
    #[error("a user with the login `{0}` already exists")]
    LoginTaken(String),
    #[error("an organisation with the slug `{0}` already exists")]
    OrganisationTaken(String),
    #[error("the organisation already has a project with the slug `{0}`")]
    ProjectTaken(String),
    #[error("the project already has a workflow named `{0}`")]
    WorkflowTaken(String),
    #[error("a pool named `{0}` already exists")]
    PoolTaken(String),
    #[error("{0} was removed while the request used it")]
    Vanished(String),
    #[error("could not hold the migration lock: {0}")]
    Lock(#[source] DbErr),
    #[error("could not hold the migration lock: another migration kept it for {0:?}")]
    LockTimeout(Duration),
    #[error(
        "the schema belongs to major version {found} of Bosun and this server is major version \
         {supported}, which never runs against a schema of a newer major version"
    )]
    NewerSchema { found: i32, supported: i32 },
    #[error("there is no execution {0}")]
    NoExecution(ExecutionId),
    #[error("{entity} {id} cannot go from `{from}` to `{to}`")]
    Transition {
        entity: &'static str,
        id: String,
        from: String,
        to: String,
    },
    #[error("{0} is not part of the current attempt of run {1}")]
    Outside(String, RunId),
    #[error("execution {execution} has no step {index}")]
    NoStep { execution: ExecutionId, index: u32 },
    #[error(
        "execution {execution} is `{status}`, and its steps change only while it is in progress"
    )]
    NotInProgress {
        execution: ExecutionId,
        status: String,
    },
}
