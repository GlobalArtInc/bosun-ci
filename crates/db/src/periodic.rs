use sea_orm::{ConnectionTrait, DatabaseTransaction};

use crate::Error;
use crate::row::statement;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Task {
    AgentSweep,
    LeaseExpiry,
    QueueTimeout,
    LogCompaction,
}

impl Task {
    fn key(self) -> i64 {
        i64::from_be_bytes(match self {
            Task::AgentSweep => *b"bosunagt",
            Task::LeaseExpiry => *b"bosunlse",
            Task::QueueTimeout => *b"bosunqto",
            Task::LogCompaction => *b"bosunlog",
        })
    }
}

pub(crate) async fn try_lock(transaction: &DatabaseTransaction, task: Task) -> Result<bool, Error> {
    Ok(transaction
        .query_one(statement(
            "SELECT pg_try_advisory_xact_lock($1) AS locked",
            [task.key().into()],
        ))
        .await?
        .map(|row| row.try_get::<bool>("", "locked"))
        .transpose()?
        .unwrap_or(false))
}
