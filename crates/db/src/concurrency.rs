use bosun_types::id::{JobId, RunId};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, QueryResult};

use crate::row::statement;
use crate::runs::Locked;
use crate::{Error, row};

const TABLE: &str = "concurrency_member";

const COLUMNS: &str = "id, key, run_id, job_id, running";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: i64,
    pub key: String,
    pub run_id: RunId,
    pub job_id: Option<JobId>,
    pub running: bool,
}

impl Locked<'_> {
    pub async fn memberships(&self) -> Result<Vec<Member>, Error> {
        self.grouped()?;
        self.transaction()
            .query_all(statement(
                &format!("SELECT {COLUMNS} FROM concurrency_member WHERE run_id = $1 ORDER BY id"),
                [Uuid::from(self.state().id).into()],
            ))
            .await?
            .iter()
            .map(decode)
            .collect()
    }

    pub async fn members(&self, key: &str) -> Result<Vec<Member>, Error> {
        self.grouped()?;
        self.transaction()
            .query_all(statement(
                &format!(
                    "SELECT {COLUMNS} FROM concurrency_member WHERE project_id = $1 AND key = $2 \
                     ORDER BY id"
                ),
                [Uuid::from(self.state().project_id).into(), key.into()],
            ))
            .await?
            .iter()
            .map(decode)
            .collect()
    }

    pub async fn join(&self, key: &str, job: Option<JobId>, running: bool) -> Result<(), Error> {
        self.grouped()?;
        self.transaction()
            .execute(statement(
                "INSERT INTO concurrency_member (project_id, key, run_id, job_id, running) \
                 VALUES ($1, $2, $3, $4, $5)",
                [
                    Uuid::from(self.state().project_id).into(),
                    key.into(),
                    Uuid::from(self.state().id).into(),
                    job.map(Uuid::from).into(),
                    running.into(),
                ],
            ))
            .await?;
        Ok(())
    }

    pub async fn cancelled_by_group(&self) -> Result<bool, Error> {
        if !self.state().concurrency || !self.state().cancel_requested {
            return Ok(false);
        }
        let first = self
            .transaction()
            .query_one(statement(
                "SELECT kind FROM run_event WHERE run_id = $1 AND job_id IS NULL \
                 AND (kind = 'concurrency.cancelled' \
                 OR (kind = 'command.accepted' AND data -> 'command' ->> 'type' = 'run.cancel')) \
                 ORDER BY seq LIMIT 1",
                [Uuid::from(self.state().id).into()],
            ))
            .await?;
        match first {
            Some(first) => Ok(first.try_get::<String>("", "kind")? == "concurrency.cancelled"),
            None => Ok(false),
        }
    }

    pub async fn admit_member(&self, member: i64) -> Result<(), Error> {
        self.grouped()?;
        self.transaction()
            .execute(statement(
                "UPDATE concurrency_member SET running = true WHERE id = $1 AND project_id = $2",
                [member.into(), Uuid::from(self.state().project_id).into()],
            ))
            .await?;
        Ok(())
    }

    pub async fn leave(&self, member: i64) -> Result<(), Error> {
        self.grouped()?;
        self.transaction()
            .execute(statement(
                "DELETE FROM concurrency_member WHERE id = $1 AND project_id = $2",
                [member.into(), Uuid::from(self.state().project_id).into()],
            ))
            .await?;
        Ok(())
    }

    fn grouped(&self) -> Result<(), Error> {
        if self.state().concurrency {
            Ok(())
        } else {
            Err(Error::Row {
                table: "run",
                message: format!(
                    "run {} uses no concurrency group, so it was locked without the concurrency \
                     lock of its project",
                    self.state().id
                ),
            })
        }
    }
}

fn decode(found: &QueryResult) -> Result<Member, Error> {
    Ok(Member {
        id: found.try_get("", "id")?,
        key: found.try_get("", "key")?,
        run_id: row::id(TABLE, found.try_get::<Uuid>("", "run_id")?)?,
        job_id: row::optional_id(TABLE, found.try_get("", "job_id")?)?,
        running: found.try_get("", "running")?,
    })
}
