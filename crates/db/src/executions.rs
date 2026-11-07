use bosun_types::id::{AttemptId, ExecutionId};
use bosun_types::status::ExecutionStatus;
use sea_orm::ConnectionTrait;
use sea_orm::prelude::Uuid;

use crate::row::statement;
use crate::{Error, row};

pub async fn status_in_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: AttemptId,
    execution: ExecutionId,
) -> Result<Option<ExecutionStatus>, Error> {
    db.query_one(statement(
        "SELECT e.status FROM execution e JOIN job j ON j.id = e.job_id \
         WHERE e.id = $1 AND j.attempt_id = $2",
        [Uuid::from(execution).into(), Uuid::from(attempt).into()],
    ))
    .await?
    .map(|found| row::name("execution", found.try_get("", "status")?))
    .transpose()
}
